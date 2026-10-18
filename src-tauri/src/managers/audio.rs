use crate::audio_toolkit::{
    is_bluetooth_audio_active, list_input_devices, list_output_devices,
    vad::{frames_for_duration_ms, EarshotVad, SmoothedVad, VAD_ONSET_MS, VAD_PREFILL_MS},
    AudioRecorder, SileroVad, VoiceActivityDetector,
};
use crate::errors::AppError;
use crate::helpers::clamshell;
use crate::managers::model::{EngineType, ModelManager};
use crate::portable;
use crate::settings::{get_settings, write_settings, AppSettings, VadBackend};
use crate::streaming_cancel_flag::StreamingCancelFlag;
use crate::usb_watchdog;
use crate::usb_watchdog::UsbWatchdog;
use crate::utils;
use crate::LiveCaptionsTranscriptionManager;
use log::{debug, error, info, trace, warn};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use specta::Type;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{Emitter, Manager};

const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const EARSHOT_VAD_THRESHOLD: f32 = 0.5;

fn set_mute(mute: bool) {
    // Expected behavior:
    // - Windows: works on most systems using standard audio drivers.
    // - Linux: works on many systems (PipeWire, PulseAudio, ALSA),
    //   but some distros may lack the tools used.
    // - macOS: works on most standard setups via AppleScript.
    // If unsupported, fails silently.

    #[cfg(target_os = "windows")]
    {
        unsafe {
            use windows::Win32::{
                Media::Audio::{
                    eMultimedia, eRender, Endpoints::IAudioEndpointVolume, IMMDeviceEnumerator,
                    MMDeviceEnumerator,
                },
                System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED},
            };

            macro_rules! unwrap_or_return {
                ($expr:expr) => {
                    match $expr {
                        Ok(val) => val,
                        Err(_) => return,
                    }
                };
            }

            // Initialize the COM library for this thread.
            // If already initialized (e.g., by another library like Tauri), this does nothing.
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);

            let all_devices: IMMDeviceEnumerator =
                unwrap_or_return!(CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL));
            let default_device =
                unwrap_or_return!(all_devices.GetDefaultAudioEndpoint(eRender, eMultimedia));
            let volume_interface = unwrap_or_return!(
                default_device.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None)
            );

            let _ = volume_interface.SetMute(mute, std::ptr::null());
        }
    }

    #[cfg(target_os = "linux")]
    {
        use std::process::Command;

        let mute_val = if mute { "1" } else { "0" };
        let amixer_state = if mute { "mute" } else { "unmute" };

        // Try multiple backends to increase compatibility
        // 1. PipeWire (wpctl)
        if Command::new("wpctl")
            .args(["set-mute", "@DEFAULT_AUDIO_SINK@", mute_val])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
        {
            return;
        }

        // 2. PulseAudio (pactl)
        if Command::new("pactl")
            .args(["set-sink-mute", "@DEFAULT_SINK@", mute_val])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
        {
            return;
        }

        // 3. ALSA (amixer)
        let _ = Command::new("amixer")
            .args(["set", "Master", amixer_state])
            .output();
    }

    #[cfg(target_os = "macos")]
    {
        use std::process::Command;
        let script = format!(
            "set volume output muted {}",
            if mute { "true" } else { "false" }
        );
        let _ = Command::new("osascript").args(["-e", &script]).output();
    }
}

/// Reads the current system output mute state, mirroring `set_mute`'s backends.
///
/// Returns `Some(true)`/`Some(false)` when the state could be determined, or
/// `None` when it couldn't. Callers treat `None` as "unknown" and fall back to
/// unmuting on stop so we never strand the user's audio muted.
#[cfg(target_os = "windows")]
fn get_mute() -> Option<bool> {
    unsafe {
        use windows::Win32::{
            Media::Audio::{
                eMultimedia, eRender, Endpoints::IAudioEndpointVolume, IMMDeviceEnumerator,
                MMDeviceEnumerator,
            },
            System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED},
        };

        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);

        let all_devices: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()?;
        let default_device = all_devices
            .GetDefaultAudioEndpoint(eRender, eMultimedia)
            .ok()?;
        let volume_interface = default_device
            .Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None)
            .ok()?;

        Some(volume_interface.GetMute().ok()?.as_bool())
    }
}

#[cfg(target_os = "linux")]
fn get_mute() -> Option<bool> {
    use std::process::Command;

    if let Ok(out) = Command::new("wpctl")
        .args(["get-volume", "@DEFAULT_AUDIO_SINK@"])
        .output()
    {
        if out.status.success() {
            return Some(String::from_utf8_lossy(&out.stdout).contains("[MUTED]"));
        }
    }

    if let Ok(out) = Command::new("pactl")
        .env("LC_ALL", "C")
        .args(["get-sink-mute", "@DEFAULT_SINK@"])
        .output()
    {
        if out.status.success() {
            let s = String::from_utf8_lossy(&out.stdout).to_lowercase();
            if s.contains("yes") {
                return Some(true);
            }
            if s.contains("no") {
                return Some(false);
            }
        }
    }

    if let Ok(out) = Command::new("amixer")
        .env("LC_ALL", "C")
        .args(["get", "Master"])
        .output()
    {
        if out.status.success() {
            let s = String::from_utf8_lossy(&out.stdout);
            if s.contains("[off]") {
                return Some(true);
            }
            if s.contains("[on]") {
                return Some(false);
            }
        }
    }

    None
}

#[cfg(target_os = "macos")]
fn get_mute() -> Option<bool> {
    use std::process::Command;

    let out = Command::new("osascript")
        .args(["-e", "output muted of (get volume settings)"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    match String::from_utf8_lossy(&out.stdout).trim() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

#[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
fn get_mute() -> Option<bool> {
    None
}

/// Restores the system mute state after our forced mute. We only unmute when
/// the system was NOT already muted beforehand; if it was muted we leave it.
/// If the prior state is unknown we default to unmuting so audio is never
/// left stranded muted by us.
fn restore_mute(prev_muted: Option<bool>) {
    if prev_muted != Some(true) {
        set_mute(false);
    }
}

const WHISPER_SAMPLE_RATE: usize = 16000;

/// Threshold for "no audio detected" during recording.
/// Max RMS below this indicates the microphone is likely dead or muted.
/// ~0.001 is ~-60dB for normalized f32 samples — essentially silence.
const NO_AUDIO_THRESHOLD: f32 = 0.001;

/* ──────────────────────────────────────────────────────────────── */

#[derive(Clone, Debug)]
pub enum RecordingState {
    Idle,
    Recording {
        binding_id: String,
        start_time: Instant,
    },
}

#[derive(Clone, Debug)]
pub enum MicrophoneMode {
    AlwaysOn,
    OnDemand,
}

/// Payload for the `device-list-changed` event emitted when audio
/// devices are hot-plugged (added or removed).
#[derive(Clone, Debug, Serialize, Deserialize, Type, tauri_specta::Event)]
pub struct DeviceListChanged {
    /// Input devices that appeared since the last check.
    pub added_input: Vec<String>,
    /// Input devices that disappeared since the last check.
    pub removed_input: Vec<String>,
    /// Current list of all input device names.
    pub current_input: Vec<String>,
    /// Output devices that appeared since the last check.
    pub added_output: Vec<String>,
    /// Output devices that disappeared since the last check.
    pub removed_output: Vec<String>,
    /// Current list of all output device names.
    pub current_output: Vec<String>,
}

/* ──────────────────────────────────────────────────────────────── */

fn create_audio_recorder(
    backend: VadBackend,
    app_handle: &tauri::AppHandle,
    vad_threshold: f32,
    vad_hangover_frames: usize,
    last_voice_activity_ms: Arc<AtomicU64>,
    close_generation: Arc<AtomicU64>,
    selected_channel: Option<u16>,
) -> Result<AudioRecorder, anyhow::Error> {
    let detector: Box<dyn VoiceActivityDetector> = match backend {
        VadBackend::Silero => {
            let vad_path = app_handle
                .path()
                .resolve(
                    "resources/models/silero_vad_v4.onnx",
                    tauri::path::BaseDirectory::Resource,
                )
                .map_err(|e| anyhow::anyhow!("Failed to resolve VAD path: {e}"))?;
            Box::new(
                SileroVad::new(
                    vad_path.to_str().ok_or_else(|| {
                        anyhow::anyhow!("VAD path is not valid UTF-8: {vad_path:?}")
                    })?,
                    vad_threshold,
                )
                .map_err(|e| anyhow::anyhow!("Failed to create SileroVad: {e}"))?,
            )
        }
        VadBackend::Earshot => Box::new(
            EarshotVad::new(EARSHOT_VAD_THRESHOLD)
                .map_err(|e| anyhow::anyhow!("Failed to create EarshotVad: {e}"))?,
        ),
    };

    // Earshot uses 16 ms frames while Silero uses 30 ms. Convert the existing
    // time-based capture profile to each detector's frame size so selecting a
    // backend does not shorten pre-roll, onset, or post-speech audio.
    let frame_samples = detector.frame_samples();
    let prefill_frames = frames_for_duration_ms(VAD_PREFILL_MS, frame_samples);
    // Preserve the fork's user-facing VAD sensitivity setting, which is
    // expressed in Silero 30ms frame units (15-35). Convert to milliseconds
    // first so the hangover scales correctly for either backend (e.g. 35 frames
    // -> 1050ms -> 35 Silero frames or 66 Earshot frames).
    let hangover_ms = vad_hangover_frames as u64 * 30;
    let offline_hangover_frames = frames_for_duration_ms(hangover_ms, frame_samples);
    let onset_frames = frames_for_duration_ms(VAD_ONSET_MS, frame_samples);
    let smoothed_vad = SmoothedVad::new(
        detector,
        prefill_frames,
        offline_hangover_frames,
        onset_frames,
    );

    info!(
        "Initialized {:?} VAD backend ({} samples/frame)",
        backend, frame_samples
    );

    // Check if live captions is enabled
    let settings = get_settings(app_handle);
    let live_captions_enabled = settings.live_captions_enabled;
    let live_captions_model = settings.live_captions_model.clone();
    let pre_buffer_ms = settings.pre_recording_buffer_ms;
    let noise_suppression_enabled = settings.noise_suppression_enabled;
    let noise_suppression_level = settings.noise_suppression_level;

    // Decide which transcription manager and cancel flag to wire to the
    // recorder's streaming callback. When the user has configured a dedicated
    // live captions model, the stream router and 2.5s batch fallback callback
    // must both target the live captions TM and use its separate cancel flag.
    // When no separate model is configured, live captions fall back to the
    // main TM's 2.5s batch callback and must use the main cancel flag.
    let use_live_captions_model = !live_captions_model.is_empty();

    // Attach a real-time stream router only when a dedicated live captions
    // model is configured. The router is what feeds real-time frames to the
    // live captions model; without it, the recorder falls back to the legacy
    // 2.5s batch callback path.
    let stream_router: Option<Arc<crate::managers::transcription::StreamRouter>> =
        if use_live_captions_model {
            app_handle
                .try_state::<LiveCaptionsTranscriptionManager>()
                .map(|tm| tm.0.lock().stream_router())
        } else {
            None
        };
    let stream_router_attached = stream_router.is_some();

    // Recorder with VAD plus a spectrum-level callback that forwards updates to
    // the frontend, and optionally a streaming transcription callback for live captions.
    let mut recorder = AudioRecorder::new()
        .map_err(|e| anyhow::anyhow!("Failed to create AudioRecorder: {}", e))?
        .with_vad(Box::new(smoothed_vad))
        .with_voice_activity_stamp(last_voice_activity_ms)
        .with_close_generation(close_generation)
        .with_selected_channel(selected_channel)
        .with_level_callback({
            let app_handle = app_handle.clone();
            move |levels| {
                utils::emit_levels(&app_handle, &levels);
            }
        })
        .with_stream_router(stream_router);

    // Add noise suppression before VAD if enabled.
    // This improves speech detection accuracy in noisy environments
    // by removing background noise before VAD processes the audio.
    if noise_suppression_enabled {
        info!(
            "Noise suppression enabled (level: {:?})",
            noise_suppression_level
        );
        recorder = recorder.with_noise_suppressor(noise_suppression_level);
    }

    // Always create the streaming callback so it can be enabled/disabled
    // at runtime via the streaming_enabled flag (Strategy pattern). This
    // eliminates the need to recreate the entire AudioRecorder when toggling
    // live captions, avoiding stream teardown/restart that causes Bug 1.
    //
    // We wire the callback to the correct TM/cancel-flag pair based on
    // whether a dedicated live captions model is configured.
    let (cancel_flag, tm_arc): (
        Option<crate::streaming_cancel_flag::StreamingCancelFlag>,
        Option<Arc<Mutex<crate::managers::transcription::TranscriptionManager>>>,
    ) = if use_live_captions_model {
        let cancel_flag = app_handle
            .try_state::<crate::LiveCaptionsStreamingCancelFlag>()
            .map(|state| state.0.inner().clone());
        let tm = app_handle
            .try_state::<LiveCaptionsTranscriptionManager>()
            .map(|state| Arc::clone(&state.0));
        (cancel_flag, tm)
    } else {
        let cancel_flag = app_handle
            .try_state::<StreamingCancelFlag>()
            .map(|state| state.inner().clone());
        let tm = app_handle
            .try_state::<Arc<Mutex<crate::managers::transcription::TranscriptionManager>>>()
            .map(|state| state.inner().clone());
        (cancel_flag, tm)
    };

    let cancel_flag_available = cancel_flag.is_some();
    let tm_available = tm_arc.is_some();

    info!(
        "[Live Captions] Recorder config: use_live_captions_model={}, stream_router_attached={}, cancel_flag_available={}, tm_available={}",
        use_live_captions_model,
        stream_router_attached,
        cancel_flag_available,
        tm_available
    );

    let recorder = match (cancel_flag, tm_arc) {
        (Some(cancel_flag), Some(tm)) => {
            info!(
                "[Live Captions] Setting up streaming callback (enabled={}, live_captions_model={})",
                live_captions_enabled,
                use_live_captions_model
            );
            // Capture the recorder/stream generation at callback creation
            // time. Already-spawned tasks check it at entry and right before
            // emit so they return early when the stream has been closed.
            let close_generation = recorder.close_generation.clone();
            let callback_generation = close_generation.load(Ordering::SeqCst);
            recorder
                .with_streaming_cancel_flag(cancel_flag.clone())
                .with_streaming_callback({
                    let app_handle = app_handle.clone();
                    let cancel_flag = cancel_flag.clone();
                    let close_generation = close_generation.clone();
                    move |samples| {
                        let flag = cancel_flag.load_flag();
                        let seq = cancel_flag.load_seq();
                        debug!("[STREAM-CB] entry cancel_flag={} seq={}", flag, seq);

                        // Check cancellation WITHOUT lock (atomic load).
                        // This avoids acquiring the TranscriptionManager mutex on
                        // every streaming callback, which would cause lock contention
                        // with the main transcription path.
                        if flag {
                            debug!(
                                "[STREAM-CB] Skipping streaming transcription - cancellation requested seq={}",
                                seq
                            );
                            return;
                        }

                        // This callback runs in the audio thread, so we need to spawn
                        // a blocking task to avoid blocking audio capture.
                        let app_handle = app_handle.clone();
                        let cancel_flag = cancel_flag.clone();
                        let tm = Arc::clone(&tm);
                        let captured_generation = callback_generation;
                        let close_generation = close_generation.clone();
                        tauri::async_runtime::spawn_blocking(move || {
                            // Re-check cancellation after acquiring blocking thread
                            if cancel_flag.load_flag() {
                                debug!(
                                    "[STREAM-CB] Skipping streaming transcription (blocking) - cancellation requested seq={}",
                                    cancel_flag.load_seq()
                                );
                                return;
                            }

                            // Guard against late execution after the recorder/stream
                            // has been closed or recreated.
                            let current_generation = close_generation.load(Ordering::SeqCst);
                            if current_generation != captured_generation {
                                debug!(
                                    "[STREAM-CB] Generation mismatch ({} != {}), skipping transcription from stale task",
                                    current_generation, captured_generation
                                );
                                return;
                            }

                            // Transcribe the audio samples. Use a bounded acquisition lock so
                            // a wedged prior streaming callback cannot block this frame forever.
                            let lock_start = Instant::now();
                            let tm_guard = tm.try_lock_for(Duration::from_secs(2));
                            match tm_guard {
                                None => {
                                    warn!(
                                        "[STREAM-LOCK] timed out acquiring TM lock after 2s, skipping frame"
                                    );
                                    return;
                                }
                                Some(guard) => {
                                    let lock_dt = lock_start.elapsed();
                                    if lock_dt > Duration::from_millis(250) {
                                        warn!("[STREAM-LOCK] tm.lock() acquired in {:?} (transcribe pending)", lock_dt);
                                    } else {
                                        debug!("[STREAM-LOCK] tm.lock() acquired in {:?} (transcribe pending)", lock_dt);
                                    }
                                    // Clone any state needed after the guard is dropped.
                                    let app_handle = app_handle.clone();
                                    let tm = Arc::clone(&tm);
                                    let cancel_flag = cancel_flag.clone();
                                    let close_generation = close_generation.clone();
                                    let transcription_result = guard.transcribe(samples);
                                    drop(guard);

                                    match transcription_result {
                                        Ok(result) if !result.text.is_empty() => {
                                            info!(
                                                "[Live Captions] Streaming transcription succeeded: text_len={}",
                                                result.text.len()
                                            );
                                            // Check again after transcription in case it was cancelled mid-work
                                            // Using the Arc<AtomicBool> to avoid lock contention
                                            if cancel_flag.load_flag() {
                                                debug!(
                                                    "[STREAM-CB] Discarding streaming transcription result - cancelled seq={}",
                                                    cancel_flag.load_seq()
                                                );
                                                return;
                                            }

                                            // Re-check generation right before emitting the event,
                                            // in case the stream was closed while transcription ran.
                                            let current_generation = close_generation.load(Ordering::SeqCst);
                                            if current_generation != captured_generation {
                                                debug!(
                                                    "[STREAM-CB] Generation mismatch ({} != {}) before emit, discarding stale transcription result",
                                                    current_generation, captured_generation
                                                );
                                                return;
                                            }

                                            // Emit partial transcription event with text for frontend display
                                            info!(
                                                "[Live Captions] Emitting partial-transcription event: text_len={}",
                                                result.text.len()
                                            );
                                            if let Err(e) = app_handle.emit("partial-transcription", &result) {
                                                warn!("Failed to emit partial-transcription event: {}", e);
                                            }
                                        }
                                        Ok(_) => {
                                            // Empty transcription - skip
                                            debug!("[Live Captions] Streaming transcription returned empty text");
                                        }
                                        Err(e) => {
                                            if matches!(e, AppError::ModelNotLoaded) {
                                                info!("[Live Captions] Model not loaded — initiating model load for next streaming cycle");
                                                // Re-acquire briefly for the model load re-kick; a bounded lock prevents
                                                // a wedged prior streaming callback from blocking this frame forever.
                                                let load_lock_start = Instant::now();
                                                if let Some(load_guard) = tm.try_lock_for(Duration::from_secs(2)) {
                                                    load_guard.initiate_model_load();
                                                } else {
                                                    warn!("[STREAM-LOCK] timed out acquiring TM lock for model-load re-kick after 2s, skipping");
                                                }
                                                let load_lock_dt = load_lock_start.elapsed();
                                                if load_lock_dt > Duration::from_millis(250) {
                                                    warn!("[STREAM-LOCK] model-load re-kick took {:?}", load_lock_dt);
                                                } else {
                                                    debug!("[STREAM-LOCK] model-load re-kick took {:?}", load_lock_dt);
                                                }
                                            } else if matches!(e, AppError::TranscriptionBusy) {
                                                debug!("[Live Captions] Streaming transcription skipped (busy): {}", e);
                                            } else {
                                                warn!("[Live Captions] Streaming transcription failed: {}", e);
                                            }
                                        }
                                    }
                                }
                            }
                        });
                    }
                })
                .with_streaming_enabled(live_captions_enabled)
        }
        _ => {
            warn!(
                "[Live Captions] Streaming callback not attached: cancel_flag_available={} tm_available={} — live captions will work after next recorder recreation",
                cancel_flag_available,
                tm_available
            );
            recorder
        }
    };

    // Add pre-buffer for always-on mode (captures audio before hotkey press)
    let recorder = if pre_buffer_ms > 0 {
        recorder.with_pre_buffer_ms(pre_buffer_ms)
    } else {
        recorder
    };

    Ok(recorder)
}

/* ──────────────────────────────────────────────────────────────── */

#[derive(Clone)]
pub struct AudioRecordingManager {
    state: Arc<Mutex<RecordingState>>,
    mode: Arc<Mutex<MicrophoneMode>>,
    app_handle: tauri::AppHandle,

    recorder: Arc<Mutex<Option<AudioRecorder>>>,
    is_open: Arc<AtomicBool>,
    is_recording: Arc<AtomicBool>,
    did_mute: Arc<AtomicBool>,
    prev_muted: Arc<Mutex<Option<bool>>>,
    close_generation: Arc<AtomicU64>,
    pub usb_watchdog: Arc<UsbWatchdog>,
    /// When a Bluetooth output device is detected, we keep the mic stream
    /// alive permanently (like always-on mode) regardless of the user's
    /// microphone mode setting. This prevents macOS from repeatedly
    /// switching the BT headset between A2DP (stereo) and HFP/SCO (mono)
    /// profiles every time the stream opens/closes, which causes audio
    /// dropouts on the headphones.
    bt_keep_alive: Arc<AtomicBool>,
    /// Queue of pronunciation recordings awaiting idle-time processing.
    /// Each entry is `(audio_samples, canonical_word, recording_id, file_path)`.
    /// New recordings are pushed to the back; the worker pops from the front.
    pub pending_pronunciation: Arc<Mutex<VecDeque<(Vec<f32>, String, String, String)>>>,
    /// Handle to the background processing thread, if any.
    pub pronunciation_thread: Arc<Mutex<Option<std::thread::JoinHandle<()>>>>,
    /// Directory where pronunciation WAV files are saved.
    #[allow(dead_code)]
    pub pronunciation_recordings_dir: PathBuf,
    /// Background liveness monitor thread handle
    liveness_monitor: Arc<Mutex<Option<std::thread::JoinHandle<()>>>>,
    /// Flag to signal liveness monitor to stop
    liveness_stop: Arc<AtomicBool>,
    /// Resolution of a *named* microphone (selected or clamshell) to its cpal
    /// device, cached so on-demand recording starts skip the full device
    /// enumeration (~40-110ms). Keyed by the resolved name, so a settings
    /// change misses naturally; cleared when an open fails (device unplugged)
    /// so the retry re-enumerates. The system-default case is never cached.
    cached_device: Arc<Mutex<Option<(String, cpal::Device)>>>,

    /// Background device hot-plug monitor thread handle
    device_monitor: Arc<Mutex<Option<std::thread::JoinHandle<()>>>>,
    /// Sender to signal the device monitor to stop
    device_monitor_stop: Arc<Mutex<Option<std::sync::mpsc::Sender<()>>>>,
    /// Previous input device names (for change detection)
    prev_input_devices: Arc<Mutex<Vec<String>>>,
    /// Previous output device names (for change detection)
    prev_output_devices: Arc<Mutex<Vec<String>>>,
    /// Shared voice-activity timestamp (elapsed ms since recording start).
    /// The recorder stamps this when VAD detects speech; the coordinator reads
    /// it to reset the 5-minute silence timeout.
    last_voice_activity_ms: Arc<AtomicU64>,
}

// Helper types for microphone resolution live in the parent module so that
// methods below can reference them without scoping issues inside an impl block.
enum DesiredMicrophone {
    Default,
    Selected(String),
    Clamshell(String),
}

struct MicrophoneResolution {
    device: Option<cpal::Device>,
    unavailable_selected_microphone: Option<String>,
}

impl AudioRecordingManager {
    /* ---------- construction ------------------------------------------------ */

    pub fn new(app: &tauri::AppHandle) -> Result<Self, anyhow::Error> {
        let settings = get_settings(app);
        let mode = if settings.always_on_microphone {
            MicrophoneMode::AlwaysOn
        } else {
            MicrophoneMode::OnDemand
        };

        let usb_watchdog = Arc::new(UsbWatchdog::new(
            settings.usb_watchdog_enabled,
            &settings.usb_watchdog_device_name,
            Some(app.clone()),
        ));

        // Set up pronunciation recordings directory
        let app_data_dir = portable::app_data_dir(app)?;
        let pronunciation_recordings_dir = app_data_dir.join("pronunciation_recordings");
        std::fs::create_dir_all(&pronunciation_recordings_dir).ok();
        info!(
            "Pronunciation recordings directory: {:?}",
            pronunciation_recordings_dir
        );

        let manager = Self {
            state: Arc::new(Mutex::new(RecordingState::Idle)),
            mode: Arc::new(Mutex::new(mode.clone())),
            app_handle: app.clone(),

            recorder: Arc::new(Mutex::new(None)),
            is_open: Arc::new(AtomicBool::new(false)),
            is_recording: Arc::new(AtomicBool::new(false)),
            did_mute: Arc::new(AtomicBool::new(false)),
            prev_muted: Arc::new(Mutex::new(None)),
            close_generation: Arc::new(AtomicU64::new(0)),
            usb_watchdog: usb_watchdog.clone(),
            bt_keep_alive: Arc::new(AtomicBool::new(false)),
            pending_pronunciation: Arc::new(Mutex::new(VecDeque::new())),
            pronunciation_thread: Arc::new(Mutex::new(None)),
            pronunciation_recordings_dir,
            liveness_monitor: Arc::new(Mutex::new(None)),
            liveness_stop: Arc::new(AtomicBool::new(false)),
            device_monitor: Arc::new(Mutex::new(None)),
            device_monitor_stop: Arc::new(Mutex::new(None)),
            prev_input_devices: Arc::new(Mutex::new(Vec::new())),
            prev_output_devices: Arc::new(Mutex::new(Vec::new())),
            last_voice_activity_ms: Arc::new(AtomicU64::new(0)),
            cached_device: Arc::new(Mutex::new(None)),
        };

        // Check for Bluetooth output devices — if detected, keep the mic
        // stream alive permanently to prevent A2DP↔HFP profile switching
        // that causes audio dropouts on Bluetooth headphones.
        let bt_active = manager.is_bluetooth_output_active();
        if bt_active {
            info!("Bluetooth output device detected at startup — enabling mic stream keep-alive to prevent audio dropouts");
            manager.bt_keep_alive.store(true, Ordering::Release);
        }

        // Start background liveness monitor to detect zombie streams after sleep/wake
        manager.start_liveness_monitor();

        // Start background device hot-plug monitor
        manager.start_device_monitor();

        // Defer opening the always-on/BT mic stream off the main thread.
        // Synchronously opening cpal/CoreAudio on the main thread during app
        // startup has been observed to cause heap corruption (SIGABRT) when
        // other Tauri initialization is happening concurrently. The stream is
        // not required to be ready before we return the manager; the liveness
        // monitor will self-heal if it isn't open yet.
        let should_open_stream = bt_active || matches!(mode, MicrophoneMode::AlwaysOn);
        if should_open_stream {
            let manager_clone = manager.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(100));
                if let Err(e) = manager_clone.start_microphone_stream() {
                    error!("Deferred always-on/BT microphone stream open failed: {}", e);
                } else {
                    info!("Deferred always-on/BT microphone stream opened successfully");
                }
            });
        }

        Ok(manager)
    }

    /* ---------- liveness monitoring ---------------------------------------- */

    /// Start a background thread that periodically checks if the always-on
    /// microphone stream is alive and restarts it if it becomes a zombie
    /// (open but not producing audio, e.g. after macOS sleep/wake).
    fn start_liveness_monitor(&self) {
        let is_open = self.is_open.clone();
        let recorder = self.recorder.clone();
        let mode = self.mode.clone();
        let app_handle = self.app_handle.clone();
        let stop_flag = self.liveness_stop.clone();
        let usb_watchdog = self.usb_watchdog.clone();
        let bt_keep_alive = self.bt_keep_alive.clone();

        let handle = std::thread::spawn(move || {
            // Cooldown before the first check so the deferred startup stream
            // open has time to complete before the liveness monitor decides the
            // stream is dead and tries to self-heal it.
            info!("Liveness monitor: sleeping 2 seconds before first check to allow deferred stream startup");
            std::thread::sleep(Duration::from_secs(2));

            loop {
                // Check every 3 seconds
                std::thread::sleep(Duration::from_secs(3));

                if stop_flag.load(Ordering::Relaxed) {
                    debug!("Liveness monitor stopping");
                    break;
                }

                // Only monitor when always-on or BT keep-alive is active
                let is_always_on = {
                    let guard = mode.lock();
                    matches!(*guard, MicrophoneMode::AlwaysOn)
                };
                let bt_keep_alive_flag = bt_keep_alive.load(Ordering::Acquire);

                if !is_always_on && !bt_keep_alive_flag {
                    continue;
                }

                // Invariant watchdog: check that the stream is open when it should be.
                // This self-heals the "always-on stream died and wasn't restarted" bug
                // (Bug 1) — if the stream SHOULD be open but isn't, restart it.
                let stream_open = is_open.load(Ordering::Acquire);

                if !stream_open {
                    // Invariant violation: stream should be open but isn't.
                    // Self-heal by restarting it.
                    warn!(
                        "Liveness monitor: invariant violation — always-on stream is not open. Self-healing by restarting"
                    );

                    if let Some(rm) = app_handle.try_state::<Arc<AudioRecordingManager>>() {
                        if let Err(e) = rm.start_microphone_stream() {
                            error!("Liveness monitor failed to self-heal dead stream: {}", e);
                        } else {
                            info!("Liveness monitor: self-healed always-on stream (was not open)");
                        }
                    }
                    continue;
                }

                // Check if stream is alive (has received audio recently)
                let stream_alive = {
                    let recorder_guard = recorder.lock();
                    recorder_guard.as_ref().map_or(false, |r| {
                        r.is_stream_alive(Self::STREAM_LIVENESS_TIMEOUT_MS)
                    })
                };

                if !stream_alive {
                    warn!(
                        "Liveness monitor: always-on stream appears dead (no audio for {}ms) — restarting",
                        Self::STREAM_LIVENESS_TIMEOUT_MS
                    );

                    // Notify USB watchdog about the dead stream
                    usb_watchdog.on_stream_alive_check(false);

                    // Restart the stream via the app handle
                    if let Some(rm) = app_handle.try_state::<Arc<AudioRecordingManager>>() {
                        // Check if user was actively recording before showing overlay
                        let was_recording = rm.is_recording();

                        // Stop the current stream
                        if rm.is_open.load(Ordering::Acquire) {
                            rm.stop_microphone_stream();
                        }

                        // Only show USB-cycling overlay if user was actively recording.
                        // If the stream died during sleep/wake while not recording,
                        // silently recover in the background without showing the visualizer.
                        if was_recording {
                            utils::show_usb_cycling_overlay(&app_handle);
                            crate::tray::change_tray_icon(
                                &app_handle,
                                crate::tray::TrayIconState::Recording,
                            );

                            // Emit stage event so the overlay shows progress dots and elapsed time
                            usb_watchdog::emit_stage_event_with_handle(
                                &Some(app_handle.clone()),
                                "recovering",
                                "Recovering microphone stream...",
                            );
                        }

                        // Try to restart
                        if let Err(e) = rm.start_microphone_stream() {
                            error!("Liveness monitor failed to restart stream: {}", e);

                            if was_recording {
                                // Emit failed event so frontend can recover from stuck state
                                usb_watchdog::emit_cycle_event_with_handle(
                                    &Some(app_handle.clone()),
                                    "usb-power-cycle-failed",
                                    &format!("Stream restart failed: {}", e),
                                );
                                utils::hide_recording_overlay(&app_handle);
                            }

                            crate::tray::change_tray_icon(
                                &app_handle,
                                crate::tray::TrayIconState::Idle,
                            );
                        } else {
                            crate::tray::change_tray_icon(
                                &app_handle,
                                crate::tray::TrayIconState::Idle,
                            );
                            info!("Liveness monitor: stream restarted successfully");

                            if was_recording {
                                // Emit finished event so frontend clears the USB cycling state
                                usb_watchdog::emit_cycle_event_with_handle(
                                    &Some(app_handle.clone()),
                                    "usb-power-cycle-finished",
                                    "Stream recovered successfully",
                                );
                            }
                        }
                    }
                }
            }
        });

        *self.liveness_monitor.lock() = Some(handle);
        debug!("Liveness monitor started");
    }

    /// Stop the background liveness monitor thread
    #[allow(dead_code)]
    fn stop_liveness_monitor(&self) {
        self.liveness_stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.liveness_monitor.lock().take() {
            let _ = handle.join();
        }
    }

    /* ---------- device hot-plug monitoring --------------------------------- */

    /// Start a background thread that periodically polls for audio device
    /// changes and emits `device-list-changed` events when devices are
    /// added or removed. This enables the frontend to update its device
    /// selectors without requiring a manual refresh.
    fn start_device_monitor(&self) {
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        *self.device_monitor_stop.lock() = Some(tx);

        let app_handle = self.app_handle.clone();
        let prev_input = self.prev_input_devices.clone();
        let prev_output = self.prev_output_devices.clone();

        // Seed with the current device list so we don't emit a spurious
        // "everything was added" event on the first poll.
        let initial_input = list_input_devices()
            .map(|devs| devs.iter().map(|d| d.name.clone()).collect::<Vec<_>>())
            .unwrap_or_default();
        let initial_output = list_output_devices()
            .map(|devs| devs.iter().map(|d| d.name.clone()).collect::<Vec<_>>())
            .unwrap_or_default();

        {
            let mut guard = prev_input.lock();
            *guard = initial_input;
        }
        {
            let mut guard = prev_output.lock();
            *guard = initial_output;
        }

        let handle = std::thread::Builder::new()
            .name("device-monitor".into())
            .spawn(move || {
                loop {
                    // Wait 2 seconds, or exit early if we receive a shutdown signal
                    match rx.recv_timeout(Duration::from_secs(2)) {
                        Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                            debug!("Device monitor stopping");
                            break;
                        }
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                            // Timeout means 2 seconds elapsed — time to poll
                        }
                    }

                    // Enumerate current devices
                    let current_input: Vec<String> = match list_input_devices() {
                        Ok(devs) => devs.iter().map(|d| d.name.clone()).collect(),
                        Err(e) => {
                            debug!("Device monitor: failed to list input devices: {}", e);
                            continue;
                        }
                    };

                    let current_output: Vec<String> = match list_output_devices() {
                        Ok(devs) => devs.iter().map(|d| d.name.clone()).collect(),
                        Err(e) => {
                            debug!("Device monitor: failed to list output devices: {}", e);
                            continue;
                        }
                    };

                    let prev_input_guard = prev_input.lock();
                    let prev_output_guard = prev_output.lock();

                    let prev_input_set: std::collections::HashSet<_> =
                        prev_input_guard.iter().cloned().collect();
                    let prev_output_set: std::collections::HashSet<_> =
                        prev_output_guard.iter().cloned().collect();
                    let current_input_set: std::collections::HashSet<_> =
                        current_input.iter().cloned().collect();
                    let current_output_set: std::collections::HashSet<_> =
                        current_output.iter().cloned().collect();

                    let added_input: Vec<String> = current_input_set
                        .difference(&prev_input_set)
                        .cloned()
                        .collect();
                    let removed_input: Vec<String> = prev_input_set
                        .difference(&current_input_set)
                        .cloned()
                        .collect();
                    let added_output: Vec<String> = current_output_set
                        .difference(&prev_output_set)
                        .cloned()
                        .collect();
                    let removed_output: Vec<String> = prev_output_set
                        .difference(&current_output_set)
                        .cloned()
                        .collect();

                    drop(prev_input_guard);
                    drop(prev_output_guard);

                    // Only emit when something actually changed
                    if !added_input.is_empty()
                        || !removed_input.is_empty()
                        || !added_output.is_empty()
                        || !removed_output.is_empty()
                    {
                        info!(
                            "Device change detected: +{} -{} input, +{} -{} output",
                            added_input.len(),
                            removed_input.len(),
                            added_output.len(),
                            removed_output.len(),
                        );

                        // Update stored device lists
                        {
                            let mut guard = prev_input.lock();
                            *guard = current_input.clone();
                        }
                        {
                            let mut guard = prev_output.lock();
                            *guard = current_output.clone();
                        }

                        let payload = DeviceListChanged {
                            added_input,
                            removed_input,
                            current_input,
                            added_output,
                            removed_output,
                            current_output,
                        };

                        if let Err(e) = app_handle.emit("device-list-changed", &payload) {
                            warn!("Failed to emit device-list-changed event: {}", e);
                        }
                    }
                }
            })
            .expect("Failed to spawn device monitor thread");

        *self.device_monitor.lock() = Some(handle);
        debug!("Device hot-plug monitor started");
    }

    /// Stop the background device monitor thread.
    fn stop_device_monitor(&self) {
        if let Some(tx) = self.device_monitor_stop.lock().take() {
            let _ = tx.send(());
        }
        if let Some(handle) = self.device_monitor.lock().take() {
            let _ = handle.join();
        }
        debug!("Device hot-plug monitor stopped");
    }

    /* ---------- helper methods --------------------------------------------- */

    /// The microphone name the settings ask for, or `None` for the system
    /// default. Only runs the clamshell probe (an `ioreg` subprocess, ~10-20ms)
    /// when a clamshell microphone is actually configured.
    fn desired_microphone(&self, settings: &AppSettings) -> DesiredMicrophone {
        if let Some(clamshell_microphone) = &settings.clamshell_microphone {
            let clamshell_started = Instant::now();
            let is_clamshell = clamshell::is_clamshell().unwrap_or(false);
            debug!(
                "device resolve: clamshell_check={:?} (clamshell={})",
                clamshell_started.elapsed(),
                is_clamshell
            );
            if is_clamshell {
                return DesiredMicrophone::Clamshell(clamshell_microphone.clone());
            }
        }
        match &settings.selected_microphone {
            Some(name) => DesiredMicrophone::Selected(name.clone()),
            None => DesiredMicrophone::Default,
        }
    }

    pub fn invalidate_device_cache(&self) {
        *self.cached_device.lock() = None;
    }

    fn resolve_microphone_device(&self, settings: &AppSettings) -> MicrophoneResolution {
        let desired = self.desired_microphone(settings);
        let (device_name, selected_microphone) = match desired {
            DesiredMicrophone::Default => {
                debug!("device resolve: no mic configured -> system default");
                return MicrophoneResolution {
                    device: None,
                    unavailable_selected_microphone: None,
                };
            }
            DesiredMicrophone::Selected(name) => (name.clone(), Some(name)),
            DesiredMicrophone::Clamshell(name) => (name, None),
        };

        // Cache hit: skip the full enumeration. A stale device (unplugged)
        // fails at open, where the caller invalidates and retries fresh.
        if let Some((cached_name, device)) = self.cached_device.lock().as_ref() {
            if *cached_name == device_name {
                debug!("device resolve: cache hit for '{}'", device_name);
                return MicrophoneResolution {
                    device: Some(device.clone()),
                    unavailable_selected_microphone: None,
                };
            }
        }

        // Only report a selected microphone as unavailable when enumeration
        // itself succeeded. A backend enumeration error may be transient and
        // must not erase the user's persisted preference.
        let enumerate_started = Instant::now();
        let (device, enumeration_succeeded) = match list_input_devices() {
            Ok(devices) => (
                devices
                    .into_iter()
                    .find(|d| d.name == device_name)
                    .map(|d| d.device),
                true,
            ),
            Err(e) => {
                debug!("Failed to list devices, using default: {}", e);
                (None, false)
            }
        };
        debug!(
            "device resolve: enumerate={:?} (found={})",
            enumerate_started.elapsed(),
            device.is_some()
        );

        if let Some(d) = &device {
            *self.cached_device.lock() = Some((device_name, d.clone()));
        }

        let unavailable_selected_microphone = if enumeration_succeeded && device.is_none() {
            selected_microphone
        } else {
            None
        };
        MicrophoneResolution {
            device,
            unavailable_selected_microphone,
        }
    }

    /// Keep persisted settings and the UI aligned with a successful runtime
    /// fallback. Re-read first so recovery cannot clear a microphone the user
    /// selected concurrently while the stream was being rebuilt.
    fn persist_default_microphone_after_fallback(&self, unavailable_name: &str) {
        let mut settings = get_settings(&self.app_handle);
        if settings.selected_microphone.as_deref() != Some(unavailable_name) {
            return;
        }

        settings.selected_microphone = None;
        write_settings(&self.app_handle, settings);
        let _ = self.app_handle.emit(
            "settings-changed",
            serde_json::json!({
                "setting": "selected_microphone",
                "value": "Default"
            }),
        );
    }

    fn get_effective_microphone_device(&self, settings: &AppSettings) -> Option<cpal::Device> {
        self.resolve_microphone_device(settings).device
    }

    /// Returns true if a Bluetooth output device is currently active (either the
    /// system default or the user's selected output device). When Bluetooth audio
    /// is active, we keep the microphone stream alive between recordings to
    /// prevent macOS from repeatedly switching the Bluetooth headset between
    /// A2DP (stereo) and HFP/SCO (mono with mic) profiles, which causes audio
    /// dropouts on the headphones.
    fn is_bluetooth_output_active(&self) -> bool {
        let settings = get_settings(&self.app_handle);
        let selected = settings.selected_output_device.as_deref();
        is_bluetooth_audio_active(selected)
    }

    fn schedule_lazy_close(&self) {
        let gen = self.close_generation.fetch_add(1, Ordering::SeqCst) + 1;
        let app = self.app_handle.clone();
        let bt_keep_alive = self.bt_keep_alive.clone();
        std::thread::spawn(move || {
            std::thread::sleep(STREAM_IDLE_TIMEOUT);
            let Some(rm) = app.try_state::<Arc<AudioRecordingManager>>() else {
                debug!("AudioRecordingManager not available for lazy close");
                return;
            };
            // Hold state lock only for the check so the close does not race
            // with a concurrent try_start_recording. We must drop the lock
            // before calling stop_microphone_stream() because that method takes
            // the state lock itself in its Phase 2; parking_lot is non-reentrant,
            // so holding it here would deadlock.
            let should_close = {
                let state = rm.state.lock();
                // Never close the stream if BT keep-alive is active
                if bt_keep_alive.load(Ordering::Acquire) {
                    debug!("Skipping lazy close: BT keep-alive is active");
                    false
                } else {
                    rm.close_generation.load(Ordering::SeqCst) == gen
                        && matches!(*state, RecordingState::Idle)
                }
            };
            if should_close {
                // Re-check generation without the lock: a new start bumps it and
                // must win the race against this close.
                if rm.close_generation.load(Ordering::SeqCst) == gen {
                    info!(
                        "Closing idle microphone stream after {:?}",
                        STREAM_IDLE_TIMEOUT
                    );
                    rm.stop_microphone_stream();
                } else {
                    debug!("Skipping lazy close: generation changed after state check");
                }
            }
        });
    }

    /* ---------- microphone life-cycle -------------------------------------- */

    /// Applies mute if mute_while_recording is enabled and stream is open.
    /// Snapshots the system's prior mute state first so `remove_mute` can
    /// restore it instead of unconditionally unmuting.
    pub fn apply_mute(&self) {
        let settings = get_settings(&self.app_handle);
        if !settings.mute_while_recording {
            return;
        }

        // Already muted this session — don't re-snapshot, or a duplicate/late
        // apply would overwrite prev_muted with our own forced-muted state and
        // strand audio muted on stop.
        if self.did_mute.load(Ordering::Acquire) {
            return;
        }

        if self.is_open.load(Ordering::Acquire) {
            let prev = get_mute();
            *self.prev_muted.lock() = prev;
            set_mute(true);
            self.did_mute.store(true, Ordering::Release);
            debug!("Mute applied (prev_muted={:?})", prev);
        }
    }

    /// Removes mute if it was applied, restoring the system's prior mute state
    /// (a system already muted before recording stays muted).
    pub fn remove_mute(&self) {
        if self.did_mute.swap(false, Ordering::AcqRel) {
            let prev = *self.prev_muted.lock();
            restore_mute(prev);
            *self.prev_muted.lock() = None;
            debug!("Mute removed (restored prev_muted={:?})", prev);
        }
    }

    pub fn preload_vad(&self) -> Result<(), anyhow::Error> {
        let mut recorder_opt = self.recorder.lock();
        if recorder_opt.is_none() {
            let settings = get_settings(&self.app_handle);
            let vad_threshold = settings.vad_sensitivity.threshold();
            let vad_hangover_frames = settings.vad_sensitivity.hangover_frames();

            let selected_channel = settings.selected_channel;
            *recorder_opt = Some(create_audio_recorder(
                settings.vad_backend,
                &self.app_handle,
                vad_threshold,
                vad_hangover_frames,
                Arc::clone(&self.last_voice_activity_ms),
                Arc::clone(&self.close_generation),
                selected_channel,
            )?);
        }
        Ok(())
    }

    pub fn start_microphone_stream(&self) -> Result<(), anyhow::Error> {
        // `is_open` only records that we opened a stream at some point, not
        // that one is still running. If capture has since failed (mic unplugged
        // mid-session, USB dropout), rebuild it before the next recording
        // instead of handing the caller a stalled recorder.
        if self.is_open.load(Ordering::Acquire) {
            let needs_reopen = self
                .recorder
                .lock()
                .as_ref()
                .is_some_and(|rec| rec.needs_reopen());

            if !needs_reopen {
                trace!("Microphone stream already active");
                return Ok(());
            }

            warn!("Microphone stream is no longer running (device disconnected?); reopening");

            // Torn down inline rather than via stop_microphone_stream(), which
            // takes the recorder + state locks we don't need here.
            if self.did_mute.swap(false, Ordering::AcqRel) {
                let prev = *self.prev_muted.lock();
                restore_mute(prev);
                *self.prev_muted.lock() = None;
            }
            if let Some(rec) = self.recorder.lock().as_mut() {
                let _ = rec.close();
            }
            self.is_recording.store(false, Ordering::Release);
            self.is_open.store(false, Ordering::Release);
            // Fall through and open a fresh stream.
        }

        // Try the normal open first. If it fails and USB watchdog is enabled,
        // attempt a power cycle + retry.
        //
        // Note: on_mic_open_failed() blocks until the power cycle and settle
        // period complete, so the retry below runs with the device
        // re-enumerated and ready.
        match self.start_microphone_stream_inner() {
            Ok(()) => {
                // Note: We don't call on_mic_open_succeeded() here because
                // the stream might still be "dead" (zombie device).
                // We reset the failure counter only when we actually
                // receive audio samples.
                Ok(())
            }
            Err(e) => {
                if self.usb_watchdog.on_mic_open_failed() {
                    // Watchdog completed a power cycle (blocking). Retry the mic open now
                    // that the device should have re-enumerated.
                    warn!("Mic open failed ({}), USB watchdog cycled - retrying", e);

                    // KEY FIX: Recreate the AudioRecorder to discard any stale
                    // CPAL device handles from before the power cycle.
                    self.recreate_recorder()?;

                    match self.start_microphone_stream_inner() {
                        Ok(()) => {
                            usb_watchdog::emit_stage_event_with_handle(
                                &Some(self.app_handle.clone()),
                                "recovered",
                                "Microphone stream recovered",
                            );
                            info!("Mic stream recovered after USB power cycle");
                            Ok(())
                        }
                        Err(retry_err) => {
                            error!("Mic open still failed after USB power cycle: {}", retry_err);
                            Err(retry_err)
                        }
                    }
                } else {
                    Err(e)
                }
            }
        }
    }

    /// Recreate the AudioRecorder to discard stale CPAL device handles.
    /// This is needed after a USB power cycle where the old device handle
    /// may no longer be valid, or when settings that affect the recorder
    /// (like pre-recording buffer) are changed.
    pub fn recreate_recorder(&self) -> Result<(), anyhow::Error> {
        info!("Recreating AudioRecorder to discard stale device handles");

        // RAII transaction guard: capture the stream's open state and whether
        // it *should* be running before we tear it down. After recreation, we
        // self-heal by restarting the stream if it was open and should still
        // be running (always-on or BT keep-alive). This makes it impossible
        // for callers to forget the restart step — the function that breaks
        // the invariant is responsible for restoring it.
        let was_open = self.is_open.load(Ordering::Acquire);
        let should_be_running = was_open
            && (matches!(*self.mode.lock(), MicrophoneMode::AlwaysOn)
                || self.bt_keep_alive.load(Ordering::Acquire));

        // Mark the stream as closed before tearing down — prevents concurrent
        // operations from acting on a recorder that is about to be replaced.
        if was_open {
            self.is_open.store(false, Ordering::Release);
        }

        // Take the old recorder and drop it (this stops any existing stream)
        let mut recorder_opt = self.recorder.lock();
        if recorder_opt.is_some() {
            // Close the old recorder to clean up resources.
            // Errors are logged but not propagated — the old recorder is
            // being discarded regardless, so we must continue with recreation.
            if let Some(mut old_rec) = recorder_opt.take() {
                info!("Closing old recorder before recreation");
                if let Err(e) = old_rec.close() {
                    warn!(
                        "Error closing old recorder during recreation (continuing anyway): {}",
                        e
                    );
                }
            }
        }

        let settings = get_settings(&self.app_handle);
        let vad_threshold = settings.vad_sensitivity.threshold();
        let vad_hangover_frames = settings.vad_sensitivity.hangover_frames();

        let selected_channel = settings.selected_channel;
        let new_recorder = create_audio_recorder(
            settings.vad_backend,
            &self.app_handle,
            vad_threshold,
            vad_hangover_frames,
            Arc::clone(&self.last_voice_activity_ms),
            Arc::clone(&self.close_generation),
            selected_channel,
        )?;

        *recorder_opt = Some(new_recorder);
        drop(recorder_opt);

        // RAII self-healing: if the stream was open and should still be
        // running (always-on or BT keep-alive), restart it automatically.
        // This ensures that recreate_recorder() never leaves the system in
        // a state where the "stream is open if always-on" invariant is
        // violated — the function that breaks it also restores it.
        if should_be_running {
            info!("Recreate_recorder: stream was running before recreation, self-healing by restarting");
            if let Err(e) = self.start_microphone_stream() {
                error!(
                    "Recreate_recorder: failed to self-heal stream restart: {}",
                    e
                );
                return Err(e);
            }
            info!("Recreate_recorder: stream self-healed successfully");
        }

        info!("AudioRecorder recreated successfully");
        Ok(())
    }

    fn start_microphone_stream_inner(&self) -> Result<(), anyhow::Error> {
        let start_time = Instant::now();

        // Don't mute immediately - caller will handle muting after audio feedback.
        // If a stale forced-mute somehow survived, restore it rather than just
        // clearing the flag, which would strand system audio muted.
        if self.did_mute.swap(false, Ordering::AcqRel) {
            let prev = *self.prev_muted.lock();
            restore_mute(prev);
            *self.prev_muted.lock() = None;
        }

        // Get the selected device from settings, considering clamshell mode.
        // No pre-flight enumeration here: when nothing is configured the
        // recorder resolves the system default itself, and a machine with no
        // input devices at all fails inside open() with the same
        // "No input device found" error this used to check for.
        let settings = get_settings(&self.app_handle);
        let resolve_started = Instant::now();
        let mut resolution = self.resolve_microphone_device(&settings);
        let resolve_elapsed = resolve_started.elapsed();

        // Ensure VAD is loaded if it wasn't for whatever reason
        let vad_started = Instant::now();
        self.preload_vad()?;
        let vad_elapsed = vad_started.elapsed();

        let open_started = Instant::now();
        let mut recorder_opt = self.recorder.lock();
        if let Some(rec) = recorder_opt.as_mut() {
            if let Err(first_err) = rec.open(resolution.device.clone()) {
                // A cached device or config may have gone stale (unplugged,
                // rate/format changed). Re-resolve from a fresh enumeration and
                // retry once before surfacing the error.
                warn!(
                    "Recorder open failed ({}); re-resolving device and retrying once",
                    first_err
                );
                self.invalidate_device_cache();
                resolution = self.resolve_microphone_device(&settings);
                rec.open(resolution.device.clone())
                    .map_err(|e| anyhow::anyhow!("Failed to open recorder: {}", e))?;
            }
        }
        debug!(
            "mic stream breakdown: device_resolve={:?} vad_ensure={:?} open={:?}",
            resolve_elapsed,
            vad_elapsed,
            open_started.elapsed()
        );
        drop(recorder_opt);

        self.is_open.store(true, Ordering::Release);

        if let Some(unavailable_name) = resolution.unavailable_selected_microphone {
            // Do this only after the default stream opened successfully. A
            // failed fallback must not erase the user's microphone preference.
            self.persist_default_microphone_after_fallback(&unavailable_name);
        }
        // This timing covers through cpal's stream.play() returning — i.e. the
        // point cpal surfaces as "stream running." It does NOT guarantee the
        // host audio device is producing samples yet; the first input callback
        // fires asynchronously one buffer period later (hardware dependent,
        // typically ~10–200ms on macOS, longer on Bluetooth/USB).
        info!(
            "Microphone stream initialized in {:?}",
            start_time.elapsed()
        );
        Ok(())
    }

    pub fn stop_microphone_stream(&self) {
        if !self.is_open.load(Ordering::Acquire) {
            return;
        }

        if self.did_mute.swap(false, Ordering::AcqRel) {
            let prev = *self.prev_muted.lock();
            restore_mute(prev);
            *self.prev_muted.lock() = None;
        }

        // Phase 1: Stop the recorder if recording, then close it.
        // Hold the recorder lock only for the stop+close (device I/O),
        // NOT across the state lock. This avoids an AB-BA deadlock with
        // try_start_recording which takes state → recorder.
        {
            let mut rec_guard = self.recorder.lock();
            if let Some(rec) = rec_guard.as_mut() {
                if self.is_recording.load(Ordering::Acquire) {
                    if let Err(e) = rec.stop() {
                        warn!("Error stopping recorder during stream shutdown: {}", e);
                        // Continue with close — the recorder may be in an inconsistent
                        // state, but we still want to release resources.
                    }
                    self.is_recording.store(false, Ordering::Release);
                }
                if let Err(e) = rec.close() {
                    warn!("Error closing recorder during stream shutdown: {}", e);
                    // State will be reset below — the recorder will be recreated on
                    // the next start_microphone_stream() call if needed.
                }
            }
        } // recorder lock dropped HERE

        // Phase 2: Transition state to Idle (separate lock, no recorder lock held).
        *self.state.lock() = RecordingState::Idle;

        self.is_open.store(false, Ordering::Release);
        debug!("Microphone stream stopped");
    }

    /* ---------- mode switching --------------------------------------------- */

    pub fn update_mode(&self, new_mode: MicrophoneMode) -> Result<(), anyhow::Error> {
        let cur_mode = self.mode.lock().clone();

        match (cur_mode, &new_mode) {
            (MicrophoneMode::AlwaysOn, MicrophoneMode::OnDemand) => {
                // Don't close the stream if BT keep-alive is active
                if self.bt_keep_alive.load(Ordering::Acquire) {
                    info!("BT keep-alive active: keeping mic stream open despite mode switch to OnDemand");
                } else if matches!(*self.state.lock(), RecordingState::Idle) {
                    self.close_generation.fetch_add(1, Ordering::SeqCst);
                    self.stop_microphone_stream();
                }
            }
            (MicrophoneMode::OnDemand, MicrophoneMode::AlwaysOn) => {
                // Stream may already be open from BT keep-alive
                if !self.is_open.load(Ordering::Acquire) {
                    self.close_generation.fetch_add(1, Ordering::SeqCst);
                    self.start_microphone_stream()?;
                }
            }
            _ => {}
        }

        *self.mode.lock() = new_mode;
        Ok(())
    }

    /* ---------- recording --------------------------------------------------- */

    /// Duration (ms) without receiving audio data before we consider the
    /// microphone stream dead and attempt to restart it.
    const STREAM_LIVENESS_TIMEOUT_MS: u64 = 3000;

    pub fn try_start_recording(&self, binding_id: &str) -> Result<(), String> {
        // Quick check under lock — verify we're in Idle state. If the state is
        // Recording but the is_recording flag has been cleared (e.g., by
        // cancel_recording's lock-timeout escape hatch), the state is stale and
        // we self-recover to Idle before proceeding.
        {
            let mut state = self.state.lock();
            match *state {
                RecordingState::Idle => {}
                RecordingState::Recording {
                    binding_id: ref stale_binding,
                    ..
                } => {
                    if self.is_recording.load(Ordering::Acquire) {
                        return Err("Already recording".to_string());
                    }
                    warn!(
                        "[RECOVERY] RecordingState is Recording but is_recording=false for binding {} — recovering to Idle",
                        stale_binding
                    );
                    *state = RecordingState::Idle;
                }
            }
        }
        // State lock is released here. The actual state transition happens
        // below, after the potentially-slow liveness check.

        let bt_keep_alive = self.bt_keep_alive.load(Ordering::Acquire);
        let is_always_on = matches!(*self.mode.lock(), MicrophoneMode::AlwaysOn);

        // Opens the stream in on-demand mode. In always-on mode the stream
        // is normally already open and this is a cheap aliveness check —
        // but if the capture worker died (device disconnect), it rebuilds
        // the stream instead of leaving every subsequent start wedged on
        // "Recorder not available".
        //
        // In on-demand mode (or when BT keep-alive is active), ensure the stream is open.
        // In always-on mode, check if the stream is alive and restart if needed.
        // KEY FIX: Also handle the case where the stream is NOT open at all
        // (e.g., after a failed USB power cycle recovery).
        // ALSO FIX: Check if device changed (e.g., USB cycling fallback to webcam,
        // now configured device has returned).
        let need_stream_open = if is_always_on || bt_keep_alive {
            // Always-on mode: check if stream is alive AND if device is correct
            let is_open = self.is_open.load(Ordering::Acquire);

            if !is_open {
                // Stream is not open at all — need to restart it
                warn!("Always-on microphone stream is not open — restarting");
                true
            } else if self
                .recorder
                .lock()
                .as_ref()
                .is_some_and(|rec| rec.needs_reopen())
            {
                warn!("Always-on microphone stream capture worker died — rebuilding");
                true
            } else {
                // Check if device has changed (USB cycling may have caused fallback)
                let settings = get_settings(&self.app_handle);
                let configured_device_name = if let Ok(is_clamshell) = clamshell::is_clamshell() {
                    if is_clamshell && settings.clamshell_microphone.is_some() {
                        settings.clamshell_microphone.as_ref()
                    } else {
                        settings.selected_microphone.as_ref()
                    }
                } else {
                    settings.selected_microphone.as_ref()
                };

                let current_device_name = {
                    let recorder_guard = self.recorder.lock();
                    recorder_guard.as_ref().and_then(|r| r.device_name())
                };

                let device_changed = match (&configured_device_name, &current_device_name) {
                    (Some(configured), Some(current)) => {
                        let changed = configured.as_str() != current.as_str();
                        if changed {
                            info!(
                                "Device mismatch detected: configured={:?}, current={:?}",
                                configured, current
                            );
                        }
                        changed
                    }
                    (Some(_), None) => true, // Configured but no current device
                    (None, Some(_)) => false, // No configured device, use whatever is open
                    (None, None) => false,
                };

                if device_changed {
                    info!(
                        "Device changed from {:?} to {:?}, reopening stream before recording",
                        current_device_name, configured_device_name
                    );
                    // Stop the current stream and recreate with correct device
                    self.stop_microphone_stream();
                    if let Err(e) = self.recreate_recorder() {
                        warn!("Failed to recreate recorder for device change: {}", e);
                    }
                    true
                } else {
                    // Stream is open with correct device, check if it's alive
                    let stream_alive = self.recorder.lock().as_ref().map_or(false, |r| {
                        r.is_stream_alive(Self::STREAM_LIVENESS_TIMEOUT_MS)
                    });

                    if !stream_alive {
                        warn!(
                            "Always-on microphone stream appears dead (no audio for {}ms) — restarting",
                            Self::STREAM_LIVENESS_TIMEOUT_MS
                        );
                        true
                    } else {
                        false // Stream is alive with correct device, no action needed
                    }
                }
            }
        } else {
            // On-demand mode: need to open stream
            true
        };

        if need_stream_open {
            // Show USB-cycling indicator on overlay so the user knows
            // the mic is being recovered (this can take 10+ seconds if
            // the USB watchdog triggers a power cycle).
            if is_always_on {
                utils::show_usb_cycling_overlay(&self.app_handle);
                crate::tray::change_tray_icon(
                    &self.app_handle,
                    crate::tray::TrayIconState::Recording,
                );
            }

            // Cancel any pending lazy close
            self.close_generation.fetch_add(1, Ordering::SeqCst);

            // Stop the stream if it's open (to clean up any stale state)
            if self.is_open.load(Ordering::Acquire) {
                self.stop_microphone_stream();
            }

            if let Err(e) = self.start_microphone_stream() {
                let msg = format!("{e}");
                error!("Failed to open/restart microphone stream: {msg}");

                // Clean up UI state on failure
                if is_always_on {
                    utils::hide_recording_overlay(&self.app_handle);
                    crate::tray::change_tray_icon(
                        &self.app_handle,
                        crate::tray::TrayIconState::Idle,
                    );
                }
                return Err(msg);
            }

            // No stabilization sleep: the recorder consumer now processes Cmd::Start as soon
            // as it is queued and begins recording on the first real audio chunk, so the
            // chunk itself is the readiness signal.

            if is_always_on {
                // Don't hide the cycling overlay here — the caller
                // (TranscribeAction::start) will show the recording overlay
                // which seamlessly transitions from USB-cycling to recording.
                // Only reset the tray icon; the overlay will be replaced.
                crate::tray::change_tray_icon(&self.app_handle, crate::tray::TrayIconState::Idle);
            }
        }

        // Re-acquire the state lock for the actual state transition.
        let mut state = self.state.lock();
        if matches!(*state, RecordingState::Recording { .. })
            && !self.is_recording.load(Ordering::Acquire)
        {
            // The state enum still says Recording but the live capture flag
            // has already been cleared, usually by the cancel recovery path.
            // Heal the stale enum and proceed with a normal start.
            warn!("[RECOVERY] try_start_recording check 2: healing stale Recording state (atomic=false)");
            *state = RecordingState::Idle;
        }
        if let RecordingState::Idle = *state {
            if let Some(rec) = self.recorder.lock().as_ref() {
                if rec.start(binding_id).is_ok() {
                    self.is_recording.store(true, Ordering::Release);
                    *state = RecordingState::Recording {
                        binding_id: binding_id.to_string(),
                        start_time: Instant::now(),
                    };
                    debug!("Recording started for binding {binding_id}");
                    return Ok(());
                }
            }
            Err("Recorder not available".to_string())
        } else {
            Err("Already recording".to_string())
        }
    }

    pub fn update_selected_device(&self) -> Result<(), anyhow::Error> {
        // Device settings changed; re-enumerate the device and restart capture.
        self.invalidate_device_cache();
        let was_open = self.is_open.load(Ordering::Acquire);
        if was_open {
            self.close_generation.fetch_add(1, Ordering::SeqCst);
            self.stop_microphone_stream();
            // Re-evaluate BT keep-alive after device change
            self.refresh_bluetooth_keep_alive();
            if self.bt_keep_alive.load(Ordering::Acquire)
                || matches!(*self.mode.lock(), MicrophoneMode::AlwaysOn)
            {
                self.start_microphone_stream()?;
            }
        }
        Ok(())
    }

    pub fn update_selected_channel(
        &self,
        selected_channel: Option<u16>,
    ) -> Result<(), anyhow::Error> {
        // Serialize against recording start/stop. Restarting an active capture
        // would discard its samples and leave the manager's recording state out
        // of sync with the new recorder.
        {
            let state = self.state.lock();
            if !matches!(*state, RecordingState::Idle) {
                return Err(anyhow::anyhow!(
                    "Cannot change the input channel while recording"
                ));
            }
        }

        let previous_channel = get_settings(&self.app_handle).selected_channel;
        let was_open = self.is_open.load(Ordering::Acquire);
        if was_open {
            self.close_generation.fetch_add(1, Ordering::SeqCst);
            self.stop_microphone_stream();
        }
        if let Some(recorder) = self.recorder.lock().as_mut() {
            recorder.set_selected_channel(selected_channel);
        }
        if was_open {
            if let Err(error) = self.start_microphone_stream() {
                if let Some(recorder) = self.recorder.lock().as_mut() {
                    recorder.set_selected_channel(previous_channel);
                }
                return Err(error);
            }
        }
        Ok(())
    }

    pub fn update_vad_backend(&self, backend: VadBackend) -> Result<(), anyhow::Error> {
        {
            let state = self.state.lock();
            if !matches!(*state, RecordingState::Idle) {
                return Err(anyhow::anyhow!(
                    "Cannot change the VAD backend while recording"
                ));
            }
        }

        let settings = get_settings(&self.app_handle);
        let vad_threshold = settings.vad_sensitivity.threshold();
        let vad_hangover_frames = settings.vad_sensitivity.hangover_frames();
        let selected_channel = settings.selected_channel;

        let replacement = create_audio_recorder(
            backend,
            &self.app_handle,
            vad_threshold,
            vad_hangover_frames,
            Arc::clone(&self.last_voice_activity_ms),
            Arc::clone(&self.close_generation),
            selected_channel,
        )?;

        let was_open = self.is_open.load(Ordering::Acquire);
        self.close_generation.fetch_add(1, Ordering::SeqCst);
        if was_open {
            self.stop_microphone_stream();
        }

        let previous_recorder = self.recorder.lock().replace(replacement);
        if was_open {
            if let Err(change_error) = self.start_microphone_stream() {
                if let Some(recorder) = self.recorder.lock().as_mut() {
                    let _ = recorder.close();
                }
                *self.recorder.lock() = previous_recorder;
                if let Err(rollback_error) = self.start_microphone_stream() {
                    error!(
                        "Failed to restore mic stream after VAD backend change: {rollback_error}"
                    );
                }
                return Err(anyhow::anyhow!(
                    "Failed to reopen mic with {:?} VAD: {change_error}",
                    backend
                ));
            }
        }

        info!("VAD backend changed to {:?}", backend);
        Ok(())
    }

    /// Re-evaluate whether a Bluetooth output device is active and update
    /// the keep-alive flag accordingly. When BT output becomes active, open
    /// the mic stream if it isn't already. When BT output goes away, close
    /// the stream if we're in OnDemand mode and not recording.
    pub fn refresh_bluetooth_keep_alive(&self) {
        let bt_active = self.is_bluetooth_output_active();

        if bt_active && !self.bt_keep_alive.load(Ordering::Acquire) {
            info!("Bluetooth output device detected — enabling mic stream keep-alive to prevent audio dropouts");
            self.bt_keep_alive.store(true, Ordering::Release);
            // Open the mic stream if not already open
            if !self.is_open.load(Ordering::Acquire) {
                if let Err(e) = self.start_microphone_stream() {
                    error!("Failed to open mic stream for BT keep-alive: {}", e);
                }
            }
        } else if !bt_active && self.bt_keep_alive.load(Ordering::Acquire) {
            info!("Bluetooth output device no longer detected — disabling mic stream keep-alive");
            self.bt_keep_alive.store(false, Ordering::Release);
            // Close the stream if we're in OnDemand mode and not recording
            if matches!(*self.mode.lock(), MicrophoneMode::OnDemand) && !self.is_recording() {
                self.close_generation.fetch_add(1, Ordering::SeqCst);
                self.stop_microphone_stream();
            }
        }
    }

    pub fn stop_recording(&self, binding_id: &str) -> Option<(Vec<f32>, Vec<f32>)> {
        let t_start = std::time::Instant::now();
        debug!(
            "[STOP-DIAGNOSTIC] stop_recording started for binding '{}'",
            binding_id
        );

        let t_state_lock = std::time::Instant::now();
        let state = self.state.lock();
        debug!(
            "[STOP-DIAGNOSTIC] state.lock acquired in {:?}",
            t_state_lock.elapsed()
        );

        match *state {
            RecordingState::Recording {
                binding_id: ref active,
                start_time,
            } if active == binding_id => {
                debug!(
                    "[STOP-DIAGNOSTIC] Stopping recording for binding '{}'",
                    binding_id
                );
                // Calculate recording duration for USB watchdog checks
                let recording_duration = start_time.elapsed().as_secs_f32();
                debug!("Recording duration: {:.1}s", recording_duration);

                // NOTE: We intentionally keep the state as Recording during the
                // smart-stop buffer period so that try_start_recording() rejects
                // new recordings while we are still capturing trailing audio.
                drop(state);

                // BUGFIX (Fix 6): Use split send/wait pattern for both smart_stop
                // and stop. Previously, the recorder lock was held for the entire
                // duration of the wait (up to 5 seconds for stop, up to
                // max_buffer_ms + 2s for smart_stop), blocking cancel_recording(),
                // level callbacks, and streaming callbacks. Now we send the
                // command while holding the lock, drop the lock, then wait for
                // the response — allowing other operations to proceed during the wait.
                let settings = get_settings(&self.app_handle);

                let (processed_samples, raw_samples) = if settings.extra_recording_buffer_ms > 0 {
                    debug!(
                        "Smart-stop: starting volume-aware buffer (max {}ms)",
                        settings.extra_recording_buffer_ms
                    );
                    // Phase 1: Send the smart_stop command while holding the lock.
                    // This is instantaneous — it just sends a message through the channel.
                    let t_rec_lock = std::time::Instant::now();
                    let resp_rx = {
                        // Diagnostic: try-lock with timeout to detect a wedged recorder mutex.
                        let guard = match self.recorder.try_lock_for(Duration::from_secs(2)) {
                            Some(g) => {
                                debug!(
                                    "[STOP-DIAGNOSTIC] recorder.lock acquired in {:?}",
                                    t_rec_lock.elapsed()
                                );
                                g
                            }
                            None => {
                                warn!(
                                    "[STOP-DIAGNOSTIC] recorder.lock NOT acquired after 2s on smart_stop — recorder mutex may be wedged; proceeding with None"
                                );
                                // Force state to Idle so the UI doesn't stay stuck.
                                {
                                    let mut state = self.state.lock();
                                    *state = RecordingState::Idle;
                                }
                                self.is_recording.store(false, Ordering::Release);
                                return Some((Vec::new(), Vec::new()));
                            }
                        };
                        match guard.as_ref() {
                            Some(rec) => {
                                let t_send = std::time::Instant::now();
                                let rx = rec
                                    .smart_stop_send(settings.extra_recording_buffer_ms)
                                    .map_err(|e| {
                                        error!("smart_stop_send() failed: {e}");
                                        e
                                    })
                                    .ok();
                                debug!(
                                    "[STOP-DIAGNOSTIC] smart_stop_send completed in {:?}",
                                    t_send.elapsed()
                                );
                                rx
                            }
                            None => {
                                error!("Recorder not available for smart_stop");
                                None
                            }
                        }
                    };
                    // Lock is now dropped. Other operations can proceed.

                    // Phase 2: Wait for the response without holding the lock.
                    let t_wait = std::time::Instant::now();
                    let result = match resp_rx {
                        Some(rx) => AudioRecorder::wait_for_smart_stop_result(
                            rx,
                            settings.extra_recording_buffer_ms,
                        )
                        .unwrap_or_else(|e| {
                            error!("smart_stop wait failed: {e}");
                            (Vec::new(), Vec::new())
                        }),
                        None => (Vec::new(), Vec::new()),
                    };
                    debug!(
                        "[STOP-DIAGNOSTIC] smart_stop wait completed in {:?}",
                        t_wait.elapsed()
                    );
                    result
                } else {
                    // Phase 1: Send the stop command while holding the lock.
                    let t_rec_lock = std::time::Instant::now();
                    let resp_rx = {
                        // Diagnostic: try-lock with timeout to detect a wedged recorder mutex.
                        let guard = match self.recorder.try_lock_for(Duration::from_secs(2)) {
                            Some(g) => {
                                debug!(
                                    "[STOP-DIAGNOSTIC] recorder.lock acquired in {:?}",
                                    t_rec_lock.elapsed()
                                );
                                g
                            }
                            None => {
                                warn!(
                                    "[STOP-DIAGNOSTIC] recorder.lock NOT acquired after 2s — recorder mutex may be wedged; proceeding with None"
                                );
                                // Force state to Idle so the UI doesn't stay stuck.
                                {
                                    let mut state = self.state.lock();
                                    *state = RecordingState::Idle;
                                }
                                self.is_recording.store(false, Ordering::Release);
                                return Some((Vec::new(), Vec::new()));
                            }
                        };
                        match guard.as_ref() {
                            Some(rec) => {
                                let t_send = std::time::Instant::now();
                                let rx = rec
                                    .stop_send()
                                    .map_err(|e| {
                                        error!("stop_send() failed: {e}");
                                        e
                                    })
                                    .ok();
                                debug!(
                                    "[STOP-DIAGNOSTIC] stop_send completed in {:?}",
                                    t_send.elapsed()
                                );
                                rx
                            }
                            None => {
                                error!("Recorder not available");
                                None
                            }
                        }
                    };
                    // Lock is now dropped.

                    // Phase 2: Wait for the response without holding the lock.
                    let t_wait = std::time::Instant::now();
                    let result = match resp_rx {
                        Some(rx) => AudioRecorder::wait_for_stop_result(rx).unwrap_or_else(|e| {
                            error!("stop wait failed: {e}");
                            (Vec::new(), Vec::new())
                        }),
                        None => (Vec::new(), Vec::new()),
                    };
                    debug!(
                        "[STOP-DIAGNOSTIC] stop wait completed in {:?}",
                        t_wait.elapsed()
                    );
                    result
                };

                // Now transition to Idle after the buffer is complete.
                {
                    let t_idle_lock = std::time::Instant::now();
                    let mut state = self.state.lock();
                    debug!(
                        "[STOP-DIAGNOSTIC] idle state.lock acquired in {:?}",
                        t_idle_lock.elapsed()
                    );
                    *state = RecordingState::Idle;
                }

                self.is_recording.store(false, Ordering::Release);
                debug!(
                    "[STOP-DIAGNOSTIC] stop_recording returning {} processed / {} raw samples (total elapsed {:?})",
                    processed_samples.len(),
                    raw_samples.len(),
                    t_start.elapsed()
                );

                // Check for very low audio levels (potential dead/muted mic)
                // This detects cases where the mic "works" but captures only silence.
                // NOTE: We intentionally do NOT call on_recording_finished() here because
                // low audio is a failure condition - we want to count it as a failure
                // and potentially trigger USB cycling. The next successful recording will
                // reset the failure counter via on_recording_finished() -> on_mic_open_succeeded().
                let max_level = self
                    .recorder
                    .lock()
                    .as_ref()
                    .map(|r| r.get_max_level())
                    .unwrap_or(0.0);

                if processed_samples.len() > 0 && max_level < NO_AUDIO_THRESHOLD {
                    warn!(
                        "Recording had very low audio level (max RMS: {:.6}, threshold: {:.6}, duration: {:.1}s) - mic may be dead/muted",
                        max_level, NO_AUDIO_THRESHOLD, recording_duration
                    );
                    // Treat as a failure - USB watchdog will count it
                    // Pass recording duration so it can require minimum duration before counting
                    if self.usb_watchdog.on_low_audio_level(recording_duration) {
                        if let Err(e) = self.restart_microphone_if_needed() {
                            error!(
                                "Failed to restart microphone after low-audio-level USB cycle: {}",
                                e
                            );
                        }
                    }
                } else {
                    // Normal path: inform USB watchdog about recording result.
                    // If 0 samples were captured, this may trigger an automatic USB cycle.
                    // Pass recording duration so it can require minimum duration before counting.
                    if self
                        .usb_watchdog
                        .on_recording_finished(processed_samples.len(), recording_duration)
                    {
                        // Watchdog completed a power cycle. Restart the stream if needed.
                        if let Err(e) = self.restart_microphone_if_needed() {
                            error!(
                                "Failed to restart microphone after dead-stream USB cycle: {}",
                                e
                            );
                        }
                    }
                }

                // In on-demand mode, decide whether to close the mic stream.
                // When a Bluetooth output device is active, we keep the stream
                // alive permanently to prevent the A2DP↔HFP profile switch that
                // causes audio dropouts on BT headphones.
                if matches!(*self.mode.lock(), MicrophoneMode::OnDemand) {
                    let bt_keep_alive = self.bt_keep_alive.load(Ordering::Acquire);
                    if bt_keep_alive {
                        debug!("BT keep-alive active: keeping mic stream open");
                    } else if get_settings(&self.app_handle).lazy_stream_close {
                        self.schedule_lazy_close();
                    } else {
                        self.stop_microphone_stream();
                    }
                }

                // Pad short audio with silence at the BEGINNING to reduce Whisper
                // hallucinations and prevent clipping of the first words.
                // Very short clips (< 3s) padded with silence at the end would cause
                // Whisper to miss the first words. By padding at the start, we give
                // Whisper context and ensure the captured speech timing is preserved.
                // A 3-second minimum gives Whisper enough context to produce a good
                // transcription without hallucinating.
                //
                // This pad is applied only when the selected model loads through
                // EngineType::TranscribeCpp (the GGUF/GGML path). That family includes
                // Whisper plus other ASR archs (Parakeet-GGUF, Voxtral, Qwen3-ASR,
                // SenseVoice, Moonshine-GGUF, ...); the pad was calibrated for Whisper's
                // short-clip hallucination mode and is conservatively applied to the
                // whole TranscribeCpp family because (a) leading silence is generally
                // benign for ASR models and (b) the AudioRecordingManager layer has no
                // access to the loaded model's arch() (that probe happens later in
                // TranscriptionManager, transcription.rs:1586). ONNX engines (Parakeet,
                // Moonshine, SenseVoice, GigaAM, Canary, Cohere) are excluded entirely.
                //
                // NOTE: trailing silence is currently NOT trimmed from the pipeline.
                // `audio_toolkit::vad::trim_trailing_silence` exists but is dead code.
                // Investigate enabling it for future cleanup only after testing against
                // first-word clipping regressions.
                let s_len = processed_samples.len();
                let min_samples = WHISPER_SAMPLE_RATE * 3; // 3 seconds minimum
                let engine_type = self
                    .app_handle
                    .try_state::<Arc<ModelManager>>()
                    .and_then(|mm| mm.get_model_info(&settings.selected_model))
                    .map(|info| info.engine_type);
                let is_whisper_family = match engine_type {
                    Some(EngineType::TranscribeCpp) => {
                        debug!(
                            "Applying 3s leading silence pad for TranscribeCpp engine (selected_model={})",
                            settings.selected_model
                        );
                        true
                    }
                    Some(other) => {
                        debug!(
                            "Skipping 3s leading silence pad for non-Whisper engine {:?} (selected_model={})",
                            other, settings.selected_model
                        );
                        false
                    }
                    None => {
                        debug!(
                            "No model info for '{}'; skipping 3s leading silence pad",
                            settings.selected_model
                        );
                        false
                    }
                };
                let padded_processed = if is_whisper_family && s_len > 0 && s_len < min_samples {
                    let needed_silence = min_samples - s_len;
                    debug!(
                        "Padding {}-sample processed clip with {} samples of leading silence",
                        s_len, needed_silence
                    );
                    let mut padded = vec![0.0f32; needed_silence];
                    padded.extend_from_slice(&processed_samples);
                    padded
                } else {
                    processed_samples
                };
                Some((padded_processed, raw_samples))
            }
            RecordingState::Recording {
                binding_id: ref active,
                ..
            } => {
                warn!(
                    "stop_recording binding mismatch: requested '{}', active '{}' \
                     (coordinator state may be wedged; cancel-recording recovers)",
                    binding_id, active
                );
                None
            }
            _ => {
                warn!(
                    "stop_recording called for '{}' but not currently recording (state={:?})",
                    binding_id, *state
                );
                None
            }
        }
    }
    pub fn is_recording(&self) -> bool {
        matches!(*self.state.lock(), RecordingState::Recording { .. })
    }

    /// True only when the state enum says Recording AND the capture flag is set.
    /// Consult when deciding whether a just-dispatched start actually produced a
    /// live recording; the cancel escape hatch clears the flag while a stale
    /// enum state may linger until the recovery worker heals it.
    pub fn is_recording_effective(&self) -> bool {
        matches!(*self.state.lock(), RecordingState::Recording { .. })
            && self.is_recording.load(Ordering::Acquire)
    }

    /// Diagnostic wrapper around `is_recording()` that logs the result and
    /// any unexpectedly slow state-lock acquisition. Use this on the
    /// stop/cancel path to capture whether the recorder state matches the
    /// coordinator's assumptions without spamming the hot recording loop.
    pub fn is_recording_with_log(&self, context: &str) -> bool {
        let t = std::time::Instant::now();
        let result = self.is_recording();
        let dt = t.elapsed();
        if dt > std::time::Duration::from_millis(100) {
            warn!("is_recording({}) took {:?}, result={}", context, dt, result);
        } else {
            debug!("is_recording({}) = {} (took {:?})", context, result, dt);
        }
        result
    }

    /// Return the elapsed milliseconds since recording start at which VAD last
    /// detected speech. The coordinator uses this to implement a sliding timeout:
    /// if speech has occurred within the last `RECORDING_TIMEOUT`, the deadline
    /// is reset.
    pub fn last_voice_activity_ms(&self) -> u64 {
        self.last_voice_activity_ms.load(Ordering::Acquire)
    }

    /// Check if the microphone stream is open (always-on mode or BT keep-alive)
    pub fn is_stream_open(&self) -> bool {
        self.is_open.load(Ordering::Acquire)
    }

    /// Check if always-on microphone mode is enabled
    pub fn is_always_on(&self) -> bool {
        matches!(*self.mode.lock(), MicrophoneMode::AlwaysOn)
    }

    /// Check if Bluetooth keep-alive is active
    pub fn is_bt_keep_alive(&self) -> bool {
        self.bt_keep_alive.load(Ordering::Acquire)
    }

    /// Cancel any ongoing recording without returning audio samples.
    ///
    /// BUGFIX (Fix 6): Uses the split send/wait pattern to avoid holding the
    /// recorder lock during the blocking stop() call. This prevents lock contention
    /// where stop_recording's smart_stop holds the lock for seconds while
    /// cancel_recording tries to acquire it.
    ///
    /// Phase 1: Send the stop command while holding the lock (instant).
    /// Phase 2: Drop the lock, then wait for the response (up to 5 seconds).
    /// If the lock can't be acquired within 1 second (stop_recording is likely
    /// holding it during smart_stop), proceed without stopping the recorder —
    /// it will stop on its own when the current operation completes.
    pub fn cancel_recording(&self) {
        info!(
            "[CANCEL-TIMING] cancel_recording START on thread {:?}",
            std::thread::current().id()
        );

        // Mark the emergency backup as cancelled (renames in-progress file to
        // *_cancelled.wav). Do this before we tear down the recorder so a crash
        // during stop still leaves a recoverable file. The recorder worker
        // also owns a clone of the global backup and will drop its reference
        // after the stop reply, but the explicit cancel here is authoritative.
        if let Some(backup) = crate::emergency_save::get_emergency_backup() {
            let cancelled_path = backup.cancel_recording();
            log::info!(
                "[EMERGENCY-BACKUP] cancel_recording finalised: {:?}",
                cancelled_path
            );
        }

        let t_state_lock = std::time::Instant::now();
        let mut state = match self.state.try_lock_for(std::time::Duration::from_secs(2)) {
            Some(guard) => {
                info!(
                    "[CANCEL-TIMING] cancel_recording: state.try_lock_for(2s) acquired in {:?}",
                    t_state_lock.elapsed()
                );
                guard
            }
            None => {
                warn!("[CANCEL-TIMING] Could not acquire state lock within 2s — forcing is_recording=false and spawning recovery worker");
                // Force the recording flag off so the app can recover. The enum
                // state and the live recorder capture are repaired by a detached
                // worker so this cancel path returns immediately and never wedges
                // the caller (e.g. the global shortcut thread).
                self.is_recording.store(false, Ordering::Release);
                let this = self.clone();
                std::thread::spawn(move || this.recovery_after_state_lock_timeout());
                return;
            }
        };

        if let RecordingState::Recording { .. } = *state {
            info!("[CANCEL-TIMING] cancel_recording: state is Recording, beginning stop sequence");
            *state = RecordingState::Idle;
            drop(state);

            // Phase 1: Send the stop command while holding the lock.
            // Try to acquire the lock with a timeout. If stop_recording is
            // currently holding the lock during a smart_stop send phase, this
            // should succeed quickly. If it's in the wait phase, the lock is
            // already dropped and we can proceed.
            let resp_rx = {
                let t_recorder_lock = std::time::Instant::now();
                let guard = self
                    .recorder
                    .try_lock_for(std::time::Duration::from_secs(1));
                info!("[CANCEL-TIMING] cancel_recording: recorder.try_lock_for(1s) took {:?}, acquired={}", t_recorder_lock.elapsed(), guard.is_some());
                match guard {
                    Some(guard) => match guard.as_ref() {
                        Some(rec) => {
                            let t_stop_send = std::time::Instant::now();
                            let rx = rec.stop_send().ok();
                            let dt = t_stop_send.elapsed();
                            if dt > std::time::Duration::from_millis(100) {
                                warn!(
                                    "[CANCEL-TIMING] cancel_recording: stop_send() took {:?}",
                                    dt
                                );
                            } else {
                                info!(
                                    "[CANCEL-TIMING] cancel_recording: stop_send() took {:?}",
                                    dt
                                );
                            }
                            rx
                        }
                        None => {
                            warn!("Recorder not available during cancel");
                            None
                        }
                    },
                    None => {
                        // Couldn't acquire the lock within timeout. The stop_recording
                        // is likely in its wait phase. We've already set the state
                        // to Idle, so the recording will end when stop_recording
                        // completes. The CancelSignal also prevents transcription.
                        warn!("Could not acquire recorder lock for cancel — recording will stop on its own");
                        None
                    }
                }
            };
            // Lock is now dropped.

            // Phase 2: Wait for the stop response without holding the lock.
            // This can block for up to 5 seconds, but other operations can
            // now proceed since we've dropped the lock.
            if let Some(rx) = resp_rx {
                let t_wait = std::time::Instant::now();
                if let Err(e) = AudioRecorder::wait_for_stop_result(rx) {
                    warn!("Error waiting for recorder stop during cancel: {}", e);
                }
                info!(
                    "[CANCEL-TIMING] cancel_recording: wait_for_stop_result took {:?}",
                    t_wait.elapsed()
                );
            }

            self.is_recording.store(false, Ordering::Release);

            // In on-demand mode, decide whether to close the mic stream.

            // When a Bluetooth output device is active, we keep the stream
            // alive permanently to prevent the A2DP↔HFP profile switch.
            let t_mode_lock = std::time::Instant::now();
            if matches!(*self.mode.lock(), MicrophoneMode::OnDemand) {
                info!(
                    "[CANCEL-TIMING] cancel_recording: mode.lock() acquired in {:?}",
                    t_mode_lock.elapsed()
                );
                let bt_keep_alive = self.bt_keep_alive.load(Ordering::Acquire);
                if bt_keep_alive {
                    debug!("BT keep-alive active: keeping mic stream open");
                } else if get_settings(&self.app_handle).lazy_stream_close {
                    self.schedule_lazy_close();
                } else {
                    self.stop_microphone_stream();
                }
            } else {
                info!(
                    "[CANCEL-TIMING] cancel_recording: mode.lock() (not OnDemand) took {:?}",
                    t_mode_lock.elapsed()
                );
            }
        }

        info!("[CANCEL-TIMING] cancel_recording COMPLETE");
    }

    /// Detached recovery worker run when `cancel_recording` cannot acquire the
    /// state lock within its 2-second timeout. Bounded in every phase so it never
    /// hangs the caller or leaks resources.
    ///
    /// Accepted race: if a new recording start is mid-flight while phase A's
    /// `stop_send` queues, the queued `Cmd::Stop` could kill the fresh session
    /// (a few-ms window, logged). This is vastly better than leaving the manager
    /// permanently wedged with a stale Recording state and a live capture.
    fn recovery_after_state_lock_timeout(&self) {
        let t_start = std::time::Instant::now();
        info!("[CANCEL-TIMING] recovery worker START");

        // Phase A — stop the live recorder capture without waiting for samples.
        // wait_for_stop_result can block for up to 5s; cancel discards samples,
        // so dropping the reply receiver is safe (recorder.rs uses `let _ =` on
        // the send and replaces any previous pending reply channel).
        let mut stopped_capture = false;
        for attempt in 1..=3 {
            let t = std::time::Instant::now();
            if let Some(guard) = self
                .recorder
                .try_lock_for(std::time::Duration::from_millis(500))
            {
                info!(
                    "[CANCEL-TIMING] recovery worker: recorder.try_lock acquired on attempt {} in {:?}",
                    attempt,
                    t.elapsed()
                );
                if let Some(rec) = guard.as_ref() {
                    if let Err(e) = rec.stop_send() {
                        warn!("[CANCEL-TIMING] recovery worker: stop_send failed: {}", e);
                    } else {
                        info!("[CANCEL-TIMING] recovery worker: stop_send dispatched");
                    }
                }
                stopped_capture = true;
                break;
            } else {
                warn!(
                    "[CANCEL-TIMING] recovery worker: recorder.try_lock attempt {} timed out after {:?}",
                    attempt,
                    t.elapsed()
                );
            }
        }
        if !stopped_capture {
            warn!("[CANCEL-TIMING] recovery worker: could not acquire recorder lock; another owner will finish its stop");
        }

        // Phase B — heal the stale Recording state. The atomic gate guarantees a
        // genuinely-new recording (atomic true) or a smart-stop trailing window
        // (state Recording + atomic true) is never touched.
        let mut healed = false;
        let t_heal_start = std::time::Instant::now();
        while t_heal_start.elapsed() < std::time::Duration::from_secs(10) {
            let t = std::time::Instant::now();
            if let Some(mut state) = self
                .state
                .try_lock_for(std::time::Duration::from_millis(500))
            {
                info!(
                    "[CANCEL-TIMING] recovery worker: state.try_lock acquired in {:?}",
                    t.elapsed()
                );
                if matches!(*state, RecordingState::Recording { .. })
                    && !self.is_recording.load(Ordering::Acquire)
                {
                    *state = RecordingState::Idle;
                    info!("[CANCEL-TIMING] recovery worker healed stale Recording state");
                    healed = true;
                }
                break;
            } else {
                warn!(
                    "[CANCEL-TIMING] recovery worker: state.try_lock timed out after {:?}",
                    t.elapsed()
                );
            }
        }

        // Phase C — on-demand stream close parity with the normal cancel path.
        // Only run if we actually healed; stop_microphone_stream takes the state
        // lock in its Phase 2, so it must NEVER be called while holding the lock.
        if healed {
            let bt_keep_alive = self.bt_keep_alive.load(Ordering::Acquire);
            let lazy_stream_close = get_settings(&self.app_handle).lazy_stream_close;
            if matches!(*self.mode.lock(), MicrophoneMode::OnDemand) && !bt_keep_alive {
                let t = std::time::Instant::now();
                if lazy_stream_close {
                    self.schedule_lazy_close();
                    info!(
                        "[CANCEL-TIMING] recovery worker: schedule_lazy_close took {:?}",
                        t.elapsed()
                    );
                } else {
                    self.stop_microphone_stream();
                    info!(
                        "[CANCEL-TIMING] recovery worker: stop_microphone_stream took {:?}",
                        t.elapsed()
                    );
                }
            }
        }

        info!(
            "[CANCEL-TIMING] recovery worker COMPLETE in {:?} (healed={})",
            t_start.elapsed(),
            healed
        );
    }

    /// Restart the microphone stream if it should be active.
    /// Called after USB power cycling completes to fix the "mic not listening,
    /// volume bars not moving" issue.
    /// Returns Ok(()) if the stream was restarted or wasn't needed, Err if restart failed.
    pub fn restart_microphone_if_needed(&self) -> Result<(), anyhow::Error> {
        let is_always_on = matches!(*self.mode.lock(), MicrophoneMode::AlwaysOn);
        let bt_keep_alive = self.bt_keep_alive.load(Ordering::Acquire);

        if is_always_on || bt_keep_alive {
            info!("Restarting microphone stream after USB power cycle");
            // Stop the current stream if open
            if self.is_open.load(Ordering::Acquire) {
                self.stop_microphone_stream();
            }
            // Recreate the recorder to discard stale CPAL handles
            self.recreate_recorder()?;
            // Start a fresh stream
            self.start_microphone_stream()
        } else {
            debug!("Microphone stream not needed (not always-on, no BT keep-alive)");
            Ok(())
        }
    }

    /// Runtime toggle for streaming transcription (Strategy pattern).
    /// When enabled and a streaming callback is attached, the callback
    /// will be invoked periodically during recording to produce live captions.
    /// When disabled, the callback exists but is not invoked, allowing
    /// instant toggling without recreating the recorder.
    ///
    /// This eliminates Bug 1 where toggling live captions required
    /// destroying and recreating the AudioRecorder, which tore down the
    /// mic stream. Callers who forgot to restart the stream after
    /// recreation ended up with a dead always-on mic.
    ///
    /// If the recorder doesn't have a streaming callback (e.g., TranscriptionManager
    /// wasn't available at creation time), this method will recreate the recorder
    /// to attach the callback.
    pub fn set_streaming_enabled(&self, enabled: bool) -> Result<(), anyhow::Error> {
        let needs_recreation = {
            let recorder_guard = self.recorder.lock();
            match recorder_guard.as_ref() {
                Some(rec) => {
                    let has_callback = rec.is_streaming_enabled() || rec.has_streaming_callback();
                    // If the recorder already has a streaming callback, just toggle the flag
                    !has_callback && enabled
                    // No callback but trying to enable — need to recreate to attach it
                }
                None => enabled, // No recorder at all, need to create one
            }
        };

        if needs_recreation {
            info!("[Live Captions] Recorder has no streaming callback — recreating to attach it");
            // recreate_recorder() will self-heal the stream if needed (RAII guard)
            return self.recreate_recorder();
        }

        // Toggle the flag — no stream recreation needed
        let recorder_guard = self.recorder.lock();
        if let Some(rec) = recorder_guard.as_ref() {
            let previous = rec.set_streaming_enabled(enabled);
            info!(
                "[Live Captions] Streaming {} (was {})",
                if enabled { "enabled" } else { "disabled" },
                if previous { "enabled" } else { "disabled" }
            );
        } else {
            warn!("[Live Captions] No recorder available to toggle streaming");
        }
        Ok(())
    }

    /// Check if the microphone stream is currently open and alive.
    /// Returns (is_open, is_alive).
    /// This is useful for diagnostics and recovery checks.
    pub fn check_stream_health(&self) -> (bool, bool) {
        let is_open = self.is_open.load(Ordering::Acquire);
        if !is_open {
            return (false, false);
        }

        let stream_alive = {
            let recorder_guard = self.recorder.lock();
            recorder_guard.as_ref().map_or(false, |r| {
                r.is_stream_alive(Self::STREAM_LIVENESS_TIMEOUT_MS)
            })
        };

        (true, stream_alive)
    }
}

impl Drop for AudioRecordingManager {
    fn drop(&mut self) {
        info!("AudioRecordingManager dropping — stopping background threads");
        self.stop_device_monitor();
        self.liveness_stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.liveness_monitor.lock().take() {
            let _ = handle.join();
        }
    }
}
