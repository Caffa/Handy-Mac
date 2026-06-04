use crate::audio_toolkit::{
    is_bluetooth_audio_active, list_input_devices, vad::SmoothedVad, AudioRecorder, SileroVad,
};
use crate::helpers::clamshell;
use crate::portable;
use crate::settings::{get_settings, AppSettings};
use crate::usb_watchdog;
use crate::usb_watchdog::UsbWatchdog;
use crate::utils;
use log::{debug, error, info, warn};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tauri::Manager;

/// Helper to lock a mutex with error logging instead of panic.
/// Returns the guard, or logs an error and recovers from poisoned state.
fn lock_with_log<'a, T>(mutex: &'a Mutex<T>, name: &str) -> MutexGuard<'a, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            error!("Mutex '{}' was poisoned: {:?}", name, poisoned);
            warn!(
                "Recovering from poisoned mutex '{}' - data may be inconsistent",
                name
            );
            poisoned.into_inner()
        }
    }
}

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

/* ──────────────────────────────────────────────────────────────── */

#[derive(Clone, Debug)]
pub enum RecordingState {
    Idle,
    Recording { binding_id: String },
}

#[derive(Clone, Debug)]
pub enum MicrophoneMode {
    AlwaysOn,
    OnDemand,
}

/* ──────────────────────────────────────────────────────────────── */

fn create_audio_recorder(
    vad_path: &str,
    app_handle: &tauri::AppHandle,
) -> Result<AudioRecorder, anyhow::Error> {
    let silero = SileroVad::new(vad_path, 0.3)
        .map_err(|e| anyhow::anyhow!("Failed to create SileroVad: {}", e))?;
    let smoothed_vad = SmoothedVad::new(Box::new(silero), 15, 15, 2);

    // Recorder with VAD plus a spectrum-level callback that forwards updates to
    // the frontend.
    let recorder = AudioRecorder::new()
        .map_err(|e| anyhow::anyhow!("Failed to create AudioRecorder: {}", e))?
        .with_vad(Box::new(smoothed_vad))
        .with_level_callback({
            let app_handle = app_handle.clone();
            move |levels| {
                utils::emit_levels(&app_handle, &levels);
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
    pub usb_watchdog: Arc<UsbWatchdog>,
    /// When a Bluetooth output device is detected, we keep the mic stream
    /// alive permanently (like always-on mode) regardless of the user's
    /// microphone mode setting. This prevents macOS from repeatedly
    /// switching the BT headset between A2DP (stereo) and HFP/SCO (mono)
    /// profiles every time the stream opens/closes, which causes audio
    /// dropouts on the headphones.
    bt_keep_alive: Arc<Mutex<bool>>,
    /// Queue of pronunciation recordings awaiting idle-time processing.
    /// Each entry is `(audio_samples, canonical_word, recording_id, file_path)`.
    /// New recordings are pushed to the back; the worker pops from the front.
    pub pending_pronunciation: Arc<Mutex<VecDeque<(Vec<f32>, String, String, String)>>>,
    /// Handle to the background processing thread, if any.
    pub pronunciation_thread: Arc<Mutex<Option<std::thread::JoinHandle<()>>>>,
    /// Directory where pronunciation WAV files are saved.
    pub pronunciation_recordings_dir: PathBuf,
    /// Background liveness monitor thread handle
    liveness_monitor: Arc<Mutex<Option<std::thread::JoinHandle<()>>>>,
    /// Flag to signal liveness monitor to stop
    liveness_stop: Arc<AtomicBool>,
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
            is_open: Arc::new(Mutex::new(false)),
            is_recording: Arc::new(Mutex::new(false)),
            did_mute: Arc::new(Mutex::new(false)),
            close_generation: Arc::new(AtomicU64::new(0)),
            usb_watchdog: usb_watchdog.clone(),
            bt_keep_alive: Arc::new(Mutex::new(false)),
            pending_pronunciation: Arc::new(Mutex::new(VecDeque::new())),
            pronunciation_thread: Arc::new(Mutex::new(None)),
            pronunciation_recordings_dir,
            liveness_monitor: Arc::new(Mutex::new(None)),
            liveness_stop: Arc::new(AtomicBool::new(false)),
        };

        // Check for Bluetooth output devices — if detected, keep the mic
        // stream alive permanently to prevent A2DP↔HFP profile switching
        // that causes audio dropouts on Bluetooth headphones.
        let bt_active = manager.is_bluetooth_output_active();
        if bt_active {
            info!("Bluetooth output device detected at startup — enabling mic stream keep-alive to prevent audio dropouts");
            *lock_with_log(&manager.bt_keep_alive, "bt_keep_alive") = true;
            manager.start_microphone_stream()?;
        } else if matches!(mode, MicrophoneMode::AlwaysOn) {
            manager.start_microphone_stream()?;
        }

        // Start background liveness monitor to detect zombie streams after sleep/wake
        manager.start_liveness_monitor();

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

        let handle = std::thread::spawn(move || {
            loop {
                // Check every 3 seconds
                std::thread::sleep(Duration::from_secs(3));

                if stop_flag.load(Ordering::Relaxed) {
                    debug!("Liveness monitor stopping");
                    break;
                }

                // Only monitor in always-on mode
                let is_always_on = {
                    let guard = lock_with_log(&mode, "mode");
                    matches!(*guard, MicrophoneMode::AlwaysOn)
                };

                if !is_always_on {
                    continue;
                }

                // Check if stream is open
                let stream_open = *lock_with_log(&is_open, "is_open");
                if !stream_open {
                    continue;
                }

                // Check if stream is alive (has received audio recently)
                let stream_alive = {
                    let recorder_guard = lock_with_log(&recorder, "recorder");
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
                        // Stop the current stream
                        {
                            let open = lock_with_log(&rm.is_open, "is_open");
                            if *open {
                                drop(open);
                                rm.stop_microphone_stream();
                            }
                        }

                        // Show USB-cycling overlay so user knows recovery is happening
                        utils::show_usb_cycling_overlay(&app_handle);
                        crate::tray::change_tray_icon(
                            &app_handle,
                            crate::tray::TrayIconState::Recording,
                        );

                        // Try to restart
                        if let Err(e) = rm.start_microphone_stream() {
                            error!("Liveness monitor failed to restart stream: {}", e);
                            utils::hide_recording_overlay(&app_handle);
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
                        }
                    }
                }
            }
        });

        *lock_with_log(&self.liveness_monitor, "liveness_monitor") = Some(handle);
        debug!("Liveness monitor started");
    }

    /// Stop the background liveness monitor thread
    fn stop_liveness_monitor(&self) {
        self.liveness_stop.store(true, Ordering::Relaxed);
        if let Some(handle) = lock_with_log(&self.liveness_monitor, "liveness_monitor").take() {
            let _ = handle.join();
        }
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
            let rm = app.state::<Arc<AudioRecordingManager>>();
            // Hold state lock across the check AND close to serialize against
            // try_start_recording, preventing a race where the stream is closed
            // under an active recording.
            let state = lock_with_log(&rm.state, "state");
            // Never close the stream if BT keep-alive is active
            if *lock_with_log(&bt_keep_alive, "bt_keep_alive") {
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
        let mut did_mute_guard = lock_with_log(&self.did_mute, "did_mute");

        if settings.mute_while_recording && *lock_with_log(&self.is_open, "is_open") {
            set_mute(true);
            *did_mute_guard = true;
            debug!("Mute applied");
        }
    }

    /// Removes mute if it was applied
    pub fn remove_mute(&self) {
        let mut did_mute_guard = lock_with_log(&self.did_mute, "did_mute");
        if *did_mute_guard {
            set_mute(false);
            *did_mute_guard = false;
            debug!("Mute removed");
        }
    }

    pub fn preload_vad(&self) -> Result<(), anyhow::Error> {
        let mut recorder_opt = lock_with_log(&self.recorder, "recorder");
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
                vad_path.to_str().expect("VAD path should be valid UTF-8"),
                &self.app_handle,
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
    /// may no longer be valid.
    fn recreate_recorder(&self) -> Result<(), anyhow::Error> {
        info!("Recreating AudioRecorder to discard stale device handles");

        // Take the old recorder and drop it (this stops any existing stream)
        let mut recorder_opt = lock_with_log(&self.recorder, "recorder");
        if recorder_opt.is_some() {
            // Close the old recorder to clean up resources
            if let Some(mut old_rec) = recorder_opt.take() {
                let _ = old_rec.close();
            }
        }

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
            vad_path.to_str().expect("VAD path should be valid UTF-8"),
            &self.app_handle,
        )?;

        *recorder_opt = Some(new_recorder);
        drop(recorder_opt);

        // Reset the is_open flag since we just recreated the recorder
        *lock_with_log(&self.is_open, "is_open") = false;

        info!("AudioRecorder recreated successfully");
        Ok(())
    }

    fn start_microphone_stream_inner(&self) -> Result<(), anyhow::Error> {
        let mut open_flag = lock_with_log(&self.is_open, "is_open");
        if *open_flag {
            debug!("Microphone stream already active");
            return Ok(());
        }

        let start_time = Instant::now();

        // Don't mute immediately - caller will handle muting after audio feedback
        let mut did_mute_guard = lock_with_log(&self.did_mute, "did_mute");
        *did_mute_guard = false;

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

        let mut recorder_opt = lock_with_log(&self.recorder, "recorder");
        if let Some(rec) = recorder_opt.as_mut() {
            rec.open(selected_device)
                .map_err(|e| anyhow::anyhow!("Failed to open recorder: {}", e))?;
        }

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
        let mut open_flag = lock_with_log(&self.is_open, "is_open");
        if !*open_flag {
            return;
        }

        let mut did_mute_guard = lock_with_log(&self.did_mute, "did_mute");
        if *did_mute_guard {
            set_mute(false);
        }
        *did_mute_guard = false;

        if let Some(rec) = lock_with_log(&self.recorder, "recorder").as_mut() {
            // If still recording, stop first.
            if *lock_with_log(&self.is_recording, "is_recording") {
                let _ = rec.stop();
                *lock_with_log(&self.is_recording, "is_recording") = false;
            }
            let _ = rec.close();
        }

        *open_flag = false;
        debug!("Microphone stream stopped");
    }

    /* ---------- mode switching --------------------------------------------- */

    pub fn update_mode(&self, new_mode: MicrophoneMode) -> Result<(), anyhow::Error> {
        let cur_mode = lock_with_log(&self.mode, "mode").clone();

        match (cur_mode, &new_mode) {
            (MicrophoneMode::AlwaysOn, MicrophoneMode::OnDemand) => {
                // Don't close the stream if BT keep-alive is active
                if *lock_with_log(&self.bt_keep_alive, "bt_keep_alive") {
                    info!("BT keep-alive active: keeping mic stream open despite mode switch to OnDemand");
                } else if matches!(*lock_with_log(&self.state, "state"), RecordingState::Idle) {
                    self.close_generation.fetch_add(1, Ordering::SeqCst);
                    self.stop_microphone_stream();
                }
            }
            (MicrophoneMode::OnDemand, MicrophoneMode::AlwaysOn) => {
                // Stream may already be open from BT keep-alive
                if !*lock_with_log(&self.is_open, "is_open") {
                    self.close_generation.fetch_add(1, Ordering::SeqCst);
                    self.start_microphone_stream()?;
                }
            }
            _ => {}
        }

        *lock_with_log(&self.mode, "mode") = new_mode;
        Ok(())
    }

    /* ---------- recording --------------------------------------------------- */

    /// Duration (ms) without receiving audio data before we consider the
    /// microphone stream dead and attempt to restart it.
    const STREAM_LIVENESS_TIMEOUT_MS: u64 = 3000;

    pub fn try_start_recording(&self, binding_id: &str) -> Result<(), String> {
        // Quick check under lock — just verify we're in Idle state.
        {
            let state = lock_with_log(&self.state, "state");
            if !matches!(*state, RecordingState::Idle) {
                return Err("Already recording".to_string());
            }
        }
        // State lock is released here. The actual state transition happens
        // below, after the potentially-slow liveness check.

        let bt_keep_alive = *lock_with_log(&self.bt_keep_alive, "bt_keep_alive");
        let is_always_on = matches!(*lock_with_log(&self.mode, "mode"), MicrophoneMode::AlwaysOn);

        // In on-demand mode (or when BT keep-alive is active), ensure the stream is open.
        // In always-on mode, check if the stream is alive and restart if needed.
        // KEY FIX: Also handle the case where the stream is NOT open at all
        // (e.g., after a failed USB power cycle recovery).
        let need_stream_open = if is_always_on {
            // Always-on mode: check if stream is alive
            let is_open = *lock_with_log(&self.is_open, "is_open");

            if !is_open {
                // Stream is not open at all — need to restart it
                warn!("Always-on microphone stream is not open — restarting");
                true
            } else {
                // Stream is open, but check if it's actually producing data
                let stream_alive = lock_with_log(&self.recorder, "recorder")
                    .as_ref()
                    .map_or(false, |r| {
                        r.is_stream_alive(Self::STREAM_LIVENESS_TIMEOUT_MS)
                    });

                self.usb_watchdog.on_stream_alive_check(stream_alive);

                if !stream_alive {
                    warn!(
                        "Always-on microphone stream appears dead (no audio for {}ms) — restarting",
                        Self::STREAM_LIVENESS_TIMEOUT_MS
                    );
                    true
                } else {
                    false // Stream is alive, no action needed
                }
            }
        } else {
            // On-demand mode: need to open stream (unless BT keep-alive has it open)
            !bt_keep_alive
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
            if *lock_with_log(&self.is_open, "is_open") {
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
        let mut state = lock_with_log(&self.state, "state");
        if let RecordingState::Idle = *state {
            if let Some(rec) = lock_with_log(&self.recorder, "recorder").as_ref() {
                if rec.start().is_ok() {
                    *lock_with_log(&self.is_recording, "is_recording") = true;
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
        // If currently open, restart the microphone stream to use the new device
        if *lock_with_log(&self.is_open, "is_open") {
            self.close_generation.fetch_add(1, Ordering::SeqCst);
            self.stop_microphone_stream();
            // Re-evaluate BT keep-alive after device change
            self.refresh_bluetooth_keep_alive();
            if *lock_with_log(&self.bt_keep_alive, "bt_keep_alive")
                || matches!(*lock_with_log(&self.mode, "mode"), MicrophoneMode::AlwaysOn)
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
        let mut bt_keep_alive = lock_with_log(&self.bt_keep_alive, "bt_keep_alive");

        if bt_active && !*bt_keep_alive {
            info!("Bluetooth output device detected — enabling mic stream keep-alive to prevent audio dropouts");
            *bt_keep_alive = true;
            drop(bt_keep_alive);
            // Open the mic stream if not already open
            if !*lock_with_log(&self.is_open, "is_open") {
                if let Err(e) = self.start_microphone_stream() {
                    error!("Failed to open mic stream for BT keep-alive: {}", e);
                }
            }
        } else if !bt_active && *bt_keep_alive {
            info!("Bluetooth output device no longer detected — disabling mic stream keep-alive");
            *bt_keep_alive = false;
            drop(bt_keep_alive);
            // Close the stream if we're in OnDemand mode and not recording
            if matches!(*lock_with_log(&self.mode, "mode"), MicrophoneMode::OnDemand)
                && !self.is_recording()
            {
                self.close_generation.fetch_add(1, Ordering::SeqCst);
                self.stop_microphone_stream();
            }
        }
    }

    pub fn stop_recording(&self, binding_id: &str) -> Option<Vec<f32>> {
        let state = lock_with_log(&self.state, "state");

        match *state {
            RecordingState::Recording {
                binding_id: ref active,
            } if active == binding_id => {
                // NOTE: We intentionally keep the state as Recording during the
                // smart-stop buffer period so that try_start_recording() rejects
                // new recordings while we are still capturing trailing audio.
                drop(state);

                // Use volume-aware stop when an extra recording buffer is
                // configured.  This continues recording for up to the configured
                // time but stops early when the microphone level drops below the
                // estimated noise floor, avoiding unnecessary waiting.
                let settings = get_settings(&self.app_handle);

                let samples = if settings.extra_recording_buffer_ms > 0 {
                    debug!(
                        "Smart-stop: starting volume-aware buffer (max {}ms)",
                        settings.extra_recording_buffer_ms
                    );
                    if let Some(rec) = lock_with_log(&self.recorder, "recorder").as_ref() {
                        match rec.smart_stop(settings.extra_recording_buffer_ms) {
                            Ok(buf) => buf,
                            Err(e) => {
                                error!("smart_stop() failed: {e}");
                                Vec::new()
                            }
                        }
                    } else {
                        error!("Recorder not available for smart_stop");
                        Vec::new()
                    }
                } else {
                    if let Some(rec) = lock_with_log(&self.recorder, "recorder").as_ref() {
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
                    }
                };

                // Now transition to Idle after the buffer is complete.
                {
                    let mut state = lock_with_log(&self.state, "state");
                    *state = RecordingState::Idle;
                }

                *lock_with_log(&self.is_recording, "is_recording") = false;

                // Inform the USB watchdog about the recording result.
                // If 0 samples were captured, this may trigger an automatic USB cycle.
                if self.usb_watchdog.on_recording_finished(samples.len()) {
                    // Watchdog completed a power cycle. Restart the stream if needed.
                    if let Err(e) = self.restart_microphone_if_needed() {
                        error!(
                            "Failed to restart microphone after dead-stream USB cycle: {}",
                            e
                        );
                    }
                }

                // In on-demand mode, decide whether to close the mic stream.
                // When a Bluetooth output device is active, we keep the stream
                // alive permanently to prevent the A2DP↔HFP profile switch that
                // causes audio dropouts on BT headphones.
                if matches!(*lock_with_log(&self.mode, "mode"), MicrophoneMode::OnDemand) {
                    let bt_keep_alive = *lock_with_log(&self.bt_keep_alive, "bt_keep_alive");
                    if bt_keep_alive {
                        debug!("BT keep-alive active: keeping mic stream open");
                    } else if get_settings(&self.app_handle).lazy_stream_close {
                        self.schedule_lazy_close();
                    } else {
                        self.stop_microphone_stream();
                    }
                }

                // Pad short audio to reduce Whisper hallucinations.
                // Very short clips (< 3s) padded with silence cause Whisper to
                // hallucinate repetitive text. A 3-second minimum gives Whisper
                // enough context to produce a good transcription without
                // hallucinating. The VAD-based trim_trailing_silence in the
                // transcription pipeline further cleans up any trailing silence.
                let s_len = samples.len();
                let min_samples = WHISPER_SAMPLE_RATE * 3; // 3 seconds minimum
                if s_len > 0 && s_len < min_samples {
                    let mut padded = samples;
                    padded.resize(min_samples, 0.0);
                    Some(padded)
                } else {
                    Some(samples)
                }
            }
            _ => None,
        }
    }
    pub fn is_recording(&self) -> bool {
        matches!(
            *lock_with_log(&self.state, "state"),
            RecordingState::Recording { .. }
        )
    }

    /// Cancel any ongoing recording without returning audio samples
    pub fn cancel_recording(&self) {
        let mut state = lock_with_log(&self.state, "state");

        if let RecordingState::Recording { .. } = *state {
            *state = RecordingState::Idle;
            drop(state);

            if let Some(rec) = lock_with_log(&self.recorder, "recorder").as_ref() {
                let _ = rec.stop(); // Discard the result
            }

            *lock_with_log(&self.is_recording, "is_recording") = false;

            // In on-demand mode, decide whether to close the mic stream.

            // When a Bluetooth output device is active, we keep the stream
            // alive permanently to prevent the A2DP↔HFP profile switch.
            if matches!(*lock_with_log(&self.mode, "mode"), MicrophoneMode::OnDemand) {
                let bt_keep_alive = *lock_with_log(&self.bt_keep_alive, "bt_keep_alive");
                if bt_keep_alive {
                    debug!("BT keep-alive active: keeping mic stream open");
                } else if get_settings(&self.app_handle).lazy_stream_close {
                    self.schedule_lazy_close();
                } else {
                    self.stop_microphone_stream();
                }
            }
        }
    }

    /// Restart the microphone stream if it should be active.
    /// Called after USB power cycling completes to fix the "mic not listening,
    /// volume bars not moving" issue.
    /// Returns Ok(()) if the stream was restarted or wasn't needed, Err if restart failed.
    pub fn restart_microphone_if_needed(&self) -> Result<(), anyhow::Error> {
        let is_always_on = matches!(*lock_with_log(&self.mode, "mode"), MicrophoneMode::AlwaysOn);
        let bt_keep_alive = *lock_with_log(&self.bt_keep_alive, "bt_keep_alive");

        if is_always_on || bt_keep_alive {
            info!("Restarting microphone stream after USB power cycle");
            // Stop the current stream if open
            if *lock_with_log(&self.is_open, "is_open") {
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
}
