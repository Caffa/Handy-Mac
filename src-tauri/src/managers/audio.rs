use crate::audio_toolkit::{
    is_bluetooth_audio_active, list_input_devices, list_output_devices, process_transcription_text,
    vad::SmoothedVad, AudioRecorder, SileroVad,
};
use crate::errors::AppError;
use crate::helpers::clamshell;
use crate::managers::model::{EngineType, ModelManager};
use crate::managers::transcription::TranscriptionManager;
use crate::portable;
use crate::settings::{get_settings, AppSettings};
use crate::usb_watchdog;
use crate::usb_watchdog::UsbWatchdog;
use crate::utils;
use log::{debug, error, info, warn};
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
    vad_path: &str,
    app_handle: &tauri::AppHandle,
    vad_threshold: f32,
    vad_hangover_frames: usize,
) -> Result<AudioRecorder, anyhow::Error> {
    let silero = SileroVad::new(vad_path, vad_threshold)
        .map_err(|e| anyhow::anyhow!("Failed to create SileroVad: {}", e))?;
    let smoothed_vad = SmoothedVad::new(Box::new(silero), 15, vad_hangover_frames, 2);

    // Check if live captions is enabled
    let settings = get_settings(app_handle);
    let live_captions_enabled = settings.live_captions_enabled;
    let pre_buffer_ms = settings.pre_recording_buffer_ms;
    let noise_suppression_enabled = settings.noise_suppression_enabled;
    let noise_suppression_level = settings.noise_suppression_level;

    // Recorder with VAD plus a spectrum-level callback that forwards updates to
    // the frontend, and optionally a streaming transcription callback for live captions.
    let mut recorder = AudioRecorder::new()
        .map_err(|e| anyhow::anyhow!("Failed to create AudioRecorder: {}", e))?
        .with_vad(Box::new(smoothed_vad))
        .with_level_callback({
            let app_handle = app_handle.clone();
            move |levels| {
                utils::emit_levels(&app_handle, &levels);
            }
        });

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
    // If TranscriptionManager isn't available yet, skip the callback but
    // still create the recorder — the callback can be attached later via
    // recreate_recorder() when TM becomes available.
    let cancel_flag = app_handle
        .try_state::<Arc<Mutex<TranscriptionManager>>>()
        .map(|tm_state| tm_state.lock().streaming_cancel_flag());

    let recorder = match cancel_flag {
        Some(cancel_flag) => {
            info!(
                "[Live Captions] Setting up streaming callback (enabled={})",
                live_captions_enabled
            );
            recorder
                .with_streaming_callback({
                    let app_handle = app_handle.clone();
                    let cancel_flag = cancel_flag.clone();
                    move |samples| {
                        // Check cancellation WITHOUT lock (atomic load).
                        // This avoids acquiring the TranscriptionManager mutex on
                        // every streaming callback, which would cause lock contention
                        // with the main transcription path.
                        if cancel_flag.load(Ordering::Acquire) {
                            debug!("Skipping streaming transcription - cancellation requested");
                            return;
                        }

                        // This callback runs in the audio thread, so we need to spawn
                        // a blocking task to avoid blocking audio capture.
                        let app_handle = app_handle.clone();
                        let cancel_flag = cancel_flag.clone();
                        tauri::async_runtime::spawn_blocking(move || {
                            // Re-check cancellation after acquiring blocking thread
                            if cancel_flag.load(Ordering::Acquire) {
                                debug!("Skipping streaming transcription (blocking) - cancellation requested");
                                return;
                            }

                            // Get the transcription manager from app state
                            let tm = match app_handle.try_state::<Arc<Mutex<TranscriptionManager>>>() {
                                Some(tm) => tm,
                                None => {
                                    warn!("[Live Captions] TranscriptionManager not available for streaming callback — cannot transcribe");
                                    return;
                                }
                            };

                            // Transcribe the audio samples
                            let transcription_result = tm.lock().transcribe(samples);
                            match transcription_result {
                                Ok(mut result) if !result.text.is_empty() => {
                                    info!(
                                        "[Live Captions] Streaming transcription succeeded: text_len={}, segments={}",
                                        result.text.len(),
                                        result.segments.as_ref().map(|s| s.len()).unwrap_or(0)
                                    );
                                    // Check again after transcription in case it was cancelled mid-work
                                    // Using the Arc<AtomicBool> to avoid lock contention
                                    if cancel_flag.load(Ordering::Acquire) {
                                        debug!("Discarding streaming transcription result - cancelled");
                                        return;
                                    }

                                    // Apply text post-processing to live captions so they
                                    // benefit from the same pipeline as final transcriptions:
                                    // word corrections, filler removal, spelling conversion,
                                    // and repetition suppression.
                                    let settings = get_settings(&app_handle);
                                    let is_whisper = app_handle
                                        .try_state::<Arc<ModelManager>>()
                                        .map(|mm| {
                                            mm.get_model_info(&result.model_id)
                                                .map(|info| matches!(info.engine_type, EngineType::Whisper))
                                                .unwrap_or(false)
                                        })
                                        .unwrap_or(false);

                                    // Process each segment's text individually.
                                    // The top-level `result.text` is already processed by
                                    // TranscriptionManager::transcribe(), but segment texts
                                    // are raw from the engine. The frontend uses segment
                                    // texts for live caption display when available.
                                    if let Some(ref mut segments) = result.segments {
                                        for segment in segments.iter_mut() {
                                            if segment.text.is_empty() {
                                                continue;
                                            }
                                            segment.text = process_transcription_text(
                                                &segment.text,
                                                settings.word_correction_mode.clone(),
                                                &settings.custom_words,
                                                &settings.advanced_custom_words,
                                                &settings.word_replacements,
                                                settings.word_correction_threshold,
                                                is_whisper,
                                                &settings.app_language,
                                                &settings.custom_filler_words,
                                                settings.convert_us_to_british,
                                                settings.spelling_dictionary,
                                                settings.repetition_suppression_level,
                                            );
                                        }
                                    }

                                    // Emit partial transcription event with segments for frontend merge
                                    info!(
                                        "[Live Captions] Emitting partial-transcription event: text_len={}, segments={}",
                                        result.text.len(),
                                        result.segments.as_ref().map(|s| s.len()).unwrap_or(0)
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
                                        tm.lock().initiate_model_load();
                                    } else if matches!(e, AppError::TranscriptionBusy) {
                                        debug!("[Live Captions] Streaming transcription skipped (busy): {}", e);
                                    } else {
                                        warn!("[Live Captions] Streaming transcription failed: {}", e);
                                    }
                                }
                            }
                        });
                    }
                })
                .with_streaming_enabled(live_captions_enabled)
        }
        None => {
            warn!("[Live Captions] TranscriptionManager not available yet — streaming callback will not be attached; live captions will work after next recorder recreation");
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
    /// Background device hot-plug monitor thread handle
    device_monitor: Arc<Mutex<Option<std::thread::JoinHandle<()>>>>,
    /// Sender to signal the device monitor to stop
    device_monitor_stop: Arc<Mutex<Option<std::sync::mpsc::Sender<()>>>>,
    /// Previous input device names (for change detection)
    prev_input_devices: Arc<Mutex<Vec<String>>>,
    /// Previous output device names (for change detection)
    prev_output_devices: Arc<Mutex<Vec<String>>>,
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
        };

        // Check for Bluetooth output devices — if detected, keep the mic
        // stream alive permanently to prevent A2DP↔HFP profile switching
        // that causes audio dropouts on Bluetooth headphones.
        let bt_active = manager.is_bluetooth_output_active();
        if bt_active {
            info!("Bluetooth output device detected at startup — enabling mic stream keep-alive to prevent audio dropouts");
            manager.bt_keep_alive.store(true, Ordering::Release);
            manager.start_microphone_stream()?;
        } else if matches!(mode, MicrophoneMode::AlwaysOn) {
            manager.start_microphone_stream()?;
        }

        // Start background liveness monitor to detect zombie streams after sleep/wake
        manager.start_liveness_monitor();

        // Start background device hot-plug monitor
        manager.start_device_monitor();

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

    fn get_effective_microphone_device(&self, settings: &AppSettings) -> Option<cpal::Device> {
        // Check if we're in clamshell mode and have a clamshell microphone configured
        let use_clamshell_mic = if let Ok(is_clamshell) = clamshell::is_clamshell() {
            is_clamshell && settings.clamshell_microphone.is_some()
        } else {
            false
        };

        let device_name = if use_clamshell_mic {
            settings
                .clamshell_microphone
                .as_ref()
                .expect("clamshell microphone should exist when use_clamshell_mic is true")
        } else {
            settings.selected_microphone.as_ref()?
        };

        // Find the device by name
        match list_input_devices() {
            Ok(devices) => devices
                .into_iter()
                .find(|d| d.name == *device_name)
                .map(|d| d.device),
            Err(e) => {
                debug!("Failed to list devices, using default: {}", e);
                None
            }
        }
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
            // Hold state lock across the check AND close to serialize against
            // try_start_recording, preventing a race where the stream is closed
            // under an active recording.
            let state = rm.state.lock();
            // Never close the stream if BT keep-alive is active
            if bt_keep_alive.load(Ordering::Acquire) {
                debug!("Skipping lazy close: BT keep-alive is active");
                return;
            }
            if rm.close_generation.load(Ordering::SeqCst) == gen
                && matches!(*state, RecordingState::Idle)
            {
                // stop_microphone_stream does not acquire the state lock,
                // so holding it here is safe (no deadlock).
                info!(
                    "Closing idle microphone stream after {:?}",
                    STREAM_IDLE_TIMEOUT
                );
                rm.stop_microphone_stream();
            }
        });
    }

    /* ---------- microphone life-cycle -------------------------------------- */

    /// Applies mute if mute_while_recording is enabled and stream is open
    pub fn apply_mute(&self) {
        let settings = get_settings(&self.app_handle);

        if settings.mute_while_recording && self.is_open.load(Ordering::Acquire) {
            set_mute(true);
            self.did_mute.store(true, Ordering::Release);
            debug!("Mute applied");
        }
    }

    /// Removes mute if it was applied
    pub fn remove_mute(&self) {
        if self.did_mute.swap(false, Ordering::AcqRel) {
            set_mute(false);
            debug!("Mute removed");
        }
    }

    pub fn preload_vad(&self) -> Result<(), anyhow::Error> {
        let mut recorder_opt = self.recorder.lock();
        if recorder_opt.is_none() {
            let settings = get_settings(&self.app_handle);
            let vad_threshold = settings.vad_sensitivity.threshold();
            let vad_hangover_frames = settings.vad_sensitivity.hangover_frames();

            let vad_path = self
                .app_handle
                .path()
                .resolve(
                    "resources/models/silero_vad_v4.onnx",
                    tauri::path::BaseDirectory::Resource,
                )
                .map_err(|e| anyhow::anyhow!("Failed to resolve VAD path: {}", e))?;
            *recorder_opt = Some(create_audio_recorder(
                vad_path.to_str().expect("VAD path should be valid UTF-8"),
                &self.app_handle,
                vad_threshold,
                vad_hangover_frames,
            )?);
        }
        Ok(())
    }

    pub fn start_microphone_stream(&self) -> Result<(), anyhow::Error> {
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

        // Create a fresh recorder with VAD and level callback
        let vad_path = self
            .app_handle
            .path()
            .resolve(
                "resources/models/silero_vad_v4.onnx",
                tauri::path::BaseDirectory::Resource,
            )
            .map_err(|e| anyhow::anyhow!("Failed to resolve VAD path: {}", e))?;

        let new_recorder = create_audio_recorder(
            vad_path
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("VAD path is not valid UTF-8: {:?}", vad_path))?,
            &self.app_handle,
            vad_threshold,
            vad_hangover_frames,
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
        if self.is_open.load(Ordering::Acquire) {
            // Stream is already open, but check if the device has changed.
            // This can happen after USB cycling where the stream was opened
            // with a fallback device (e.g., webcam) and now the configured
            // device (e.g., RØDE mic) has returned.
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

            // Get the currently open device name
            let current_device_name = {
                let recorder_guard = self.recorder.lock();
                recorder_guard.as_ref().and_then(|r| r.device_name())
            };

            // If device has changed, close and reopen
            let device_changed = match (&configured_device_name, &current_device_name) {
                (Some(configured), Some(current)) => {
                    let changed = configured.as_str() != current.as_str();
                    if changed {
                        info!(
                            "Device mismatch detected in start_microphone_stream_inner: configured={:?}, current={:?}",
                            configured, current
                        );
                    }
                    changed
                }
                (Some(_), None) => true,  // Configured but no current device
                (None, Some(_)) => false, // No configured device, use whatever is open
                (None, None) => false,
            };

            if device_changed {
                info!(
                    "Device changed from {:?} to {:?}, reopening stream",
                    current_device_name, configured_device_name
                );
                self.stop_microphone_stream();
                // Recreate recorder to get fresh visualizer with correct sample rate
                self.recreate_recorder()?;
                // Re-check is_open after recreation
                if self.is_open.load(Ordering::Acquire) {
                    debug!("Microphone stream already active after device change");
                    return Ok(());
                }
            } else {
                debug!("Microphone stream already active");
                return Ok(());
            }
        }

        let start_time = Instant::now();

        // Don't mute immediately - caller will handle muting after audio feedback
        self.did_mute.store(false, Ordering::Release);

        // Get the selected device from settings, considering clamshell mode
        let settings = get_settings(&self.app_handle);
        let selected_device = self.get_effective_microphone_device(&settings);

        // Pre-flight check: if no device was selected/configured AND no devices
        // exist at all, fail early with a clear error instead of letting cpal
        // produce a cryptic backend-specific message.
        if selected_device.is_none() {
            let has_any_device = list_input_devices()
                .map(|devices| !devices.is_empty())
                .unwrap_or(false);
            if !has_any_device {
                return Err(anyhow::anyhow!("No input device found"));
            }
        }

        // Ensure VAD is loaded if it wasn't for whatever reason
        self.preload_vad()?;

        let mut recorder_opt = self.recorder.lock();
        if let Some(rec) = recorder_opt.as_mut() {
            rec.open(selected_device)
                .map_err(|e| anyhow::anyhow!("Failed to open recorder: {}", e))?;
        }

        self.is_open.store(true, Ordering::Release);
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
            set_mute(false);
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
        // Quick check under lock — just verify we're in Idle state.
        {
            let state = self.state.lock();
            if !matches!(*state, RecordingState::Idle) {
                return Err("Already recording".to_string());
            }
        }
        // State lock is released here. The actual state transition happens
        // below, after the potentially-slow liveness check.

        let bt_keep_alive = self.bt_keep_alive.load(Ordering::Acquire);
        let is_always_on = matches!(*self.mode.lock(), MicrophoneMode::AlwaysOn);

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

            // Allow the new stream to stabilize before recording
            std::thread::sleep(Duration::from_millis(200));

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
        if let RecordingState::Idle = *state {
            if let Some(rec) = self.recorder.lock().as_ref() {
                if rec.start().is_ok() {
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
        // If currently open, restart the microphone stream to use the new device
        if self.is_open.load(Ordering::Acquire) {
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

    pub fn stop_recording(&self, binding_id: &str) -> Option<Vec<f32>> {
        let state = self.state.lock();

        match *state {
            RecordingState::Recording {
                binding_id: ref active,
                start_time,
            } if active == binding_id => {
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

                let samples = if settings.extra_recording_buffer_ms > 0 {
                    debug!(
                        "Smart-stop: starting volume-aware buffer (max {}ms)",
                        settings.extra_recording_buffer_ms
                    );
                    // Phase 1: Send the smart_stop command while holding the lock.
                    // This is instantaneous — it just sends a message through the channel.
                    let resp_rx = {
                        let guard = self.recorder.lock();
                        match guard.as_ref() {
                            Some(rec) => rec
                                .smart_stop_send(settings.extra_recording_buffer_ms)
                                .map_err(|e| {
                                    error!("smart_stop_send() failed: {e}");
                                    e
                                })
                                .ok(),
                            None => {
                                error!("Recorder not available for smart_stop");
                                None
                            }
                        }
                    };
                    // Lock is now dropped. Other operations can proceed.

                    // Phase 2: Wait for the response without holding the lock.
                    match resp_rx {
                        Some(rx) => AudioRecorder::wait_for_smart_stop_result(
                            rx,
                            settings.extra_recording_buffer_ms,
                        )
                        .unwrap_or_else(|e| {
                            error!("smart_stop wait failed: {e}");
                            Vec::new()
                        }),
                        None => Vec::new(),
                    }
                } else {
                    // Phase 1: Send the stop command while holding the lock.
                    let resp_rx = {
                        let guard = self.recorder.lock();
                        match guard.as_ref() {
                            Some(rec) => rec
                                .stop_send()
                                .map_err(|e| {
                                    error!("stop_send() failed: {e}");
                                    e
                                })
                                .ok(),
                            None => {
                                error!("Recorder not available");
                                None
                            }
                        }
                    };
                    // Lock is now dropped.

                    // Phase 2: Wait for the response without holding the lock.
                    match resp_rx {
                        Some(rx) => AudioRecorder::wait_for_stop_result(rx).unwrap_or_else(|e| {
                            error!("stop wait failed: {e}");
                            Vec::new()
                        }),
                        None => Vec::new(),
                    }
                };

                // Now transition to Idle after the buffer is complete.
                {
                    let mut state = self.state.lock();
                    *state = RecordingState::Idle;
                }

                self.is_recording.store(false, Ordering::Release);

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

                if samples.len() > 0 && max_level < NO_AUDIO_THRESHOLD {
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
                        .on_recording_finished(samples.len(), recording_duration)
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
                // transcription without hallucinating. The VAD-based trim_trailing_silence
                // in the transcription pipeline further cleans up any trailing silence.
                let s_len = samples.len();
                let min_samples = WHISPER_SAMPLE_RATE * 3; // 3 seconds minimum
                if s_len > 0 && s_len < min_samples {
                    let needed_silence = min_samples - s_len;
                    let mut padded = vec![0.0f32; needed_silence];
                    padded.extend_from_slice(&samples);
                    Some(padded)
                } else {
                    Some(samples)
                }
            }
            _ => None,
        }
    }
    pub fn is_recording(&self) -> bool {
        matches!(*self.state.lock(), RecordingState::Recording { .. })
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
                warn!("[CANCEL-TIMING] Could not acquire state lock within 2s — forcing is_recording=false and proceeding");
                // Force the recording flag off even if we can't get the state lock,
                // so the app can recover. The recorder will stop on its own.
                self.is_recording.store(false, Ordering::Release);
                return;
            }
        };

        if let RecordingState::Recording { .. } = *state {
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
                        Some(rec) => rec.stop_send().ok(),
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
