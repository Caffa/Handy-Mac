use crate::audio_toolkit::{
    is_bluetooth_audio_active, list_output_devices,
    vad::{frames_for_duration_ms, EarshotVad, SmoothedVad, VAD_ONSET_MS, VAD_PREFILL_MS},
    CaptureControl, CaptureSink, CaptureSinkConfig, CaptureVad, SileroVad, SpeechScorer, VadPolicy,
    VoiceActivityDetector,
};
use crate::errors::AppError;
use crate::helpers::clamshell;
use crate::logging;
use crate::managers::model::ModelManager;
use crate::portable;
use crate::settings::{get_settings, get_settings_safe, AppSettings, VadBackend};
use crate::streaming_cancel_flag::StreamingCancelFlag;
use crate::usb_watchdog;
use crate::usb_watchdog::UsbWatchdog;
use crate::utils;
use crate::LiveCaptionsTranscriptionManager;
use handy_recorder::{Channels, EndReason, ErrorKind, Recorder, RecorderConfig, StartError};
use log::{debug, error, info, trace, warn};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use specta::Type;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};
use tauri::{Emitter, Manager};

use crate::audio_toolkit::audio::{AudioFrameCallback, LevelCallback};

const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const EARSHOT_VAD_THRESHOLD: f32 = 0.5;

/// Threshold for "no audio detected" during recording.
/// Max RMS below this indicates the microphone is likely dead or muted.
/// ~0.001 is ~-60dB for normalized f32 samples — essentially silence.
const NO_AUDIO_THRESHOLD: f32 = 0.001;

/// Smart-stop: the trailing buffer closes after this much time below the
/// noise-floor-derived voice threshold (from the deleted in-house recorder).
const SILENCE_THRESHOLD_MS: u64 = 500;
/// Smart-stop: the trailing buffer always captures at least this long, so a
/// final plosive right at the hotkey release is not clipped.
const MIN_TRAILING_BUFFER_MS: u64 = 200;

const WHISPER_SAMPLE_RATE: usize = 16000;

/// The 2.5s live-captions batch interval (no dedicated streaming model).
const STREAMING_INTERVAL_MS: u64 = 2500;
/// Post-VAD speech frames required before the first batch invoke (~90ms).
const MIN_SPEECH_FRAMES_BEFORE_STREAMING: u32 = 3;

/// Minimum interval between standing-stream rebuilds triggered by input
/// device-list changes. Dock re-enumerations can fire several list diffs in
/// quick succession; the rate limit keeps us to one rebuild per burst.
const STANDING_REBUILD_MIN_INTERVAL_MS: u64 = 10_000;

/// How long after an input device-list change the stream counts as
/// "changed recently" (feeds the zero-RMS abort gate and flap arming).
const DEVICE_FLAP_RECENT_WINDOW_MS: u64 = 120_000;
/// TTL of the one-shot wake bypass armed on a device flap (covers the
/// user's first few post-flap dictations).
const DEVICE_FLAP_BYPASS_TTL_SECS: u64 = 180;
/// Minimum interval between flap-bypass arms (one arm per flap burst).
const FLAP_ARM_MIN_INTERVAL_MS: u64 = 60_000;

/// Decide whether an input device-list change should rebuild the standing
/// (always-on / BT keep-alive) stream. Skipped entirely while a recording is
/// active (`restart_microphone_if_needed` bails then anyway) and rate-limited
/// so one dock re-enumeration burst triggers at most one rebuild.
fn should_rebuild_standing_stream(elapsed_ms: u64, recording: bool) -> bool {
    if recording {
        return false;
    }
    elapsed_ms >= STANDING_REBUILD_MIN_INTERVAL_MS
}

/// True when an input device change happened within `window_ms` of `now_ms`.
/// A stamp of 0 (never stamped) is never "recent".
fn device_change_recent(change_epoch_ms: u64, now_ms: u64, window_ms: u64) -> bool {
    change_epoch_ms > 0 && now_ms.saturating_sub(change_epoch_ms) <= window_ms
}

/// True when a flap bypass may be armed: never armed (stamp 0) always
/// allows it; otherwise at least `FLAP_ARM_MIN_INTERVAL_MS` must have passed
/// since the last arm.
fn should_arm_flap_bypass(last_arm_epoch_ms: u64, now_ms: u64) -> bool {
    last_arm_epoch_ms == 0 || now_ms.saturating_sub(last_arm_epoch_ms) >= FLAP_ARM_MIN_INTERVAL_MS
}

/// Armed-recording zero-RMS abort: an armed recording that has run this long
/// with max-RMS still at digital silence is dead regardless of chunk
/// heartbeats (a zombie standing stream keeps *delivering* silence after a
/// dock power-loss re-enumeration, so recency-based liveness stays green).
const ZERO_SAMPLE_ABORT_MS: u64 = 2500;
/// Digital-silence RMS ceiling for the abort predicate. Room tone is
/// ~0.001 (−60 dBFS); only true digital silence proves the device is not
/// delivering real samples. Shared with the USB watchdog (imported there):
/// a recording of exact zeros is dead signal, never a healthy delivery.
pub const ZERO_SAMPLE_MAX_RMS: f32 = 1e-6;

/// Current wall-clock time in milliseconds since the Unix epoch.
fn epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Pure predicate: an armed recording is dead when it has run at least
/// `ZERO_SAMPLE_ABORT_MS` with max-RMS still at digital silence AND either
/// the stream was delivering nonzero RMS before arming (zombie signature)
/// OR the input device changed recently (dock-flap case: the stream was
/// already silent since the flap, so there is no nonzero pre-arm window to
/// observe). A mic that was silent before arming too with no recent device
/// change (hardware-muted / dead since boot) is not fixed by a rebuild, so
/// no abort — the recording runs and the existing stop path reports 0
/// samples.
fn should_abort_dead_recording(
    armed: bool,
    elapsed_ms: u64,
    max_level: f32,
    pre_arm_max_rms: f32,
    device_changed_recently: bool,
) -> bool {
    armed
        && elapsed_ms >= ZERO_SAMPLE_ABORT_MS
        && max_level <= ZERO_SAMPLE_MAX_RMS
        && (pre_arm_max_rms > ZERO_SAMPLE_MAX_RMS || device_changed_recently)
}

/// Pure decision for the bounded auto-retry: the one-shot budget must be
/// available AND the coordinator must be Idle (a user re-press during the
/// watchdog accounting window may have already started a new recording).
fn retry_decision(budget_available: bool, coordinator_idle: bool) -> bool {
    budget_available && coordinator_idle
}

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

#[derive(Clone, Debug)]
pub enum RecordingState {
    Idle,
    Recording {
        binding_id: String,
        start_time: Instant,
    },
    /// Collecting: the hotkey released but the smart-stop trailing window (or
    /// the final drain) is still running. Counts as active for every guard.
    Stopping,
}

#[derive(Clone, Debug)]
pub enum MicrophoneMode {
    AlwaysOn,
    OnDemand,
}

/// Tracks our forced "mute while recording" so we can restore the user's audio
/// exactly as it was. `did_mute` is true while our mute is active; `prev_muted`
/// is the system mute state captured just before we muted, used to decide
/// whether to unmute on stop (so a system that was already muted stays muted).
#[derive(Debug, Default, Clone, Copy)]
struct MuteState {
    did_mute: bool,
    prev_muted: Option<bool>,
}

/// The persisted microphone preference currently in effect. Clamshell and
/// regular selections are kept distinct so losing a clamshell-only device does
/// not erase the user's normal microphone preference.
enum DesiredMicrophone {
    Default,
    Selected(String),
    Clamshell(String),
}

/// Result of resolving the persisted preference to a recorder device ID.
/// `device: None` opens the system default. The unavailable name is populated
/// only when enumeration succeeded and confirmed that the user's regular
/// selected microphone is missing.
struct MicrophoneResolution {
    device: Option<String>,
    /// The ID came from `cached_device`, so it may be stale.
    from_cache: bool,
    unavailable_selected_microphone: Option<String>,
}

/// The VAD detector. A recording borrows it for its sink and `stop` hands it
/// back; a recording whose sink is lost loses it, and the next one reloads it.
enum VadSlot {
    Unloaded,
    Idle(CaptureVad),
    /// Lent to the active recording (or the standing always-on sink). Keeps
    /// the frame size, so a microphone opened meanwhile still gets VAD-sized
    /// chunks.
    Lent {
        frame_samples: usize,
    },
}

/// An open microphone: the stream is running whether or not it is recording.
struct OpenMicrophone {
    recorder: Recorder<CaptureSink>,
    /// Set when this recorder fails. Its own flag, so a failure handler that
    /// runs late can't mark a replacement recorder failed.
    failed: Arc<AtomicBool>,
}

impl OpenMicrophone {
    fn needs_reopen(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }
}

/// The `recording-error` event's `error_type` for a recording that couldn't
/// start.
pub fn recording_error_type(error: &anyhow::Error) -> &'static str {
    match error
        .downcast_ref::<handy_recorder::Error>()
        .map(handy_recorder::Error::kind)
    {
        Some(ErrorKind::PermissionDenied) => "microphone_permission_denied",
        // A missing selected microphone falls back to the default, so this
        // means there is no input device at all.
        Some(ErrorKind::DeviceUnavailable) => "no_input_device",
        _ => "unknown",
    }
}

/* ──────────────────────────────────────────────────────────────── */

/// A fresh detector for `backend`, without Handy's smoothing: what a
/// recording's VAD wraps, and what scores audio for long-form chunking, so
/// every path uses the selected backend. The fork's per-backend threshold
/// logic is kept: Silero follows the user's VAD sensitivity setting, Earshot
/// keeps its fixed experimental threshold.
pub fn create_detector(
    backend: VadBackend,
    app_handle: &tauri::AppHandle,
) -> Result<Box<dyn SpeechScorer>, anyhow::Error> {
    let settings = get_settings(app_handle);
    let vad_threshold = settings.vad_sensitivity.threshold();

    Ok(match backend {
        VadBackend::Silero => {
            let vad_path = app_handle
                .path()
                .resolve(
                    "resources/models/silero-vad-v6.2-F32.gguf",
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
    })
}

/// Builds the capture VAD from the current settings. The fork's user-facing
/// VAD sensitivity setting is expressed in legacy Silero-v4 30 ms frame units
/// (15-35); it is converted to milliseconds first so the hangover keeps its
/// meaning across frame geometries (Silero v6.2 = 32 ms frames) and scales
/// correctly for either backend.
fn create_capture_vad(
    backend: VadBackend,
    app_handle: &tauri::AppHandle,
) -> Result<CaptureVad, anyhow::Error> {
    let settings = get_settings(app_handle);
    let vad_hangover_frames = settings.vad_sensitivity.hangover_frames();

    let detector: Box<dyn VoiceActivityDetector> = create_detector(backend, app_handle)?;

    // Earshot uses 16 ms frames while Silero v6.2 uses 32 ms. Convert the
    // existing time-based capture profile to each detector's frame size so
    // selecting a backend does not shorten pre-roll, onset, or post-speech
    // audio.
    let frame_samples = detector.frame_samples();
    let prefill_frames = frames_for_duration_ms(VAD_PREFILL_MS, frame_samples);
    // The setting's hangover unit is the legacy Silero-v4 30 ms frame.
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

    Ok(CaptureVad::new(
        Box::new(smoothed_vad),
        offline_hangover_frames,
        frames_for_duration_ms(
            crate::audio_toolkit::vad::VAD_STREAMING_HANGOVER_MS,
            frame_samples,
        ),
    ))
}

/// 16 kHz mono in VAD-frame chunks, from `device` (`None`: the system
/// default). `channel` picks one zero-based input channel; `None` averages
/// them all.
fn microphone_config(
    device: Option<String>,
    channel: Option<u16>,
    frames_per_chunk: usize,
) -> RecorderConfig {
    RecorderConfig {
        device,
        channels: channel.map_or(Channels::MixToMono, Channels::Only),
        frames_per_chunk: Some(frames_per_chunk),
        ..RecorderConfig::speech()
    }
}

fn open_microphone(config: RecorderConfig) -> Result<OpenMicrophone, handy_recorder::Error> {
    let failed = Arc::new(AtomicBool::new(false));
    let recorder = Recorder::open_with_failure_handler(config, {
        let failed = Arc::clone(&failed);
        // handy-recorder logs the failure. The stream is rebuilt on the next
        // start; a recording in progress keeps the audio captured before it.
        move |_| failed.store(true, Ordering::Release)
    })?;
    Ok(OpenMicrophone { recorder, failed })
}

/// Opens with the selected channel, or averages all channels if the device
/// doesn't have it (a different microphone than the one it was chosen on).
fn open_microphone_on_channel(
    device: Option<String>,
    channel: Option<u16>,
    frames_per_chunk: usize,
) -> Result<OpenMicrophone, handy_recorder::Error> {
    match open_microphone(microphone_config(device.clone(), channel, frames_per_chunk)) {
        Err(error) if error.kind() == ErrorKind::InvalidChannel => {
            warn!("{error}; averaging all input channels instead");
            open_microphone(microphone_config(device, None, frames_per_chunk))
        }
        result => result,
    }
}

/* ──────────────────────────────────────────────────────────────── */

/// One recording session's first-sample notification. Waiting on this never
/// blocks the shortcut coordinator: callers hand it to a dedicated worker.
pub struct RecordingReadiness {
    receiver: mpsc::Receiver<()>,
    generation: u64,
}

impl RecordingReadiness {
    pub fn wait(self) -> bool {
        self.receiver.recv().is_ok()
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }
}

/// Not `Clone` on purpose: a by-value clone moved into a thread would run the
/// full `Drop` when the thread ends (killing the device/liveness monitors).
/// Share via `Arc<AudioRecordingManager>` — Arc clones never run `Drop`.
pub struct AudioRecordingManager {
    /// Never assign through this directly — route every write through
    /// `set_state()`, which keeps `recording_active` in sync.
    state: Arc<Mutex<RecordingState>>,
    mode: Arc<Mutex<MicrophoneMode>>,
    app_handle: tauri::AppHandle,

    recorder: Arc<Mutex<Option<OpenMicrophone>>>,
    vad: Arc<Mutex<VadSlot>>,
    /// Zero-based input channel to record, or `None` to average them all.
    /// Held here rather than read from settings, which are persisted only
    /// after a channel change reopens successfully.
    selected_channel: Arc<Mutex<Option<u16>>>,
    /// Shared between the manager and the active sink: telemetry, pre-buffer,
    /// smart-stop sensors, and the live-captions batch gates.
    capture: Arc<CaptureControl>,
    /// Accumulated post-VAD slices of the current capture session, drained by
    /// the 2.5s batch live-captions callback.
    batch_buffer: Arc<Mutex<Vec<f32>>>,
    /// Whether a dedicated live-captions model's StreamRouter is attached.
    router_attached: bool,
    /// Spectrum levels for the overlay.
    on_levels: LevelCallback,
    /// VAD-passed frames for the batch live-captions fallback.
    on_audio: AudioFrameCallback,
    is_open: Arc<Mutex<bool>>,
    is_recording: Arc<Mutex<bool>>,
    mute_state: Arc<Mutex<MuteState>>,
    close_generation: Arc<AtomicU64>,
    cancel_generation: Arc<AtomicU64>,
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
    /// Resolution of a *named* microphone (selected or clamshell) to its
    /// recorder device ID, cached so on-demand recording starts skip an extra
    /// device enumeration. Keyed by the resolved name, so a settings change
    /// misses naturally; cleared when an open fails (device unplugged) so the
    /// retry re-enumerates. The system-default case is never cached — the
    /// recorder resolves the current default itself.
    cached_device: Arc<Mutex<Option<(String, String)>>>,
    /// A recording that outlives the hotkey: in always-on mode with the
    /// pre-hotkey buffer or the batch live-captions fallback active, the
    /// manager keeps one sink running while idle. Its `armed` flag is false,
    /// so it maintains the rolling buffer (and caption seams) without
    /// producing recording audio; a hotkey press arms it in place.
    standing_active: Arc<AtomicBool>,
    /// Lock-free mirror of "is the state in {Recording, Stopping}",
    /// maintained by `set_state()`. The hot-path `is_recording()` reads THIS
    /// instead of the `state` mutex, so a UI poll can no longer deadlock
    /// the main/webview thread when a worker holds `state` across a slow
    /// CoreAudio open/close.
    recording_active: Arc<AtomicBool>,
    /// Invalidates asynchronous first-sample UI/chime work when a recording is
    /// stopped or cancelled. This prevents a slow device from producing a late
    /// "ready" indication for a session the user already ended.
    capture_generation: Arc<AtomicU64>,
    /// Background device hot-plug monitor thread handle
    device_monitor: Arc<Mutex<Option<std::thread::JoinHandle<()>>>>,
    /// Sender to signal the device monitor to stop
    device_monitor_stop: Arc<Mutex<Option<std::sync::mpsc::Sender<()>>>>,
    /// Previous input device names (for change detection)
    prev_input_devices: Arc<Mutex<Vec<String>>>,
    /// Previous output device names (for change detection)
    prev_output_devices: Arc<Mutex<Vec<String>>>,
    /// Epoch ms of the last standing-stream rebuild triggered by an input
    /// device-list change (rate limit: one rebuild per
    /// `STANDING_REBUILD_MIN_INTERVAL_MS`).
    last_standing_rebuild_epoch: Arc<AtomicU64>,
    /// One-shot auto-retry budget for the zero-RMS abort's synthetic press.
    /// Set when a recording actually delivers samples (stop path, or the
    /// monitor observing a delivering armed recording); consumed by exactly
    /// one synthetic press so a still-dead stream cannot loop retries.
    auto_retry_allowed: Arc<AtomicBool>,
    /// Epoch ms of the most recent input device-list change (device flap).
    /// Feeds the zero-RMS abort's device_changed_recently gate.
    last_input_change_epoch_ms: Arc<AtomicU64>,
    /// Epoch ms of the last flap-bypass arm (rate limit: one arm per
    /// `FLAP_ARM_MIN_INTERVAL_MS`).
    last_flap_bypass_arm_epoch_ms: Arc<AtomicU64>,
}

// Helper types for microphone resolution live in the parent module so that
// methods below can reference them without scoping issues inside an impl block.
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

        // Live-captions wiring: when a dedicated model is configured, its
        // StreamRouter consumes raw frames; otherwise the 2.5s batch fallback
        // targets the right transcription manager. Both seams are always
        // built so the runtime toggle never needs a stream recreation.
        let live_captions_model = settings.live_captions_model.clone();
        let use_live_captions_model = !live_captions_model.is_empty();
        let router_attached = use_live_captions_model;

        let pre_buffer_max_samples = (settings.pre_recording_buffer_ms.saturating_mul(16)) as usize;

        let capture = Arc::new(CaptureControl::new(pre_buffer_max_samples, router_attached));
        capture
            .streaming_enabled
            .store(settings.live_captions_enabled, Ordering::Release);

        let batch_buffer: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));

        let on_levels: LevelCallback = {
            let app_handle = app.clone();
            Arc::new(move |levels| utils::emit_levels(&app_handle, &levels))
        };

        // Post-VAD seam: accumulate slices and fire the batch callback under
        // the fork's gates (enabled flag, ≥3 consecutive speech frames, 2.5s
        // interval, router not active, cancel flag, generation guards).
        let on_audio: AudioFrameCallback = {
            let capture = Arc::clone(&capture);
            let batch_buffer = Arc::clone(&batch_buffer);
            let app_handle = app.clone();
            let (cancel_flag, tm_arc): (
                Option<StreamingCancelFlag>,
                Option<Arc<Mutex<crate::managers::transcription::TranscriptionManager>>>,
            ) = if use_live_captions_model {
                let cancel_flag = app
                    .try_state::<crate::LiveCaptionsStreamingCancelFlag>()
                    .map(|state| state.0.inner().clone());
                let tm = app
                    .try_state::<LiveCaptionsTranscriptionManager>()
                    .map(|state| Arc::clone(&state.0));
                (cancel_flag, tm)
            } else {
                let cancel_flag = app
                    .try_state::<StreamingCancelFlag>()
                    .map(|state| state.inner().clone());
                let tm = app
                    .try_state::<Arc<Mutex<crate::managers::transcription::TranscriptionManager>>>()
                    .map(|state| state.inner().clone());
                (cancel_flag, tm)
            };
            let router = if use_live_captions_model {
                app.try_state::<LiveCaptionsTranscriptionManager>()
                    .and_then(|tm| Some(tm.0.lock().stream_router()))
            } else {
                None
            };

            Arc::new(move |frame| {
                batch_buffer.lock().extend_from_slice(frame);

                if !capture.streaming_enabled.load(Ordering::Acquire) {
                    return;
                }
                if capture.router_attached {
                    // A dedicated model's router handles streaming; the batch
                    // fallback would double-transcribe.
                    return;
                }
                if capture.speech_frames.load(Ordering::Relaxed)
                    < MIN_SPEECH_FRAMES_BEFORE_STREAMING
                {
                    return;
                }
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::SystemTime::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;
                let last = capture.last_batch_invoke_ms.load(Ordering::Relaxed);
                if last != 0 && now.saturating_sub(last) < STREAMING_INTERVAL_MS {
                    return;
                }
                if let Some(router) = &router {
                    if router.is_open() {
                        return;
                    }
                }
                let Some(cancel_flag) = &cancel_flag else {
                    return;
                };
                let Some(tm) = &tm_arc else {
                    return;
                };
                if cancel_flag.load_flag() {
                    debug!("[STREAM-CB] Skipping streaming transcription — cancellation requested");
                    return;
                }
                capture.last_batch_invoke_ms.store(now, Ordering::Relaxed);

                let samples = batch_buffer.lock().clone();
                if samples.is_empty() {
                    return;
                }

                // This closure runs on the sink's delivery thread, so spawn a
                // blocking task to keep transcription off audio capture.
                let app_handle = app_handle.clone();
                let cancel_flag = cancel_flag.clone();
                let tm = Arc::clone(tm);
                tauri::async_runtime::spawn_blocking(move || {
                    if cancel_flag.load_flag() {
                        debug!("[STREAM-CB] Skipping streaming transcription (blocking) — cancellation requested");
                        return;
                    }

                    // Bounded acquisition: a wedged prior streaming callback
                    // cannot block this frame forever.
                    let lock_start = Instant::now();
                    let Some(guard) = tm.try_lock_for(Duration::from_secs(2)) else {
                        warn!("[STREAM-LOCK] timed out acquiring TM lock after 2s, skipping frame");
                        return;
                    };
                    let lock_dt = lock_start.elapsed();
                    if lock_dt > Duration::from_millis(250) {
                        warn!(
                            "[STREAM-LOCK] tm.lock() acquired in {:?} (transcribe pending)",
                            lock_dt
                        );
                    }
                    let transcription_result = guard.transcribe(samples);
                    drop(guard);

                    match transcription_result {
                        Ok(result) if !result.text.is_empty() => {
                            if cancel_flag.load_flag() {
                                debug!("[STREAM-CB] Discarding streaming transcription result — cancelled");
                                return;
                            }
                            info!(
                                "[Live Captions] Emitting partial-transcription event: text_len={}",
                                result.text.len()
                            );
                            if let Err(e) = app_handle.emit("partial-transcription", &result) {
                                warn!("Failed to emit partial-transcription event: {}", e);
                            }
                        }
                        Ok(_) => {
                            debug!("[Live Captions] Streaming transcription returned empty text");
                        }
                        Err(e) => {
                            if matches!(e, AppError::ModelNotLoaded) {
                                info!("[Live Captions] Model not loaded — initiating model load for next streaming cycle");
                                if let Some(load_guard) = tm.try_lock_for(Duration::from_secs(2)) {
                                    load_guard.initiate_model_load();
                                } else {
                                    warn!("[STREAM-LOCK] timed out acquiring TM lock for model-load re-kick after 2s, skipping");
                                }
                            } else if matches!(e, AppError::TranscriptionBusy) {
                                debug!(
                                    "[Live Captions] Streaming transcription skipped (busy): {}",
                                    e
                                );
                            } else {
                                warn!("[Live Captions] Streaming transcription failed: {}", e);
                            }
                        }
                    }
                });
            })
        };

        let manager = Self {
            state: Arc::new(Mutex::new(RecordingState::Idle)),
            mode: Arc::new(Mutex::new(mode.clone())),
            app_handle: app.clone(),

            recorder: Arc::new(Mutex::new(None)),
            vad: Arc::new(Mutex::new(VadSlot::Unloaded)),
            selected_channel: Arc::new(Mutex::new(settings.selected_channel)),
            capture: Arc::clone(&capture),
            batch_buffer,
            router_attached,
            on_levels,
            on_audio,
            is_open: Arc::new(Mutex::new(false)),
            is_recording: Arc::new(Mutex::new(false)),
            mute_state: Arc::new(Mutex::new(MuteState::default())),
            close_generation: Arc::new(AtomicU64::new(0)),
            cancel_generation: Arc::new(AtomicU64::new(0)),
            usb_watchdog: usb_watchdog.clone(),
            bt_keep_alive: Arc::new(AtomicBool::new(false)),
            pending_pronunciation: Arc::new(Mutex::new(VecDeque::new())),
            pronunciation_thread: Arc::new(Mutex::new(None)),
            pronunciation_recordings_dir,
            liveness_monitor: Arc::new(Mutex::new(None)),
            liveness_stop: Arc::new(AtomicBool::new(false)),
            cached_device: Arc::new(Mutex::new(None)),
            standing_active: Arc::new(AtomicBool::new(false)),
            recording_active: Arc::new(AtomicBool::new(false)),
            capture_generation: Arc::new(AtomicU64::new(0)),
            device_monitor: Arc::new(Mutex::new(None)),
            device_monitor_stop: Arc::new(Mutex::new(None)),
            prev_input_devices: Arc::new(Mutex::new(Vec::new())),
            prev_output_devices: Arc::new(Mutex::new(Vec::new())),
            // Stamp with the current time so the first real device change is
            // correctly measured as "idle 0ms", not against the epoch (a 0
            // init produced garbage "idle 1794159154121ms" logs).
            last_standing_rebuild_epoch: Arc::new(AtomicU64::new(epoch_ms())),
            auto_retry_allowed: Arc::new(AtomicBool::new(false)),
            last_input_change_epoch_ms: Arc::new(AtomicU64::new(0)),
            last_flap_bypass_arm_epoch_ms: Arc::new(AtomicU64::new(0)),
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
        // Synchronously opening CoreAudio on the main thread during app
        // startup has been observed to cause heap corruption (SIGABRT) when
        // other Tauri initialization is happening concurrently. The stream is
        // not required to be ready before we return the manager; the liveness
        // monitor will self-heal if it isn't open yet.
        let should_open_stream = bt_active || matches!(mode, MicrophoneMode::AlwaysOn);
        if should_open_stream {
            // Capture only the AppHandle: a thread-owned manager clone runs
            // the FULL Drop when the thread ends (stop_device_monitor +
            // liveness_stop), killing both background monitors seconds after
            // startup. Resolve the managed instance instead — manage() runs
            // synchronously right after new() returns, so the short sleep
            // below guarantees try_state finds it.
            let app_handle = app.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(100));
                let Some(rm) = app_handle.try_state::<Arc<AudioRecordingManager>>() else {
                    error!(
                        "Deferred open: AudioRecordingManager not managed yet; the liveness monitor will self-heal the stream"
                    );
                    return;
                };
                if let Err(e) = rm.start_microphone_stream() {
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
    ///
    /// handy-recorder runs its own watchdog (NoAudio 10s / Stalled 5s) that
    /// flips the recorder's `failed` flag; this monitor adds the fork's
    /// self-healing on top: it notices the failure and rebuilds the stream
    /// without waiting for the next hotkey press.
    fn start_liveness_monitor(&self) {
        let is_open = self.is_open.clone();
        let recorder = self.recorder.clone();
        let capture = Arc::clone(&self.capture);
        let standing_active = self.standing_active.clone();
        let recording_active = self.recording_active.clone();
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

                if usb_watchdog.is_cycling() {
                    debug!("Liveness monitor: USB cycle in progress, skipping check");
                    continue;
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
                let stream_open = *is_open.lock();

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

                // Liveness: the crate flips `failed` via its own watchdog; a
                // running sink (armed or standing) additionally proves the
                // stream alive by delivering chunks. With no sink running,
                // the crate discards audio, so chunk recency says nothing.
                let failed = recorder.lock().as_ref().is_some_and(|m| m.needs_reopen());
                let sink_running = recording_active.load(Ordering::Acquire)
                    || standing_active.load(Ordering::Acquire);
                let chunks_stale = sink_running && {
                    let last = capture.last_chunk_ms();
                    last == 0
                        || (std::time::SystemTime::now()
                            .duration_since(std::time::SystemTime::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis() as u64)
                            .saturating_sub(last)
                            > Self::STREAM_LIVENESS_TIMEOUT_MS
                };

                if failed || chunks_stale {
                    warn!(
                        "Liveness monitor: always-on stream appears dead (failed={failed}, chunks_stale={chunks_stale}) — restarting"
                    );

                    // Notify USB watchdog about the dead stream
                    usb_watchdog.on_stream_alive_check(false);

                    // Restart the stream via the app handle
                    if let Some(rm) = app_handle.try_state::<Arc<AudioRecordingManager>>() {
                        // Check if user was actively recording before showing overlay
                        let was_recording = rm.is_recording();

                        // Stop the current stream if open
                        if *rm.is_open.lock() {
                            rm.stop_microphone_stream();
                        }

                        // Only show USB-cycling overlay if user was actively recording.
                        // If the stream died during sleep/wake while not recording,
                        // silently recover in the background without showing the visualizer.
                        if was_recording {
                            utils::show_usb_cycling_overlay(&app_handle);
                            crate::tray::set_tray_state(
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

                            crate::tray::set_tray_state(
                                &app_handle,
                                crate::tray::TrayIconState::Idle,
                            );
                        } else {
                            crate::tray::set_tray_state(
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

                // ── Zero-RMS dead-recording abort ────────────────────────
                // A zombie standing stream can keep *delivering* digitally
                // silent chunks (dock power-loss re-enumeration), so chunk
                // recency stays green while an armed recording captures
                // nothing. An armed recording past ZERO_SAMPLE_ABORT_MS with
                // max-RMS at digital silence is dead regardless of
                // heartbeats: abort it, finalize the session, rebuild the
                // stream, and retry once (Toggle only).
                let armed = capture.armed.load(Ordering::Acquire);
                if armed {
                    let armed_at = capture.armed_at_ms.load(Ordering::Acquire);
                    let now_ms = std::time::SystemTime::now()
                        .duration_since(std::time::SystemTime::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64;
                    let armed_elapsed_ms = now_ms.saturating_sub(armed_at);
                    let max_level = capture.get_max_level();

                    // An armed recording that is actually delivering re-arms
                    // the one-shot auto-retry budget (covers the
                    // mid-recording dock power-loss case).
                    if max_level > ZERO_SAMPLE_MAX_RMS {
                        if let Some(rm) = app_handle.try_state::<Arc<AudioRecordingManager>>() {
                            rm.auto_retry_allowed.store(true, Ordering::Release);
                        }
                    }
                    let pre_arm_rms = capture.pre_arm_max_rms();
                    // Dock-flap case: if an input device change happened
                    // recently, the stream may have been silent since BEFORE
                    // arming (no nonzero pre-arm window to observe), so the
                    // device-change recency substitutes for the pre-arm gate.
                    let device_changed_recently = app_handle
                        .try_state::<Arc<AudioRecordingManager>>()
                        .map(|rm| {
                            device_change_recent(
                                rm.last_input_change_epoch_ms.load(Ordering::Acquire),
                                now_ms,
                                DEVICE_FLAP_RECENT_WINDOW_MS,
                            )
                        })
                        .unwrap_or(false);

                    // Race guard: re-read armed + armed_at before acting so a
                    // recording the user already stopped (or a newer
                    // recording that re-stamped armed_at_ms) is not aborted.
                    if should_abort_dead_recording(
                        armed,
                        armed_elapsed_ms,
                        max_level,
                        pre_arm_rms,
                        device_changed_recently,
                    ) && capture.armed.load(Ordering::Acquire)
                        && capture.armed_at_ms.load(Ordering::Acquire) == armed_at
                    {
                        warn!(
                            "Liveness monitor: armed recording captured only digital silence for {}ms (max_rms {:.2e}) — aborting dead recording",
                            armed_elapsed_ms, max_level
                        );
                        logging::emit(logging::AppEvent::WakeRecovery {
                            stage: "zero_sample_abort".to_string(),
                            reason: format!("{}ms armed, max_rms {}", armed_elapsed_ms, max_level),
                            device_seen_secs: None,
                        });

                        // Capture the binding before cancel resets the
                        // coordinator to Idle, so the retry reuses the same
                        // hotkey the user pressed.
                        let binding_id = app_handle
                            .try_state::<crate::TranscriptionCoordinator>()
                            .map(|c| c.get_state())
                            .and_then(|state| match state {
                                crate::transcription_coordinator::AppState::Recording {
                                    binding_id,
                                    ..
                                } => Some(binding_id),
                                _ => None,
                            });

                        // Final revalidation immediately before cancelling: a
                        // user stop (or a newer recording) that landed since
                        // the predicate read must not be clobbered by this
                        // stale abort.
                        let still_armed = capture.armed.load(Ordering::Acquire)
                            && capture.armed_at_ms.load(Ordering::Acquire) == armed_at;
                        if !still_armed {
                            debug!("Liveness monitor: recording ended before abort — skipping");
                        } else {
                            // Cancel FIRST: this is what un-arms the capture
                            // and hides the overlay. Watchdog accounting goes
                            // after because it can block 6-15s in a power
                            // cycle; a user stop + re-press during that window
                            // must not be cancelled by a stale abort. The
                            // monitor thread holds no manager locks here, so
                            // this cannot deadlock with cancel's lock
                            // acquisitions.
                            crate::utils::cancel_current_operation(&app_handle);

                            // THEN count the dead recording with the watchdog
                            // (may cycle per the existing gates: threshold,
                            // bypass, cooldown, cycling guard). max_level is
                            // 0.0 by construction — the abort predicate only
                            // fires on digital silence.
                            usb_watchdog.on_recording_finished(
                                0,
                                armed_elapsed_ms as f32 / 1000.0,
                                0.0,
                            );

                            // Rebuild the stream on the (likely re-enumerated)
                            // device, then auto-retry once (Toggle only) so the
                            // user does not have to press again.
                            let rebuilt = app_handle
                                .try_state::<Arc<AudioRecordingManager>>()
                                .map(|rm| rm.restart_microphone_if_needed())
                                .unwrap_or_else(|| {
                                    Err(anyhow::anyhow!("AudioRecordingManager not available"))
                                });

                            let settings = get_settings(&app_handle);
                            let binding = binding_id.unwrap_or_else(|| "transcribe".to_string());
                            let hotkey = settings
                                .bindings
                                .get(&binding)
                                .map(|b| b.current_binding.clone())
                                .filter(|h| !h.is_empty());

                            match rebuilt {
                                Ok(())
                                    if settings.shortcut_activation
                                        == crate::settings::ShortcutActivation::Toggle =>
                                {
                                    if let Some(hotkey) = hotkey {
                                        // Re-check the coordinator is Idle
                                        // immediately before dispatching: a
                                        // user re-press during the watchdog
                                        // accounting window may have already
                                        // started a new recording.
                                        let coordinator_idle = app_handle
                                            .try_state::<crate::TranscriptionCoordinator>()
                                            .map(|c| {
                                                matches!(
                                                    c.get_state(),
                                                    crate::transcription_coordinator::AppState::Idle { .. }
                                                )
                                            })
                                            .unwrap_or(false);
                                        // One-shot budget: only a recording
                                        // that delivered samples re-arms it,
                                        // so a still-dead stream cannot loop
                                        // synthetic presses.
                                        let budget_available = app_handle
                                            .try_state::<Arc<AudioRecordingManager>>()
                                            .map(|rm| rm.auto_retry_allowed.load(Ordering::Acquire))
                                            .unwrap_or(false);
                                        if retry_decision(budget_available, coordinator_idle) {
                                            // Consume the budget only when the
                                            // press actually dispatches.
                                            if let Some(rm) =
                                                app_handle.try_state::<Arc<AudioRecordingManager>>()
                                            {
                                                rm.auto_retry_allowed
                                                    .store(false, Ordering::Release);
                                            }
                                            logging::emit(logging::AppEvent::WakeRecovery {
                                                stage: "retry_started".to_string(),
                                                reason: format!(
                                                    "rebuilt stream, retrying binding '{}'",
                                                    binding
                                                ),
                                                device_seen_secs: None,
                                            });
                                            info!(
                                                "Liveness monitor: retrying recording via synthetic press on '{}'",
                                                binding
                                            );
                                            // NEVER inline on this thread:
                                            // handle_shortcut_event can block on the
                                            // coordinator channel and keyboard loops
                                            // (2026-10-17 lesson).
                                            let ah = app_handle.clone();
                                            tauri::async_runtime::spawn_blocking(move || {
                                                crate::shortcut::handler::handle_shortcut_event(
                                                    &ah, &binding, &hotkey, true,
                                                );
                                            });
                                        } else {
                                            warn!(
                                                "Liveness monitor: auto-retry budget unavailable or coordinator busy — notifying instead"
                                            );
                                            crate::actions::router::send_macos_notification(
                                                &app_handle,
                                                "Handy",
                                                "Microphone did not recover — press to try again",
                                            );
                                        }
                                    } else {
                                        warn!(
                                            "Liveness monitor: no hotkey bound to '{}' — cannot auto-retry",
                                            binding
                                        );
                                        crate::actions::router::send_macos_notification(
                                            &app_handle,
                                            "Handy",
                                            "Microphone did not recover — press to try again",
                                        );
                                    }
                                }
                                Ok(()) => {
                                    // PushToTalk / HoldOrToggle: a synthetic
                                    // press cannot drive a hold, so notify only.
                                    crate::actions::router::send_macos_notification(
                                        &app_handle,
                                        "Handy",
                                        "Microphone did not recover — press to try again",
                                    );
                                }
                                Err(e) => {
                                    logging::emit(logging::AppEvent::WakeRecovery {
                                        stage: "retry_failed".to_string(),
                                        reason: format!("stream rebuild failed: {}", e),
                                        device_seen_secs: None,
                                    });
                                    error!(
                                        "Liveness monitor: post-abort stream rebuild failed: {}",
                                        e
                                    );
                                    crate::actions::router::send_macos_notification(
                                        &app_handle,
                                        "Handy",
                                        "Microphone did not recover — press to try again",
                                    );
                                }
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
        // Bind the handle out of the mutex first, then join — no guard held
        // across the join (same pattern as stop_device_monitor).
        let handle = self.liveness_monitor.lock().take();
        if let Some(handle) = handle {
            let _ = handle.join();
        }
    }

    /* ---------- device monitoring ------------------------------------------- */

    /// Start a background thread that polls the audio device lists every 2
    /// seconds and emits `device-list-changed` events when devices are
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
        let initial_input = handy_recorder::list_input_devices()
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
                    let current_input: Vec<String> = match handy_recorder::list_input_devices() {
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

                    // Compare against the previous lists
                    let (added_input, removed_input) = {
                        let prev = prev_input.lock();
                        (
                            current_input
                                .iter()
                                .filter(|d| !prev.contains(d))
                                .cloned()
                                .collect::<Vec<_>>(),
                            prev.iter()
                                .filter(|d| !current_input.contains(d))
                                .cloned()
                                .collect::<Vec<_>>(),
                        )
                    };

                    let (added_output, removed_output) = {
                        let prev = prev_output.lock();
                        (
                            current_output
                                .iter()
                                .filter(|d| !prev.contains(d))
                                .cloned()
                                .collect::<Vec<_>>(),
                            prev.iter()
                                .filter(|d| !current_output.contains(d))
                                .cloned()
                                .collect::<Vec<_>>(),
                        )
                    };

                    if !added_input.is_empty()
                        || !removed_input.is_empty()
                        || !added_output.is_empty()
                        || !removed_output.is_empty()
                    {
                        info!(
                            "Device change detected: +{:?} -{:?} (input), +{:?} -{:?} (output)",
                            added_input, removed_input, added_output, removed_output
                        );

                        let event = DeviceListChanged {
                            added_input: added_input.clone(),
                            removed_input: removed_input.clone(),
                            current_input: current_input.clone(),
                            added_output: added_output.clone(),
                            removed_output: removed_output.clone(),
                            current_output: current_output.clone(),
                        };

                        if let Err(e) = app_handle.emit("device-list-changed", event) {
                            warn!("Failed to emit device-list-changed event: {}", e);
                        }

                        // Update the previous lists
                        *prev_input.lock() = current_input;
                        *prev_output.lock() = current_output;

                        // Device list changed — if we have an open stream and
                        // its device disappeared, the crate's failure handler
                        // has/will mark it; the liveness monitor rebuilds.

                        // An INPUT device change (dock re-enumeration,
                        // unplug/replug) can leave the standing stream bound
                        // to a dead device while every recency-based liveness
                        // layer still reports healthy (chunks keep trickling
                        // or the stream idles as "open"). Rebuild the standing
                        // stream so the next dictation arms on fresh audio.
                        if !added_input.is_empty() || !removed_input.is_empty() {
                            // Resolve the managed instance instead of owning a
                            // clone: a by-value clone runs the full Drop when
                            // this thread ends, killing both monitors.
                            if let Some(rm) =
                                app_handle.try_state::<Arc<AudioRecordingManager>>()
                            {
                                let now_ms = epoch_ms();

                                // Stamp the flap time BEFORE any gating: the
                                // zero-RMS abort's device_changed_recently
                                // gate must see it even when the rebuild is
                                // rate-limited or a recording is active.
                                rm.last_input_change_epoch_ms
                                    .store(now_ms, Ordering::Release);

                                // Arm the one-shot wake bypass on a flap so
                                // the FIRST post-flap dead dictation cycles
                                // immediately (exact zeros are decisive; the
                                // 10s duration gates never see these short
                                // presses). Settings-gated like the wake
                                // path; rate-limited to one arm per burst.
                                let settings = get_settings_safe(&app_handle);
                                let flap_gated = settings.usb_watchdog_enabled
                                    && settings.usb_watchdog_cycle_on_wake
                                    && !settings.usb_watchdog_device_name.is_empty();
                                if flap_gated && should_arm_flap_bypass(
                                    rm.last_flap_bypass_arm_epoch_ms.load(Ordering::Acquire),
                                    now_ms,
                                ) {
                                    rm.last_flap_bypass_arm_epoch_ms
                                        .store(now_ms, Ordering::Release);
                                    // Quiet arm: this call site emits the
                                    // one distinguishable event below.
                                    rm.usb_watchdog
                                        .arm_wake_bypass_quiet(DEVICE_FLAP_BYPASS_TTL_SECS);
                                    logging::emit(logging::AppEvent::WakeRecovery {
                                        stage: "device_flap_bypass_armed".to_string(),
                                        reason: format!(
                                            "input devices changed: +{} -{} (arming {}s one-shot bypass)",
                                            added_input.len(),
                                            removed_input.len(),
                                            DEVICE_FLAP_BYPASS_TTL_SECS
                                        ),
                                        device_seen_secs: None,
                                    });
                                }

                                let last = rm
                                    .last_standing_rebuild_epoch
                                    .load(Ordering::Acquire);
                                let elapsed_ms = now_ms.saturating_sub(last);
                                let recording = rm.recording_active.load(Ordering::Acquire);

                                if should_rebuild_standing_stream(elapsed_ms, recording) {
                                    rm.last_standing_rebuild_epoch
                                        .store(now_ms, Ordering::Release);
                                    logging::emit(logging::AppEvent::WakeRecovery {
                                        stage: "device_list_rebuild".to_string(),
                                        reason: format!(
                                            "input devices changed: +{} -{} (idle {}ms, rebuilding standing stream)",
                                            added_input.len(),
                                            removed_input.len(),
                                            elapsed_ms
                                        ),
                                        device_seen_secs: None,
                                    });
                                    info!(
                                        "Device monitor: input list changed while idle — rebuilding standing stream"
                                    );
                                    // No hub cycling here: the change itself is
                                    // the evidence the device tree moved; a fresh
                                    // open is enough.
                                    rm.invalidate_device_cache();
                                    if let Err(e) = rm.restart_microphone_if_needed() {
                                        warn!(
                                            "Device monitor: standing stream rebuild failed: {}",
                                            e
                                        );
                                    }
                                }
                            } else {
                                debug!(
                                    "Device monitor: manager not managed yet — skipping standing-stream rebuild"
                                );
                            }
                        }
                    }
                }
            });

        *self.device_monitor.lock() = Some(handle.expect("failed to spawn device monitor"));
    }

    /// Stop the device monitor thread
    fn stop_device_monitor(&self) {
        // Drop the sender to signal the thread to stop
        *self.device_monitor_stop.lock() = None;
        // Bind the handle out of the mutex FIRST, then join: keeping the
        // parking_lot guard alive across join() blocks any other thread
        // touching this mutex for the whole (bounded but real) join.
        let handle = self.device_monitor.lock().take();
        if let Some(handle) = handle {
            let _ = handle.join();
        }
    }

    /* ---------- device resolution ------------------------------------------- */

    fn is_bluetooth_output_active(&self) -> bool {
        let settings = get_settings(&self.app_handle);
        is_bluetooth_audio_active(settings.selected_output_device.as_deref())
    }

    /// The persisted microphone preference currently in effect. Only runs the
    /// clamshell probe (an `ioreg` subprocess, ~10-20ms) when a clamshell
    /// microphone is actually configured.
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
                    from_cache: false,
                    unavailable_selected_microphone: None,
                };
            }
            DesiredMicrophone::Selected(name) => (name.clone(), Some(name)),
            DesiredMicrophone::Clamshell(name) => (name, None),
        };

        // Cache hit: skip the enumeration. A stale ID (device unplugged)
        // fails at open, where the caller invalidates and retries fresh.
        if let Some((cached_name, id)) = self.cached_device.lock().clone().as_ref() {
            if *cached_name == device_name {
                debug!("device resolve: cache hit for '{}'", device_name);
                return MicrophoneResolution {
                    device: Some(id.clone()),
                    from_cache: true,
                    unavailable_selected_microphone: None,
                };
            }
        }

        // Only report a selected microphone as unavailable when enumeration
        // itself succeeded. A backend enumeration error may be transient and
        // must not erase the user's persisted preference.
        let enumerate_started = Instant::now();
        let (device, enumeration_succeeded) = match handy_recorder::list_input_devices() {
            Ok(devices) => (
                devices
                    .into_iter()
                    .find(|d| d.name == device_name)
                    .map(|d| d.id),
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
        if let Some(id) = &device {
            *self.cached_device.lock() = Some((device_name, id.clone()));
        }

        let unavailable_selected_microphone = if enumeration_succeeded && device.is_none() {
            selected_microphone
        } else {
            None
        };
        MicrophoneResolution {
            device,
            from_cache: false,
            unavailable_selected_microphone,
        }
    }

    /// Keep persisted settings and the UI aligned with a successful runtime
    /// fallback. Re-read first so recovery cannot clear a microphone the user
    /// selected concurrently while the stream was being rebuilt.
    fn persist_default_microphone_after_fallback(&self, unavailable_name: &str) {
        let mut cleared = false;
        crate::settings::patch_settings(&self.app_handle, |s| {
            if s.selected_microphone.as_deref() == Some(unavailable_name) {
                s.selected_microphone = None;
                cleared = true;
            }
        });
        if cleared {
            let _ = self.app_handle.emit(
                "settings-changed",
                serde_json::json!({
                    "setting": "selected_microphone",
                    "value": "Default"
                }),
            );
        }
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
            let state = rm.state.lock();
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

    /* ---------- VAD --------------------------------------------------------- */

    pub fn preload_vad(&self) -> Result<(), anyhow::Error> {
        let mut vad = self.vad.lock();
        if matches!(*vad, VadSlot::Unloaded) {
            let backend = get_settings(&self.app_handle).vad_backend;
            *vad = VadSlot::Idle(create_capture_vad(backend, &self.app_handle)?);
        }
        Ok(())
    }

    /// Samples per VAD frame: the chunk size the microphone is opened with.
    fn vad_frame_samples(&self) -> Result<usize, anyhow::Error> {
        self.preload_vad()?;
        match &*self.vad.lock() {
            VadSlot::Idle(vad) => Ok(vad.frame_samples()),
            VadSlot::Lent { frame_samples } => Ok(*frame_samples),
            VadSlot::Unloaded => unreachable!("preload_vad just loaded it"),
        }
    }

    /// Lends the detector to a new recording. Called only while idle.
    fn lend_vad(&self) -> Result<CaptureVad, anyhow::Error> {
        {
            let mut slot = self.vad.lock();
            // No recording is active, so a lent detector went with a lost sink.
            if matches!(*slot, VadSlot::Lent { .. }) {
                warn!("The last recording did not return its VAD detector; reloading it");
                *slot = VadSlot::Unloaded;
            }
        }
        self.preload_vad()?;
        let mut slot = self.vad.lock();
        match std::mem::replace(&mut *slot, VadSlot::Unloaded) {
            VadSlot::Idle(vad) => {
                *slot = VadSlot::Lent {
                    frame_samples: vad.frame_samples(),
                };
                Ok(vad)
            }
            other => {
                *slot = other;
                Err(anyhow::anyhow!("VAD is not available"))
            }
        }
    }

    fn return_vad(&self, vad: CaptureVad) {
        *self.vad.lock() = VadSlot::Idle(vad);
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

        // Lock order: is_open before mute_state (matches stop_microphone_stream).
        let is_open = self.is_open.lock();
        let mut mute_guard = self.mute_state.lock();
        // Already muted this session — don't re-snapshot, or a duplicate/late
        // apply would overwrite prev_muted with our own forced-muted state and
        // strand audio muted on stop.
        if mute_guard.did_mute {
            return;
        }
        if *is_open {
            mute_guard.prev_muted = get_mute();
            set_mute(true);
            mute_guard.did_mute = true;
            debug!("Mute applied (prev_muted={:?})", mute_guard.prev_muted);
        }
    }

    /// Removes mute if it was applied, restoring the system's prior mute state
    /// (a system already muted before recording stays muted).
    pub fn remove_mute(&self) {
        let mut mute_guard = self.mute_state.lock();
        if mute_guard.did_mute {
            restore_mute(mute_guard.prev_muted);
            mute_guard.did_mute = false;
            debug!(
                "Mute removed (restored prev_muted={:?})",
                mute_guard.prev_muted
            );
        }
    }

    /// Whether a standing (idle but sink-running) capture should exist right
    /// now: always-on/BT mode plus a reason to consume idle audio — the
    /// pre-hotkey buffer or the batch live-captions fallback.
    fn wants_standing_capture(&self) -> bool {
        let is_always_on = matches!(*self.mode.lock(), MicrophoneMode::AlwaysOn);
        let bt = self.bt_keep_alive.load(Ordering::Acquire);
        if !is_always_on && !bt {
            return false;
        }
        let pre_buffer = self.capture.pre_buffer_max_samples > 0;
        let batch_captions =
            self.capture.streaming_enabled.load(Ordering::Acquire) && !self.router_attached;
        pre_buffer || batch_captions
    }

    /// Bring the standing capture in line with the current mode/settings.
    /// Starts one when idle audio must be consumed, stops it when not. The
    /// stream must already be open; `start_microphone_stream` calls this on
    /// its way out, so a closed stream just means "nothing to do yet".
    fn refresh_standing_capture(&self) {
        if self.recording_active.load(Ordering::Acquire) {
            return; // A hotkey recording owns the recorder.
        }
        if !self.wants_standing_capture() {
            if self.standing_active.load(Ordering::Acquire) {
                debug!("Standing capture no longer needed — stopping it");
                if let Some(microphone) = self.recorder.lock().as_ref() {
                    let _ = self.collect_recording(microphone); // Discard
                }
                self.standing_active.store(false, Ordering::Release);
                self.capture.disarm();
            }
            return;
        }

        if self.standing_active.load(Ordering::Acquire) || !*self.is_open.lock() {
            return;
        }

        let start = Instant::now();
        // Fresh standing sink: borrow the detector, reset per-session state.
        let vad = match self.lend_vad() {
            Ok(vad) => vad,
            Err(e) => {
                debug!("Standing capture: VAD unavailable: {e}");
                return;
            }
        };
        *self.capture.policy.lock() = VadPolicy::Offline;
        self.capture.disarm();
        self.capture.processed.lock().clear();

        let sink = CaptureSink::new(
            vad,
            CaptureSinkConfig {
                on_levels: Arc::clone(&self.on_levels),
                on_audio: Arc::clone(&self.on_audio),
                on_raw_frame: None, // Router feed attaches with hotkey sinks.
                noise_suppressor: self.noise_suppressor(),
                control: Arc::clone(&self.capture),
                emergency_backup: None,
            },
        );

        let recorder = self.recorder.lock();
        let Some(microphone) = recorder.as_ref() else {
            self.return_vad(sink.into_vad());
            return;
        };
        match microphone.recorder.start(sink) {
            Ok(()) => {
                self.standing_active.store(true, Ordering::Release);
                debug!(
                    "Standing capture started in {:?} (pre_buffer={}ms, batch_captions={})",
                    start.elapsed(),
                    self.capture.pre_buffer_max_samples / 16,
                    self.capture.streaming_enabled.load(Ordering::Acquire) && !self.router_attached
                );
            }
            Err(StartError { error, sink }) => {
                self.return_vad(sink.into_vad());
                debug!("Standing capture start failed: {error}");
            }
        }
    }

    /// A fresh noise suppressor for one sink, when the setting is on.
    fn noise_suppressor(&self) -> Option<Arc<Mutex<crate::audio_toolkit::NoiseSuppressor>>> {
        let settings = get_settings(&self.app_handle);
        if settings.noise_suppression_enabled {
            Some(Arc::new(Mutex::new(
                crate::audio_toolkit::NoiseSuppressor::new(settings.noise_suppression_level),
            )))
        } else {
            None
        }
    }

    pub fn start_microphone_stream(&self) -> Result<(), anyhow::Error> {
        let mut open_flag = self.is_open.lock();
        if *open_flag {
            // `is_open` only records that we opened a stream, not that it is
            // still running. If the recorder has since failed (mic unplugged,
            // stalled, permission revoked), rebuild it before the next
            // recording instead of handing the caller a failed recorder.
            let needs_reopen = self
                .recorder
                .lock()
                .as_ref()
                .is_none_or(OpenMicrophone::needs_reopen);

            if !needs_reopen {
                // trace, not debug: with the aliveness check in
                // try_start_recording this now fires on every keypress in
                // always-on mode.
                trace!("Microphone stream already active");
                drop(open_flag);
                self.refresh_standing_capture();
                // The stream opened, but delivery is unproven: clear
                // post-cycle grace only. The failure counter resets solely
                // via on_recording_finished(>0) — an open must not erase a
                // real failure the threshold has not reached yet.
                self.usb_watchdog.on_stream_open_confirmed();
                return Ok(());
            }

            warn!("Microphone stream failed; reopening");

            // Torn down inline rather than via stop_microphone_stream(), which
            // takes the `is_open` lock we are already holding.
            {
                let mut mute_guard = self.mute_state.lock();
                if mute_guard.did_mute {
                    restore_mute(mute_guard.prev_muted);
                    mute_guard.did_mute = false;
                }
            }
            self.standing_active.store(false, Ordering::Release);
            let failed = self.recorder.lock().take();
            if let Some(microphone) = failed {
                self.close_microphone(microphone);
            }
            *open_flag = false;
            self.invalidate_device_cache();
            // Fall through to the same fresh resolution and fallback path used
            // when an on-demand stream opens after its device was unplugged.
        }

        let start_time = Instant::now();

        // Don't mute immediately - caller will handle muting after audio feedback.
        // The previous stream restored audio on close, so did_mute should already
        // be false here; if it somehow isn't, restore rather than just clearing the
        // flag, which would strand system audio muted.
        {
            let mut mute_guard = self.mute_state.lock();
            if mute_guard.did_mute {
                restore_mute(mute_guard.prev_muted);
                mute_guard.did_mute = false;
            }
        }

        // Get the selected device from settings, considering clamshell mode.
        // No pre-flight enumeration here: when nothing is configured the
        // recorder resolves the system default itself, and a machine with no
        // input devices at all fails inside open() with `DeviceUnavailable`.
        let settings = get_settings(&self.app_handle);
        let resolve_started = Instant::now();
        let mut resolution = self.resolve_microphone_device(&settings);
        let resolve_elapsed = resolve_started.elapsed();

        // Ensure VAD is loaded if it wasn't for whatever reason; its frame
        // size is the chunk size.
        let vad_started = Instant::now();
        let frames_per_chunk = self.vad_frame_samples()?;
        let vad_elapsed = vad_started.elapsed();

        let channel = *self.selected_channel.lock();
        let open_started = Instant::now();
        let open_result = match open_microphone_on_channel(
            resolution.device.clone(),
            channel,
            frames_per_chunk,
        ) {
            Err(first_err) if resolution.from_cache => {
                // The cached device ID may have gone stale (unplugged).
                // Re-resolve from a fresh enumeration and retry once before
                // surfacing the error.
                warn!("Recorder open failed ({first_err}); re-resolving device and retrying once");
                self.invalidate_device_cache();
                resolution = self.resolve_microphone_device(&settings);
                open_microphone_on_channel(resolution.device.clone(), channel, frames_per_chunk)
            }
            result => result,
        };

        // Diagnostics only: selected mic disappeared and we silently fell back
        // to the default. Do NOT count this as an open failure — a later
        // successful recording resets the counter, and direct-cycling on
        // fallback would over-cycle during dock/clamshell transitions.
        if let Some(ref unavailable_name) = resolution.unavailable_selected_microphone {
            if settings.usb_watchdog_enabled && !settings.usb_watchdog_device_name.is_empty() {
                warn!(
                    "Selected microphone '{}' is missing; watchdog enabled for '{}', falling back to default mic without cycling",
                    unavailable_name,
                    settings.usb_watchdog_device_name
                );
            }
        }

        let microphone = match open_result {
            Ok(m) => m,
            Err(e) => {
                // Count this as a mic-open failure for the USB watchdog threshold.
                let cycled = self.usb_watchdog.on_mic_open_failed();
                if cycled {
                    warn!("USB watchdog power-cycled hub after mic open failure; retrying once");
                    self.invalidate_device_cache();
                    resolution = self.resolve_microphone_device(&settings);
                    open_microphone_on_channel(
                        resolution.device.clone(),
                        channel,
                        frames_per_chunk,
                    )?
                } else {
                    return Err(e.into());
                }
            }
        };
        debug!(
            "mic stream breakdown: device_resolve={:?} vad_ensure={:?} open={:?}",
            resolve_elapsed,
            vad_elapsed,
            open_started.elapsed()
        );
        *self.recorder.lock() = Some(microphone);

        // The stream opened, but delivery is unproven: clear post-cycle
        // grace only. The failure counter resets solely via
        // on_recording_finished(>0) — an open must not erase a real failure
        // the threshold has not reached yet.
        self.usb_watchdog.on_stream_open_confirmed();

        *open_flag = true;
        if let Some(unavailable_name) = resolution.unavailable_selected_microphone {
            // Do this only after the default stream opened successfully. A
            // failed fallback must not erase the user's microphone preference.
            self.persist_default_microphone_after_fallback(&unavailable_name);
        }
        // The stream is running, but the device may not be delivering audio
        // yet (Bluetooth can take seconds); a recording's readiness waits for
        // its first chunk.
        info!(
            "Microphone stream initialized in {:?}",
            start_time.elapsed()
        );
        drop(open_flag);
        self.refresh_standing_capture();
        Ok(())
    }

    pub fn stop_microphone_stream(&self) {
        let mut open_flag = self.is_open.lock();
        if !*open_flag {
            return;
        }

        {
            let mut mute_guard = self.mute_state.lock();
            if mute_guard.did_mute {
                restore_mute(mute_guard.prev_muted);
            }
            mute_guard.did_mute = false;
        }

        self.standing_active.store(false, Ordering::Release);
        let microphone = self.recorder.lock().take();
        if let Some(microphone) = microphone {
            self.close_microphone(microphone);
        }

        *open_flag = false;
        debug!("Microphone stream stopped");
    }

    /// Closes a microphone, first discarding any recording on it (its
    /// detector comes back).
    fn close_microphone(&self, microphone: OpenMicrophone) {
        let mut is_recording = self.is_recording.lock();
        if *is_recording {
            let _ = self.collect_recording(&microphone);
            *is_recording = false;
        }
        if let Err(e) = microphone.recorder.close() {
            warn!("Closing the microphone failed: {e}");
        }
    }

    /// Stops the recording on `microphone`, returning its captured audio and
    /// taking back the detector it borrowed.
    fn collect_recording(&self, microphone: &OpenMicrophone) -> (Vec<f32>, Vec<f32>) {
        match microphone.recorder.stop() {
            Ok(stopped) => {
                match &stopped.end_reason {
                    EndReason::StopCalled => {}
                    EndReason::RecorderFailed(error) => {
                        microphone.failed.store(true, Ordering::Release);
                        warn!("Recording cut short by a microphone failure; keeping the audio before it: {error}");
                    }
                    EndReason::SinkPanicked(message) => {
                        error!("Recording cut short; audio processing panicked: {message}");
                    }
                }
                if stopped.dropped_frames > 0 {
                    warn!(
                        "Recording completed after dropping {} microphone frames",
                        stopped.dropped_frames
                    );
                }
                let captured = stopped.sink.finish();
                self.return_vad(captured.vad);
                (captured.samples, captured.raw_samples)
            }
            Err(e) => {
                // The recording and its detector are lost with the sink.
                error!("Failed to stop recording: {e}");
                if e.kind() != ErrorKind::NotRecording {
                    microphone.failed.store(true, Ordering::Release);
                }
                *self.vad.lock() = VadSlot::Unloaded;
                (Vec::new(), Vec::new())
            }
        }
    }

    /* ---------- mode switching --------------------------------------------- */

    pub fn update_mode(&self, new_mode: MicrophoneMode) -> Result<(), anyhow::Error> {
        let cur_mode = self.mode.lock().clone();

        match (cur_mode, &new_mode) {
            (MicrophoneMode::AlwaysOn, MicrophoneMode::OnDemand) => {
                if matches!(*self.state.lock(), RecordingState::Idle)
                    && !self.bt_keep_alive.load(Ordering::Acquire)
                {
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

        *self.mode.lock() = new_mode;
        self.refresh_standing_capture();
        Ok(())
    }

    /// Tear down and reopen the microphone stream, discarding any stale
    /// device state. Kept under its fork name for the shortcut/USB paths.
    pub fn recreate_recorder(&self) -> Result<(), anyhow::Error> {
        self.invalidate_device_cache();
        if *self.is_open.lock() {
            self.stop_microphone_stream();
        }
        self.start_microphone_stream()
    }

    /// Re-evaluate the standing capture after a settings change that affects
    /// it (pre-buffer size, live captions toggle, BT keep-alive refresh).
    #[allow(dead_code)] // settings-change hook; the live toggle is set_streaming_enabled
    pub fn refresh_capture_configuration(&self) {
        let settings = get_settings(&self.app_handle);
        self.capture
            .streaming_enabled
            .store(settings.live_captions_enabled, Ordering::Release);
        self.refresh_standing_capture();
    }

    /* ---------- recording --------------------------------------------------- */

    /// The one place `state` is written. Derives `recording_active` (the
    /// lock-free mirror read by `is_recording()`) from the new value itself,
    /// so the two can never drift: a new `RecordingState` variant only needs
    /// its active-set membership decided here, once.
    fn set_state(&self, guard: &mut RecordingState, new_state: RecordingState) {
        *guard = new_state;
        self.recording_active.store(
            matches!(
                *guard,
                RecordingState::Recording { .. } | RecordingState::Stopping
            ),
            Ordering::SeqCst,
        );
    }

    /// Arm the shared capture state for a hotkey recording: per-recording
    /// counters reset, the batch buffer clears, the emergency backup starts,
    /// and the readiness sender is installed for the next armed chunk.
    fn arm_capture(&self, binding_id: &str, vad_policy: VadPolicy) {
        *self.capture.policy.lock() = vad_policy;
        self.capture.arm(binding_id);
        self.batch_buffer.lock().clear();
        if let Some(backup) = crate::emergency_save::get_emergency_backup() {
            backup.start_recording(binding_id);
        }
    }

    /// Install a fresh readiness sender and hand back its receiver. MUST run
    /// before `arm_capture`: the sink fires the sender on the first armed
    /// chunk, so the sender has to already be in place when `armed` flips.
    fn install_readiness(&self) -> mpsc::Receiver<()> {
        let (tx, rx) = mpsc::channel();
        *self.capture.ready.lock() = Some(tx);
        rx
    }

    pub fn try_start_recording(
        &self,
        binding_id: &str,
        vad_policy: VadPolicy,
    ) -> Result<RecordingReadiness, anyhow::Error> {
        let mut state = self.state.lock();
        if !matches!(*state, RecordingState::Idle) {
            // If the state is Recording but the is_recording flag has been
            // cleared (e.g., by cancel_recording's lock-timeout escape hatch),
            // the state is stale and we self-recover to Idle before proceeding.
            if let RecordingState::Recording {
                binding_id: ref stale_binding,
                ..
            } = *state
            {
                if !*self.is_recording.lock() {
                    warn!(
                        "[RECOVERY] RecordingState is Recording but is_recording=false for binding {} — recovering to Idle",
                        stale_binding
                    );
                    self.set_state(&mut state, RecordingState::Idle);
                }
            }
            if !matches!(*state, RecordingState::Idle) {
                return Err(anyhow::anyhow!("Already recording"));
            }
        }

        // Cancel any pending lazy close (no-op in always-on mode, where
        // closes are never scheduled).
        self.close_generation.fetch_add(1, Ordering::SeqCst);

        let standing = self.standing_active.load(Ordering::Acquire);

        // A standing capture is the recording's sink already running; arming
        // it needs no stream work. Otherwise, open the stream (on-demand) or
        // run the cheap aliveness check (always-on), reopening on failure.
        if !standing {
            let mut reopened = false;
            loop {
                if let Err(e) = self.start_microphone_stream() {
                    error!("Failed to open microphone stream: {e}");
                    return Err(e);
                }

                // A standing capture may have just come up alongside the
                // stream (always-on + pre-buffer); prefer arming it over
                // stacking a second sink on the recorder.
                if self.standing_active.load(Ordering::Acquire) {
                    break;
                }

                let sink = CaptureSink::new(
                    self.lend_vad()?,
                    CaptureSinkConfig {
                        on_levels: Arc::clone(&self.on_levels),
                        on_audio: Arc::clone(&self.on_audio),
                        // Dedicated live-captions model: raw frames feed the
                        // StreamRouter pre-VAD (fork's live-captions seam).
                        on_raw_frame: self.raw_router_feed(),
                        noise_suppressor: self.noise_suppressor(),
                        control: Arc::clone(&self.capture),
                        emergency_backup: crate::emergency_save::get_emergency_backup().cloned(),
                    },
                );

                let recorder = self.recorder.lock();
                let Some(microphone) = recorder.as_ref() else {
                    self.return_vad(sink.into_vad());
                    return Err(anyhow::anyhow!("Recorder not available"));
                };
                match microphone.recorder.start(sink) {
                    Ok(()) => {
                        let generation = self.capture_generation.fetch_add(1, Ordering::AcqRel) + 1;
                        let receiver = self.install_readiness();
                        self.arm_capture(binding_id, vad_policy);
                        *self.is_recording.lock() = true;
                        self.set_state(
                            &mut state,
                            RecordingState::Recording {
                                binding_id: binding_id.to_string(),
                                start_time: Instant::now(),
                            },
                        );
                        debug!("Recording requested for binding {binding_id}");
                        return Ok(RecordingReadiness {
                            receiver,
                            generation,
                        });
                    }
                    Err(StartError { error, sink }) => {
                        self.return_vad(sink.into_vad());
                        if reopened || error.kind() == ErrorKind::AlreadyRecording {
                            return Err(
                                anyhow::Error::new(error).context("Failed to start recorder")
                            );
                        }
                        warn!("Microphone failed before this recording ({error}); reopening");
                        microphone.failed.store(true, Ordering::Release);
                        reopened = true;
                    }
                }
            }
        }

        // Standing capture path: arm it in place.
        let generation = self.capture_generation.fetch_add(1, Ordering::AcqRel) + 1;
        let receiver = self.install_readiness();
        self.arm_capture(binding_id, vad_policy);
        *self.is_recording.lock() = true;
        self.set_state(
            &mut state,
            RecordingState::Recording {
                binding_id: binding_id.to_string(),
                start_time: Instant::now(),
            },
        );
        debug!("Recording armed on standing capture for binding {binding_id}");
        Ok(RecordingReadiness {
            receiver,
            generation,
        })
    }

    /// The raw pre-VAD frame feed for a dedicated live-captions model.
    fn raw_router_feed(&self) -> Option<AudioFrameCallback> {
        let router = self
            .app_handle
            .try_state::<LiveCaptionsTranscriptionManager>()
            .map(|tm| tm.0.lock().stream_router())?;
        Some(Arc::new(move |frame: &[f32]| router.feed(frame)))
    }

    /// Replace the VAD implementation while idle. If the microphone stream is
    /// currently warm (always-on or lazy-close mode), reopen it with the new
    /// detector's chunk size before reporting success. A failed reopen restores
    /// the previous detector so the persisted setting can remain unchanged.
    pub fn update_vad_backend(&self, backend: VadBackend) -> Result<(), anyhow::Error> {
        let state = self.state.lock();
        if !matches!(*state, RecordingState::Idle) {
            return Err(anyhow::anyhow!(
                "Cannot change the VAD backend while recording"
            ));
        }

        let replacement = create_capture_vad(backend, &self.app_handle)?;
        let was_open = *self.is_open.lock();

        // Invalidate any delayed close before swapping the detector.
        self.close_generation.fetch_add(1, Ordering::SeqCst);
        if was_open {
            self.stop_microphone_stream();
        }

        let previous = std::mem::replace(&mut *self.vad.lock(), VadSlot::Idle(replacement));
        if was_open {
            if let Err(change_error) = self.start_microphone_stream() {
                *self.vad.lock() = previous;

                if let Err(rollback_error) = self.start_microphone_stream() {
                    error!(
                        "Failed to restore microphone stream after VAD backend change failed: {rollback_error}"
                    );
                }
                return Err(anyhow::anyhow!(
                    "Failed to reopen microphone with {:?} VAD: {change_error}",
                    backend
                ));
            }
        }

        info!("VAD backend changed to {:?}", backend);
        drop(state);
        Ok(())
    }

    pub fn update_selected_device(&self) -> Result<(), anyhow::Error> {
        // Device settings changed; re-enumerate the device and restart capture.
        self.invalidate_device_cache();
        let was_open = *self.is_open.lock();
        if was_open {
            self.close_generation.fetch_add(1, Ordering::SeqCst);
            self.stop_microphone_stream();
            self.start_microphone_stream()?;
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
        let state = self.state.lock();
        if !matches!(*state, RecordingState::Idle) {
            return Err(anyhow::anyhow!(
                "Cannot change the input channel while recording"
            ));
        }

        let was_open = *self.is_open.lock();
        if was_open {
            self.close_generation.fetch_add(1, Ordering::SeqCst);
            self.stop_microphone_stream();
        }
        let previous_channel =
            std::mem::replace(&mut *self.selected_channel.lock(), selected_channel);
        if was_open {
            if let Err(error) = self.start_microphone_stream() {
                *self.selected_channel.lock() = previous_channel;
                return Err(error);
            }
        }
        drop(state);
        Ok(())
    }

    /// Invalidate pending first-sample UI and audio-feedback work immediately.
    /// Called at the beginning of stop, before the slower capture drain starts.
    pub fn invalidate_recording_readiness(&self) {
        self.capture_generation.fetch_add(1, Ordering::AcqRel);
    }

    pub fn is_recording_readiness_current(&self, generation: u64) -> bool {
        self.capture_generation.load(Ordering::Acquire) == generation
    }

    #[allow(dead_code)] // upstream manager API; the fork's cancel paths snapshot inline
    pub fn cancel_generation(&self) -> u64 {
        self.cancel_generation.load(Ordering::Acquire)
    }

    pub fn was_cancelled_since(&self, generation: u64) -> bool {
        self.cancel_generation.load(Ordering::Acquire) != generation
    }

    pub fn stop_recording(&self, binding_id: &str) -> Option<(Vec<f32>, Vec<f32>)> {
        let t_start = std::time::Instant::now();
        debug!(
            "[STOP-DIAGNOSTIC] stop_recording started for binding '{}'",
            binding_id
        );

        // Cancellations that arrive from here on end the trailing window and
        // discard the captured samples (upstream's cancel-generation guard).
        let cancel_generation = self.cancel_generation.load(Ordering::Acquire);
        self.invalidate_recording_readiness();

        let state = self.state.lock();

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

                // Release the outer guard BEFORE the Stopping re-lock below:
                // parking_lot::Mutex is non-reentrant, so re-locking while the
                // match-scrutinee guard is still alive deadlocks every stop
                // (try_start_recording / cancel / is_recording_effective all
                // wedge behind it, leaving the mic stream stranded).
                drop(state);

                // Transition to Stopping: try_start_recording() must reject new
                // recordings while the smart-stop trailing window captures
                // trailing audio, and the lock-free mirror keeps that cheap.
                // The guard was released above, so another thread may have
                // changed the state between our snapshot and this re-lock:
                // re-verify it still describes our recording, and treat a
                // racing transition (cancel, second stop) like the mismatch
                // arms — bail without touching the new state.
                {
                    let mut state = self.state.lock();
                    match *state {
                        RecordingState::Recording {
                            binding_id: ref active,
                            ..
                        } if active == binding_id => {
                            self.set_state(&mut state, RecordingState::Stopping);
                        }
                        ref raced => {
                            warn!(
                                "stop_recording for '{}' raced a state change (now {:?}); not returning audio",
                                binding_id, raced
                            );
                            return None;
                        }
                    }
                }

                let settings = get_settings(&self.app_handle);

                // Smart-stop, re-expressed manager-side: the recording keeps
                // running while we hold a volume-aware trailing window, so
                // trailing consonants and breath tails reach the model. The
                // window closes early once the level has sat under the
                // noise-floor-derived voice threshold for
                // SILENCE_THRESHOLD_MS, never before MIN_TRAILING_BUFFER_MS.
                let buffer_ms = settings.extra_recording_buffer_ms;
                if buffer_ms > 0 {
                    self.capture.trailing.store(true, Ordering::Release);
                    debug!(
                        "Smart-stop: volume-aware trailing window (max {}ms)",
                        buffer_ms
                    );
                    let started = Instant::now();
                    let buffer = Duration::from_millis(buffer_ms);
                    while started.elapsed() < buffer {
                        if self.was_cancelled_since(cancel_generation) {
                            debug!("Recording stop cancelled during extra buffer");
                            break;
                        }
                        let elapsed_ms = started.elapsed().as_millis() as u64;
                        let quiet_for = (std::time::SystemTime::now()
                            .duration_since(std::time::SystemTime::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis() as u64)
                            .saturating_sub(
                                self.capture
                                    .last_voice_above_floor_ms
                                    .load(Ordering::Relaxed),
                            );
                        if elapsed_ms >= MIN_TRAILING_BUFFER_MS && quiet_for >= SILENCE_THRESHOLD_MS
                        {
                            debug!(
                                "Smart-stop: silence for {quiet_for}ms after {elapsed_ms}ms — closing trailing window"
                            );
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(25));
                    }
                    debug!("Smart-stop window held for {:?}", started.elapsed());
                }

                // Collect the recording (crate stop: pause-ack + full drain +
                // resampler tail — the deleted recorder's tail-audio handling).
                let (processed_samples, raw_samples) = match self.recorder.lock().as_ref() {
                    Some(microphone) => self.collect_recording(microphone),
                    None => {
                        error!("Recorder not available");
                        (Vec::new(), Vec::new())
                    }
                };
                self.standing_active.store(false, Ordering::Release);
                self.capture.disarm();

                // Now transition to Idle after the buffer is complete.
                {
                    let mut state = self.state.lock();
                    self.set_state(&mut state, RecordingState::Idle);
                }

                *self.is_recording.lock() = false;
                debug!(
                    "[STOP-DIAGNOSTIC] stop_recording returning {} processed / {} raw samples (total elapsed {:?})",
                    processed_samples.len(),
                    raw_samples.len(),
                    t_start.elapsed()
                );

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
                } else {
                    // Always-on: a standing capture may be wanted again.
                    self.refresh_standing_capture();
                }

                if self.was_cancelled_since(cancel_generation) {
                    debug!("Recording stop cancelled; discarding captured samples");
                    if let Some(backup) = crate::emergency_save::get_emergency_backup() {
                        let _ = backup.cancel_recording();
                    }
                    return None;
                }

                // Post-recording watchdog accounting. Three mutually
                // exclusive outcomes (never double-counted):
                // 1. exact digital zeros → dead signal, cycle now (no
                //    duration gate: even 0.3s of exact zeros is decisive;
                //    intentionally NOT counted as a successful finish);
                // 2. low but nonzero audio → ambiguous, keeps the 10s gate;
                // 3. anything else → normal delivered/empty accounting.
                let max_level = self.capture.get_max_level();

                if !processed_samples.is_empty() && max_level <= ZERO_SAMPLE_MAX_RMS {
                    warn!(
                        "Recording delivered exact digital zeros (max RMS {:.2e}, duration {:.1}s) - dead signal, requesting USB cycle",
                        max_level, recording_duration
                    );
                    if self
                        .usb_watchdog
                        .on_digital_silence(recording_duration, max_level)
                    {
                        if let Err(e) = self.restart_microphone_if_needed() {
                            error!(
                                "Failed to restart microphone after digital-silence USB cycle: {}",
                                e
                            );
                        }
                        crate::actions::router::send_macos_notification(
                            &self.app_handle,
                            "Handy",
                            "Microphone delivered no signal — USB port cycled",
                        );
                    }
                } else if !processed_samples.is_empty() && max_level < NO_AUDIO_THRESHOLD {
                    warn!(
                        "Recording had very low audio level (max RMS: {:.6}, threshold: {:.6}, duration: {:.1}s) - mic may be dead/muted",
                        max_level, NO_AUDIO_THRESHOLD, recording_duration
                    );
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
                    if !processed_samples.is_empty() {
                        // A recording that delivered samples re-arms the
                        // one-shot auto-retry budget consumed by the
                        // zero-RMS abort's synthetic press.
                        self.auto_retry_allowed.store(true, Ordering::Release);
                    }
                    if self.usb_watchdog.on_recording_finished(
                        processed_samples.len(),
                        recording_duration,
                        max_level,
                    ) {
                        if let Err(e) = self.restart_microphone_if_needed() {
                            error!(
                                "Failed to restart microphone after dead-stream USB cycle: {}",
                                e
                            );
                        }
                    }
                }

                // Emergency backup: the recording survived; finalize it.
                if let Some(backup) = crate::emergency_save::get_emergency_backup() {
                    let _ = backup.complete_recording();
                }

                // Pad short audio with silence at the BEGINNING to reduce Whisper
                // hallucinations and prevent clipping of the first words.
                // Every model now loads through transcribe-cpp (the GGUF/GGML
                // path) since the ONNX engines were retired, so the pad applies
                // to any selected model known to the registry; an unknown model
                // keeps unpadded audio, as before.
                let s_len = processed_samples.len();
                let min_samples = WHISPER_SAMPLE_RATE * 3; // 3 seconds minimum
                let is_whisper_family = self
                    .app_handle
                    .try_state::<Arc<ModelManager>>()
                    .and_then(|mm| mm.get_model_info(&settings.selected_model))
                    .map(|info| {
                        debug!(
                            "Applying 3s leading silence pad for transcribe-cpp model '{}' (selected_model={})",
                            info.name, settings.selected_model
                        );
                        true
                    })
                    .unwrap_or_else(|| {
                        debug!(
                            "No model info for '{}'; skipping 3s leading silence pad",
                            settings.selected_model
                        );
                        false
                    });
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
                    "stop_recording called for '{}' but not currently recording",
                    binding_id
                );
                None
            }
        }
    }

    /// Lock-free: mirrors the `state` {Recording, Stopping} membership via
    /// an atomic maintained by `set_state()`. Polled from the webview/main
    /// thread, so it MUST NOT take the `state` mutex (a worker can hold it
    /// across a slow CoreAudio open/close → main-thread deadlock / UI
    /// freeze).
    pub fn is_recording(&self) -> bool {
        self.recording_active.load(Ordering::SeqCst)
    }

    /// True only when the state enum says Recording AND the capture flag is set.
    /// Consult when deciding whether a just-dispatched start actually produced a
    /// live recording; the cancel escape hatch clears the flag while a stale
    /// enum state may linger until the recovery worker heals it.
    pub fn is_recording_effective(&self) -> bool {
        matches!(*self.state.lock(), RecordingState::Recording { .. }) && *self.is_recording.lock()
    }

    /// Diagnostic wrapper around `is_recording()` that logs the result and
    /// any unexpectedly slow state-lock acquisition.
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
        self.capture.last_voice_activity_ms.load(Ordering::Acquire)
    }

    /// Check if the microphone stream is open (always-on mode or BT keep-alive)
    pub fn is_stream_open(&self) -> bool {
        *self.is_open.lock()
    }

    /// Check if always-on microphone mode is enabled
    pub fn is_always_on(&self) -> bool {
        matches!(*self.mode.lock(), MicrophoneMode::AlwaysOn)
    }

    /// Check if Bluetooth keep-alive is active
    #[allow(dead_code)] // fork diagnostics API
    pub fn is_bt_keep_alive(&self) -> bool {
        self.bt_keep_alive.load(Ordering::Acquire)
    }

    /// Cancel any ongoing recording without returning audio samples.
    ///
    /// If the state lock cannot be acquired within 2 seconds (a worker is
    /// mid-stop), the flag is force-cleared and a detached recovery worker
    /// heals the state, so this path never wedges the caller (e.g. the global
    /// shortcut thread).
    pub fn cancel_recording(&self) {
        info!("[CANCEL-TIMING] cancel_recording START");

        // Mark the emergency backup as cancelled (renames in-progress file to
        // *_cancelled.wav). Do this before we tear down the recording so a
        // crash during stop still leaves a recoverable file.
        if let Some(backup) = crate::emergency_save::get_emergency_backup() {
            let cancelled_path = backup.cancel_recording();
            log::info!(
                "[EMERGENCY-BACKUP] cancel_recording finalised: {:?}",
                cancelled_path
            );
        }

        self.invalidate_recording_readiness();
        self.cancel_generation.fetch_add(1, Ordering::AcqRel);

        let mut state = match self.state.try_lock_for(Duration::from_secs(2)) {
            Some(guard) => guard,
            None => {
                warn!("[CANCEL-TIMING] Could not acquire state lock within 2s — forcing is_recording=false and spawning recovery worker");
                *self.is_recording.lock() = false;
                // Capture only the AppHandle: a thread-owned manager clone
                // runs the FULL Drop when the thread ends, killing the device
                // and liveness monitors.
                let app = self.app_handle.clone();
                std::thread::spawn(move || {
                    let Some(rm) = app.try_state::<Arc<AudioRecordingManager>>() else {
                        error!("Cancel recovery: AudioRecordingManager not managed");
                        return;
                    };
                    rm.recovery_after_state_lock_timeout();
                });
                return;
            }
        };

        match *state {
            RecordingState::Recording { .. } => {
                self.set_state(&mut state, RecordingState::Idle);
                drop(state);

                // The crate stop is synchronous and bounded (pause-ack +
                // drain); no split send/wait is needed anymore.
                if let Some(microphone) = self.recorder.lock().as_ref() {
                    let _ = self.collect_recording(microphone); // Discard
                }
                self.standing_active.store(false, Ordering::Release);
                self.capture.disarm();

                *self.is_recording.lock() = false;

                // In on-demand mode, decide whether to close the mic stream.
                if matches!(*self.mode.lock(), MicrophoneMode::OnDemand) {
                    let bt_keep_alive = self.bt_keep_alive.load(Ordering::Acquire);
                    if bt_keep_alive {
                        debug!("BT keep-alive active: keeping mic stream open");
                    } else if get_settings(&self.app_handle).lazy_stream_close {
                        self.schedule_lazy_close();
                    } else {
                        self.stop_microphone_stream();
                    }
                } else {
                    self.refresh_standing_capture();
                }
            }
            RecordingState::Stopping => {
                debug!("Cancellation requested while recording is stopping");
            }
            RecordingState::Idle => {}
        }

        info!("[CANCEL-TIMING] cancel_recording COMPLETE");
    }

    /// Detached recovery worker run when `cancel_recording` cannot acquire the
    /// state lock within its 2-second timeout. Bounded in every phase so it never
    /// hangs the caller or leaks resources.
    fn recovery_after_state_lock_timeout(&self) {
        let t_start = std::time::Instant::now();
        info!("[CANCEL-TIMING] recovery worker START");

        // Phase A — stop the live capture without waiting for samples. The
        // crate's stop is bounded; if the recorder mutex stays busy, another
        // owner will finish its stop anyway.
        for attempt in 1..=3 {
            let t = std::time::Instant::now();
            if let Some(guard) = self.recorder.try_lock_for(Duration::from_millis(500)) {
                info!(
                    "[CANCEL-TIMING] recovery worker: recorder.try_lock acquired on attempt {} in {:?}",
                    attempt,
                    t.elapsed()
                );
                if let Some(microphone) = guard.as_ref() {
                    let _ = self.collect_recording(microphone); // Discard
                    info!("[CANCEL-TIMING] recovery worker: capture collected");
                }
                break;
            } else {
                warn!(
                    "[CANCEL-TIMING] recovery worker: recorder.try_lock attempt {} timed out after {:?}",
                    attempt,
                    t.elapsed()
                );
            }
        }
        self.standing_active.store(false, Ordering::Release);
        self.capture.disarm();

        // Phase B — heal the stale Recording state. The atomic gate guarantees
        // a genuinely-new recording (atomic true) is never touched.
        let mut healed = false;
        let t_heal_start = std::time::Instant::now();
        while t_heal_start.elapsed() < Duration::from_secs(10) {
            let t = std::time::Instant::now();
            if let Some(mut state) = self.state.try_lock_for(Duration::from_millis(500)) {
                info!(
                    "[CANCEL-TIMING] recovery worker: state.try_lock acquired in {:?}",
                    t.elapsed()
                );
                if matches!(*state, RecordingState::Recording { .. }) && !*self.is_recording.lock()
                {
                    self.set_state(&mut state, RecordingState::Idle);
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
        // lock in schedule_lazy_close, so it must NEVER be called while holding
        // the lock.
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
        // Wake-recovery / post-cycle restart must never tear down an in-flight
        // dictation: close_microphone() discards collect_recording() and the
        // captured audio would be lost. Bail out while recording.
        if self.is_recording() {
            info!("Skipping post-cycle microphone restart: a recording is currently active");
            return Ok(());
        }

        let is_always_on = matches!(*self.mode.lock(), MicrophoneMode::AlwaysOn);
        let bt_keep_alive = self.bt_keep_alive.load(Ordering::Acquire);

        if is_always_on || bt_keep_alive {
            info!("Restarting microphone stream after USB power cycle");
            // Stop the current stream if open
            if *self.is_open.lock() {
                self.stop_microphone_stream();
            }
            // Start a fresh stream (fresh recorder: the crate's failed flag
            // dies with the old recorder).
            self.start_microphone_stream()
        } else {
            debug!("Microphone stream not needed (not always-on, no BT keep-alive)");
            Ok(())
        }
    }

    /// Runtime toggle for the batch live-captions fallback (Strategy pattern).
    /// When enabled and no dedicated model's router is attached, the batch
    /// callback is invoked periodically during capture to produce live
    /// captions. When disabled, the callback exists but is not invoked,
    /// allowing instant toggling without recreating the stream.
    pub fn set_streaming_enabled(&self, enabled: bool) -> Result<(), anyhow::Error> {
        self.capture
            .streaming_enabled
            .store(enabled, Ordering::Release);
        info!(
            "[Live Captions] Batch streaming {}",
            if enabled { "enabled" } else { "disabled" }
        );
        // The standing capture exists for batch captions when idle; re-evaluate.
        self.refresh_standing_capture();
        Ok(())
    }

    /// Diagnostic snapshot of the microphone stream.
    /// Returns (is_open, is_alive).
    ///
    /// This is **not** a wake-recovery authority: an idle always-on stream may
    /// report `is_alive=true` because the handy-recorder crate only marks a
    /// stream failed when callbacks stop while a sink is running. Wake recovery
    /// therefore cycles the USB hub unconditionally after confirming device
    /// presence, not based on this value.
    #[allow(dead_code)]
    pub fn check_stream_health(&self) -> (bool, bool) {
        let is_open = *self.is_open.lock();
        if !is_open {
            return (false, false);
        }

        let failed = self
            .recorder
            .lock()
            .as_ref()
            .is_some_and(|m| m.needs_reopen());
        let sink_running = self.is_recording() || self.standing_active.load(Ordering::Acquire);
        let chunks_fresh = sink_running && {
            let last = self.capture.last_chunk_ms();
            last != 0
                && (std::time::SystemTime::now()
                    .duration_since(std::time::SystemTime::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64)
                    .saturating_sub(last)
                    <= Self::STREAM_LIVENESS_TIMEOUT_MS
        };
        let stream_alive = !failed && (!sink_running || chunks_fresh);

        (true, stream_alive)
    }
}

/// Duration (ms) without receiving audio data before we consider the
/// microphone stream dead and attempt to restart it.
impl AudioRecordingManager {
    const STREAM_LIVENESS_TIMEOUT_MS: u64 = 3000;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A recorder error, without touching a device: the request is rejected
    /// before one is opened.
    fn recorder_error() -> handy_recorder::Error {
        let config = RecorderConfig {
            sample_rate: Some(0),
            ..RecorderConfig::speech()
        };
        match Recorder::<CaptureSink>::open(config) {
            Err(error) => error,
            Ok(_) => panic!("a 0 Hz output rate should be rejected"),
        }
    }

    #[test]
    fn recorder_errors_are_classified_through_added_context() {
        let error = recorder_error();
        assert_eq!(error.kind(), ErrorKind::UnsupportedFormat);

        let wrapped = anyhow::Error::new(error).context("Failed to start recorder");
        assert_eq!(
            wrapped
                .downcast_ref::<handy_recorder::Error>()
                .map(handy_recorder::Error::kind),
            Some(ErrorKind::UnsupportedFormat)
        );
        assert_eq!(recording_error_type(&wrapped), "unknown");
    }

    #[test]
    fn errors_from_outside_the_recorder_are_unknown() {
        assert_eq!(
            recording_error_type(&anyhow::anyhow!("Already recording")),
            "unknown"
        );
    }

    #[test]
    fn microphone_config_maps_the_selected_channel() {
        let all = microphone_config(None, None, 480);
        assert_eq!(all.channels, Channels::MixToMono);
        assert_eq!(all.sample_rate, Some(16_000));
        assert_eq!(all.frames_per_chunk, Some(480));

        let second = microphone_config(Some("id".into()), Some(1), 256);
        assert_eq!(second.channels, Channels::Only(1));
        assert_eq!(second.device.as_deref(), Some("id"));
        assert_eq!(second.frames_per_chunk, Some(256));
    }

    #[test]
    fn standing_stream_rebuild_predicate() {
        // While recording: never rebuild.
        assert!(!should_rebuild_standing_stream(u64::MAX, true));

        // Idle, inside the rate-limit window: no rebuild.
        assert!(!should_rebuild_standing_stream(0, false));
        assert!(!should_rebuild_standing_stream(
            STANDING_REBUILD_MIN_INTERVAL_MS - 1,
            false
        ));

        // Idle, rate-limit window elapsed: rebuild.
        assert!(should_rebuild_standing_stream(
            STANDING_REBUILD_MIN_INTERVAL_MS,
            false
        ));
        assert!(should_rebuild_standing_stream(u64::MAX, false));

        // First ever rebuild (epoch 0 baseline): elapsed == now, always large
        // enough.
        assert!(should_rebuild_standing_stream(20_000, false));
    }

    #[test]
    fn device_change_recent_predicate() {
        let now = 1_000_000;

        // Stamp 0 (never stamped): never recent.
        assert!(!device_change_recent(0, now, DEVICE_FLAP_RECENT_WINDOW_MS));

        // Recent (well inside the window).
        assert!(device_change_recent(
            now - 5_000,
            now,
            DEVICE_FLAP_RECENT_WINDOW_MS
        ));

        // Stale (outside the window).
        assert!(!device_change_recent(
            now - DEVICE_FLAP_RECENT_WINDOW_MS - 1,
            now,
            DEVICE_FLAP_RECENT_WINDOW_MS
        ));

        // Exactly at the window boundary counts as recent.
        assert!(device_change_recent(
            now - DEVICE_FLAP_RECENT_WINDOW_MS,
            now,
            DEVICE_FLAP_RECENT_WINDOW_MS
        ));
    }

    #[test]
    fn flap_bypass_arm_predicate() {
        let now = 2_000_000;

        // Never armed (stamp 0): arm allowed.
        assert!(should_arm_flap_bypass(0, now));

        // Inside the 60s rate limit: no re-arm.
        assert!(!should_arm_flap_bypass(now - 30_000, now));
        assert!(!should_arm_flap_bypass(
            now - FLAP_ARM_MIN_INTERVAL_MS + 1,
            now
        ));

        // At/after the rate limit: arm allowed.
        assert!(should_arm_flap_bypass(now - FLAP_ARM_MIN_INTERVAL_MS, now));
    }

    #[test]
    fn zero_sample_abort_predicate() {
        // Not armed: never abort.
        assert!(!should_abort_dead_recording(
            false, 60_000, 0.0, 0.001, false
        ));

        // Armed but below the elapsed threshold: no abort (covers every
        // normal short recording, silent or not).
        assert!(!should_abort_dead_recording(
            true,
            ZERO_SAMPLE_ABORT_MS - 1,
            0.0,
            0.001,
            false
        ));

        // Armed and elapsed, but real audio captured (room tone ~0.001,
        // speech louder): no abort.
        assert!(!should_abort_dead_recording(
            true, 60_000, 0.001, 0.001, false
        ));
        assert!(!should_abort_dead_recording(
            true, 60_000, 0.5, 0.001, false
        ));

        // Pre-arm silent AND no recent device change (muted mic / dead since
        // boot): no abort even with in-arm digital silence — a rebuild
        // cannot fix it. (Muted-mic gate.)
        assert!(!should_abort_dead_recording(true, 60_000, 0.0, 0.0, false));
        assert!(!should_abort_dead_recording(
            true,
            60_000,
            0.0,
            ZERO_SAMPLE_MAX_RMS,
            false
        ));

        // Armed, elapsed, digital silence in-arm, stream WAS delivering
        // before arming (zombie signature): abort.
        assert!(should_abort_dead_recording(
            true,
            ZERO_SAMPLE_ABORT_MS,
            0.0,
            0.001,
            false
        ));
        assert!(should_abort_dead_recording(
            true,
            60_000,
            ZERO_SAMPLE_MAX_RMS,
            ZERO_SAMPLE_MAX_RMS * 1.1,
            false
        ));
        assert!(!should_abort_dead_recording(
            true,
            60_000,
            ZERO_SAMPLE_MAX_RMS * 1.1,
            0.001,
            false
        ));

        // Dock-flap incident case: pre-arm silent (stream already dead since
        // the flap) BUT the device changed recently — abort so the watchdog
        // can cycle the port.
        assert!(should_abort_dead_recording(true, 60_000, 0.0, 0.0, true));
        assert!(should_abort_dead_recording(
            true,
            ZERO_SAMPLE_ABORT_MS,
            0.0,
            0.0,
            true
        ));
    }

    #[test]
    fn auto_retry_budget_decision() {
        // Budget available + coordinator idle: press allowed.
        assert!(retry_decision(true, true));
        // Budget already consumed: no second press.
        assert!(!retry_decision(false, true));
        // Coordinator busy (a user re-press already started a recording):
        // never dispatch a synthetic press.
        assert!(!retry_decision(true, false));
        assert!(!retry_decision(false, false));
    }
}

impl Drop for AudioRecordingManager {
    fn drop(&mut self) {
        info!("AudioRecordingManager dropping — stopping background threads");
        self.stop_device_monitor();
        self.liveness_stop.store(true, Ordering::Relaxed);
        // Bind the handle out of the mutex first, then join — no guard held
        // across the join (same pattern as stop_device_monitor).
        let handle = self.liveness_monitor.lock().take();
        if let Some(handle) = handle {
            let _ = handle.join();
        }
    }
}
