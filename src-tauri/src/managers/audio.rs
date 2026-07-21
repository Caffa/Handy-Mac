use crate::audio_toolkit::{
    list_input_devices,
    vad::{
        SmoothedVad, VAD_OFFLINE_HANGOVER_FRAMES, VAD_ONSET_FRAMES, VAD_PREFILL_FRAMES,
        VAD_STREAMING_HANGOVER_FRAMES,
    },
    AudioRecorder, SileroVad, VadPolicy,
};
use crate::helpers::clamshell;
use crate::managers::transcription::StreamRouter;
use crate::settings::{get_settings, AppSettings};
use crate::usb_watchdog;
use crate::usb_watchdog::UsbWatchdog;
use crate::utils;
use log::{debug, error, info, warn};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::Manager;

const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const VAD_THRESHOLD: f32 = 0.3;

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

/// Pad short audio samples to ensure minimum length for Whisper.
///
/// If the sample count is between 1 and `WHISPER_SAMPLE_RATE` (1 second),
/// the samples are zero-padded to 1.25 seconds (WHISPER_SAMPLE_RATE * 5 / 4).
/// This gives Whisper enough context to produce reliable transcriptions from
/// very short recordings.
///
/// Returns `None` for empty input (caller should handle the "no audio" case),
/// and returns samples unchanged (no padding) if they are already >= 1 second.
fn pad_short_samples(samples: Vec<f32>) -> Option<Vec<f32>> {
    let len = samples.len();
    if len == 0 {
        None
    } else if len < WHISPER_SAMPLE_RATE {
        let mut padded = samples;
        padded.resize(WHISPER_SAMPLE_RATE * 5 / 4, 0.0);
        Some(padded)
    } else {
        Some(samples)
    }
}

/* ──────────────────────────────────────────────────────────────── */

#[derive(Clone, Debug, PartialEq)]
pub enum RecordingState {
    Idle,
    Recording { binding_id: String },
    Stopping,
}

#[derive(Clone, Debug, PartialEq)]
pub enum MicrophoneMode {
    AlwaysOn,
    OnDemand,
}

/* ──────────────────────────────────────────────────────────────── */

fn create_audio_recorder(
    vad_path: &Path,
    app_handle: &tauri::AppHandle,
    stream_router: Arc<StreamRouter>,
) -> Result<AudioRecorder, anyhow::Error> {
    // A single Silero engine covers both the offline and streaming policies (never
    // active at once within a recording), so the recorder reconfigures its
    // hangover tail per session rather than keeping two ONNX sessions resident.
    let silero = SileroVad::new(vad_path, VAD_THRESHOLD)
        .map_err(|e| anyhow::anyhow!("Failed to create SileroVad: {}", e))?;
    let smoothed_vad = SmoothedVad::new(
        Box::new(silero),
        VAD_PREFILL_FRAMES,
        VAD_OFFLINE_HANGOVER_FRAMES,
        VAD_ONSET_FRAMES,
    );

    // Recorder with VAD, a spectrum-level callback that forwards level updates to
    // the frontend, and an audio-frame callback that feeds live streaming via a
    // shared `StreamRouter` (captured directly, not via Tauri state — see its docs).
    let recorder = AudioRecorder::new()
        .map_err(|e| anyhow::anyhow!("Failed to create AudioRecorder: {}", e))?
        .with_vad(
            Box::new(smoothed_vad),
            VAD_OFFLINE_HANGOVER_FRAMES,
            VAD_STREAMING_HANGOVER_FRAMES,
        )
        .with_level_callback({
            let app_handle = app_handle.clone();
            move |levels| {
                utils::emit_levels(&app_handle, &levels);
            }
        })
        .with_audio_callback({
            let router = stream_router;
            move |frame| {
                router.feed(frame);
            }
        });

    Ok(recorder)
}

/* ──────────────────────────────────────────────────────────────── */

#[derive(Clone)]
pub struct AudioRecordingManager {
    state: Arc<Mutex<RecordingState>>,
    mode: Arc<Mutex<MicrophoneMode>>,
    app_handle: tauri::AppHandle,

    recorder: Arc<Mutex<Option<AudioRecorder>>>,
    is_open: Arc<Mutex<bool>>,
    is_recording: Arc<Mutex<bool>>,
    did_mute: Arc<Mutex<bool>>,
    close_generation: Arc<AtomicU64>,
    cancel_generation: Arc<AtomicU64>,
    stream_router: Arc<StreamRouter>,
    /// Resolution of a *named* microphone (selected or clamshell) to its cpal
    /// device, cached so on-demand recording starts skip the full device
    /// enumeration (~40-110ms). Keyed by the resolved name, so a settings
    /// change misses naturally; cleared when an open fails (device unplugged)
    /// so the retry re-enumerates. The system-default case is never cached —
    /// the recorder resolves the current default itself, cheaply.
    cached_device: Arc<Mutex<Option<(String, cpal::Device)>>>,
    /// USB watchdog for recovering dead USB audio devices via power cycling.
    pub usb_watchdog: Arc<UsbWatchdog>,
}

impl AudioRecordingManager {
    /* ---------- construction ------------------------------------------------ */

    pub fn new(
        app: &tauri::AppHandle,
        stream_router: Arc<StreamRouter>,
    ) -> Result<Self, anyhow::Error> {
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

        let manager = Self {
            state: Arc::new(Mutex::new(RecordingState::Idle)),
            mode: Arc::new(Mutex::new(mode.clone())),
            app_handle: app.clone(),

            recorder: Arc::new(Mutex::new(None)),
            is_open: Arc::new(Mutex::new(false)),
            is_recording: Arc::new(Mutex::new(false)),
            did_mute: Arc::new(Mutex::new(false)),
            close_generation: Arc::new(AtomicU64::new(0)),
            cancel_generation: Arc::new(AtomicU64::new(0)),
            stream_router,
            cached_device: Arc::new(Mutex::new(None)),
            usb_watchdog,
        };

        // Always-on?  Open immediately.
        if matches!(mode, MicrophoneMode::AlwaysOn) {
            manager.start_microphone_stream()?;
        }

        Ok(manager)
    }

    /* ---------- helper methods --------------------------------------------- */

    /// The microphone name the settings ask for, or `None` for the system
    /// default. Only runs the clamshell probe (an `ioreg` subprocess, ~10-20ms)
    /// when a clamshell microphone is actually configured.
    fn desired_device_name(&self, settings: &AppSettings) -> Option<String> {
        if settings.clamshell_microphone.is_some() {
            let clamshell_started = Instant::now();
            let is_clamshell = clamshell::is_clamshell().unwrap_or(false);
            debug!(
                "device resolve: clamshell_check={:?} (clamshell={})",
                clamshell_started.elapsed(),
                is_clamshell
            );
            if is_clamshell {
                return settings.clamshell_microphone.clone();
            }
        }
        settings.selected_microphone.clone()
    }

    pub fn invalidate_device_cache(&self) {
        *self.cached_device.lock().unwrap() = None;
    }

    fn get_effective_microphone_device(&self, settings: &AppSettings) -> Option<cpal::Device> {
        let device_name = match self.desired_device_name(settings) {
            Some(name) => name,
            None => {
                debug!("device resolve: no mic configured -> system default");
                return None;
            }
        };

        // Cache hit: skip the full enumeration. A stale device (unplugged)
        // fails at open, where the caller invalidates and retries fresh.
        if let Some((cached_name, device)) = self.cached_device.lock().unwrap().as_ref() {
            if *cached_name == device_name {
                debug!("device resolve: cache hit for '{}'", device_name);
                return Some(device.clone());
            }
        }

        // Find the device by name
        let enumerate_started = Instant::now();
        let device = match list_input_devices() {
            Ok(devices) => devices
                .into_iter()
                .find(|d| d.name == device_name)
                .map(|d| d.device),
            Err(e) => {
                debug!("Failed to list devices, using default: {}", e);
                None
            }
        };
        debug!(
            "device resolve: enumerate={:?} (found={})",
            enumerate_started.elapsed(),
            device.is_some()
        );
        if let Some(d) = &device {
            *self.cached_device.lock().unwrap() = Some((device_name, d.clone()));
        }
        device
    }

    fn schedule_lazy_close(&self) {
        let gen = self.close_generation.fetch_add(1, Ordering::SeqCst) + 1;
        let app = self.app_handle.clone();
        std::thread::spawn(move || {
            std::thread::sleep(STREAM_IDLE_TIMEOUT);
            let rm = app.state::<Arc<AudioRecordingManager>>();
            // Hold state lock across the check AND close to serialize against
            // try_start_recording, preventing a race where the stream is closed
            // under an active recording.
            let state = rm.state.lock().unwrap();
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
        let mut did_mute_guard = self.did_mute.lock().unwrap();

        if settings.mute_while_recording && *self.is_open.lock().unwrap() {
            set_mute(true);
            *did_mute_guard = true;
            debug!("Mute applied");
        }
    }

    /// Removes mute if it was applied
    pub fn remove_mute(&self) {
        let mut did_mute_guard = self.did_mute.lock().unwrap();
        if *did_mute_guard {
            set_mute(false);
            *did_mute_guard = false;
            debug!("Mute removed");
        }
    }

    pub fn preload_vad(&self) -> Result<(), anyhow::Error> {
        let mut recorder_opt = self.recorder.lock().unwrap();
        if recorder_opt.is_none() {
            let vad_path = self
                .app_handle
                .path()
                .resolve(
                    "resources/models/silero_vad_v4.onnx",
                    tauri::path::BaseDirectory::Resource,
                )
                .map_err(|e| anyhow::anyhow!("Failed to resolve VAD path: {}", e))?;
            *recorder_opt = Some(create_audio_recorder(
                &vad_path,
                &self.app_handle,
                Arc::clone(&self.stream_router),
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
            Ok(()) => Ok(()),
            Err(e) => {
                if self.usb_watchdog.on_mic_open_failed() {
                    // Watchdog completed a power cycle (blocking). Retry the mic
                    // open now that the device should have re-enumerated.
                    warn!("Mic open failed ({}), USB watchdog cycled - retrying", e);
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

    /// Inner implementation of start_microphone_stream — opens the mic stream
    /// without any USB watchdog retry logic.
    fn start_microphone_stream_inner(&self) -> Result<(), anyhow::Error> {
        let mut open_flag = self.is_open.lock().unwrap();
        if *open_flag {
            debug!("Microphone stream already active");
            return Ok(());
        }

        let start_time = Instant::now();

        // Don't mute immediately - caller will handle muting after audio feedback
        let mut did_mute_guard = self.did_mute.lock().unwrap();
        *did_mute_guard = false;

        // Get the selected device from settings, considering clamshell mode.
        // No pre-flight enumeration here: when nothing is configured the
        // recorder resolves the system default itself, and a machine with no
        // input devices at all fails inside open() with the same
        // "No input device found" error this used to check for.
        let settings = get_settings(&self.app_handle);
        let resolve_started = Instant::now();
        let selected_device = self.get_effective_microphone_device(&settings);
        let resolve_elapsed = resolve_started.elapsed();

        // Ensure VAD is loaded if it wasn't for whatever reason
        let vad_started = Instant::now();
        self.preload_vad()?;
        let vad_elapsed = vad_started.elapsed();

        let open_started = Instant::now();
        let mut recorder_opt = self.recorder.lock().unwrap();
        if let Some(rec) = recorder_opt.as_mut() {
            if let Err(first_err) = rec.open(selected_device.clone()) {
                // A cached device or config may have gone stale (unplugged,
                // rate/format changed). Re-resolve from a fresh enumeration and
                // retry once before surfacing the error.
                warn!("Recorder open failed ({first_err}); re-resolving device and retrying once");
                self.invalidate_device_cache();
                let fresh_device = self.get_effective_microphone_device(&settings);
                rec.open(fresh_device)
                    .map_err(|e| anyhow::anyhow!("Failed to open recorder: {}", e))?;
            }
        }
        debug!(
            "mic stream breakdown: device_resolve={:?} vad_ensure={:?} open={:?}",
            resolve_elapsed,
            vad_elapsed,
            open_started.elapsed()
        );

        *open_flag = true;
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
        let mut open_flag = self.is_open.lock().unwrap();
        if !*open_flag {
            return;
        }

        let mut did_mute_guard = self.did_mute.lock().unwrap();
        if *did_mute_guard {
            set_mute(false);
        }
        *did_mute_guard = false;

        if let Some(rec) = self.recorder.lock().unwrap().as_mut() {
            // If still recording, stop first.
            if *self.is_recording.lock().unwrap() {
                let _ = rec.stop();
                *self.is_recording.lock().unwrap() = false;
            }
            let _ = rec.close();
        }

        *open_flag = false;
        debug!("Microphone stream stopped");
    }

    /* ---------- mode switching --------------------------------------------- */

    pub fn update_mode(&self, new_mode: MicrophoneMode) -> Result<(), anyhow::Error> {
        let cur_mode = self.mode.lock().unwrap().clone();

        match (cur_mode, &new_mode) {
            (MicrophoneMode::AlwaysOn, MicrophoneMode::OnDemand) => {
                if matches!(*self.state.lock().unwrap(), RecordingState::Idle) {
                    self.close_generation.fetch_add(1, Ordering::SeqCst);
                    self.stop_microphone_stream();
                }
            }
            (MicrophoneMode::OnDemand, MicrophoneMode::AlwaysOn) => {
                self.close_generation.fetch_add(1, Ordering::SeqCst);
                self.start_microphone_stream()?;
            }
            _ => {}
        }

        *self.mode.lock().unwrap() = new_mode;
        Ok(())
    }

    /* ---------- recording --------------------------------------------------- */

    pub fn try_start_recording(
        &self,
        binding_id: &str,
        vad_policy: VadPolicy,
    ) -> Result<(), String> {
        let mut state = self.state.lock().unwrap();

        if let RecordingState::Idle = *state {
            // Ensure microphone is open in on-demand mode
            if matches!(*self.mode.lock().unwrap(), MicrophoneMode::OnDemand) {
                // Cancel any pending lazy close
                self.close_generation.fetch_add(1, Ordering::SeqCst);
                if let Err(e) = self.start_microphone_stream() {
                    let msg = format!("{e}");
                    error!("Failed to open microphone stream: {msg}");
                    return Err(msg);
                }
            }

            if let Some(rec) = self.recorder.lock().unwrap().as_ref() {
                if rec.start(vad_policy).is_ok() {
                    *self.is_recording.lock().unwrap() = true;
                    *state = RecordingState::Recording {
                        binding_id: binding_id.to_string(),
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
        // Device settings changed; drop the cached resolution so the next
        // open re-enumerates. (The name-keyed cache would miss anyway; this
        // just avoids holding a stale cpal::Device alive.)
        self.invalidate_device_cache();
        // If currently open, restart the microphone stream to use the new device
        if *self.is_open.lock().unwrap() {
            self.close_generation.fetch_add(1, Ordering::SeqCst);
            self.stop_microphone_stream();
            self.start_microphone_stream()?;
        }
        Ok(())
    }

    pub fn cancel_generation(&self) -> u64 {
        self.cancel_generation.load(Ordering::Acquire)
    }

    pub fn was_cancelled_since(&self, generation: u64) -> bool {
        self.cancel_generation.load(Ordering::Acquire) != generation
    }

    pub fn stop_recording(&self, binding_id: &str, cancel_generation: u64) -> Option<Vec<f32>> {
        let mut state = self.state.lock().unwrap();

        match *state {
            RecordingState::Recording {
                binding_id: ref active,
            } if active == binding_id => {
                *state = RecordingState::Stopping;
                drop(state);

                // Optionally keep recording for a bit longer to capture trailing audio.
                // This is only the explicit user setting; streaming VAD must not add
                // hidden post-release capture time.
                let settings = get_settings(&self.app_handle);
                let buffer_ms = settings.extra_recording_buffer_ms;
                if buffer_ms > 0 {
                    debug!(
                        "Extra recording buffer: sleeping {}ms before stopping",
                        buffer_ms
                    );
                    let started = Instant::now();
                    let buffer = Duration::from_millis(buffer_ms);
                    while started.elapsed() < buffer {
                        if self.was_cancelled_since(cancel_generation) {
                            debug!("Recording stop cancelled during extra buffer");
                            break;
                        }
                        let remaining = buffer.saturating_sub(started.elapsed());
                        std::thread::sleep(remaining.min(Duration::from_millis(25)));
                    }
                }

                let samples = if let Some(rec) = self.recorder.lock().unwrap().as_ref() {
                    match rec.stop() {
                        Ok(buf) => buf,
                        Err(e) => {
                            error!("stop() failed: {e}");
                            Vec::new()
                        }
                    }
                } else {
                    error!("Recorder not available");
                    Vec::new()
                };

                *self.is_recording.lock().unwrap() = false;
                *self.state.lock().unwrap() = RecordingState::Idle;

                // In on-demand mode, close the mic (lazily if the setting is enabled)
                if matches!(*self.mode.lock().unwrap(), MicrophoneMode::OnDemand) {
                    if get_settings(&self.app_handle).lazy_stream_close {
                        self.schedule_lazy_close();
                    } else {
                        self.stop_microphone_stream();
                    }
                }

                if self.was_cancelled_since(cancel_generation) {
                    debug!("Recording stop cancelled; discarding captured samples");
                    return None;
                }

                // Pad if very short; return None for empty (cancelled/no audio)
                pad_short_samples(samples)
            }
            _ => None,
        }
    }
    pub fn is_recording(&self) -> bool {
        matches!(
            *self.state.lock().unwrap(),
            RecordingState::Recording { .. } | RecordingState::Stopping
        )
    }

    /// Cancel any ongoing recording without returning audio samples
    pub fn cancel_recording(&self) {
        self.cancel_generation.fetch_add(1, Ordering::AcqRel);
        let mut state = self.state.lock().unwrap();

        match *state {
            RecordingState::Recording { .. } => {
                *state = RecordingState::Idle;
                drop(state);

                if let Some(rec) = self.recorder.lock().unwrap().as_ref() {
                    let _ = rec.stop(); // Discard the result
                }

                *self.is_recording.lock().unwrap() = false;

                // In on-demand mode, close the mic (lazily if the setting is enabled)
                if matches!(*self.mode.lock().unwrap(), MicrophoneMode::OnDemand) {
                    if get_settings(&self.app_handle).lazy_stream_close {
                        self.schedule_lazy_close();
                    } else {
                        self.stop_microphone_stream();
                    }
                }
            }
            RecordingState::Stopping => {
                debug!("Cancellation requested while recording is stopping");
            }
            RecordingState::Idle => {}
        }
    }

    /// Restart the microphone stream if it should be active.
    /// Called after USB power cycling completes to fix the "mic not listening,
    /// volume bars not moving" issue.
    /// Returns Ok(()) if the stream was restarted or wasn't needed, Err if restart failed.
    pub fn restart_microphone_if_needed(&self) -> Result<(), anyhow::Error> {
        let is_always_on = matches!(*self.mode.lock().unwrap(), MicrophoneMode::AlwaysOn);

        if is_always_on {
            info!("Restarting microphone stream after USB power cycle");
            // Stop the current stream if open
            if *self.is_open.lock().unwrap() {
                self.stop_microphone_stream();
            }
            // Start a fresh stream
            self.start_microphone_stream()
        } else {
            debug!("Microphone stream not needed (not always-on)");
            Ok(())
        }
    }

    /// Check if the microphone stream is currently open and alive.
    /// Returns (is_open, is_alive).
    /// This is useful for diagnostics and recovery checks (e.g. after
    /// macOS sleep/wake).
    ///
    /// Note: Without a liveness probe on AudioRecorder, `is_alive` currently
    /// mirrors `is_open`. A full liveness check will be added in a later PR.
    pub fn check_stream_health(&self) -> (bool, bool) {
        let is_open = *self.is_open.lock().unwrap();
        (is_open, is_open)
    }

    /// Check if the microphone stream is currently open.
    /// Used by shortcut setter commands to decide whether to recreate the
    /// recorder after a settings change (e.g. noise suppression, VAD sensitivity).
    pub fn is_stream_open(&self) -> bool {
        *self.is_open.lock().unwrap()
    }

    /// Recreate the `AudioRecorder` from scratch, discarding stale device
    /// handles. If the stream was running in always-on mode, it is restarted
    /// automatically (RAII self-heal).
    ///
    /// This is called by shortcut setter commands when a setting that affects
    /// the recorder (VAD sensitivity, noise suppression, etc.) changes at
    /// runtime, so the new value takes effect immediately without requiring a
    /// full app restart.
    pub fn recreate_recorder(&self) -> Result<(), anyhow::Error> {
        info!("Recreating AudioRecorder to discard stale device handles");

        // Capture whether the stream should be running before we tear it down.
        let was_open = *self.is_open.lock().unwrap();
        let should_be_running =
            was_open && matches!(*self.mode.lock().unwrap(), MicrophoneMode::AlwaysOn);

        // Mark the stream as closed before tearing down — prevents concurrent
        // operations from acting on a recorder that is about to be replaced.
        if was_open {
            *self.is_open.lock().unwrap() = false;
        }

        // Take the old recorder and drop it (this stops any existing stream)
        let mut recorder_opt = self.recorder.lock().unwrap();
        if recorder_opt.is_some() {
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

        // Create a fresh recorder with VAD, level callback, and audio callback.
        let vad_path = self
            .app_handle
            .path()
            .resolve(
                "resources/models/silero_vad_v4.onnx",
                tauri::path::BaseDirectory::Resource,
            )
            .map_err(|e| anyhow::anyhow!("Failed to resolve VAD path: {}", e))?;

        let new_recorder =
            create_audio_recorder(&vad_path, &self.app_handle, Arc::clone(&self.stream_router))?;

        *recorder_opt = Some(new_recorder);
        drop(recorder_opt);

        // RAII self-healing: if the stream was open and should still be
        // running (always-on mode), restart it automatically.
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

    /// Toggle live-streaming (partial-transcription) mode on the recorder.
    ///
    /// The streaming callback is set on the `AudioRecorder` at creation time,
    /// so toggling it requires recreating the recorder. This method delegates
    /// to `recreate_recorder()` which handles the RAII pattern (restarting the
    /// stream if it was open before recreation).
    pub fn set_streaming_enabled(&self, _enabled: bool) -> Result<(), anyhow::Error> {
        info!("[Live Captions] Toggling streaming mode via recorder recreation");
        self.recreate_recorder()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, AtomicBool, Ordering};

    // ─── RecordingState tests ───────────────────────────────────────────

    #[test]
    fn recording_state_idle_clone_debug() {
        let state = RecordingState::Idle;
        let cloned = state.clone();
        assert_eq!(format!("{:?}", cloned), "Idle");
    }

    #[test]
    fn recording_state_recording_clone_debug() {
        let state = RecordingState::Recording {
            binding_id: "test-binding-1".to_string(),
        };
        let cloned = state.clone();
        assert!(matches!(cloned, RecordingState::Recording { .. }));
        // Debug output includes the binding_id
        let debug_str = format!("{:?}", cloned);
        assert!(debug_str.contains("test-binding-1"));
    }

    #[test]
    fn recording_state_stopping_clone_debug() {
        let state = RecordingState::Stopping;
        let cloned = state.clone();
        assert_eq!(format!("{:?}", cloned), "Stopping");
    }

    #[test]
    fn recording_state_pattern_matching() {
        // Verify pattern matching works correctly for each variant
        let idle = RecordingState::Idle;
        let recording = RecordingState::Recording {
            binding_id: "abc".to_string(),
        };
        let stopping = RecordingState::Stopping;

        assert!(matches!(idle, RecordingState::Idle));
        assert!(matches!(recording, RecordingState::Recording { .. }));
        assert!(matches!(stopping, RecordingState::Stopping));

        // Verify that Idle and Stopping don't match Recording
        assert!(!matches!(idle, RecordingState::Recording { .. }));
        assert!(!matches!(stopping, RecordingState::Recording { .. }));
    }

    #[test]
    fn recording_state_equality() {
        // Recording { binding_id } with same id should be equal
        let a = RecordingState::Recording {
            binding_id: "x".to_string(),
        };
        let b = RecordingState::Recording {
            binding_id: "x".to_string(),
        };
        assert_eq!(a, b);

        // Different binding_id should not be equal
        let c = RecordingState::Recording {
            binding_id: "y".to_string(),
        };
        assert_ne!(a, c);

        // Different variants should not be equal
        let idle = RecordingState::Idle;
        assert_ne!(a, idle);
    }

    // ─── MicrophoneMode tests ───────────────────────────────────────────

    #[test]
    fn microphone_mode_variants() {
        let always_on = MicrophoneMode::AlwaysOn;
        let on_demand = MicrophoneMode::OnDemand;

        assert!(matches!(always_on, MicrophoneMode::AlwaysOn));
        assert!(matches!(on_demand, MicrophoneMode::OnDemand));
        assert_ne!(always_on, on_demand);
    }

    #[test]
    fn microphone_mode_clone_debug() {
        let mode = MicrophoneMode::AlwaysOn;
        let cloned = mode.clone();
        assert_eq!(format!("{:?}", cloned), "AlwaysOn");

        let mode = MicrophoneMode::OnDemand;
        let cloned = mode.clone();
        assert_eq!(format!("{:?}", cloned), "OnDemand");
    }

    // ─── Constants tests ─────────────────────────────────────────────────

    #[test]
    fn whisper_sample_rate_value() {
        assert_eq!(WHISPER_SAMPLE_RATE, 16000);
    }

    #[test]
    fn stream_idle_timeout_value() {
        assert_eq!(STREAM_IDLE_TIMEOUT, Duration::from_secs(30));
    }

    #[test]
    fn vad_threshold_value() {
        assert!((VAD_THRESHOLD - 0.3f32).abs() < f32::EPSILON);
    }

    // ─── pad_short_samples tests (Bug #4 — pre-recording buffer crash) ─

    #[test]
    fn pad_short_samples_empty_returns_none() {
        let result = pad_short_samples(vec![]);
        assert!(result.is_none(), "Empty samples should return None");
    }

    #[test]
    fn pad_short_samples_single_sample_pads() {
        let result = pad_short_samples(vec![0.5]);
        assert!(result.is_some());
        let padded = result.unwrap();
        // Should be padded to WHISPER_SAMPLE_RATE * 5 / 4 = 20000
        assert_eq!(padded.len(), WHISPER_SAMPLE_RATE * 5 / 4);
        // First sample preserved
        assert_eq!(padded[0], 0.5);
        // Remaining samples are zero
        assert!(padded[1..].iter().all(|&s| s == 0.0));
    }

    #[test]
    fn pad_short_samples_short_audio_pads() {
        // 100 samples is less than WHISPER_SAMPLE_RATE (16000)
        let samples: Vec<f32> = (0..100).map(|i| i as f32 / 100.0).collect();
        let result = pad_short_samples(samples.clone());
        assert!(result.is_some());
        let padded = result.unwrap();
        assert_eq!(padded.len(), WHISPER_SAMPLE_RATE * 5 / 4);
        // Original samples preserved at the beginning
        assert_eq!(padded[0], 0.0);
        assert!((padded[99] - 0.99).abs() < f32::EPSILON);
        // Zero-padded region starts at original length
        assert_eq!(padded[100], 0.0);
    }

    #[test]
    fn pad_short_samples_exact_threshold_no_pad() {
        // Exactly WHISPER_SAMPLE_RATE samples — should NOT be padded
        let samples: Vec<f32> = vec![0.5; WHISPER_SAMPLE_RATE];
        let result = pad_short_samples(samples.clone());
        assert!(result.is_some());
        assert_eq!(result.unwrap().len(), WHISPER_SAMPLE_RATE);
    }

    #[test]
    fn pad_short_samples_above_threshold_no_pad() {
        // More than WHISPER_SAMPLE_RATE samples — should NOT be padded
        let samples: Vec<f32> = vec![0.25; WHISPER_SAMPLE_RATE + 1000];
        let result = pad_short_samples(samples.clone());
        assert!(result.is_some());
        assert_eq!(result.unwrap().len(), WHISPER_SAMPLE_RATE + 1000);
    }

    #[test]
    fn pad_short_samples_preserves_original_data() {
        // Original samples must be preserved exactly, padding must be zeros
        let original: Vec<f32> = vec![1.0, -0.5, 0.25, -0.125, 0.0625];
        let result = pad_short_samples(original.clone());
        assert!(result.is_some());
        let padded = result.unwrap();
        for (i, &val) in original.iter().enumerate() {
            assert!(
                (padded[i] - val).abs() < f32::EPSILON,
                "Original sample {} corrupted: expected {}, got {}",
                i,
                val,
                padded[i]
            );
        }
        // Padded region should be all zeros
        for (i, &val) in padded.iter().enumerate().skip(original.len()) {
            assert_eq!(val, 0.0, "Padded sample {} should be 0.0, got {}", i, val);
        }
    }

    #[test]
    fn pad_short_samples_padded_length_calculation() {
        // Verify the padded length is exactly WHISPER_SAMPLE_RATE * 5 / 4
        let expected_padded_len = WHISPER_SAMPLE_RATE * 5 / 4; // = 20000
        let result = pad_short_samples(vec![0.0; 100]);
        assert_eq!(result.unwrap().len(), expected_padded_len);
    }

    #[test]
    fn pad_short_samples_just_below_threshold() {
        // One sample below the threshold should still be padded
        let samples = vec![0.5; WHISPER_SAMPLE_RATE - 1];
        let result = pad_short_samples(samples);
        assert!(result.is_some());
        assert_eq!(result.unwrap().len(), WHISPER_SAMPLE_RATE * 5 / 4);
    }

    #[test]
    fn pad_short_samples_just_above_threshold() {
        // One sample above the threshold should NOT be padded
        let samples = vec![0.5; WHISPER_SAMPLE_RATE + 1];
        let result = pad_short_samples(samples);
        assert!(result.is_some());
        assert_eq!(result.unwrap().len(), WHISPER_SAMPLE_RATE + 1);
    }

    // ─── AtomicU64 generation pattern tests (Bug #9 — mutex poisoning,
    //     Bug #10 — state not resetting) ─────────────────────────────────
    //
    // These test the AtomicU64 patterns used for cancel_generation and
    // close_generation, which provide lock-free cancellation. They verify
    // the exact ordering semantics used in production code.

    #[test]
    fn cancel_generation_monotonic_increment() {
        // cancel_generation uses fetch_add(1, AcqRel) — must be monotonic
        let gen = AtomicU64::new(0);
        let v1 = gen.fetch_add(1, Ordering::AcqRel);
        let v2 = gen.fetch_add(1, Ordering::AcqRel);
        let v3 = gen.fetch_add(1, Ordering::AcqRel);
        assert_eq!(v1, 0, "First fetch_add should return initial value");
        assert_eq!(v2, 1);
        assert_eq!(v3, 2);
        assert_eq!(gen.load(Ordering::Acquire), 3);
    }

    #[test]
    fn was_cancelled_since_correctness() {
        // was_cancelled_since checks if the current generation differs from
        // a snapshot. This is the lock-free cancellation pattern used in
        // stop_recording. Replicated here as a free function since the
        // method requires &self (AudioRecordingManager).
        fn was_cancelled_since(gen: &AtomicU64, snapshot: u64) -> bool {
            gen.load(Ordering::Acquire) != snapshot
        }

        let gen = AtomicU64::new(42);

        // Not cancelled if generation hasn't changed
        assert!(
            !was_cancelled_since(&gen, 42),
            "Should not be cancelled if generation matches"
        );

        // Cancelled if generation has been incremented
        gen.fetch_add(1, Ordering::AcqRel);
        assert!(
            was_cancelled_since(&gen, 42),
            "Should be cancelled after generation increments"
        );

        // Not cancelled against the new generation
        assert!(
            !was_cancelled_since(&gen, 43),
            "Should not be cancelled with updated snapshot"
        );
    }

    #[test]
    fn cancel_generation_thread_safety() {
        // Multiple threads incrementing cancel_generation must produce
        // a monotonically increasing final value with no lost updates.
        use std::sync::Arc;
        use std::thread;

        let gen = Arc::new(AtomicU64::new(0));
        let threads = 8;
        let increments_per_thread = 1000u64;
        let mut handles = vec![];

        for _ in 0..threads {
            let gen = Arc::clone(&gen);
            handles.push(thread::spawn(move || {
                for _ in 0..increments_per_thread {
                    gen.fetch_add(1, Ordering::AcqRel);
                }
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }

        let expected = threads * increments_per_thread as usize;
        assert_eq!(
            gen.load(Ordering::Acquire) as usize,
            expected,
            "All increments should be accounted for"
        );
    }

    #[test]
    fn close_generation_monotonic_increment() {
        // close_generation uses fetch_add(1, SeqCst) — must be monotonic
        let gen = AtomicU64::new(0);
        let v1 = gen.fetch_add(1, Ordering::SeqCst);
        let v2 = gen.fetch_add(1, Ordering::SeqCst);
        assert_eq!(v1, 0);
        assert_eq!(v2, 1);
        assert_eq!(gen.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn generation_snapshot_prevents_stale_stop() {
        // Simulates the pattern in stop_recording:
        // 1. Caller snapshots cancel_generation before starting
        // 2. A cancel happens (generation increments)
        // 3. After work, caller checks was_cancelled_since(snapshot)
        //    to decide whether to discard the result.
        fn was_cancelled_since(gen: &AtomicU64, snapshot: u64) -> bool {
            gen.load(Ordering::Acquire) != snapshot
        }

        let gen = AtomicU64::new(0);

        // Step 1: Snapshot before any cancel
        let snapshot = gen.load(Ordering::Acquire);

        // Step 2: Cancel happens (e.g., cancel_recording increments)
        gen.fetch_add(1, Ordering::AcqRel);

        // Step 3: Check — should detect the cancel
        assert!(
            was_cancelled_since(&gen, snapshot),
            "Should detect cancel after generation increment"
        );
    }

    // ─── RecordingState state machine invariants ────────────────────────
    //
    // These test the state machine invariants that are documented in
    // the code but not enforced by the type system. They serve as
    // regression guards for Bug #10 (state not resetting).

    #[test]
    fn recording_state_transitions_are_exhaustive() {
        // Verify that all RecordingState variants are handled in a match
        let states: Vec<RecordingState> = vec![
            RecordingState::Idle,
            RecordingState::Recording {
                binding_id: "test".to_string(),
            },
            RecordingState::Stopping,
        ];

        for state in states {
            // Every state should be classifiable
            let is_idle = matches!(state, RecordingState::Idle);
            let is_recording = matches!(state, RecordingState::Recording { .. });
            let is_stopping = matches!(state, RecordingState::Stopping);
            assert!(
                is_idle || is_recording || is_stopping,
                "State should match exactly one variant"
            );

            // Exactly one should be true
            let count = [is_idle, is_recording, is_stopping]
                .iter()
                .filter(|&&b| b)
                .count();
            assert_eq!(count, 1, "State should match exactly one variant");
        }
    }

    #[test]
    fn recording_state_default_is_idle() {
        // The initial state in AudioRecordingManager::new is Idle.
        // This test documents that invariant.
        let state = RecordingState::Idle;
        assert!(matches!(state, RecordingState::Idle));
    }

    #[test]
    fn microphone_mode_default_matches_setting() {
        // In production, the default mode depends on the `always_on_microphone`
        // setting. This test documents the two possible modes.
        let modes = [MicrophoneMode::AlwaysOn, MicrophoneMode::OnDemand];
        assert_eq!(modes.len(), 2);
        assert_ne!(modes[0], modes[1]);
    }

    // ─── AtomicBool streaming state pattern tests ───────────────────────
    //
    // These test the AtomicBool patterns used for is_streaming and
    // streaming_cancel_flag in the broader audio system. Although these
    // fields are on AudioRecorder (not AudioRecordingManager), the
    // pattern is the same and worth testing here as documentation.

    #[test]
    fn atomic_bool_set_check_clear() {
        let flag = AtomicBool::new(false);
        assert!(!flag.load(Ordering::Acquire));

        flag.store(true, Ordering::Release);
        assert!(flag.load(Ordering::Acquire));

        flag.store(false, Ordering::Release);
        assert!(!flag.load(Ordering::Acquire));
    }

    #[test]
    fn atomic_bool_swap_returns_old() {
        let flag = AtomicBool::new(false);
        let old = flag.swap(true, Ordering::AcqRel);
        assert!(!old, "swap should return the previous value");
        assert!(flag.load(Ordering::Acquire));
    }

    #[test]
    fn atomic_bool_compare_exchange() {
        let flag = AtomicBool::new(false);
        let result = flag.compare_exchange(
            false, // expected
            true,  // new value
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        assert!(result.is_ok());
        assert!(flag.load(Ordering::Acquire));

        // Second CAS should fail since value is now true
        let result = flag.compare_exchange(
            false, // expected (won't match)
            true,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        assert!(result.is_err());
    }

    // ─── Duration/Instant tests for STREAM_IDLE_TIMEOUT ─────────────────

    #[test]
    fn stream_idle_timeout_is_30_seconds() {
        assert_eq!(STREAM_IDLE_TIMEOUT, Duration::from_secs(30));
        // Verify the timeout is reasonable (between 10s and 5min)
        assert!(STREAM_IDLE_TIMEOUT >= Duration::from_secs(10));
        assert!(STREAM_IDLE_TIMEOUT <= Duration::from_secs(300));
    }

    #[test]
    fn stop_recording_buffer_duration_calculation() {
        // Verify the buffer duration math used in stop_recording:
        // `Duration::from_millis(buffer_ms)` and the sleep loop
        // `remaining.min(Duration::from_millis(25))`
        let buffer_ms = 500u64;
        let buffer = Duration::from_millis(buffer_ms);
        assert_eq!(buffer, Duration::from_millis(500));

        // The min(Duration::from_millis(25)) caps sleep increments
        let remaining = Duration::from_millis(100);
        let sleep_increment = remaining.min(Duration::from_millis(25));
        assert_eq!(sleep_increment, Duration::from_millis(25));

        let remaining_small = Duration::from_millis(10);
        let sleep_increment_small = remaining_small.min(Duration::from_millis(25));
        assert_eq!(sleep_increment_small, Duration::from_millis(10));
    }
}
