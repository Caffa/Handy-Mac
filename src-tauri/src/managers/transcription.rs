use crate::audio_toolkit::process_transcription_text;
use crate::errors::{AppError, AppResult};
use crate::managers::audio::AudioRecordingManager;
use crate::managers::model::{EngineType, ModelManager};
use crate::settings::{
    get_settings, AppSettings, ModelUnloadTimeout, OrtAcceleratorSetting,
    TranscribeAcceleratorSetting,
};
use anyhow::Result;
use log::{debug, error, info, warn};
use parking_lot::{Condvar as ParkingLotCondvar, Mutex as ParkingLotMutex};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime};
use tauri::{AppHandle, Emitter, Manager};
use tauri_specta::Event;
use transcribe_cpp::{
    Backend, Feature, Model, ModelOptions, RunExtension, RunOptions, Session, StreamOptions, Task,
    WhisperRunOptions,
};
use transcribe_rs::{
    onnx::{
        canary::CanaryModel,
        cohere::CohereModel,
        gigaam::GigaAMModel,
        moonshine::{MoonshineModel, MoonshineVariant, StreamingModel},
        parakeet::{ParakeetModel, ParakeetParams, TimestampGranularity},
        sense_voice::{SenseVoiceModel, SenseVoiceParams},
        Quantization,
    },
    SpeechModel, TranscribeOptions,
};

const STREAM_PERF_LOG_INTERVAL: Duration = Duration::from_secs(5);
const STREAM_FINALIZE_REPLY_TIMEOUT: Duration = Duration::from_secs(30);

/// Number of consecutive non-panic engine failures before we treat the
/// loaded engine as wedged and force a full reload. The panic path already
/// drops the engine on the first panic; this threshold lets a single transient
/// error recover without an expensive reload.
const ENGINE_HEALTH_RELOAD_THRESHOLD: u32 = 2;

/// Maximum time a caller will wait for an in-progress transcription to finish
/// before declaring the slot busy. The watchdog below is independent and much
/// larger to avoid killing legitimate long transcriptions.
pub(crate) const TRANSCRIBING_SLOT_WAIT_TIMEOUT: Duration = Duration::from_secs(30);

/// If the wait timeout above expires and the current owner has held the slot
/// for at least this long, the waiter force-releases the slot once and retries
/// acquisition. This recovers when a streaming/batch worker wedges inside the
/// engine call and the user tries to start a new recording shortly after.
/// Set equal to the wait timeout so we only recover from holds that were
/// already stale when the waiter arrived.
const TRANSCRIBING_SLOT_FORCE_RELEASE_ON_OWN_TIMEOUT: Duration = TRANSCRIBING_SLOT_WAIT_TIMEOUT;

/// If a transcription claims the `is_transcribing` slot for longer than this
/// and the owner did not release it, the next waiter force-releases it. This
/// recovers from worker threads that wedge without clearing the flag (e.g. a
/// panic that is swallowed or a GPU callback that never returns). Keep this
/// large — it only has to be shorter than the forever-hang it replaces.
const TRANSCRIBING_SLOT_FORCE_RELEASE_AFTER: Duration = Duration::from_secs(60);

/// Background idle-watchdog threshold for force-releasing a stuck
/// `is_transcribing` slot. This must be significantly longer than any realistic
/// final transcription, because the watcher runs while *not* recording and a long
/// final transcription also runs after `is_recording` becomes false. The waiter
/// watchdog above handles active recovery; this one only catches the case
/// where the slot is left stuck during an idle gap.
const TRANSCRIBING_SLOT_BG_FORCE_RELEASE_AFTER: Duration = Duration::from_secs(300);

/// Maximum time `transcribe_internal` will wait for a model load to finish.
/// Prevents a wedged loader thread from blocking the final-transcription path
/// indefinitely; if this fires we defensively clear the flag and fail fast.
const MODEL_LOAD_WAIT_TIMEOUT: Duration = Duration::from_secs(60);

fn panic_payload_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "unknown panic".to_string()
    }
}

/// Optional text cleanup must never discard a successful model result. The
/// transform is pure and owns its input, so recovering the untouched text is
/// safe even if a bug in custom-word or filler filtering unwinds.
fn fail_open_text_transform<F>(raw: String, transform: F) -> String
where
    F: FnOnce(String) -> String,
{
    let fallback = raw.clone();
    match catch_unwind(AssertUnwindSafe(|| transform(raw))) {
        Ok(processed) => processed,
        Err(payload) => {
            error!(
                "Optional transcription text post-processing panicked: {}; using the raw transcription",
                panic_payload_message(payload.as_ref())
            );
            fallback
        }
    }
}

/// Result of a transcription call, including metadata about which model was used.
#[derive(Clone, Debug, Serialize)]
pub struct TranscriptionOutput {
    /// The transcribed text.
    pub text: String,
    /// The model ID that produced this transcription (e.g. "turbo", "parakeet-tdt-0.6b-v2").
    pub model_id: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ModelStateEvent {
    pub event_type: String,
    pub model_id: Option<String>,
    pub model_name: Option<String>,
    pub error: Option<String>,
}

/// Live transcription snapshot emitted to the overlay during a streaming run.
/// `committed` is the append-only, flicker-free prefix; `tentative` is the
/// volatile suffix the model may still rewrite.
#[derive(Clone, Debug, Serialize, Deserialize, Type, tauri_specta::Event)]
pub struct StreamTextEvent {
    pub committed: String,
    pub tentative: String,
}

/// Phase of the streaming overlay card, emitted to drive its UI state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "lowercase")]
pub enum StreamPhase {
    /// Receiving audio / live text (or waiting for the stream to begin). Rust
    /// does not emit this today; the frontend starts in this phase and Rust only
    /// emits transitions away from it.
    Listening,
    /// Finalizing or post-processing — show a spinner.
    Working,
}

/// Semantic kind of "working" phase, used to localize the spinner label.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "lowercase")]
pub enum StreamWorkKind {
    Transcribing,
    Polishing,
}

/// Emitted to switch the streaming overlay to a working spinner.
#[derive(Clone, Debug, Serialize, Deserialize, Type, tauri_specta::Event)]
pub struct StreamPhaseEvent {
    pub phase: StreamPhase,
    /// Present only when `phase` is `Working`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<StreamWorkKind>,
}

/// Commands sent to the streaming worker thread. Audio frames and the finalize
/// request travel the same channel so FIFO ordering guarantees every fed frame
/// is processed before finalize runs.
enum StreamCmd {
    Feed(Vec<f32>),
    /// Flush the stream and reply with the final text, or `None` if no stream
    /// was ever active (caller should fall back to batch transcription).
    Finalize(mpsc::Sender<Option<String>>),
    Cancel,
}

/// Routes real-time audio frames to the active streaming worker. Shared between
/// the [`TranscriptionManager`] (opens/closes the route) and the audio recorder's
/// per-frame callback (feeds frames). The recorder holds an `Arc<StreamRouter>`
/// directly, so a frame with no stream pending costs a single relaxed atomic
/// load — no Tauri state lookup, no mutex lock.
pub struct StreamRouter {
    /// Command channel to the active streaming worker, present from
    /// `start_stream` until `finalize_stream`/`cancel_stream`.
    tx: Mutex<Option<mpsc::Sender<StreamCmd>>>,
    /// True while a stream is pending or active (channel is open). The audio
    /// callback checks this first to avoid the mutex lock when no stream runs.
    open: Arc<AtomicBool>,
}

impl StreamRouter {
    fn new() -> Self {
        Self {
            tx: Mutex::new(None),
            open: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Open a fresh command channel for a new streaming session, returning the
    /// receiver the worker should drain. Caller must ensure no prior channel is
    /// still open.
    fn open(&self) -> mpsc::Receiver<StreamCmd> {
        let (tx, rx) = mpsc::channel::<StreamCmd>();
        *self.tx.lock().unwrap() = Some(tx);
        self.open.store(true, Ordering::Relaxed);
        rx
    }

    /// Take the sender out (closing the channel to new feeds). Returns the
    /// sender so the caller can send the final `Finalize`/`Cancel` command.
    fn take(&self) -> Option<mpsc::Sender<StreamCmd>> {
        self.open.store(false, Ordering::Relaxed);
        self.tx.lock().unwrap().take()
    }

    /// Drop the channel and mark closed without sending a final command (used
    /// when the worker exits without a finalize/cancel handshake).
    fn clear(&self) {
        self.open.store(false, Ordering::Relaxed);
        *self.tx.lock().unwrap() = None;
    }

    /// Forward a 16 kHz frame to the active streaming worker. Cheap no-op (a
    /// single relaxed atomic load) when no stream is pending.
    pub fn feed(&self, frame: &[f32]) {
        if !self.open.load(Ordering::Relaxed) {
            return;
        }
        if let Some(tx) = self.tx.lock().unwrap().as_ref() {
            let _ = tx.send(StreamCmd::Feed(frame.to_vec()));
        }
    }

    /// Whether a stream is pending or active.
    pub fn is_open(&self) -> bool {
        self.open.load(Ordering::Relaxed)
    }
}

enum LoadedEngine {
    /// Whisper-family models (whisper, breeze-asr, custom .bin/.gguf) via
    /// transcribe-cpp. Holds the live `Session`, which keeps its `Model` alive
    /// internally, so repeated dictation reuses the session without reloading.
    TranscribeCpp(Session),
    Parakeet(ParakeetModel),
    Moonshine(MoonshineModel),
    MoonshineStreaming(StreamingModel),
    SenseVoice(SenseVoiceModel),
    GigaAM(GigaAMModel),
    Canary(CanaryModel),
    Cohere(CohereModel),
}

/// RAII guard that clears the `is_loading` flag and notifies waiters on drop.
/// Ensures the loading flag is always reset, even on early returns or panics.
/// If a pending model request was queued while the load was in progress, the
/// guard auto-loads that model before releasing the slot.
pub struct LoadingGuard {
    is_loading: Arc<Mutex<bool>>,
    loading_condvar: Arc<Condvar>,
    pending_model_id: Arc<Mutex<Option<String>>>,
    app_handle: AppHandle,
}

impl Drop for LoadingGuard {
    fn drop(&mut self) {
        // Recover from a poisoned mutex instead of panicking —
        // a panic inside Drop calls abort().
        let mut is_loading = match self.is_loading.lock() {
            Ok(g) => g,
            Err(e) => {
                warn!("Recovered poisoned is_loading mutex during LoadingGuard drop — a panic occurred earlier this session");
                e.into_inner()
            }
        };

        // Check for a pending model request before releasing the loading slot.
        // Keep `is_loading` true while the pending model auto-loads so another
        // switch cannot steal the slot and starve the queued request.
        let pending = self.pending_model_id.lock().unwrap().take();
        if let Some(model_id) = pending {
            info!("Auto-loading pending model: {}", model_id);
            drop(is_loading);

            let app_handle = self.app_handle.clone();
            let is_loading = self.is_loading.clone();
            let loading_condvar = self.loading_condvar.clone();
            let pending_model_id = self.pending_model_id.clone();

            std::thread::spawn(move || {
                let _guard = LoadingGuard {
                    is_loading,
                    loading_condvar,
                    pending_model_id,
                    app_handle: app_handle.clone(),
                };
                if let Some(tm) =
                    app_handle.try_state::<Arc<parking_lot::Mutex<TranscriptionManager>>>()
                {
                    let tm_clone = tm.lock().clone();
                    if let Err(e) = tm_clone.load_model(&model_id) {
                        warn!("Failed to auto-load pending model {}: {}", model_id, e);
                    }
                }
            });
        } else {
            *is_loading = false;
            self.loading_condvar.notify_all();
        }
    }
}

/// RAII guard that clears the streaming worker/lease flags on any worker exit -
/// normal return, early return, or a panic in an engine call that unwinds the
/// detached worker thread. Tokens prevent an older worker from clearing a newer
/// worker's state if a start/finalize race ever slips through.
struct StreamWorkerGuard {
    worker_id: u64,
    active_stream_worker: Arc<AtomicU64>,
    active_engine_lease: Arc<AtomicU64>,
    stream_active: Arc<AtomicBool>,
}

impl Drop for StreamWorkerGuard {
    fn drop(&mut self) {
        if self.active_stream_worker.load(Ordering::Acquire) == self.worker_id {
            self.stream_active.store(false, Ordering::Release);
        }
        let _ = self.active_engine_lease.compare_exchange(
            self.worker_id,
            0,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        let _ = self.active_stream_worker.compare_exchange(
            self.worker_id,
            0,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }
}

#[derive(Clone)]
pub struct TranscriptionManager {
    engine: Arc<Mutex<Option<LoadedEngine>>>,
    model_manager: Arc<ModelManager>,
    app_handle: AppHandle,
    current_model_id: Arc<Mutex<Option<String>>>,
    last_activity: Arc<AtomicU64>,
    shutdown_signal: Arc<AtomicBool>,
    watcher_handle: Arc<Mutex<Option<thread::JoinHandle<()>>>>,
    is_loading: Arc<Mutex<bool>>,
    loading_condvar: Arc<Condvar>,
    /// A model switch requested while another load is in progress.
    /// When the current load completes, auto-switch to this model.
    pending_model_id: Arc<Mutex<Option<String>>>,
    reload_model_on_next_use: Arc<AtomicBool>,
    /// Routes real-time audio frames to the active streaming worker; see
    /// [`StreamRouter`]. Shared with the audio recorder so per-frame feeds skip
    /// Tauri state and the manager lock.
    router: Arc<StreamRouter>,
    /// True only while a transcribe-cpp `Stream` is actually in flight (set by
    /// the worker once `stream()` succeeds). Used for overlay/UI decisions.
    stream_active: Arc<AtomicBool>,
    /// Streaming uses four independent flags: router open = frames should route,
    /// worker active = no second worker may start, engine lease = engine is out
    /// of the mutex, stream active = UI should show a live session.
    ///
    /// Monotonic id source for stream workers; zero means "no worker".
    next_stream_worker_id: Arc<AtomicU64>,
    /// Nonzero while a stream worker exists, even if it has not leased the engine
    /// yet. This prevents a second worker from starting after finalize/cancel
    /// closes the router but before the first worker has fully exited.
    active_stream_worker: Arc<AtomicU64>,
    /// Nonzero while the streaming worker has taken the engine out of `engine`.
    /// `is_model_loaded()` consults this so the model still reports "loaded"
    /// while the worker holds it.
    active_engine_lease: Arc<AtomicU64>,
    /// Flag to prevent concurrent transcription calls (streaming vs final).
    is_transcribing: Arc<ParkingLotMutex<bool>>,
    /// Timestamp when the current owner acquired `is_transcribing`. Used by
    /// the slot watchdog to detect a flag stuck by a wedged/panicked worker.
    is_transcribing_since: Arc<ParkingLotMutex<Option<Instant>>>,
    transcribing_condvar: Arc<ParkingLotCondvar>,
    cancel_streaming: Arc<AtomicBool>,
    /// Consecutive non-panic engine failures. Reaches
    /// ENGINE_HEALTH_RELOAD_THRESHOLD -> engine is dropped so the next
    /// initiate_model_load_for actually reloads instead of reusing a wedged
    /// session. Reset to 0 on any successful transcription or explicit unload.
    consecutive_engine_failures: Arc<AtomicU32>,
}

impl TranscriptionManager {
    pub fn new(app_handle: &AppHandle, model_manager: Arc<ModelManager>) -> Result<Self> {
        let manager = Self {
            engine: Arc::new(Mutex::new(None)),
            model_manager,
            app_handle: app_handle.clone(),
            current_model_id: Arc::new(Mutex::new(None)),
            last_activity: Arc::new(AtomicU64::new(Self::now_ms())),
            shutdown_signal: Arc::new(AtomicBool::new(false)),
            watcher_handle: Arc::new(Mutex::new(None)),
            is_loading: Arc::new(Mutex::new(false)),
            loading_condvar: Arc::new(Condvar::new()),
            pending_model_id: Arc::new(Mutex::new(None)),
            reload_model_on_next_use: Arc::new(AtomicBool::new(false)),
            router: Arc::new(StreamRouter::new()),
            stream_active: Arc::new(AtomicBool::new(false)),
            next_stream_worker_id: Arc::new(AtomicU64::new(1)),
            active_stream_worker: Arc::new(AtomicU64::new(0)),
            active_engine_lease: Arc::new(AtomicU64::new(0)),
            is_transcribing: Arc::new(ParkingLotMutex::new(false)),
            is_transcribing_since: Arc::new(ParkingLotMutex::new(None)),
            transcribing_condvar: Arc::new(ParkingLotCondvar::new()),
            cancel_streaming: Arc::new(AtomicBool::new(false)),
            consecutive_engine_failures: Arc::new(AtomicU32::new(0)),
        };

        // Start the idle watcher
        {
            let app_handle_cloned = app_handle.clone();
            let manager_cloned = manager.clone();
            let shutdown_signal = manager.shutdown_signal.clone();
            let handle = thread::spawn(move || {
                debug!("Idle watcher thread started");
                while !shutdown_signal.load(Ordering::Relaxed) {
                    thread::sleep(Duration::from_secs(10)); // Check every 10 seconds

                    // Check shutdown signal again after sleep
                    if shutdown_signal.load(Ordering::Relaxed) {
                        break;
                    }

                    let settings = get_settings(&app_handle_cloned);
                    let timeout = settings.model_unload_timeout;

                    // Skip Immediately — that variant is handled by
                    // maybe_unload_immediately() after each transcription.
                    // Treating it as 0s here would unload the model mid-recording.
                    if timeout == ModelUnloadTimeout::Immediately {
                        continue;
                    }

                    // While recording, keep the idle timer fresh so the
                    // model is never unloaded mid-session.
                    let is_recording = app_handle_cloned
                        .try_state::<Arc<AudioRecordingManager>>()
                        .is_some_and(|a| a.is_recording());
                    if is_recording {
                        manager_cloned.touch_activity();
                        continue;
                    }

                    // Proactive background watchdog: recover a wedged transcription
                    // slot even when no waiter is currently blocked on it.
                    manager_cloned.check_and_force_release_transcribing_slot();

                    if let Some(limit_seconds) = timeout.to_seconds() {
                        let last = manager_cloned.last_activity.load(Ordering::Relaxed);
                        let now_ms = TranscriptionManager::now_ms();
                        let idle_ms = now_ms.saturating_sub(last);
                        let limit_ms = limit_seconds * 1000;

                        if idle_ms > limit_ms {
                            // idle -> unload
                            if manager_cloned.is_model_loaded() {
                                let unload_start = std::time::Instant::now();
                                info!(
                                    "Model idle for {}s (limit: {}s), unloading",
                                    idle_ms / 1000,
                                    limit_seconds
                                );
                                match manager_cloned.unload_model() {
                                    Ok(()) => {
                                        let unload_duration = unload_start.elapsed();
                                        info!(
                                            "Model unloaded due to inactivity (took {}ms)",
                                            unload_duration.as_millis()
                                        );
                                    }
                                    Err(e) => {
                                        error!("Failed to unload idle model: {}", e);
                                    }
                                }
                            }
                        }
                    }
                }
                debug!("Idle watcher thread shutting down gracefully");
            });
            *manager.watcher_handle.lock().unwrap() = Some(handle);
        }

        Ok(manager)
    }

    /// Lock the engine mutex, recovering from poison if a previous transcription panicked.
    fn lock_engine(&self) -> MutexGuard<'_, Option<LoadedEngine>> {
        self.engine.lock().unwrap_or_else(|poisoned| {
            warn!("Engine mutex was poisoned by a previous panic, recovering");
            poisoned.into_inner()
        })
    }

    pub fn is_model_loaded(&self) -> bool {
        // The engine may be leased out to the streaming worker (taken out of
        // the mutex). It's still loaded, just in use, so report true.
        self.lock_engine().is_some() || self.active_engine_lease.load(Ordering::Acquire) != 0
    }

    /// Best-effort variant of [`is_model_loaded`] that never blocks waiting for
    /// the engine mutex. Used by the tray menu rebuild, which runs on the main
    /// thread and must not freeze the UI if a background streaming or
    /// transcription thread currently holds the engine lock.
    pub fn try_is_model_loaded(&self) -> bool {
        match self.engine.try_lock() {
            Ok(guard) => guard.is_some() || self.active_engine_lease.load(Ordering::Acquire) != 0,
            Err(std::sync::TryLockError::Poisoned(e)) => {
                warn!("Engine mutex was poisoned by a previous panic, recovering");
                e.into_inner().is_some() || self.active_engine_lease.load(Ordering::Acquire) != 0
            }
            Err(std::sync::TryLockError::WouldBlock) => {
                // Contended case: fall back to the lease counter. If a lease is
                // active the model is definitely loaded; otherwise report false
                // rather than block.
                self.active_engine_lease.load(Ordering::Acquire) != 0
            }
        }
    }

    /// Wait for an in-progress model load to finish.
    ///
    /// Called while the caller already holds the transcription manager lock.
    /// The condvar wait releases the `is_loading` mutex internally, so this
    /// method does not sleep while holding the manager lock. After waking it
    /// verifies a model is actually loaded; if the load failed or no load ran,
    /// it returns an error so callers can fail fast instead of trying to
    /// transcribe against a missing model.
    ///
    /// Note: this checks the engine is physically present in the mutex. If the
    /// engine is leased out to a streaming worker it will report as absent, so
    /// callers should only use this when they know no stream is active (e.g.
    /// during retry transcription on non-streaming audio).
    pub fn wait_for_model_load(&self) -> Result<(), String> {
        {
            let mut is_loading = self.is_loading.lock().map_err(|e| e.to_string())?;
            while *is_loading {
                is_loading = self
                    .loading_condvar
                    .wait(is_loading)
                    .map_err(|e| e.to_string())?;
            }
        }
        if self.lock_engine().is_none() {
            return Err("Model failed to load or is currently in use by streaming".to_string());
        }
        Ok(())
    }

    pub fn is_model_loading(&self) -> bool {
        *self.is_loading.lock().unwrap()
    }

    /// Set a pending model request. Called when a switch is requested while
    /// another load is in progress. The pending model will auto-load when the
    /// current load completes.
    pub fn set_pending_model(&self, model_id: &str) {
        let mut pending = self.pending_model_id.lock().unwrap();
        *pending = Some(model_id.to_string());
        info!("Pending model request queued: {}", model_id);
    }

    /// Take the pending model ID (returns None if no pending request).
    pub fn take_pending_model(&self) -> Option<String> {
        let mut pending = self.pending_model_id.lock().unwrap();
        pending.take()
    }

    /// Accelerator changes should not disturb the current transcription. Mark
    /// the cached engine stale; the next model-use path reloads it with the
    /// latest settings.
    pub fn reload_model_on_next_use(&self) {
        self.reload_model_on_next_use.store(true, Ordering::Release);
    }

    /// Atomically check whether a model load is in progress and, if not, mark
    /// one as starting. Returns a [`LoadingGuard`] whose [`Drop`] impl will
    /// clear the flag and wake waiters. Returns `None` if a load is already in
    /// progress.
    pub fn try_start_loading(&self) -> Option<LoadingGuard> {
        let mut is_loading = self.is_loading.lock().unwrap();
        if *is_loading {
            return None;
        }
        *is_loading = true;
        Some(LoadingGuard {
            is_loading: self.is_loading.clone(),
            loading_condvar: self.loading_condvar.clone(),
            pending_model_id: self.pending_model_id.clone(),
            app_handle: self.app_handle.clone(),
        })
    }

    pub fn unload_model(&self) -> Result<()> {
        let unload_start = std::time::Instant::now();
        debug!("Starting to unload model");

        // Any explicit unload resets the engine-health failure counter so a
        // manual retry or model switch starts from a clean slate.
        self.consecutive_engine_failures.store(0, Ordering::Release);

        {
            let mut engine = self.lock_engine();
            // Dropping the engine frees all resources
            *engine = None;
        }
        {
            let mut current_model = self.current_model_id.lock().unwrap();
            *current_model = None;
        }

        // Emit unloaded event
        let _ = self.app_handle.emit(
            "model-state-changed",
            ModelStateEvent {
                event_type: "unloaded".to_string(),
                model_id: None,
                model_name: None,
                error: None,
            },
        );

        let unload_duration = unload_start.elapsed();
        debug!(
            "Model unloaded manually (took {}ms)",
            unload_duration.as_millis()
        );
        Ok(())
    }

    fn now_ms() -> u64 {
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
    }

    /// Reset the idle timer to now.
    fn touch_activity(&self) {
        self.last_activity.store(Self::now_ms(), Ordering::Relaxed);
    }

    /// Request cancellation of any in-progress streaming transcription.
    /// Called when recording stops to prevent wasted work on partial audio.
    pub fn cancel_streaming(&self) {
        self.cancel_streaming.store(true, Ordering::Release);
        debug!("Streaming cancellation requested");
    }

    /// Clear the streaming cancellation flag.
    /// Should be called when starting a new recording session.
    pub fn clear_streaming_cancel(&self) {
        self.cancel_streaming.store(false, Ordering::Release);
    }

    /// Check if streaming cancellation has been requested.
    #[allow(dead_code)]
    pub fn is_streaming_cancelled(&self) -> bool {
        self.cancel_streaming.load(Ordering::Acquire)
    }

    /// Get a clone of the streaming cancel flag.
    pub fn streaming_cancel_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancel_streaming)
    }

    /// Proactive background recovery for the `is_transcribing` slot. Called by
    /// the idle watcher every 10s when not recording. Uses a much longer
    /// threshold than the waiter-side watchdog so it never interrupts a
    /// legitimate long final transcription; it only catches slots left stuck
    /// during an idle gap. When force-releasing, also clears the cached model
    /// and resets engine-health counters so the next transcription reloads a
    /// fresh engine instead of re-entering the wedged one.
    pub fn check_and_force_release_transcribing_slot(&self) {
        let mut is_transcribing = self.is_transcribing.lock();
        if !*is_transcribing {
            return;
        }
        let held_for = {
            let guard = self.is_transcribing_since.lock();
            guard
                .as_ref()
                .copied()
                .map(|s| Instant::now().duration_since(s))
        };
        if held_for.map_or(false, |d| d >= TRANSCRIBING_SLOT_BG_FORCE_RELEASE_AFTER) {
            warn!(
                "[SLOT-WATCHDOG-BG] proactive force-release of is_transcribing held for {:?}; clearing cached model and failure counter",
                held_for
            );
            *is_transcribing = false;
            *self.is_transcribing_since.lock() = None;
            self.transcribing_condvar.notify_all();

            // A slot held this long means the engine call is wedged (e.g.
            // GPU context loss, never-returning callback). Clear the cached
            // model so the next transcription actually reloads a fresh engine
            // instead of reusing the wedged one. Reset the failure counter as
            // well; the reload itself starts a clean health record.
            self.consecutive_engine_failures.store(0, Ordering::Release);
            {
                let mut current_model = self
                    .current_model_id
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                *current_model = None;
            }
            let _ = self.app_handle.emit(
                "model-state-changed",
                ModelStateEvent {
                    event_type: "unloaded".to_string(),
                    model_id: None,
                    model_name: None,
                    error: Some(
                        "Transcription slot watchdog recovered from wedged engine".to_string(),
                    ),
                },
            );
        }
    }

    /// Unloads the model immediately if the setting is enabled and the model is loaded
    pub fn maybe_unload_immediately(&self, context: &str) {
        let settings = get_settings(&self.app_handle);
        if settings.model_unload_timeout == ModelUnloadTimeout::Immediately
            && self.is_model_loaded()
        {
            info!("Immediately unloading model after {}", context);
            if let Err(e) = self.unload_model() {
                warn!("Failed to immediately unload model: {}", e);
            }
        }
    }

    pub fn load_model(&self, model_id: &str) -> Result<()> {
        self.load_model_with_device(model_id, None)
    }

    /// Like [`load_model`](Self::load_model), but lets a caller hard-select the
    /// compute device for this one load by its `transcribe_cpp::devices()`
    /// registry index (the index shown by `--list-devices`). `None` keeps the
    /// persisted accelerator setting (which may be Auto). Only affects
    /// transcribe-cpp (whisper-family) models; the selection is not persisted.
    pub fn load_model_with_device(
        &self,
        model_id: &str,
        device_index: Option<usize>,
    ) -> Result<()> {
        apply_accelerator_settings(&self.app_handle);

        let load_start = std::time::Instant::now();
        debug!("Starting to load model: {}", model_id);

        // Emit loading started event
        let _ = self.app_handle.emit(
            "model-state-changed",
            ModelStateEvent {
                event_type: "loading_started".to_string(),
                model_id: Some(model_id.to_string()),
                model_name: None,
                error: None,
            },
        );
        debug!(
            "[LOAD-TRACE] loading_started event emitted for {}",
            model_id
        );

        let model_info = self
            .model_manager
            .get_model_info(model_id)
            .ok_or_else(|| anyhow::anyhow!("Model not found: {}", model_id))?;
        debug!(
            "[LOAD-TRACE] got model_info for {}, engine_type={:?}",
            model_id, model_info.engine_type
        );

        if !model_info.is_downloaded {
            let error_msg = "Model not downloaded";
            let _ = self.app_handle.emit(
                "model-state-changed",
                ModelStateEvent {
                    event_type: "loading_failed".to_string(),
                    model_id: Some(model_id.to_string()),
                    model_name: Some(model_info.name.clone()),
                    error: Some(error_msg.to_string()),
                },
            );
            return Err(anyhow::anyhow!(error_msg));
        }

        let model_path = self.model_manager.get_model_path(model_id)?;
        debug!("[LOAD-TRACE] got model_path: {:?}", model_path);

        // Drop the current engine BEFORE building the new one so transcribe-cpp
        // frees the previous native context first — avoids holding two models at
        // once (peak memory on large GGUFs). Clear the id too: if the new load
        // fails, status should read "no loaded model", not the dropped engine.
        {
            let mut engine = self.lock_engine();
            *engine = None;
        }
        debug!(
            "[LOAD-TRACE] old engine dropped, entering engine_type match for {}",
            model_id
        );
        {
            let mut current_model = self.current_model_id.lock().unwrap();
            *current_model = None;
        }

        // Create appropriate engine based on model type
        let emit_loading_failed = |error_msg: &str| {
            let _ = self.app_handle.emit(
                "model-state-changed",
                ModelStateEvent {
                    event_type: "loading_failed".to_string(),
                    model_id: Some(model_id.to_string()),
                    model_name: Some(model_info.name.clone()),
                    error: Some(error_msg.to_string()),
                },
            );
        };

        let loaded_engine = match model_info.engine_type {
            EngineType::TranscribeCpp => {
                // The whisper backend is chosen at load time (transcribe-cpp has
                // no runtime global). With an explicit `device_index` (the
                // --device-index flag) hard-select that registered device;
                // otherwise re-read the persisted accelerator preference (so an
                // accelerator change marked for reload takes effect here).
                let (backend, device) = match device_index {
                    Some(index) => resolve_device_index(index).inspect_err(|e| {
                        emit_loading_failed(&e.to_string());
                    })?,
                    None => {
                        let settings = get_settings(&self.app_handle);
                        let accelerator = settings.transcribe_accelerator;
                        let device = resolve_gpu_device(
                            accelerator,
                            settings.transcribe_gpu_device.as_deref(),
                        );
                        // Backend::Auto accepts an exact CPU/GPU device. When
                        // no exact device is saved, retain Handy's strict
                        // accelerator/backend policy and native fallback.
                        let backend = if device.is_some() {
                            Backend::Auto
                        } else {
                            select_transcribe_backend(accelerator)
                        };
                        (backend, device)
                    }
                };
                let requested_device = device
                    .as_ref()
                    .map(transcribe_device_label)
                    .unwrap_or_else(|| "automatic".to_string());
                debug!(
                    "[LOAD-TRACE] backend resolved: {:?}, requested_device='{}'",
                    backend, requested_device
                );
                let model_options = ModelOptions { backend, device };
                // Log transcribe-cpp version for debugging load failures
                let tc_version = transcribe_cpp::version();
                let tc_compiled = transcribe_cpp::compiled_version();
                info!(
                    "Loading model '{}' via transcribe-cpp (runtime={}, compiled={}, backend={:?}, requested_device='{}')",
                    model_id, tc_version, tc_compiled, backend, requested_device
                );
                let model = Model::load_with(&model_path, &model_options).map_err(|e| {
                    let error_msg = format!("Failed to load whisper model {}: {}", model_id, e);
                    emit_loading_failed(&error_msg);
                    anyhow::anyhow!(error_msg)
                })?;
                // The bound backend may differ from the request (e.g. CPU
                // fallback under Auto); log what actually loaded.
                let bound_backend = model.backend();
                let session = model.session().map_err(|e| {
                    let error_msg = format!(
                        "Failed to create session for whisper model {}: {}",
                        model_id, e
                    );
                    emit_loading_failed(&error_msg);
                    anyhow::anyhow!(error_msg)
                })?;
                // Reconcile the registry's advertised capabilities with the
                // loaded model's real ones (GGUF metadata) so badges/gating
                // reflect runtime truth, not the pre-download probe. The
                // load-completed event below triggers the frontend refresh.
                let caps = session.model().capabilities();
                self.model_manager.set_runtime_capabilities(
                    model_id,
                    caps.supports_streaming,
                    caps.supports_translate,
                    caps.supports_language_detect,
                    caps.languages.clone(),
                );
                let bound_device = model
                    .device()
                    .map(|device| transcribe_device_label(&device))
                    .unwrap_or_else(|_| "unknown".to_string());
                info!(
                    "Loaded whisper model '{}' (requested {:?}, requested device '{}', \
                     bound backend '{}', bound device '{}', supports_streaming={}, \
                     supports_translate={}, supports_language_detect={})",
                    model_id,
                    backend,
                    requested_device,
                    bound_backend,
                    bound_device,
                    caps.supports_streaming,
                    caps.supports_translate,
                    caps.supports_language_detect
                );
                info!(
                    "Successfully loaded model '{}' via transcribe-cpp",
                    model_id
                );
                LoadedEngine::TranscribeCpp(session)
            }
            EngineType::Parakeet => {
                let engine =
                    ParakeetModel::load(&model_path, &Quantization::Int8).map_err(|e| {
                        let error_msg =
                            format!("Failed to load parakeet model {}: {}", model_id, e);
                        emit_loading_failed(&error_msg);
                        anyhow::anyhow!(error_msg)
                    })?;
                LoadedEngine::Parakeet(engine)
            }
            EngineType::Moonshine => {
                let engine = MoonshineModel::load(
                    &model_path,
                    MoonshineVariant::Base,
                    &Quantization::default(),
                )
                .map_err(|e| {
                    let error_msg = format!("Failed to load moonshine model {}: {}", model_id, e);
                    emit_loading_failed(&error_msg);
                    anyhow::anyhow!(error_msg)
                })?;
                LoadedEngine::Moonshine(engine)
            }
            EngineType::MoonshineStreaming => {
                let engine = StreamingModel::load(&model_path, 0, &Quantization::default())
                    .map_err(|e| {
                        let error_msg = format!(
                            "Failed to load moonshine streaming model {}: {}",
                            model_id, e
                        );
                        emit_loading_failed(&error_msg);
                        anyhow::anyhow!(error_msg)
                    })?;
                LoadedEngine::MoonshineStreaming(engine)
            }
            EngineType::SenseVoice => {
                let engine =
                    SenseVoiceModel::load(&model_path, &Quantization::Int8).map_err(|e| {
                        let error_msg =
                            format!("Failed to load SenseVoice model {}: {}", model_id, e);
                        emit_loading_failed(&error_msg);
                        anyhow::anyhow!(error_msg)
                    })?;
                LoadedEngine::SenseVoice(engine)
            }
            EngineType::GigaAM => {
                let engine = GigaAMModel::load(&model_path, &Quantization::Int8).map_err(|e| {
                    let error_msg = format!("Failed to load gigaam model {}: {}", model_id, e);
                    emit_loading_failed(&error_msg);
                    anyhow::anyhow!(error_msg)
                })?;
                LoadedEngine::GigaAM(engine)
            }
            EngineType::Canary => {
                let engine = CanaryModel::load(&model_path, &Quantization::Int8).map_err(|e| {
                    let error_msg = format!("Failed to load canary model {}: {}", model_id, e);
                    emit_loading_failed(&error_msg);
                    anyhow::anyhow!(error_msg)
                })?;
                LoadedEngine::Canary(engine)
            }
            EngineType::Cohere => {
                let engine = CohereModel::load(&model_path, &Quantization::Int8).map_err(|e| {
                    let error_msg = format!("Failed to load cohere model {}: {}", model_id, e);
                    emit_loading_failed(&error_msg);
                    anyhow::anyhow!(error_msg)
                })?;
                LoadedEngine::Cohere(engine)
            }
        };

        // Update the current engine and model ID
        {
            let mut engine = self.lock_engine();
            *engine = Some(loaded_engine);
        }
        {
            let mut current_model = self.current_model_id.lock().unwrap();
            *current_model = Some(model_id.to_string());
        }

        // Reset idle timer so the watcher doesn't immediately unload a just-loaded model
        self.touch_activity();

        // Emit loading completed event
        let _ = self.app_handle.emit(
            "model-state-changed",
            ModelStateEvent {
                event_type: "loading_completed".to_string(),
                model_id: Some(model_id.to_string()),
                model_name: Some(model_info.name.clone()),
                error: None,
            },
        );

        let load_duration = load_start.elapsed();
        debug!(
            "Successfully loaded transcription model: {} (took {}ms)",
            model_id,
            load_duration.as_millis()
        );

        // A successful load gives us a fresh engine, so reset the failure
        // counter before this engine goes into the pool.
        self.consecutive_engine_failures.store(0, Ordering::Release);

        Ok(())
    }

    /// Kicks off the model loading in a background thread if it's not already loaded
    pub fn initiate_model_load(&self) {
        let settings = get_settings(&self.app_handle);
        self.initiate_model_load_for(&settings.selected_model);
    }

    /// Kicks off loading of a specific model ID in a background thread.
    ///
    /// This is the model-agnostic core used by `initiate_model_load` (which
    /// loads the user's selected model) and by the live captions path, which
    /// may need a different model loaded in a second TranscriptionManager.
    /// The load runs in a detached thread so callers never block on the
    /// potentially multi-second model initialization.
    pub fn initiate_model_load_for(&self, model_id: &str) {
        let mut is_loading = self.is_loading.lock().unwrap();
        if *is_loading {
            return;
        }

        let reload_pending = self.reload_model_on_next_use.load(Ordering::Acquire);
        // If a specific model is already loaded and matches the request, skip.
        let already_loaded = !reload_pending
            && self.is_model_loaded()
            && self.get_current_model().as_deref() == Some(model_id);
        if already_loaded {
            return;
        }

        *is_loading = true;
        let model_id = model_id.to_string();
        let self_clone = self.clone();
        thread::spawn(move || {
            // This guard guarantees that `is_loading` is cleared and waiters
            // are notified in every exit path, including panics inside the load
            // thread. It deliberately runs *after* any pending-model auto-load
            // recursion in LoadingGuard::drop so that nested failures still
            // release the slot.
            struct ResetLoadingGuard {
                is_loading: Arc<Mutex<bool>>,
                loading_condvar: Arc<Condvar>,
            }
            impl Drop for ResetLoadingGuard {
                fn drop(&mut self) {
                    let mut guard = match self.is_loading.lock() {
                        Ok(g) => g,
                        Err(e) => {
                            warn!("Recovered poisoned is_loading mutex during model-load thread exit — a panic occurred earlier this session");
                            e.into_inner()
                        }
                    };
                    *guard = false;
                    self.loading_condvar.notify_all();
                }
            }
            let _reset_guard = ResetLoadingGuard {
                is_loading: self_clone.is_loading.clone(),
                loading_condvar: self_clone.loading_condvar.clone(),
            };

            if reload_pending {
                self_clone
                    .reload_model_on_next_use
                    .store(false, Ordering::Release);
            }

            let load_result = catch_unwind(AssertUnwindSafe(|| self_clone.load_model(&model_id)));
            match load_result {
                Ok(Ok(())) => {
                    info!("Model '{}' loaded successfully", model_id);
                }
                Ok(Err(e)) => {
                    error!("Failed to load model '{}': {}", model_id, e);
                }
                Err(_) => {
                    error!("[MODEL-LOAD] load thread panicked for model '{}'", model_id);
                }
            }
        });
    }

    pub fn get_current_model(&self) -> Option<String> {
        let current_model = self.current_model_id.lock().unwrap();
        current_model.clone()
    }

    /// The compute backend the currently-loaded engine is bound to, for
    /// diagnostics (e.g. confirming `--device-index` actually bound a GPU rather
    /// than falling back to CPU/auto). transcribe-cpp (whisper-family) reports
    /// its real backend string; ONNX engines report "onnx"; `None` when no
    /// model is loaded.
    pub fn current_backend(&self) -> Option<String> {
        match self.lock_engine().as_ref() {
            Some(LoadedEngine::TranscribeCpp(session)) => {
                Some(session.model().backend().to_string())
            }
            Some(_) => Some("onnx".to_string()),
            None => None,
        }
    }

    /// Whether a live streaming run is currently in flight.
    pub fn is_streaming(&self) -> bool {
        self.stream_active.load(Ordering::Acquire)
    }

    /// Shared handle to the stream router, used by the audio recorder to feed
    /// real-time frames without going through Tauri state on every frame.
    pub fn stream_router(&self) -> Arc<StreamRouter> {
        Arc::clone(&self.router)
    }

    /// Begin a live streaming transcription on the held engine's session.
    /// Audio frames pushed via [`StreamRouter::feed`] (captured directly by the
    /// audio recorder) are decoded incrementally and emitted to the overlay as
    /// [`StreamTextEvent`].
    ///
    /// Non-blocking: spawns a worker that waits for any in-progress model load,
    /// verifies the model supports streaming, then begins the stream. If the
    /// model can't stream, the worker idles until finalize/cancel and reports
    /// `None` so the caller falls back to batch transcription. Frames sent
    /// before the stream begins queue on the channel and are not lost.
    pub fn start_stream(&self) {
        if self.router.is_open() || self.active_stream_worker.load(Ordering::Acquire) != 0 {
            warn!("start_stream called while a stream worker is already active");
            return;
        }
        let worker_id = self.next_stream_worker_id.fetch_add(1, Ordering::Relaxed);
        if self
            .active_stream_worker
            .compare_exchange(0, worker_id, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            warn!("start_stream lost a race with another stream worker");
            return;
        }
        let rx = self.router.open();
        self.stream_active.store(false, Ordering::Release);

        let manager = self.clone();
        thread::spawn(move || manager.run_stream_worker(rx, worker_id));
    }

    fn run_stream_worker(&self, rx: mpsc::Receiver<StreamCmd>, worker_id: u64) {
        let _worker = StreamWorkerGuard {
            worker_id,
            active_stream_worker: Arc::clone(&self.active_stream_worker),
            active_engine_lease: Arc::clone(&self.active_engine_lease),
            stream_active: Arc::clone(&self.stream_active),
        };

        // Wait for any in-progress model load to finish (start_stream races the
        // background load kicked off when recording starts). We cannot block
        // indefinitely on the condvar: if the user stops the recording
        // immediately after starting it, `finalize_stream()`/`cancel_stream()`
        // close the router and send a command, but this worker would still be
        // asleep on `is_loading` and unable to reply. That produces a 30-second
        // hang waiting for `STREAM_FINALIZE_REPLY_TIMEOUT`.
        //
        // Fix: wait with a short timeout and poll `rx.try_recv()` between
        // sleeps. If a `Finalize` or `Cancel` arrives before the model has
        // loaded, reply with `None` (no usable stream) and exit cleanly.
        {
            let mut is_loading = self.is_loading.lock().unwrap();
            while *is_loading {
                // Drop the lock while sleeping so the loader thread can
                // acquire it to set `is_loading = false`.
                let guard = self
                    .loading_condvar
                    .wait_timeout(is_loading, Duration::from_millis(100))
                    .unwrap();
                is_loading = guard.0;

                // Check whether the caller already gave up on this stream.
                while let Ok(cmd) = rx.try_recv() {
                    match cmd {
                        StreamCmd::Feed(_) => {
                            // Audio arrived before the stream could begin.
                            // Drop it; the caller will fall back to batch.
                            debug!("Live preview: dropping feed received before model loaded");
                        }
                        StreamCmd::Finalize(reply) => {
                            info!(
                                "Live preview: finalize received while model still loading; aborting stream"
                            );
                            let _ = reply.send(None);
                            self.router.clear();
                            return;
                        }
                        StreamCmd::Cancel => {
                            info!(
                                "Live preview: cancel received while model still loading; aborting stream"
                            );
                            self.router.clear();
                            return;
                        }
                    }
                }
            }
        }

        let model_id = self.get_current_model().unwrap_or_default();

        // Take the engine out of the mutex so we own it during streaming,
        // structurally excluding any concurrent batch transcription (which
        // transcribe-cpp's compute_lock would refuse anyway). Returned when the
        // worker exits, or dropped if the model was switched/unloaded mid-stream.
        if self
            .active_engine_lease
            .compare_exchange(0, worker_id, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            warn!("Live preview: another worker already holds the transcription engine");
            self.router.clear();
            drain_until_finalize(rx);
            return;
        }
        let mut engine = match self.lock_engine().take() {
            Some(e) => e,
            None => {
                info!(
                    "Live preview: model '{}' was unloaded before streaming could begin; \
                     falling back to batch transcription",
                    model_id
                );
                let _ = self.active_engine_lease.compare_exchange(
                    worker_id,
                    0,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                );
                self.router.clear();
                drain_until_finalize(rx);
                return;
            }
        };

        // Only transcribe-cpp models expose streaming; ONNX engines fall back to
        // batch. The loaded session (not the ModelManager copy) is the source of
        // truth for run-path capabilities.
        let (supports_streaming, supports_translate, languages) = match &engine {
            LoadedEngine::TranscribeCpp(session) => {
                let model = session.model();
                let caps = model.capabilities();
                info!(
                    "Live preview: model '{}' arch='{}' variant='{}' supports_streaming={} \
                     supports_translate={} languages={:?}",
                    model_id,
                    model.arch(),
                    model.variant(),
                    caps.supports_streaming,
                    caps.supports_translate,
                    caps.languages,
                );
                (
                    caps.supports_streaming,
                    caps.supports_translate,
                    caps.languages,
                )
            }
            _ => {
                info!(
                    "Live preview: model '{}' is not a transcribe-cpp model; \
                     streaming is unavailable, using batch transcription",
                    model_id
                );
                (false, false, Vec::new())
            }
        };

        if !supports_streaming {
            self.return_engine(engine, &model_id);
            self.router.clear();
            drain_until_finalize(rx);
            return;
        }

        // Build run options mirroring the offline transcribe-cpp path: task +
        // language gated against what the model actually advertises.
        let settings = get_settings(&self.app_handle);
        let effective_language =
            effective_language_for_model(&settings, self.model_manager.as_ref(), &model_id);
        let run_plan = transcribe_cpp_run_plan(
            settings.translate_to_english,
            &effective_language,
            &languages,
            supports_translate,
        );
        let run_options = RunOptions {
            task: run_plan.task,
            language: run_plan.language,
            target_language: run_plan.target_language,
            ..Default::default()
        };

        // Run the stream on the held session. The Stream borrows the session
        // (and thus the engine) for its lifetime, so the feed/finalize loop
        // lives in a labeled block — when it exits, the borrow is released and
        // the engine can be moved into return_engine().
        let mut finalize_reply: Option<mpsc::Sender<Option<String>>> = None;
        let mut finalize_result: Option<Option<String>> = None;
        let stream_started = 'stream: {
            let session = match &mut engine {
                LoadedEngine::TranscribeCpp(s) => s,
                _ => break 'stream false,
            };

            // Read the backend string before beginning the stream — the
            // `Stream` borrows `session` mutably for its lifetime, so we can't
            // call `session.model()` once it exists.
            let backend = session.model().backend();

            // StreamOptions::default() uses CommitPolicy::Auto and lets the
            // family pick its own streaming strategy (no family-specific ext).
            let mut stream = match session.stream(&run_options, &StreamOptions::default()) {
                Ok(s) => s,
                Err(e) => {
                    error!("Failed to begin stream: {}", e);
                    break 'stream false;
                }
            };

            self.stream_active.store(true, Ordering::Release);
            self.touch_activity();
            info!(
                "Live streaming transcription started (model '{}', backend '{}')",
                model_id, backend
            );

            let mut perf = StreamPerf::new();
            while let Ok(cmd) = rx.recv() {
                match cmd {
                    StreamCmd::Feed(pcm) => {
                        self.touch_activity();
                        perf.record_feed(pcm.len());
                        let feed_start = Instant::now();
                        match stream.feed(&pcm) {
                            Ok(update) => {
                                perf.record_compute(feed_start.elapsed());
                                perf.record_update(
                                    update.revision,
                                    update.input_received_ms,
                                    update.audio_committed_ms,
                                    update.buffered_ms,
                                );
                                if update.committed_changed || update.tentative_changed {
                                    let text = stream.text();
                                    perf.record_emit();
                                    self.emit_stream_text(&text.committed, &text.tentative);
                                }
                                perf.maybe_log();
                            }
                            Err(e) => {
                                perf.record_compute(feed_start.elapsed());
                                warn!("stream feed failed: {}", e);
                            }
                        }
                    }
                    StreamCmd::Finalize(reply) => {
                        let finalize_start = Instant::now();
                        let result = match stream.finalize() {
                            // Moonshine streaming needs the full-field result
                            // (committed + tentative collapsed), not display().
                            Ok(update) => {
                                perf.record_compute(finalize_start.elapsed());
                                perf.record_update(
                                    update.revision,
                                    update.input_received_ms,
                                    update.audio_committed_ms,
                                    update.buffered_ms,
                                );
                                Some(stream.text().full)
                            }
                            Err(e) => {
                                perf.record_compute(finalize_start.elapsed());
                                error!(
                                    "stream finalize failed: {}; falling back to batch transcription",
                                    e
                                );
                                None
                            }
                        };
                        let chars = match &result {
                            Some(text) => text.len(),
                            _ => 0,
                        };
                        perf.log_finalized(chars);
                        finalize_reply = Some(reply);
                        finalize_result = Some(result);
                        break;
                    }
                    StreamCmd::Cancel => {
                        stream.reset();
                        break;
                    }
                }
            }

            true
        };
        // `stream` + the `&mut engine` borrow are released here.

        if !stream_started {
            // Stream never began (model doesn't support streaming or begin
            // failed); drain so the finalize handshake still completes and the
            // caller falls back to batch transcription. Return the engine first
            // so the fallback can immediately use it.
            self.return_engine(engine, &model_id);
            drain_until_finalize(rx);
            return;
        }

        self.return_engine(engine, &model_id);
        if let (Some(reply), Some(result)) = (finalize_reply, finalize_result) {
            let _ = reply.send(result);
        }
        // `_worker` drops here, clearing this worker's active/lease flags after
        // the engine has been returned to the pool.
    }

    /// Return the leased engine to the mutex, unless the model was switched or
    /// unloaded during transcription (in which case the stale engine is dropped).
    fn return_engine(&self, engine: LoadedEngine, expected_model_id: &str) {
        let still_current =
            self.current_model_id.lock().unwrap().as_deref() == Some(expected_model_id);
        if still_current {
            *self.lock_engine() = Some(engine);
        } else {
            info!(
                "Model changed/unloaded during transcription; dropping stale engine (was '{}')",
                expected_model_id
            );
            // `engine` drops here, freeing its resources.
        }
    }

    /// Flush the active stream and return its final, post-filtered text.
    ///
    /// `Ok(None)` means no usable stream was active and the caller may fall back
    /// to batch transcription. `Err` means finalize itself failed or timed out.
    /// A timeout may still leave the worker holding the engine, so callers
    /// should surface it instead of immediately starting a batch fallback.
    pub fn finalize_stream(&self) -> Result<Option<String>> {
        let Some(tx) = self.router.take() else {
            return Ok(None);
        };
        let (reply_tx, reply_rx) = mpsc::channel();
        if tx.send(StreamCmd::Finalize(reply_tx)).is_err() {
            return Ok(None);
        }
        let raw = match reply_rx.recv_timeout(STREAM_FINALIZE_REPLY_TIMEOUT) {
            Ok(Some(text)) => text,
            Ok(None) => return Ok(None),
            Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(None),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.stream_active.store(false, Ordering::Release);
                return Err(anyhow::anyhow!(
                    "Timed out waiting {:?} for live transcription to finalize",
                    STREAM_FINALIZE_REPLY_TIMEOUT
                ));
            }
        };

        let settings = get_settings(&self.app_handle);
        // Streaming models do not receive a decode prompt, so custom words
        // always go through the shared post-correction path.
        // Wrap the optional cleanup so a panic cannot discard the raw result.
        let filtered = fail_open_text_transform(raw, |raw| {
            process_transcription_text(
                &raw,
                settings.word_correction_mode,
                &settings.custom_words,
                &settings.advanced_custom_words,
                &settings.word_replacements,
                settings.word_correction_threshold,
                false,
                &settings.app_language,
                &settings.custom_filler_words,
                settings.convert_us_to_british,
                settings.convert_number_words_to_digits,
                settings.spelling_dictionary,
                settings.repetition_suppression_level,
            )
        });

        self.maybe_unload_immediately("streaming transcription");
        Ok(Some(filtered))
    }

    /// Abandon any active stream without producing text (e.g. on cancel).
    pub fn cancel_stream(&self) {
        if let Some(tx) = self.router.take() {
            let _ = tx.send(StreamCmd::Cancel);
        }
        self.stream_active.store(false, Ordering::Release);
    }

    /// Emit a working-phase event to the streaming overlay (spinner + label).
    pub fn emit_stream_working(&self, kind: StreamWorkKind) {
        let _ = StreamPhaseEvent {
            phase: StreamPhase::Working,
            kind: Some(kind),
        }
        .emit(&self.app_handle);
    }

    fn emit_stream_text(&self, committed: &str, tentative: &str) {
        let _ = StreamTextEvent {
            committed: committed.to_string(),
            tentative: tentative.to_string(),
        }
        .emit(&self.app_handle);
    }

    /// Acquire the `is_transcribing` slot, waiting up to `max_wait`. If the slot
    /// is currently held and the hold duration has exceeded
    /// `TRANSCRIBING_SLOT_FORCE_RELEASE_AFTER`, the slot is force-released so
    /// the caller can proceed.
    ///
    /// `cancel_check` is called between waits; if it returns true, acquisition
    /// is aborted with `AppError::TranscriptionBusy`.
    fn try_acquire_transcribing_slot<F>(
        &self,
        max_wait: Duration,
        cancel_check: F,
    ) -> AppResult<TranscribingGuard>
    where
        F: Fn() -> bool,
    {
        let wait_start = Instant::now();
        let deadline = wait_start + max_wait;
        let mut force_released_once = false;

        let mut is_transcribing = self.is_transcribing.lock();
        while *is_transcribing {
            // Watchdog: if the current owner has held the slot for too long,
            // force-release it. This recovers from a wedged worker without
            // killing legitimate long transcriptions.
            let held_for = {
                let guard = self.is_transcribing_since.lock();
                guard
                    .as_ref()
                    .copied()
                    .map(|s| Instant::now().duration_since(s))
            };
            if held_for.map_or(false, |d| d >= TRANSCRIBING_SLOT_FORCE_RELEASE_AFTER) {
                error!(
                    "[SLOT-WATCHDOG] force-releasing is_transcribing held for {:?}",
                    held_for
                );
                *is_transcribing = false;
                *self.is_transcribing_since.lock() = None;
                self.transcribing_condvar.notify_all();
            }

            if cancel_check() {
                debug!("[SLOT] aborting acquisition: cancellation requested while waiting");
                return Err(AppError::TranscriptionBusy);
            }

            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                let held_for = {
                    let guard = self.is_transcribing_since.lock();
                    guard
                        .as_ref()
                        .copied()
                        .map(|s| Instant::now().duration_since(s))
                };
                if !force_released_once
                    && held_for.map_or(false, |d| {
                        d >= TRANSCRIBING_SLOT_FORCE_RELEASE_ON_OWN_TIMEOUT
                    })
                {
                    warn!(
                        "[SLOT-RECOVER] force-releasing is_transcribing held for {:?} after wait timeout",
                        held_for
                    );
                    *is_transcribing = false;
                    *self.is_transcribing_since.lock() = None;
                    self.transcribing_condvar.notify_all();
                    force_released_once = true;
                    continue;
                }
                warn!(
                    "[SLOT-BUSY] returning TranscriptionBusy; held_for={:?}",
                    held_for
                );
                return Err(AppError::TranscriptionBusy);
            }

            // Wait in small slices so we can re-check the watchdog and cancel
            // flag without blocking until the full remaining timeout.
            let slice = remaining.min(Duration::from_millis(100));
            let timed_out = self
                .transcribing_condvar
                .wait_for(&mut is_transcribing, slice)
                .timed_out();
            if timed_out && cancel_check() {
                return Err(AppError::TranscriptionBusy);
            }
        }

        // Own the slot.
        *is_transcribing = true;
        let acquired_at = Instant::now();
        *self.is_transcribing_since.lock() = Some(acquired_at);
        drop(is_transcribing);

        let guard = TranscribingGuard {
            flag: self.is_transcribing.clone(),
            timestamp: self.is_transcribing_since.clone(),
            condvar: self.transcribing_condvar.clone(),
            acquired_at,
        };

        debug!(
            "[SLOT] acquired is_transcribing after {:?}",
            acquired_at.duration_since(wait_start)
        );
        Ok(guard)
    }

    /// Run a full batch transcription on the held engine.
    ///
    /// Streaming callers (live captions) should use [`transcribe`], which
    /// respects the streaming-cancellation flag and returns early when cancelled.
    ///
    /// Final-transcription callers (after the user stops recording) should use
    /// this variant. It ignores the cancellation flag while waiting for the
    /// `is_transcribing` slot so that a stop-induced cancellation does not abort
    /// the user's own transcription. The caller-supplied hook is invoked only
    /// after the slot has been acquired, which is the only safe moment to clear
    /// the streaming-cancellation flag. Clearing it earlier lets queued
    /// streaming callbacks race ahead and steal the slot, producing
    /// `AppError::TranscriptionBusy`.
    pub fn transcribe_with_hook<F>(
        &self,
        audio: Vec<f32>,
        clear_cancel_after_slot: F,
    ) -> AppResult<TranscriptionOutput>
    where
        F: FnOnce(),
    {
        #[cfg(debug_assertions)]
        if std::env::var("HANDY_FORCE_TRANSCRIPTION_FAILURE").is_ok() {
            return Err(AppError::Transcription {
                message: "Simulated transcription failure (HANDY_FORCE_TRANSCRIPTION_FAILURE)"
                    .to_string(),
                source: anyhow::anyhow!("Simulated failure for testing"),
            });
        }

        let guard = match self.try_acquire_transcribing_slot(
            TRANSCRIBING_SLOT_WAIT_TIMEOUT,
            || false, // final transcription ignores the streaming cancel flag
        ) {
            Ok(guard) => guard,
            Err(AppError::TranscriptionBusy) => {
                // The slot is wedged by a prior streaming/batch transcription
                // that never released it. Force-recover the slot, clear the
                // cached engine state, and retry once so the user's final
                // transcription can proceed instead of failing immediately.
                warn!(
                    "[SLOT-RECOVER] final transcription busy; force-releasing wedged slot and retrying once"
                );
                self.check_and_force_release_transcribing_slot();
                self.try_acquire_transcribing_slot(Duration::from_secs(5), || false)
                    .map_err(|_| AppError::TranscriptionBusy)?
            }
            Err(e) => return Err(e),
        };

        // NOW it is safe to clear the streaming cancel flag: we own the
        // `is_transcribing` slot, so any queued streaming callback that checks
        // the flag and the slot will see the slot is taken and abort instead of
        // racing into the engine.
        clear_cancel_after_slot();

        self.transcribe_internal(audio, guard)
    }

    /// Convenience wrapper for streaming callers that respect the cancellation
    /// flag. Returns `TranscriptionBusy` while the flag is set so the audio
    /// streaming path can skip work when recording has stopped.
    pub fn transcribe(&self, audio: Vec<f32>) -> AppResult<TranscriptionOutput> {
        #[cfg(debug_assertions)]
        if std::env::var("HANDY_FORCE_TRANSCRIPTION_FAILURE").is_ok() {
            return Err(AppError::Transcription {
                message: "Simulated transcription failure (HANDY_FORCE_TRANSCRIPTION_FAILURE)"
                    .to_string(),
                source: anyhow::anyhow!("Simulated failure for testing"),
            });
        }

        let guard = self.try_acquire_transcribing_slot(TRANSCRIBING_SLOT_WAIT_TIMEOUT, || {
            self.is_streaming_cancelled()
        })?;

        // Cancelled after acquiring the slot: bail out without touching the engine.
        if self.is_streaming_cancelled() {
            debug!("Transcribe aborted: streaming cancellation requested after acquiring transcribing slot");
            return Err(AppError::TranscriptionBusy);
        }

        self.transcribe_internal(audio, guard)
    }

    /// Shared body of `transcribe` / `transcribe_with_hook`.
    /// Kept as a private helper so the public API stays simple while the hook
    /// variant can inject work immediately after acquiring the slot.
    fn transcribe_internal(
        &self,
        audio: Vec<f32>,
        _guard: TranscribingGuard,
    ) -> AppResult<TranscriptionOutput> {
        // Update last activity timestamp
        self.touch_activity();

        let st = std::time::Instant::now();
        let audio_len = audio.len();

        debug!("Audio vector length: {}", audio_len);

        if audio.is_empty() {
            debug!("Empty audio vector");
            self.maybe_unload_immediately("empty audio");
            return Ok(TranscriptionOutput {
                text: String::new(),
                model_id: self.get_current_model().unwrap_or_default(),
            });
        }

        // Check if model is loaded, if not try to load it
        {
            // If the model is loading, wait bounded for it to complete. The
            // watchdog below prevents a wedged loader thread from blocking the
            // final-transcription path forever. On timeout we defensively clear
            // the flag and fail fast.
            let mut is_loading = match self.is_loading.lock() {
                Ok(g) => g,
                Err(e) => {
                    warn!("Recovered poisoned is_loading mutex in transcribe_internal — a panic occurred earlier this session");
                    e.into_inner()
                }
            };
            let wait_start = Instant::now();
            while *is_loading {
                if wait_start.elapsed() >= MODEL_LOAD_WAIT_TIMEOUT {
                    error!("[IS_LOADING-WATCHDOG] timed out waiting for model load");
                    *is_loading = false;
                    self.loading_condvar.notify_all();
                    // Clear any pending model request so a stale LoadingGuard::drop
                    // does not spawn another load after this watchdog fires.
                    self.take_pending_model();
                    return Err(AppError::ModelNotLoaded);
                }

                let guard = self
                    .loading_condvar
                    .wait_timeout(is_loading, Duration::from_millis(100))
                    .unwrap();
                is_loading = guard.0;
            }

            let engine_guard = self.lock_engine();
            if engine_guard.is_none() {
                return Err(AppError::ModelNotLoaded);
            }
        }

        // Get current settings for configuration
        let settings = get_settings(&self.app_handle);

        // Validate selected language against the model's supported languages.
        // If the language isn't supported, fall back to "auto" to prevent errors.
        // Validate against the model that's actually loaded (which can differ
        // from settings.selected_model when a caller loaded a specific model —
        // e.g. the --transcribe-file path's --model), not the persisted
        // selection.
        let active_model = self
            .get_current_model()
            .unwrap_or_else(|| settings.selected_model.clone());
        // Resolve the persisted language *intent* into the language this model
        // will actually use. The coercion is capability-aware (a must-pick model
        // never receives "auto") and computed fresh here — it is never written
        // back to settings, so the intent survives switching models and back.
        let validated_language =
            effective_language_for_model(&settings, self.model_manager.as_ref(), &active_model);
        if validated_language != settings.selected_language {
            debug!(
                "Language intent '{}' resolved to '{}' for model '{}'",
                settings.selected_language, validated_language, active_model
            );
        }

        // Whether the loaded model is actually whisper-family (arch string).
        // Non-whisper archs (e.g. Voxtral Small) can advertise
        // Feature::InitialPrompt yet reject the whisper-kind run extension
        // with INVALID_ARG, so the whisper extension must be gated on the
        // arch, not on the feature (see #1601).
        let mut model_is_whisper = false;

        // Perform transcription with the appropriate engine.
        // We use catch_unwind to prevent engine panics from poisoning the mutex,
        // which would make the app hang indefinitely on subsequent operations.
        debug!(
            "[ENGINE] starting transcription for model '{}' on {:.2}s of audio",
            active_model,
            audio_len as f64 / 16_000.0
        );
        let engine_start = Instant::now();
        let result = {
            let mut engine_guard = self.lock_engine();

            // Take the engine out so we own it during transcription.
            // If the engine panics, we simply don't put it back (effectively unloading it)
            // instead of poisoning the mutex.
            let mut engine = match engine_guard.take() {
                Some(e) => e,
                None => {
                    return Err(AppError::ModelNotLoaded);
                }
            };

            // Release the lock before transcribing — no mutex held during the engine call
            drop(engine_guard);

            // Probe live transcribe-cpp capabilities once (cheap GGUF-metadata
            // reads); the loaded session is the source of truth, not the
            // ModelManager copy. The whisper run extension is kind-tagged, so
            // non-whisper archs (parakeet, voxtral, …) reject it with
            // INVALID_ARG; attach it — and translate — only where supported.
            let mut model_supports_translate = false;
            let mut model_languages: Vec<String> = Vec::new();
            if let LoadedEngine::TranscribeCpp(session) = &engine {
                let model = session.model();
                let caps = model.capabilities();
                let model_takes_initial_prompt = model.supports(Feature::InitialPrompt);
                model_is_whisper = model.arch() == "whisper";
                model_supports_translate = caps.supports_translate;
                model_languages = caps.languages;
                debug!(
                    "transcribe-cpp model '{}' on '{}': initial_prompt={}, translate={}, languages={:?}",
                    settings.selected_model,
                    model.backend(),
                    model_takes_initial_prompt,
                    model_supports_translate,
                    model_languages
                );
            }

            let transcribe_result = catch_unwind(AssertUnwindSafe(|| -> Result<String> {
                match &mut engine {
                    LoadedEngine::TranscribeCpp(session) => {
                        // Custom words become the initial prompt ONLY for models
                        // that accept one (whisper family). Attaching the
                        // whisper run extension to a non-whisper arch is rejected
                        // with INVALID_ARG, so skip it there and let the fuzzy
                        // post-correction handle custom words instead.
                        let family = if settings.custom_words.is_empty() || !model_is_whisper {
                            None
                        } else {
                            Some(RunExtension::Whisper(WhisperRunOptions {
                                initial_prompt: Some(settings.custom_words.join(", ")),
                                ..Default::default()
                            }))
                        };

                        let run_plan = transcribe_cpp_run_plan(
                            settings.translate_to_english,
                            &validated_language,
                            &model_languages,
                            model_supports_translate,
                        );

                        let run_options = RunOptions {
                            task: run_plan.task,
                            language: run_plan.language,
                            target_language: run_plan.target_language,
                            family,
                            ..Default::default()
                        };

                        debug!(
                            "transcribe-cpp run: task={:?}, language={:?}, initial_prompt={}",
                            run_options.task,
                            run_options.language,
                            run_options.family.is_some()
                        );

                        session
                            .run(&audio, &run_options)
                            .map(|t| t.text)
                            .map_err(|e| {
                                anyhow::anyhow!("transcribe-cpp transcription failed: {}", e)
                            })
                    }
                    LoadedEngine::Parakeet(parakeet_engine) => {
                        let params = ParakeetParams {
                            timestamp_granularity: Some(TimestampGranularity::Segment),
                            ..Default::default()
                        };
                        parakeet_engine
                            .transcribe_with(&audio, &params)
                            .map(|r| r.text)
                            .map_err(|e| anyhow::anyhow!("Parakeet transcription failed: {}", e))
                    }
                    LoadedEngine::Moonshine(moonshine_engine) => moonshine_engine
                        .transcribe(&audio, &TranscribeOptions::default())
                        .map(|r| r.text)
                        .map_err(|e| anyhow::anyhow!("Moonshine transcription failed: {}", e)),
                    LoadedEngine::MoonshineStreaming(streaming_engine) => streaming_engine
                        .transcribe(&audio, &TranscribeOptions::default())
                        .map(|r| r.text)
                        .map_err(|e| {
                            anyhow::anyhow!("Moonshine streaming transcription failed: {}", e)
                        }),
                    LoadedEngine::SenseVoice(sense_voice_engine) => {
                        let language = match normalize_cjk_language(&validated_language) {
                            "zh" => Some("zh".to_string()),
                            "en" => Some("en".to_string()),
                            "ja" => Some("ja".to_string()),
                            "ko" => Some("ko".to_string()),
                            "yue" => Some("yue".to_string()),
                            _ => None,
                        };
                        let params = SenseVoiceParams {
                            language,
                            use_itn: Some(true),
                        };
                        sense_voice_engine
                            .transcribe_with(&audio, &params)
                            .map(|r| r.text)
                            .map_err(|e| anyhow::anyhow!("SenseVoice transcription failed: {}", e))
                    }
                    LoadedEngine::GigaAM(gigaam_engine) => gigaam_engine
                        .transcribe(&audio, &TranscribeOptions::default())
                        .map(|r| r.text)
                        .map_err(|e| anyhow::anyhow!("GigaAM transcription failed: {}", e)),
                    LoadedEngine::Canary(canary_engine) => {
                        let lang = if validated_language == "auto" {
                            None
                        } else {
                            Some(validated_language.clone())
                        };
                        let options = TranscribeOptions {
                            language: lang,
                            translate: settings.translate_to_english,
                            ..Default::default()
                        };
                        canary_engine
                            .transcribe(&audio, &options)
                            .map(|r| r.text)
                            .map_err(|e| anyhow::anyhow!("Canary transcription failed: {}", e))
                    }
                    LoadedEngine::Cohere(cohere_engine) => {
                        let lang = if validated_language == "auto" {
                            None
                        } else {
                            Some(normalize_cjk_language(&validated_language).to_string())
                        };
                        let options = TranscribeOptions {
                            language: lang,
                            ..Default::default()
                        };
                        cohere_engine
                            .transcribe(&audio, &options)
                            .map(|r| r.text)
                            .map_err(|e| anyhow::anyhow!("Cohere transcription failed: {}", e))
                    }
                }
            }));

            match transcribe_result {
                Ok(Ok(text)) => {
                    // Healthy path: reset failure counter and return engine to the pool.
                    self.consecutive_engine_failures.store(0, Ordering::Release);
                    self.return_engine(engine, &active_model);
                    text
                }
                Ok(Err(inner_err)) => {
                    // Non-panic engine error. The engine object is still alive but may
                    // be wedged (e.g. Metal/Vulkan context lost after sleep). Decide
                    // whether to keep retrying with it or force a reload based on
                    // consecutive failure count. Never drop the engine while a stream
                    // is actively using it; the batch path holds the is_transcribing
                    // slot, but we double-check defensively.
                    let failures = self
                        .consecutive_engine_failures
                        .fetch_add(1, Ordering::AcqRel)
                        + 1;
                    let stream_busy = self.active_engine_lease.load(Ordering::Acquire) != 0
                        || self.is_streaming();
                    if failures >= ENGINE_HEALTH_RELOAD_THRESHOLD && !stream_busy {
                        warn!(
                            "Engine returned error {} time(s); treating as wedged and forcing reload on next attempt (last error: {})",
                            failures, inner_err
                        );
                        // Drop the wedged engine here (do NOT return_engine). Clear
                        // current_model_id so is_model_loaded() returns false and the
                        // next initiate_model_load_for actually reloads.
                        drop(engine);
                        {
                            let mut current_model = self
                                .current_model_id
                                .lock()
                                .unwrap_or_else(|e| e.into_inner());
                            *current_model = None;
                        }
                        let _ = self.app_handle.emit(
                            "model-state-changed",
                            ModelStateEvent {
                                event_type: "unloaded".to_string(),
                                model_id: None,
                                model_name: None,
                                error: Some(format!(
                                    "Engine failed {} times; forcing reload: {}",
                                    failures, inner_err
                                )),
                            },
                        );
                        let message = format!(
                            "Transcription failed (engine marked wedged after {} consecutive errors): {}",
                            failures, inner_err
                        );
                        return Err(AppError::Transcription {
                            message,
                            source: inner_err,
                        });
                    }
                    // Below threshold or stream in flight: keep the engine, return it,
                    // and surface the error so the caller can retry.
                    self.return_engine(engine, &active_model);
                    return Err(AppError::Transcription {
                        message: format!("Transcription failed: {}", inner_err),
                        source: inner_err,
                    });
                }
                Err(panic_payload) => {
                    // Engine panicked — drop it and clear state. Also reset the
                    // failure counter so a fresh load after a panic starts clean.
                    self.consecutive_engine_failures.store(0, Ordering::Release);
                    // Engine panicked — do NOT put it back (it's in an unknown state).
                    // The engine is dropped here, effectively unloading it.
                    let panic_msg = panic_payload_message(panic_payload.as_ref());
                    error!(
                        "[ENGINE-PANIC] Transcription engine panicked after {:?}: {}. Model has been unloaded.",
                        engine_start.elapsed(),
                        panic_msg
                    );

                    // Clear the model ID so it will be reloaded on next attempt
                    {
                        let mut current_model = self
                            .current_model_id
                            .lock()
                            .unwrap_or_else(|e| e.into_inner());
                        *current_model = None;
                    }

                    let _ = self.app_handle.emit(
                        "model-state-changed",
                        ModelStateEvent {
                            event_type: "unloaded".to_string(),
                            model_id: None,
                            model_name: None,
                            error: Some(format!("Engine panicked: {}", panic_msg)),
                        },
                    );

                    return Err(AppError::Transcription {
                        message: format!(
                            "Transcription engine panicked: {}. The model has been unloaded and will reload on next attempt.",
                            panic_msg
                        ),
                        source: anyhow::anyhow!("Engine panic during transcription"),
                    });
                }
            }
        };

        // Apply word correction if custom words are configured.
        let is_whisper = model_is_whisper;

        let raw_text = result.clone();

        // Optional text cleanup must never discard the successful model result.
        let final_result = fail_open_text_transform(result, |result| {
            process_transcription_text(
                &result,
                settings.word_correction_mode,
                &settings.custom_words,
                &settings.advanced_custom_words,
                &settings.word_replacements,
                settings.word_correction_threshold,
                is_whisper,
                &settings.app_language,
                &settings.custom_filler_words,
                settings.convert_us_to_british,
                settings.convert_number_words_to_digits,
                settings.spelling_dictionary,
                settings.repetition_suppression_level,
            )
        });

        if final_result != raw_text {
            info!(
                "Text processing applied: '{}' -> '{}'",
                raw_text, final_result
            );
        }

        let et = std::time::Instant::now();
        let translation_note = if settings.translate_to_english {
            " (translated)"
        } else {
            ""
        };
        // Real-time factor. Input PCM is 16 kHz mono, so audio length in seconds
        // is samples / 16000. `speedup` is audio_secs / elapsed_secs — e.g. 4.00x
        // means transcribed 4x faster than real time
        let elapsed_secs = (et - st).as_secs_f64();
        let audio_secs = audio_len as f64 / 16_000.0;
        let speedup = real_time_factor(audio_secs, elapsed_secs);
        info!(
            "[ENGINE] transcription completed for model '{}' in {:.2}s for {:.2}s of audio ({:.2}x real-time){}",
            active_model, elapsed_secs, audio_secs, speedup, translation_note
        );

        if final_result.is_empty() {
            info!("Transcription result is empty");
        } else {
            info!("Transcription result: {}", final_result);
        }

        self.maybe_unload_immediately("transcription");

        Ok(TranscriptionOutput {
            text: final_result,
            model_id: active_model,
        })
    }

    /// Run a full transcription synchronously and return only the text.
    /// Used by experiment commands to generate variants for the same audio.
    pub fn transcribe_for_benchmark(&self, audio: Vec<f32>) -> Result<String> {
        match self.transcribe(audio) {
            Ok(output) => Ok(output.text),
            Err(e) => Err(anyhow::anyhow!("Benchmark transcription failed: {e}")),
        }
    }
}

/// RAII guard that clears the `is_transcribing` flag and notifies waiters on drop.
/// Critical invariant: if the slot was force-released by a later waiter while this
/// guard's owner was still running, the stale Drop must be a no-op. It must NOT
/// overwrite the new owner's timestamp. We detect this by recording the instant
/// at which we acquired the slot and only clearing if that same instant is still
/// stored in `is_transcribing_since`.
struct TranscribingGuard {
    flag: Arc<ParkingLotMutex<bool>>,
    timestamp: Arc<ParkingLotMutex<Option<Instant>>>,
    condvar: Arc<ParkingLotCondvar>,
    acquired_at: Instant,
}

impl Drop for TranscribingGuard {
    fn drop(&mut self) {
        let mut flag = self.flag.lock();
        // Only clear the slot if it is still held *and* still owned by this guard.
        // If a later acquire force-released the slot and started a new
        // transcription, `is_transcribing_since` will contain a different
        // instant (or None), so this stale Drop must not clobber the new owner.
        let still_mine = *self.timestamp.lock() == Some(self.acquired_at);
        if *flag && still_mine {
            *flag = false;
            *self.timestamp.lock() = None;
            self.condvar.notify_all();
            debug!(
                "[SLOT] released is_transcribing after {:?}",
                self.acquired_at.elapsed()
            );
        } else {
            warn!(
                "[SLOT-STALE-DROP] stale TranscribingGuard drop skipped (wedged owner recovered)"
            );
        }
    }
}

struct StreamPerf {
    feed_count: u64,
    emit_count: u64,
    streamed_samples: u64,
    stream_compute_elapsed: Duration,
    last_log: Instant,
    latest_revision: i32,
    latest_input_received_ms: i64,
    latest_audio_committed_ms: i64,
    latest_buffered_ms: i64,
}

impl StreamPerf {
    fn new() -> Self {
        Self {
            feed_count: 0,
            emit_count: 0,
            streamed_samples: 0,
            stream_compute_elapsed: Duration::ZERO,
            last_log: Instant::now(),
            latest_revision: 0,
            latest_input_received_ms: 0,
            latest_audio_committed_ms: 0,
            latest_buffered_ms: 0,
        }
    }

    fn record_feed(&mut self, samples: usize) {
        self.feed_count += 1;
        self.streamed_samples += samples as u64;
    }

    fn record_compute(&mut self, elapsed: Duration) {
        self.stream_compute_elapsed += elapsed;
    }

    fn record_update(
        &mut self,
        revision: i32,
        input_received_ms: i64,
        audio_committed_ms: i64,
        buffered_ms: i64,
    ) {
        self.latest_revision = revision;
        self.latest_input_received_ms = input_received_ms;
        self.latest_audio_committed_ms = audio_committed_ms;
        self.latest_buffered_ms = buffered_ms;
    }

    fn record_emit(&mut self) {
        self.emit_count += 1;
    }

    fn maybe_log(&mut self) {
        if self.last_log.elapsed() < STREAM_PERF_LOG_INTERVAL {
            return;
        }

        let audio_secs = self.audio_secs();
        let compute_secs = self.compute_secs();
        debug!(
            "Live preview perf: {:.2}s streamed audio, {:.2}s model compute ({:.2}x real-time), \
             input_received={:.2}s, committed_audio={:.2}s, buffered={}ms, revision={}, \
             {} frames fed, {} updates emitted",
            audio_secs,
            compute_secs,
            real_time_factor(audio_secs, compute_secs),
            self.latest_input_received_ms as f64 / 1000.0,
            self.latest_audio_committed_ms as f64 / 1000.0,
            self.latest_buffered_ms,
            self.latest_revision,
            self.feed_count,
            self.emit_count,
        );
        self.last_log = Instant::now();
    }

    fn log_finalized(&self, chars: usize) {
        let audio_secs = self.audio_secs();
        let compute_secs = self.compute_secs();
        info!(
            "Live preview finalized in {:.2}s model compute for {:.2}s streamed audio ({:.2}x real-time): \
             input_received={:.2}s, committed_audio={:.2}s, buffered={}ms, revision={}, \
             {} frames fed, {} updates emitted, {} chars",
            compute_secs,
            audio_secs,
            real_time_factor(audio_secs, compute_secs),
            self.latest_input_received_ms as f64 / 1000.0,
            self.latest_audio_committed_ms as f64 / 1000.0,
            self.latest_buffered_ms,
            self.latest_revision,
            self.feed_count,
            self.emit_count,
            chars
        );
    }

    fn audio_secs(&self) -> f64 {
        self.streamed_samples as f64 / 16_000.0
    }

    fn compute_secs(&self) -> f64 {
        self.stream_compute_elapsed.as_secs_f64()
    }
}

fn real_time_factor(audio_secs: f64, compute_secs: f64) -> f64 {
    if compute_secs > 0.0 {
        audio_secs / compute_secs
    } else {
        0.0
    }
}

fn normalize_cjk_language(language: &str) -> &str {
    match language {
        "zh-Hans" | "zh-Hant" => "zh",
        other => other,
    }
}

/// Resolve the persisted language intent into the language a specific model can
/// use without writing the coerced value back to settings.
fn effective_language_for_model(
    settings: &AppSettings,
    model_manager: &ModelManager,
    model_id: &str,
) -> String {
    match model_manager.get_model_info(model_id) {
        Some(info) => crate::managers::model::effective_language(
            &settings.selected_language,
            &info.supported_languages,
            info.supports_language_detection,
        ),
        None => settings.selected_language.clone(),
    }
}

struct TranscribeCppRunPlan {
    task: Task,
    language: Option<String>,
    target_language: Option<String>,
}

/// Build the transcribe-cpp language/task options shared by batch and live
/// streaming paths.
fn transcribe_cpp_run_plan(
    translate_to_english: bool,
    effective_language: &str,
    model_languages: &[String],
    model_supports_translate: bool,
) -> TranscribeCppRunPlan {
    let requested_language = match effective_language {
        "auto" => None,
        other => Some(normalize_cjk_language(other).to_string()),
    };
    // Only pass a language the loaded model actually advertises (per
    // capabilities().languages); otherwise auto-detect rather than failing with
    // UNSUPPORTED_LANGUAGE. Language-agnostic models report an empty list, so
    // they always stay on auto.
    let language = requested_language.filter(|lang| model_languages.iter().any(|l| l == lang));
    let (task, target_language) = cpp_translation_task(
        translate_to_english,
        model_supports_translate,
        language.as_deref(),
    );

    TranscribeCppRunPlan {
        task,
        language,
        target_language,
    }
}

/// Decide a transcribe-cpp run's task + translation target from settings.
///
/// "Translate to English" only fires where the model advertises translation.
/// Unlike transcribe-rs (which forces the target to English itself when its
/// `translate` flag is set), transcribe-cpp requires an explicit
/// `target_language`: a null target defaults to the *source*, so a non-English
/// source silently becomes e.g. es→es and Canary rejects the unadvertised pair.
/// An English source is skipped entirely — en→en is not a real translation, and
/// it's reachable by default since auto-detect-less models coerce intent to "en".
///
/// Returns `(task, target_language)` ready to drop into `RunOptions`.
fn cpp_translation_task(
    translate_to_english: bool,
    model_supports_translate: bool,
    source_language: Option<&str>,
) -> (Task, Option<String>) {
    let translate_to_en =
        translate_to_english && model_supports_translate && source_language != Some("en");
    if translate_to_en {
        (Task::Translate, Some("en".to_string()))
    } else {
        (Task::Transcribe, None)
    }
}

/// Drain a stream command channel, ignoring fed audio, until the caller
/// finalizes or cancels. Used when streaming can't actually run (model not
/// loaded / not streaming-capable) so the finalize handshake still completes
/// and the caller falls back to batch transcription.
fn drain_until_finalize(rx: mpsc::Receiver<StreamCmd>) {
    while let Ok(cmd) = rx.recv() {
        match cmd {
            StreamCmd::Feed(_) => {}
            StreamCmd::Finalize(reply) => {
                let _ = reply.send(None);
                break;
            }
            StreamCmd::Cancel => break,
        }
    }
}

/// Initialize the transcribe-cpp native backend once at startup: route native +
/// ggml diagnostics into the `log` facade and register compute backend modules.
/// In a static build (macOS Metal) `init_backends_default` is a harmless no-op;
/// in a `dynamic-backends` build it loads the per-ISA CPU / GPU modules. Must run
/// before the first model load.
pub fn init_transcribe_backend() {
    transcribe_cpp::init_logging();
    match transcribe_cpp::init_backends_default() {
        Ok(()) => {
            if transcribe_gpu_disabled_for_host() {
                warn!(
                    "Windows x64 build is running under emulation on an ARM64 host; \
                     disabling transcribe.cpp GPU acceleration and using CPU"
                );
            }
        }
        Err(e) => warn!("Failed to initialize transcribe-cpp backends: {}", e),
    }
}

/// Log the compute devices [`init_transcribe_backend`] registered.
///
/// Listing devices is what first opens the GPU. On macOS that loads ggml's
/// Metal library, which is compiled from source whenever the system's shader
/// cache does not hold it yet (the first launch after an install or update),
/// so the app calls this from a background thread instead of its startup
/// path. A model load that comes first waits on the same one-time compile.
pub fn report_compute_devices() {
    let devices = transcribe_compute_devices();
    info!(
        "transcribe-cpp initialized with {} compute device(s): [{}]",
        devices.len(),
        devices
            .iter()
            .map(|d| format!("{} ({})", d.name, d.kind))
            .collect::<Vec<_>>()
            .join(", ")
    );
}

/// Human-readable list of the transcribe-cpp compute devices registered at
/// startup, for the `--list-devices` flag. The reported `index` is the
/// value to pass to `--device-index`. Backends must be initialized first
/// (see [`init_transcribe_backend`]).
pub fn describe_compute_devices() -> Vec<String> {
    transcribe_compute_devices()
        .into_iter()
        .map(|d| {
            let idx = d
                .index
                .map(|i| i.to_string())
                .unwrap_or_else(|| "-".to_string());
            let name = if d.description.is_empty() {
                d.name
            } else {
                d.description
            };
            let vram_mb = d.memory_total / (1024 * 1024);
            format!(
                "index={} kind={} name={} vram={}MB",
                idx, d.kind, name, vram_mb
            )
        })
        .collect()
}

/// Resolve a `--list-devices` registry index to an exact opaque device handle
/// for a transcribe-cpp model load (the `--device-index` flag). In 0.2 index 0
/// is an exact selection too; only an omitted index requests automatic device
/// selection. Errors if the index isn't a registered, loadable primary device.
fn resolve_device_index(index: usize) -> Result<(Backend, Option<transcribe_cpp::Device>)> {
    let device = transcribe_compute_devices()
        .into_iter()
        .find(|d| d.index == Some(index))
        .ok_or_else(|| {
            anyhow::anyhow!("No compute device with index {index} (see --list-devices)")
        })?;
    if matches!(
        device.device_type,
        transcribe_cpp::DeviceType::Accel | transcribe_cpp::DeviceType::Unknown
    ) {
        return Err(anyhow::anyhow!(
            "Device index {index} ({}) cannot host a model",
            device.kind
        ));
    }

    // 0.2's opaque handle makes every index, including zero, an exact
    // selection. Backend::Auto accepts any primary device and cannot conflict
    // with the selected device's vendor backend.
    Ok((Backend::Auto, Some(device)))
}

/// Map Handy's whisper accelerator setting to a transcribe-cpp [`Backend`].
///
/// `Auto` lets the library pick the best device (with CPU fallback), while
/// `Cpu` forces strict CPU. `Gpu` only remains as the companion setting for an
/// exact device; without a valid exact device it has the retired generic GPU
/// state's new Auto semantics. An emulated x64 process on Windows ARM64 forces
/// strict CPU for every setting.
fn select_transcribe_backend(setting: TranscribeAcceleratorSetting) -> Backend {
    select_transcribe_backend_for_host(setting, transcribe_gpu_disabled_for_host())
}

fn select_transcribe_backend_for_host(
    setting: TranscribeAcceleratorSetting,
    gpu_disabled: bool,
) -> Backend {
    match effective_transcribe_accelerator(setting, gpu_disabled) {
        TranscribeAcceleratorSetting::Cpu => Backend::Cpu,
        TranscribeAcceleratorSetting::Auto | TranscribeAcceleratorSetting::Gpu => Backend::Auto,
    }
}

/// Resolve the user's persisted GPU identity to a fresh opaque 0.2 device
/// handle. Registry indices and handles are process-local, so settings store a
/// key based on the backend's stable `device_id` (falling back to name for
/// backends such as Metal that do not report one).
fn resolve_gpu_device(
    setting: TranscribeAcceleratorSetting,
    gpu_device: Option<&str>,
) -> Option<transcribe_cpp::Device> {
    if transcribe_gpu_disabled_for_host() || setting != TranscribeAcceleratorSetting::Gpu {
        return None;
    }
    let gpu_device = gpu_device?;
    let resolved = transcribe_compute_devices().into_iter().find(|device| {
        is_transcribe_gpu_device(device) && transcribe_device_key(device) == gpu_device
    });
    if resolved.is_none() {
        warn!(
            "Stored transcribe GPU device '{}' is no longer available; using automatic device selection",
            gpu_device
        );
    }
    resolved
}

fn transcribe_device_key(device: &transcribe_cpp::Device) -> String {
    let (identity_kind, identity) = match device.device_id.as_deref() {
        Some(device_id) => ("id", device_id),
        None => ("name", device.name.as_str()),
    };
    serde_json::to_string(&(device.kind.as_str(), identity_kind, identity))
        .expect("transcribe device identity is always JSON serializable")
}

fn transcribe_device_label(device: &transcribe_cpp::Device) -> String {
    if device.description.is_empty() {
        device.name.clone()
    } else {
        device.description.clone()
    }
}

/// Apply the user's ORT accelerator preference to the transcribe-rs global.
/// Called on startup and before loading a model.
///
/// The transcribe.cpp (whisper-family) backend is no longer set here: it is
/// chosen at model-load time from [`select_transcribe_backend`], so changing the
/// accelerator only needs a model reload (see `reload_model_on_next_use`).
pub fn apply_accelerator_settings(app: &tauri::AppHandle) {
    use transcribe_rs::accel;

    let settings = get_settings(app);

    info!(
        "transcribe.cpp accelerator preference: {:?} (applied on next model load)",
        settings.transcribe_accelerator
    );

    let ort_pref = match settings.ort_accelerator {
        OrtAcceleratorSetting::Auto => accel::OrtAccelerator::Auto,
        OrtAcceleratorSetting::Cpu => accel::OrtAccelerator::CpuOnly,
        OrtAcceleratorSetting::Cuda => accel::OrtAccelerator::Cuda,
        OrtAcceleratorSetting::DirectMl => accel::OrtAccelerator::DirectMl,
        OrtAcceleratorSetting::Rocm => accel::OrtAccelerator::Rocm,
    };
    accel::set_ort_accelerator(ort_pref);
    info!("ORT accelerator set to: {}", ort_pref);
}

#[derive(Serialize, Clone, Debug, Type)]
pub struct GpuDeviceOption {
    pub id: String,
    pub name: String,
    pub total_vram_mb: usize,
}

static GPU_DEVICES: OnceLock<Vec<GpuDeviceOption>> = OnceLock::new();

fn transcribe_gpu_disabled_for_host() -> bool {
    crate::utils::is_windows_x64_emulated_on_arm64()
}

fn effective_transcribe_accelerator(
    setting: TranscribeAcceleratorSetting,
    gpu_disabled: bool,
) -> TranscribeAcceleratorSetting {
    if gpu_disabled {
        TranscribeAcceleratorSetting::Cpu
    } else {
        setting
    }
}

fn is_transcribe_gpu_device(device: &transcribe_cpp::Device) -> bool {
    matches!(
        device.device_type,
        transcribe_cpp::DeviceType::Gpu | transcribe_cpp::DeviceType::Igpu
    )
}

fn transcribe_device_allowed(kind: &str, gpu_disabled: bool) -> bool {
    !gpu_disabled || matches!(kind, "cpu" | "accel")
}

fn transcribe_compute_devices() -> Vec<transcribe_cpp::Device> {
    let devices = transcribe_cpp::devices();
    let gpu_disabled = transcribe_gpu_disabled_for_host();
    if !gpu_disabled {
        return devices;
    }

    devices
        .into_iter()
        .filter(|device| transcribe_device_allowed(&device.kind, gpu_disabled))
        .collect()
}

fn available_transcribe_accelerators(gpu_disabled: bool) -> Vec<String> {
    if gpu_disabled {
        vec!["cpu".to_string()]
    } else {
        vec!["auto".to_string(), "cpu".to_string(), "gpu".to_string()]
    }
}

fn cached_gpu_devices() -> &'static [GpuDeviceOption] {
    // GPU compute devices transcribe-cpp registered at startup. `id` is a
    // persistent identity key, never the process-local registry index. It uses
    // the backend's device_id where available and its name otherwise (Metal).
    // `total_vram_mb` is 0 when the backend does not report capacity.
    GPU_DEVICES.get_or_init(|| {
        transcribe_compute_devices()
            .into_iter()
            .filter(is_transcribe_gpu_device)
            .map(|d| GpuDeviceOption {
                id: transcribe_device_key(&d),
                name: transcribe_device_label(&d),
                total_vram_mb: (d.memory_total / (1024 * 1024)) as usize,
            })
            .collect()
    })
}

#[derive(Serialize, Clone, Debug, Type)]
pub struct AvailableAccelerators {
    pub transcribe: Vec<String>,
    pub ort: Vec<String>,
    pub gpu_devices: Vec<GpuDeviceOption>,
}

/// Return the accelerators available to this process on its current host.
pub fn get_available_accelerators() -> AvailableAccelerators {
    use transcribe_rs::accel::OrtAccelerator;

    let ort_options: Vec<String> = OrtAccelerator::available()
        .into_iter()
        .map(|a| a.to_string())
        .collect();

    let transcribe_options = available_transcribe_accelerators(transcribe_gpu_disabled_for_host());

    AvailableAccelerators {
        transcribe: transcribe_options,
        ort: ort_options,
        gpu_devices: cached_gpu_devices().to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn languages(codes: &[&str]) -> Vec<String> {
        codes.iter().map(|code| (*code).to_string()).collect()
    }

    #[test]
    fn normal_hosts_preserve_every_transcribe_accelerator_setting() {
        for setting in [
            TranscribeAcceleratorSetting::Auto,
            TranscribeAcceleratorSetting::Cpu,
            TranscribeAcceleratorSetting::Gpu,
        ] {
            assert_eq!(effective_transcribe_accelerator(setting, false), setting);
        }
        assert_eq!(
            available_transcribe_accelerators(false),
            ["auto", "cpu", "gpu"]
        );
        for kind in ["cpu", "accel", "metal", "cuda", "vulkan", "gpu"] {
            assert!(transcribe_device_allowed(kind, false));
        }
    }

    #[test]
    fn emulated_x64_on_arm64_forces_every_transcribe_setting_to_cpu() {
        for setting in [
            TranscribeAcceleratorSetting::Auto,
            TranscribeAcceleratorSetting::Cpu,
            TranscribeAcceleratorSetting::Gpu,
        ] {
            assert_eq!(
                effective_transcribe_accelerator(setting, true),
                TranscribeAcceleratorSetting::Cpu
            );
        }
        assert_eq!(available_transcribe_accelerators(true), ["cpu"]);
        assert!(transcribe_device_allowed("cpu", true));
        assert!(transcribe_device_allowed("accel", true));
        for kind in ["metal", "cuda", "vulkan", "gpu", "unknown"] {
            assert!(!transcribe_device_allowed(kind, true));
        }
    }

    #[test]
    fn optional_text_transform_falls_back_to_raw_text_after_panic() {
        let raw = "原始轉錄。".to_string();
        let result = fail_open_text_transform(raw.clone(), |_| {
            panic!("simulated optional cleanup failure")
        });

        assert_eq!(result, raw);
    }

    #[test]
    fn transcribe_cpp_run_plan_maps_chinese_variants() {
        let plan = transcribe_cpp_run_plan(false, "zh-Hant", &languages(&["zh"]), true);

        assert!(matches!(plan.task, Task::Transcribe));
        assert_eq!(plan.language.as_deref(), Some("zh"));
        assert_eq!(plan.target_language, None);
    }

    #[test]
    fn transcribe_cpp_run_plan_skips_english_translation() {
        let plan = transcribe_cpp_run_plan(true, "en", &languages(&["en", "es"]), true);

        assert!(matches!(plan.task, Task::Transcribe));
        assert_eq!(plan.language.as_deref(), Some("en"));
        assert_eq!(plan.target_language, None);
    }

    #[test]
    fn transcribe_cpp_run_plan_translates_supported_non_english() {
        let plan = transcribe_cpp_run_plan(true, "es", &languages(&["en", "es"]), true);

        assert!(matches!(plan.task, Task::Translate));
        assert_eq!(plan.language.as_deref(), Some("es"));
        assert_eq!(plan.target_language.as_deref(), Some("en"));
    }

    #[test]
    fn transcribe_cpp_run_plan_requires_model_translation_support() {
        let plan = transcribe_cpp_run_plan(true, "es", &languages(&["en", "es"]), false);

        assert!(matches!(plan.task, Task::Transcribe));
        assert_eq!(plan.language.as_deref(), Some("es"));
        assert_eq!(plan.target_language, None);
    }

    // ── Slot watchdog tests: these exercise the pure mutex/condvar state
    // without constructing a full TranscriptionManager (which requires an
    // AppHandle). The same primitives are used by `try_acquire_transcribing_slot`.

    /// A held-too-long `is_transcribing` slot is force-released by the next
    /// waiter so a wedged worker cannot block transcription forever.
    #[test]
    fn transcription_slot_force_releases_after_watchdog_threshold() {
        use parking_lot::{Condvar as ParkingLotCondvar, Mutex as ParkingLotMutex};

        let flag = Arc::new(ParkingLotMutex::new(false));
        let since = Arc::new(ParkingLotMutex::new(None::<Instant>));
        let condvar = Arc::new(ParkingLotCondvar::new());

        // Simulate a transcription that grabbed the slot long ago and vanished.
        {
            let mut g = flag.lock();
            *g = true;
            *since.lock() = Some(Instant::now() - TRANSCRIBING_SLOT_FORCE_RELEASE_AFTER);
        }

        // A waiter should observe the stale hold, force-release it, and acquire.
        let wait_start = Instant::now();
        let deadline = wait_start + Duration::from_secs(30);
        let mut is_transcribing = flag.lock();
        while *is_transcribing {
            let held_for = {
                let guard = since.lock();
                guard
                    .as_ref()
                    .copied()
                    .map(|s| Instant::now().duration_since(s))
            };
            if held_for.map_or(false, |d| d >= TRANSCRIBING_SLOT_FORCE_RELEASE_AFTER) {
                *is_transcribing = false;
                *since.lock() = None;
                condvar.notify_all();
            }

            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                panic!("timed out waiting for slot");
            }
            let slice = remaining.min(Duration::from_millis(100));
            condvar.wait_for(&mut is_transcribing, slice);
        }
        *is_transcribing = true;
        *since.lock() = Some(Instant::now());
        drop(is_transcribing);

        // Slot acquired; clean it up.
        {
            let mut g = flag.lock();
            *g = false;
            *since.lock() = None;
            condvar.notify_all();
        }
    }

    /// If a model load never finishes, the `is_loading` wait eventually times
    /// out, clears the flag, and returns `ModelNotLoaded` rather than hanging.
    /// This test uses a shortened timeout so it does not run for 60 seconds.
    #[test]
    fn is_loading_wait_returns_model_not_loaded_after_timeout() {
        let is_loading = Arc::new(std::sync::Mutex::new(true));
        let condvar = Arc::new(std::sync::Condvar::new());

        let mut guard = is_loading.lock().unwrap();
        let wait_start = Instant::now();
        let mut timed_out = false;
        // Use a short test timeout so this remains a unit test; the production
        // constant is 60s, which is intentionally too long to exercise here.
        let test_timeout = Duration::from_millis(250);
        while *guard {
            if wait_start.elapsed() >= test_timeout {
                *guard = false;
                condvar.notify_all();
                timed_out = true;
                break;
            }
            let result = condvar
                .wait_timeout(guard, Duration::from_millis(100))
                .unwrap();
            guard = result.0;
        }

        assert!(timed_out, "expected the is_loading wait to time out");
        assert!(!*guard, "is_loading should have been cleared on timeout");
    }

    /// Dropping a guard whose slot was already force-released by a later owner
    /// must not clear the new owner's flag or overwrite its timestamp.
    #[test]
    fn stale_transcribing_guard_drop_does_not_clobber_new_owner() {
        use parking_lot::{Condvar as ParkingLotCondvar, Mutex as ParkingLotMutex};

        let flag = Arc::new(ParkingLotMutex::new(false));
        let since = Arc::new(ParkingLotMutex::new(None::<Instant>));
        let condvar = Arc::new(ParkingLotCondvar::new());

        // Owner A acquires the slot and stores its timestamp.
        let a_acquired_at = Instant::now();
        {
            let mut g = flag.lock();
            *g = true;
            *since.lock() = Some(a_acquired_at);
        }
        let a_guard = TranscribingGuard {
            flag: flag.clone(),
            timestamp: since.clone(),
            condvar: condvar.clone(),
            acquired_at: a_acquired_at,
        };
        // Keep A's guard alive by moving it into a variable that outlives B.
        let _a_holder = a_guard;

        // Simulate the slot being force-released by setting the timestamp to an
        // old instant, then a new owner B acquires the slot with a fresh timestamp.
        let b_acquired_at = Instant::now() - Duration::from_secs(5);
        {
            let mut g = flag.lock();
            *g = true;
            *since.lock() = Some(b_acquired_at);
        }

        // Drop owner A's stale guard.
        drop(_a_holder);

        // Owner B's slot must still be held and the timestamp must not be clobbered.
        let flag_after = *flag.lock();
        let since_after = *since.lock();
        assert!(flag_after, "stale TranscribingGuard drop cleared the slot");
        assert_eq!(
            since_after,
            Some(b_acquired_at),
            "stale TranscribingGuard drop overwrote the new owner's timestamp"
        );

        // Clean up.
        {
            let mut g = flag.lock();
            *g = false;
            *since.lock() = None;
            condvar.notify_all();
        }
    }

    #[test]
    fn slot_force_releases_on_wait_timeout_then_acquires() {
        let flag = Arc::new(ParkingLotMutex::new(false));
        let since = Arc::new(ParkingLotMutex::new(None::<Instant>));
        let condvar = Arc::new(ParkingLotCondvar::new());

        // Simulate a transcription that grabbed the slot 5s ago. The inner 60s
        // watchdog cannot fire during a 30s wait, but the outer
        // FORCE_RELEASE_ON_OWN_TIMEOUT path (>=30s held at timeout) will fire
        // at the 30s deadline (held = 35s) and let us acquire the slot.
        {
            let mut g = flag.lock();
            *g = true;
            *since.lock() = Some(Instant::now() - Duration::from_secs(5));
        }

        let guard = TranscribingGuard {
            flag: flag.clone(),
            timestamp: since.clone(),
            condvar: condvar.clone(),
            acquired_at: Instant::now(),
        };

        // Helper mirrors try_acquire_transcribing_slot's timeout branch.
        let acquired =
            try_acquire_test_slot(&flag, &since, &condvar, Duration::from_secs(30), || false);

        assert!(
            acquired.is_some(),
            "should acquire via the outer FORCE_RELEASE_ON_OWN_TIMEOUT path"
        );
        let new_guard = acquired.unwrap();
        assert!(*flag.lock(), "slot should be held by the new guard");
        assert_eq!(*since.lock(), Some(new_guard.acquired_at));

        // Clean up: drop old guard then new guard.
        drop(guard);
        drop(new_guard);
        {
            let mut g = flag.lock();
            *g = false;
            *since.lock() = None;
            condvar.notify_all();
        }
    }

    #[test]
    fn slot_returns_busy_if_retaken_after_force_release() {
        let flag = Arc::new(ParkingLotMutex::new(false));
        let since = Arc::new(ParkingLotMutex::new(None::<Instant>));
        let condvar = Arc::new(ParkingLotCondvar::new());

        // First stale owner held for 31s.
        {
            let mut g = flag.lock();
            *g = true;
            *since.lock() = Some(Instant::now() - Duration::from_secs(31));
        }

        // A second owner retakes the slot inside the on_force_release hook.
        // Because the helper drops its lock before invoking the hook, this
        // second owner can acquire the same outer lock, set the flag, and
        // release it. When the helper re-acquires the lock, the slot is busy
        // and the helper returns None.
        let second_owner_timestamp = Instant::now();
        let flag_for_hook = flag.clone();
        let since_for_hook = since.clone();
        let b_took = Arc::new(ParkingLotMutex::new(false));
        let b_took_for_hook = b_took.clone();

        let acquired = try_acquire_test_slot_with_hook(
            &flag,
            &since,
            &condvar,
            Duration::from_millis(10),
            || false,
            || {
                let mut g = flag_for_hook.lock();
                *g = true;
                *since_for_hook.lock() = Some(second_owner_timestamp);
                *b_took_for_hook.lock() = true;
                // g drops here, releasing the lock.
            },
        );

        assert!(*b_took.lock(), "second owner should have retaken the slot");
        assert!(
            acquired.is_none(),
            "should return busy when another owner retakes the slot after force-release"
        );

        // Clean up.
        {
            let mut g = flag.lock();
            *g = false;
            *since.lock() = None;
            condvar.notify_all();
        }
    }

    #[test]
    fn background_watchdog_force_releases_idle_held_slot() {
        let flag = Arc::new(ParkingLotMutex::new(false));
        let since = Arc::new(ParkingLotMutex::new(None::<Instant>));
        let condvar = Arc::new(ParkingLotCondvar::new());

        // Stale owner held for 70s.
        {
            let mut g = flag.lock();
            *g = true;
            *since.lock() = Some(Instant::now() - Duration::from_secs(70));
        }

        check_and_force_release_test_slot(&flag, &since, &condvar);

        assert!(!*flag.lock(), "watchdog should clear the stale slot");
        assert_eq!(*since.lock(), None);
    }

    #[test]
    fn stale_guard_drop_after_background_force_release_is_noop() {
        let flag = Arc::new(ParkingLotMutex::new(false));
        let since = Arc::new(ParkingLotMutex::new(None::<Instant>));
        let condvar = Arc::new(ParkingLotCondvar::new());

        // Owner A acquires the slot.
        let a_acquired_at = Instant::now();
        {
            let mut g = flag.lock();
            *g = true;
            *since.lock() = Some(a_acquired_at);
        }
        let a_guard = TranscribingGuard {
            flag: flag.clone(),
            timestamp: since.clone(),
            condvar: condvar.clone(),
            acquired_at: a_acquired_at,
        };

        // Background watchdog force-releases A's slot.
        check_and_force_release_test_slot(&flag, &since, &condvar);

        // New owner B acquires with a fresh timestamp.
        let b_acquired_at = Instant::now();
        {
            let mut g = flag.lock();
            *g = true;
            *since.lock() = Some(b_acquired_at);
            condvar.notify_all();
        }

        // Dropping the stale A guard must not clobber B.
        drop(a_guard);
        assert!(
            *flag.lock(),
            "stale guard drop should not clear the active slot"
        );
        assert_eq!(
            *since.lock(),
            Some(b_acquired_at),
            "stale guard drop should not overwrite the new owner's timestamp"
        );

        // Clean up.
        {
            let mut g = flag.lock();
            *g = false;
            *since.lock() = None;
            condvar.notify_all();
        }
    }

    /// Helpers for the slot-recovery tests above. They mirror the production
    /// logic in `try_acquire_transcribing_slot` and
    /// `check_and_force_release_transcribing_slot` but do not need an AppHandle.
    fn try_acquire_test_slot(
        flag: &Arc<ParkingLotMutex<bool>>,
        since: &Arc<ParkingLotMutex<Option<Instant>>>,
        condvar: &Arc<ParkingLotCondvar>,
        max_wait: Duration,
        cancel_check: impl Fn() -> bool,
    ) -> Option<TranscribingGuard> {
        try_acquire_test_slot_with_hook(flag, since, condvar, max_wait, cancel_check, || {})
    }

    fn try_acquire_test_slot_with_hook(
        flag: &Arc<ParkingLotMutex<bool>>,
        since: &Arc<ParkingLotMutex<Option<Instant>>>,
        condvar: &Arc<ParkingLotCondvar>,
        max_wait: Duration,
        cancel_check: impl Fn() -> bool,
        on_force_release: impl FnMut(),
    ) -> Option<TranscribingGuard> {
        let wait_start = Instant::now();
        let deadline = wait_start + max_wait;
        let mut force_released_once = false;
        let mut on_force_release = on_force_release;

        let mut is_transcribing = flag.lock();
        while *is_transcribing {
            let held_for = {
                let guard = since.lock();
                guard
                    .as_ref()
                    .copied()
                    .map(|s| Instant::now().duration_since(s))
            };

            // Inner watchdog: force-release if the owner has held far too long.
            if held_for.map_or(false, |d| d >= TRANSCRIBING_SLOT_FORCE_RELEASE_AFTER) {
                *is_transcribing = false;
                *since.lock() = None;
                condvar.notify_all();
            }

            if cancel_check() {
                return None;
            }

            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                // Outer recovery: this waiter's own timeout has expired. If the
                // slot has been held for at least the wait timeout, force-release
                // it once and retry acquisition.
                if !force_released_once
                    && held_for.map_or(false, |d| {
                        d >= TRANSCRIBING_SLOT_FORCE_RELEASE_ON_OWN_TIMEOUT
                    })
                {
                    *is_transcribing = false;
                    *since.lock() = None;
                    condvar.notify_all();
                    force_released_once = true;
                    drop(is_transcribing);
                    on_force_release();
                    is_transcribing = flag.lock();
                    continue;
                }
                return None;
            }

            let slice = remaining.min(Duration::from_millis(100));
            let timed_out = condvar.wait_for(&mut is_transcribing, slice).timed_out();
            if timed_out && cancel_check() {
                return None;
            }
        }

        let acquired_at = Instant::now();
        *is_transcribing = true;
        *since.lock() = Some(acquired_at);
        drop(is_transcribing);

        Some(TranscribingGuard {
            flag: flag.clone(),
            timestamp: since.clone(),
            condvar: condvar.clone(),
            acquired_at,
        })
    }

    fn check_and_force_release_test_slot(
        flag: &Arc<ParkingLotMutex<bool>>,
        since: &Arc<ParkingLotMutex<Option<Instant>>>,
        condvar: &Arc<ParkingLotCondvar>,
    ) {
        let mut is_transcribing = flag.lock();
        if !*is_transcribing {
            return;
        }
        let held_for = {
            let guard = since.lock();
            guard
                .as_ref()
                .copied()
                .map(|s| Instant::now().duration_since(s))
        };
        if held_for.map_or(false, |d| d >= TRANSCRIBING_SLOT_FORCE_RELEASE_AFTER) {
            *is_transcribing = false;
            *since.lock() = None;
            condvar.notify_all();
        }
    }

    /// A panic inside the model-load thread still clears `is_loading` and wakes
    /// waiters, because the thread's reset guard runs on unwind.
    #[test]
    fn model_load_thread_panic_clears_is_loading() {
        let is_loading = Arc::new(std::sync::Mutex::new(true));
        let condvar = Arc::new(std::sync::Condvar::new());

        let is_loading_clone = is_loading.clone();
        let condvar_clone = condvar.clone();

        std::thread::spawn(move || {
            struct ResetLoadingGuard {
                is_loading: Arc<std::sync::Mutex<bool>>,
                loading_condvar: Arc<std::sync::Condvar>,
            }
            impl Drop for ResetLoadingGuard {
                fn drop(&mut self) {
                    let mut guard = match self.is_loading.lock() {
                        Ok(g) => g,
                        Err(e) => {
                            warn!(
                                "Recovered poisoned is_loading mutex during model-load thread exit"
                            );
                            e.into_inner()
                        }
                    };
                    *guard = false;
                    self.loading_condvar.notify_all();
                }
            }
            let _reset_guard = ResetLoadingGuard {
                is_loading: is_loading_clone,
                loading_condvar: condvar_clone,
            };

            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                panic!("simulated model load panic");
            }))
            .ok();
            // _reset_guard drops here and must clear is_loading.
        });

        // Wait up to a few seconds for the panic thread to finish.
        let mut guard = is_loading.lock().unwrap();
        let wait_start = Instant::now();
        while *guard {
            let result = condvar
                .wait_timeout(guard, Duration::from_millis(100))
                .unwrap();
            guard = result.0;
            if wait_start.elapsed() > Duration::from_secs(5) {
                panic!("is_loading was not cleared after model-load panic");
            }
        }

        assert!(!*guard);
    }
}

impl Drop for TranscriptionManager {
    fn drop(&mut self) {
        // Skip shutdown unless this is the very last clone. TranscriptionManager
        // is cloned by initiate_model_load() and the watcher thread — those
        // clones dropping must not kill the watcher. The watcher thread holds
        // its own clone, so engine's strong_count is always >= 2 while the
        // watcher is alive. When it reaches 1, only this instance remains
        // and we can safely shut down.
        if Arc::strong_count(&self.engine) > 1 {
            return;
        }

        // Signal the watcher thread to shutdown
        self.shutdown_signal.store(true, Ordering::Relaxed);

        // Wait for the thread to finish gracefully.
        // Use match instead of unwrap to avoid panicking if the mutex is
        // poisoned — a panic inside Drop calls abort().
        let mut guard = match self.watcher_handle.lock() {
            Ok(g) => g,
            Err(e) => {
                warn!("Recovered poisoned watcher_handle mutex during TranscriptionManager drop — a panic occurred earlier this session");
                e.into_inner()
            }
        };
        if let Some(handle) = guard.take() {
            if let Err(e) = handle.join() {
                warn!("Failed to join idle watcher thread: {:?}", e);
            } else {
                debug!("Idle watcher thread joined successfully");
            }
        }
    }
}
