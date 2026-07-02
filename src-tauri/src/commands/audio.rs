use crate::audio_feedback;
use crate::audio_toolkit::audio::{list_input_devices, list_output_devices};
use crate::managers::audio::{AudioRecordingManager, MicrophoneMode};
use crate::managers::model::ModelManager;
use crate::managers::transcription::TranscriptionManager;
use crate::settings::{get_settings_safe, write_settings_safe};
use crate::usb_watchdog;
use log::{info, warn};
use serde::{Deserialize, Serialize};
use specta::Type;
use parking_lot::Mutex;
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};

/// macOS idle detection via CGEventSourceSecondsSinceLastEventType.
/// Returns how long (in Duration) since the last mouse/keyboard event.
#[cfg(target_os = "macos")]
pub mod macos_idle {
    use std::time::Duration;

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGEventSourceSecondsSinceLastEventType(state: u32, event_type: u32) -> f64;
    }

    // Constants from CGEventSource.h
    const K_CG_EVENT_SOURCE_STATE_HID_SYSTEM_STATE: u32 = 1;
    const K_CG_ANY_INPUT_EVENT_TYPE: u32 = 0;

    pub fn get_idle_time() -> Option<Duration> {
        let seconds = unsafe {
            CGEventSourceSecondsSinceLastEventType(
                K_CG_EVENT_SOURCE_STATE_HID_SYSTEM_STATE,
                K_CG_ANY_INPUT_EVENT_TYPE,
            )
        };
        if seconds > 0.0 {
            Some(Duration::from_secs_f64(seconds))
        } else {
            None
        }
    }
}

#[cfg(target_os = "windows")]
use winreg::{
    enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE},
    RegKey, HKEY,
};

#[derive(Serialize, Type)]
pub struct CustomSounds {
    start: bool,
    stop: bool,
}

fn custom_sound_exists(app: &AppHandle, sound_type: &str) -> bool {
    crate::portable::resolve_app_data(app, &format!("custom_{}.wav", sound_type))
        .map_or(false, |path| path.exists())
}

#[tauri::command]
#[specta::specta]
pub fn check_custom_sounds(app: AppHandle) -> Result<CustomSounds, String> {
    Ok(CustomSounds {
        start: custom_sound_exists(&app, "start"),
        stop: custom_sound_exists(&app, "stop"),
    })
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct AudioDevice {
    pub index: String,
    pub name: String,
    pub is_default: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Type)]
#[serde(rename_all = "snake_case")]
pub enum PermissionAccess {
    Allowed,
    Denied,
    Unknown,
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct WindowsMicrophonePermissionStatus {
    pub supported: bool,
    pub overall_access: PermissionAccess,
    pub device_access: PermissionAccess,
    pub app_access: PermissionAccess,
    pub desktop_app_access: PermissionAccess,
}

#[cfg(target_os = "windows")]
fn read_registry_permission_access(root_hkey: HKEY, path: &str) -> PermissionAccess {
    let root = RegKey::predef(root_hkey);
    let Ok(key) = root.open_subkey(path) else {
        return PermissionAccess::Unknown;
    };

    let Ok(value) = key.get_value::<String, _>("Value") else {
        return PermissionAccess::Unknown;
    };

    match value.to_ascii_lowercase().as_str() {
        "allow" => PermissionAccess::Allowed,
        "deny" => PermissionAccess::Denied,
        _ => PermissionAccess::Unknown,
    }
}

#[cfg(target_os = "windows")]
fn get_windows_microphone_permission_status_impl() -> WindowsMicrophonePermissionStatus {
    const MICROPHONE_PATH: &str =
        "Software\\Microsoft\\Windows\\CurrentVersion\\CapabilityAccessManager\\ConsentStore\\microphone";
    const DESKTOP_APPS_PATH: &str =
        "Software\\Microsoft\\Windows\\CurrentVersion\\CapabilityAccessManager\\ConsentStore\\microphone\\NonPackaged";

    let device_access = read_registry_permission_access(HKEY_LOCAL_MACHINE, MICROPHONE_PATH);
    let app_access = read_registry_permission_access(HKEY_CURRENT_USER, MICROPHONE_PATH);
    let desktop_app_access = read_registry_permission_access(HKEY_CURRENT_USER, DESKTOP_APPS_PATH);

    let overall_access = if [device_access, app_access, desktop_app_access]
        .into_iter()
        .any(|access| access == PermissionAccess::Denied)
    {
        PermissionAccess::Denied
    } else if [device_access, app_access, desktop_app_access]
        .into_iter()
        .all(|access| access == PermissionAccess::Allowed)
    {
        PermissionAccess::Allowed
    } else {
        PermissionAccess::Unknown
    };

    WindowsMicrophonePermissionStatus {
        supported: true,
        overall_access,
        device_access,
        app_access,
        desktop_app_access,
    }
}

#[tauri::command]
#[specta::specta]
pub fn get_windows_microphone_permission_status() -> Result<WindowsMicrophonePermissionStatus, String> {
    #[cfg(target_os = "windows")]
    {
        Ok(get_windows_microphone_permission_status_impl())
    }

    #[cfg(not(target_os = "windows"))]
    {
        Ok(WindowsMicrophonePermissionStatus {
            supported: false,
            overall_access: PermissionAccess::Unknown,
            device_access: PermissionAccess::Unknown,
            app_access: PermissionAccess::Unknown,
            desktop_app_access: PermissionAccess::Unknown,
        })
    }
}

#[tauri::command]
#[specta::specta]
pub fn open_microphone_privacy_settings() -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        use std::process::Command;
        Command::new("cmd")
            .args(["/C", "start", "", "ms-settings:privacy-microphone"])
            .spawn()
            .map_err(|e| format!("Failed to open Windows microphone privacy settings: {}", e))?;
        return Ok(());
    }

    #[cfg(not(target_os = "windows"))]
    {
        Err("Opening microphone privacy settings is only supported on Windows".to_string())
    }
}

#[tauri::command]
#[specta::specta]
pub fn update_microphone_mode(app: AppHandle, always_on: bool) -> Result<(), String> {
    // Update settings
    let mut settings = get_settings_safe(&app);
    settings.always_on_microphone = always_on;
    write_settings_safe(&app, settings);

    // Update the audio manager mode
    let Some(rm) = app.try_state::<Arc<AudioRecordingManager>>() else {
        return Err("AudioRecordingManager not available".to_string());
    };
    let new_mode = if always_on {
        MicrophoneMode::AlwaysOn
    } else {
        MicrophoneMode::OnDemand
    };

    let result = rm.update_mode(new_mode);
    result.map_err(|e| format!("Failed to update microphone mode: {}", e))
}

#[tauri::command]
#[specta::specta]
pub fn get_microphone_mode(app: AppHandle) -> Result<bool, String> {
    let settings = get_settings_safe(&app);
    Ok(settings.always_on_microphone)
}

#[tauri::command]
#[specta::specta]
pub fn get_available_microphones() -> Result<Vec<AudioDevice>, String> {
    let devices =
        list_input_devices().map_err(|e| format!("Failed to list audio devices: {}", e))?;

    let mut result = vec![AudioDevice {
        index: "default".to_string(),
        name: "Default".to_string(),
        is_default: true,
    }];

    result.extend(devices.into_iter().map(|d| AudioDevice {
        index: d.index,
        name: d.name,
        is_default: false, // The explicit default is handled separately
    }));

    Ok(result)
}

#[tauri::command]
#[specta::specta]
pub fn set_selected_microphone(app: AppHandle, device_name: String) -> Result<(), String> {
    let mut settings = get_settings_safe(&app);
    settings.selected_microphone = if device_name == "default" {
        None
    } else {
        Some(device_name)
    };
    write_settings_safe(&app, settings);

    // Update the audio manager to use the new device
    let Some(rm) = app.try_state::<Arc<AudioRecordingManager>>() else {
        return Err("AudioRecordingManager not available".to_string());
    };
    rm.update_selected_device()
        .map_err(|e| format!("Failed to update selected device: {}", e))?;

    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn get_selected_microphone(app: AppHandle) -> Result<String, String> {
    let settings = get_settings_safe(&app);
    Ok(settings
        .selected_microphone
        .unwrap_or_else(|| "default".to_string()))
}

#[tauri::command]
#[specta::specta]
pub fn get_available_output_devices() -> Result<Vec<AudioDevice>, String> {
    let devices =
        list_output_devices().map_err(|e| format!("Failed to list output devices: {}", e))?;

    let mut result = vec![AudioDevice {
        index: "default".to_string(),
        name: "Default".to_string(),
        is_default: true,
    }];

    result.extend(devices.into_iter().map(|d| AudioDevice {
        index: d.index,
        name: d.name,
        is_default: false, // The explicit default is handled separately
    }));

    Ok(result)
}

#[tauri::command]
#[specta::specta]
pub fn set_selected_output_device(app: AppHandle, device_name: String) -> Result<(), String> {
    let mut settings = get_settings_safe(&app);
    settings.selected_output_device = if device_name == "default" {
        None
    } else {
        Some(device_name)
    };
    write_settings_safe(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn get_selected_output_device(app: AppHandle) -> Result<String, String> {
    let settings = get_settings_safe(&app);
    Ok(settings
        .selected_output_device
        .unwrap_or_else(|| "default".to_string()))
}

#[tauri::command]
#[specta::specta]
pub async fn play_test_sound(app: AppHandle, sound_type: String) {
    let sound = match sound_type.as_str() {
        "start" => audio_feedback::SoundType::Start,
        "stop" => audio_feedback::SoundType::Stop,
        _ => {
            warn!("Unknown sound type: {}", sound_type);
            return;
        }
    };
    audio_feedback::play_test_sound(&app, sound);
}

#[tauri::command]
#[specta::specta]
pub fn set_clamshell_microphone(app: AppHandle, device_name: String) -> Result<(), String> {
    let mut settings = get_settings_safe(&app);
    settings.clamshell_microphone = if device_name == "default" {
        None
    } else {
        Some(device_name)
    };
    write_settings_safe(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn get_clamshell_microphone(app: AppHandle) -> Result<String, String> {
    let settings = get_settings_safe(&app);
    Ok(settings
        .clamshell_microphone
        .unwrap_or_else(|| "default".to_string()))
}

#[tauri::command]
#[specta::specta]
pub fn is_recording(app: AppHandle) -> Result<bool, String> {
    let Some(audio_manager) = app.try_state::<Arc<AudioRecordingManager>>() else {
        return Ok(false);
    };
    let result = audio_manager.is_recording();
    Ok(result)
}

// ============================================================================
// USB Watchdog Commands
// ============================================================================

/// Check if uhubctl is available on the system
#[tauri::command]
#[specta::specta]
pub fn is_usb_watchdog_available() -> Result<bool, String> {
    Ok(usb_watchdog::is_uhubctl_available())
}

/// List all USB devices connected to hubs visible to uhubctl
#[tauri::command]
#[specta::specta]
pub fn list_usb_devices() -> Result<Vec<usb_watchdog::UsbDevice>, String> {
    Ok(usb_watchdog::list_usb_devices())
}

/// Enable or disable the USB watchdog
#[tauri::command]
#[specta::specta]
pub fn change_usb_watchdog_enabled_setting(app: AppHandle, enabled: bool) -> Result<(), String> {
    let mut settings = get_settings_safe(&app);
    let device_name = settings.usb_watchdog_device_name.clone();
    settings.usb_watchdog_enabled = enabled;
    write_settings_safe(&app, settings);

    // Update the runtime watchdog state
    let Some(rm) = app.try_state::<Arc<AudioRecordingManager>>() else {
        return Err("AudioRecordingManager not available".to_string());
    };
    rm.usb_watchdog.update_config(enabled, device_name);

    Ok(())
}

/// Update the USB watchdog target device name
#[tauri::command]
#[specta::specta]
pub fn change_usb_watchdog_device_name_setting(
    app: AppHandle,
    device_name: String,
) -> Result<(), String> {
    let mut settings = get_settings_safe(&app);
    let enabled = settings.usb_watchdog_enabled;
    settings.usb_watchdog_device_name = device_name.clone();
    write_settings_safe(&app, settings);

    // Update the runtime watchdog state
    let Some(rm) = app.try_state::<Arc<AudioRecordingManager>>() else {
        return Err("AudioRecordingManager not available".to_string());
    };
    rm.usb_watchdog.update_config(enabled, device_name);

    Ok(())
}

/// Enable or disable the USB watchdog cycle-on-wake setting.
/// When enabled, Handy automatically power-cycles the USB audio device
/// when macOS wakes from sleep, preventing the "mic not listening" state.
#[tauri::command]
#[specta::specta]
pub fn change_usb_watchdog_cycle_on_wake_setting(
    app: AppHandle,
    cycle_on_wake: bool,
) -> Result<(), String> {
    let mut settings = get_settings_safe(&app);
    settings.usb_watchdog_cycle_on_wake = cycle_on_wake;
    write_settings_safe(&app, settings);
    Ok(())
}

/// Manually trigger a USB power cycle (for testing)
#[tauri::command]
#[specta::specta]
pub fn trigger_usb_power_cycle(app: AppHandle) -> Result<bool, String> {
    let Some(rm) = app.try_state::<Arc<AudioRecordingManager>>() else {
        return Err("AudioRecordingManager not available".to_string());
    };
    let result = rm.usb_watchdog.force_power_cycle();
    Ok(result)
}

/// Special binding ID used for pronunciation recordings to distinguish them
/// from regular transcription recordings.
const PRONUNCIATION_BINDING_ID: &str = "__pronunciation__";

/// Result of transcribing a pronunciation sample with a single model.
#[derive(Clone, Debug, Serialize, Type)]
pub struct PronunciationResult {
    /// The model ID used for this transcription.
    pub model_id: String,
    /// Human-readable model name.
    pub model_name: String,
    /// What the model heard (raw transcription text).
    pub transcription: String,
    /// Whether this transcription matches the canonical word after normalization.
    /// If true, this result should NOT be saved as a pronunciation variant
    /// because the model already heard the word correctly.
    pub matches_canonical: bool,
}

/// Normalizes text for comparison by stripping punctuation and lowercasing.
/// Used to determine if a model's transcription matches the canonical word.
/// Examples: "Hogwarts." -> "hogwarts", "Hello!" -> "hello", "CHARGE B" -> "charge b"
fn normalize_for_comparison(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphanumeric() || *c == ' ')
        .collect::<String>()
        .to_lowercase()
        .trim()
        .to_string()
}

/// Start a short recording for capturing a pronunciation sample.
///
/// This reuses the existing audio recording infrastructure but uses a special
/// binding ID to avoid conflicts with regular transcription recordings.
/// The recording must be stopped with `stop_and_schedule_pronunciation`.
#[tauri::command]
#[specta::specta]
pub fn start_pronunciation_recording(app: AppHandle) -> Result<(), String> {
    let Some(rm) = app.try_state::<Arc<AudioRecordingManager>>() else {
        return Err("AudioRecordingManager not available".to_string());
    };

    // Don't interfere with an active transcription recording
    if rm.is_recording() {
        return Err("A recording is already in progress".to_string());
    }

    rm.try_start_recording(PRONUNCIATION_BINDING_ID)
        .map_err(|e| format!("Failed to start pronunciation recording: {}", e))?;

    info!("Pronunciation recording started");
    Ok(())
}

/// Cancel an active pronunciation recording without processing.
///
/// Stops the recording and discards the audio.
#[tauri::command]
#[specta::specta]
pub fn cancel_pronunciation_recording(app: AppHandle) -> Result<(), String> {
    let Some(rm) = app.try_state::<Arc<AudioRecordingManager>>() else {
        return Err("AudioRecordingManager not available".to_string());
    };

    // Stop recording and discard audio
    let _ = rm.stop_recording(PRONUNCIATION_BINDING_ID);

    // Clear any pending pronunciation data
    {
        let mut pending = rm.pending_pronunciation.lock();
        pending.clear();
    }

    info!("Pronunciation recording cancelled");
    Ok(())
}

/// Stop the pronunciation recording and schedule deferred processing.
///
/// Stops the recording, stores the audio + canonical word, and spawns a
/// background thread that will process with all downloaded models after a delay.
/// The frontend is notified via events when processing completes.
#[tauri::command]
#[specta::specta]
pub async fn stop_and_schedule_pronunciation(
    app: AppHandle,
    canonical_word: String,
) -> Result<String, String> {
    let Some(rm) = app.try_state::<Arc<AudioRecordingManager>>() else {
        return Err("AudioRecordingManager not available".to_string());
    };

    // Stop recording and get audio samples
    let samples = rm
        .stop_recording(PRONUNCIATION_BINDING_ID)
        .ok_or_else(|| "No pronunciation recording in progress".to_string())?;

    if samples.is_empty() {
        return Err("Recording produced no audio samples".to_string());
    }

    info!(
        "Pronunciation recording stopped, {} samples captured. Scheduling deferred processing.",
        samples.len()
    );

    // Store the audio + word for deferred processing
    {
        let mut pending = rm.pending_pronunciation.lock();
        pending.push_back((
            samples,
            canonical_word.clone(),
            String::new(),
            String::new(),
        ));
    }

    // Spawn a background thread that will process after a delay
    let _rm_clone = Arc::clone(&rm);
    let app_clone = app.clone();
    let canonical_word_clone = canonical_word.clone();

    // Cancel any existing processing thread
    {
        let mut thread_handle = rm.pronunciation_thread.lock();
        if let Some(_handle) = thread_handle.take() {
            // We can't cancel a thread directly, but we can let it run and it will
            // check if new data is available
            info!("Cancelling previous pronunciation processing thread");
        }
        *thread_handle = Some(thread::spawn(move || {
            process_pronunciation_deferred(&app_clone, &canonical_word_clone);
        }));
    }

    info!(
        "Pronunciation scheduled for deferred processing. Word: '{}'",
        canonical_word
    );

    Ok(format!(
        "Recording saved. Will process pronunciation for '{}' when idle.",
        canonical_word
    ))
}

/// Process pronunciation after the system has been idle (no mouse/keyboard input).
/// This runs in a background thread.
fn process_pronunciation_deferred(app: &AppHandle, canonical_word: &str) {
    info!(
        "Waiting for system idle before processing pronunciation for '{}'...",
        canonical_word
    );

    let idle_threshold = Duration::from_secs(60); // 1 minute of idle required
    let check_interval = Duration::from_secs(5); // Check every 5 seconds

    // Poll until system is idle
    loop {
        // Check if pending data changed (cancellation)
        let Some(rm) = app.try_state::<Arc<AudioRecordingManager>>() else {
            warn!("AudioRecordingManager not available, aborting pronunciation processing");
            return;
        };
        let pending = rm.pending_pronunciation.lock();
        if !matches!(pending.front(), Some((_, w, _, _)) if w == canonical_word) {
            info!(
                "Pronunciation data changed or cleared, skipping processing for '{}'",
                canonical_word
            );
            return;
        }
        drop(pending);

        #[cfg(target_os = "macos")]
        {
            if let Some(idle) = macos_idle::get_idle_time() {
                info!("System idle for {:?}", idle);
                if idle >= idle_threshold {
                    info!("System is idle, starting pronunciation processing");
                    break;
                }
            }
        }

        #[cfg(not(target_os = "macos"))]
        {
            // Fallback: just wait 60 seconds on non-macOS
            thread::sleep(Duration::from_secs(60));
            break;
        }

        thread::sleep(check_interval);
    }

    // Now process with all models
    let Some(rm) = app.try_state::<Arc<AudioRecordingManager>>() else {
        warn!("AudioRecordingManager not available, aborting pronunciation processing");
        return;
    };
    let pending = rm.pending_pronunciation.lock();
    let (samples, word) = match pending.front() {
        Some((s, w, _, _)) if w == canonical_word => (s.clone(), w.clone()),
        _ => {
            info!(
                "Pronunciation data changed before processing started for '{}'",
                canonical_word
            );
            return;
        }
    };
    drop(pending);

    info!(
        "Starting deferred multi-model pronunciation processing for '{}'",
        canonical_word
    );

    // Emit progress start
    let _ = app.emit(
        "pronunciation-model-progress",
        serde_json::json!({
            "model_id": "",
            "model_name": "Starting...",
            "current": 0,
            "total": 0,
            "started": true,
        }),
    );

    // Process with all models
    match process_pronunciation_with_all_models(&app, &word, samples) {
        Ok(results) => {
            let _canonical_normalized = normalize_for_comparison(canonical_word);
            let new_pronunciations: Vec<String> = results
                .iter()
                .filter(|r| !r.matches_canonical)
                .map(|r| r.transcription.trim().to_string())
                .filter(|t| !t.is_empty())
                .collect();

            if new_pronunciations.is_empty() {
                let _ = app.emit(
                    "pronunciation-processing-done",
                    serde_json::json!({
                        "success": true,
                        "message": format!("All models heard '{}' correctly", canonical_word),
                        "count": 0,
                    }),
                );
            } else {
                let _ = app.emit(
                    "pronunciation-processing-done",
                    serde_json::json!({
                        "success": true,
                        "message": format!("Added {} pronunciations for '{}'", new_pronunciations.len(), canonical_word),
                        "count": new_pronunciations.len(),
                        "pronunciations": new_pronunciations,
                        "word": canonical_word,
                    }),
                );
            }

            // Clear pending
            let Some(rm) = app.try_state::<Arc<AudioRecordingManager>>() else {
                warn!("AudioRecordingManager not available, cannot clear pending pronunciation");
                return;
            };
            let mut pending = rm.pending_pronunciation.lock();
            pending.pop_front();
        }
        Err(e) => {
            warn!(
                "Deferred pronunciation processing failed for '{}': {}",
                canonical_word, e
            );
            let _ = app.emit(
                "pronunciation-processing-done",
                serde_json::json!({
                    "success": false,
                    "message": format!("Processing failed: {}", e),
                    "count": 0,
                }),
            );
        }
    }

    // Clear the thread handle
    let Some(rm) = app.try_state::<Arc<AudioRecordingManager>>() else {
        warn!("AudioRecordingManager not available, cannot clear pronunciation thread handle");
        return;
    };
    let mut thread_handle = rm.pronunciation_thread.lock();
    *thread_handle = None;
}

/// Process pronunciation with all downloaded models (shared logic).
fn process_pronunciation_with_all_models(
    app: &AppHandle,
    canonical_word: &str,
    samples: Vec<f32>,
) -> Result<Vec<PronunciationResult>, String> {
    let Some(mm) = app.try_state::<Arc<ModelManager>>() else {
        return Err("ModelManager not available".to_string());
    };
    let downloaded_models: Vec<(String, String)> = mm
        .get_available_models()
        .into_iter()
        .filter(|m| m.is_downloaded && !m.is_downloading)
        .map(|m| (m.id.clone(), m.name.clone()))
        .collect();

    if downloaded_models.is_empty() {
        return Err("No models are downloaded".to_string());
    }

    let canonical_normalized = normalize_for_comparison(canonical_word);
    let Some(tm) = app.try_state::<Arc<Mutex<TranscriptionManager>>>() else {
        warn!("TranscriptionManager not available, cannot check pronunciation");
        return Err("TranscriptionManager not available".to_string());
    };
    let tm = Arc::clone(&tm);

    let total_models = downloaded_models.len();
    let mut results: Vec<PronunciationResult> = Vec::new();
    let mut seen_transcriptions: Vec<String> = Vec::new();

    for (idx, (model_id, model_name)) in downloaded_models.iter().enumerate() {
        // Emit progress
        let _ = app.emit(
            "pronunciation-model-progress",
            serde_json::json!({
                "model_id": model_id,
                "model_name": model_name,
                "current": idx + 1,
                "total": total_models,
            }),
        );

        let tm_clone = Arc::clone(&tm);
        let model_id_clone = model_id.clone();
        let samples_clone = samples.clone();

        let transcription_result = thread::spawn(move || {
            // Load the specific model
            if let Err(e) = tm_clone.lock().load_model(&model_id_clone) {
                return Err(format!("Failed to load model {}: {}", model_id_clone, e));
            }

            // Transcribe with this model
            tm_clone
                .lock()
                .transcribe(samples_clone)
                .map_err(|e| format!("Transcription failed: {}", e))
        })
        .join()
        .map_err(|_| "Thread panicked".to_string())?;

        match transcription_result {
            Ok(output) => {
                let transcription_text = output.text.trim().to_string();

                if transcription_text.is_empty() {
                    continue;
                }

                let normalized = normalize_for_comparison(&transcription_text);
                let matches_canonical = normalized == canonical_normalized;

                let transcription_lower = transcription_text.to_lowercase();
                if seen_transcriptions
                    .iter()
                    .any(|s| s.to_lowercase() == transcription_lower)
                {
                    continue;
                }

                seen_transcriptions.push(transcription_text.clone());

                results.push(PronunciationResult {
                    model_id: model_id.clone(),
                    model_name: model_name.clone(),
                    transcription: transcription_text,
                    matches_canonical,
                });
            }
            Err(e) => {
                warn!("Model {} failed: {}", model_id, e);
            }
        }
    }

    Ok(results)
}

/// Stop the pronunciation recording and transcribe with ALL downloaded models.
///
/// This iterates through each downloaded model, loads it, transcribes the
/// pronunciation audio, and collects results. Transcriptions that match the
/// canonical word (after stripping punctuation and lowercasing) are marked
/// with `matches_canonical: true` so the frontend can skip them.
///
/// The original model is restored after all transcriptions are complete.
#[tauri::command]
#[specta::specta]
pub async fn stop_and_transcribe_pronunciation_all_models(
    app: AppHandle,
    canonical_word: String,
) -> Result<Vec<PronunciationResult>, String> {
    let Some(rm) = app.try_state::<Arc<AudioRecordingManager>>() else {
        return Err("AudioRecordingManager not available".to_string());
    };

    // Stop recording and get audio samples
    let samples = rm
        .stop_recording(PRONUNCIATION_BINDING_ID)
        .ok_or_else(|| "No pronunciation recording in progress".to_string())?;

    if samples.is_empty() {
        return Err("Recording produced no audio samples".to_string());
    }

    info!(
        "Pronunciation recording stopped for multi-model transcription, {} samples captured. \
         Canonical word: '{}'",
        samples.len(),
        canonical_word
    );

    // Get all downloaded models
    let Some(mm) = app.try_state::<Arc<ModelManager>>() else {
        return Err("ModelManager not available".to_string());
    };
    let downloaded_models: Vec<(String, String)> = mm
        .get_available_models()
        .into_iter()
        .filter(|m| m.is_downloaded && !m.is_downloading)
        .map(|m| (m.id.clone(), m.name.clone()))
        .collect();

    if downloaded_models.is_empty() {
        return Err(
            "No models are downloaded. Please download at least one model first.".to_string(),
        );
    }

    // Remember the currently selected model so we can restore it
    let original_model = {
        let settings = get_settings_safe(&app);
        settings.selected_model.clone()
    };

    let canonical_normalized = normalize_for_comparison(&canonical_word);
    let Some(tm) = app.try_state::<Arc<Mutex<TranscriptionManager>>>() else {
        warn!("TranscriptionManager not available, cannot run pronunciation check");
        return Err("TranscriptionManager not available".to_string());
    };
    let tm = Arc::clone(&tm);

    let total_models = downloaded_models.len();
    let mut results: Vec<PronunciationResult> = Vec::new();
    let mut seen_transcriptions: Vec<String> = Vec::new();

    // Transcribe with each downloaded model
    for (idx, (model_id, model_name)) in downloaded_models.iter().enumerate() {
        // Emit progress event so the frontend can show which model is being processed
        let _ = app.emit(
            "pronunciation-model-progress",
            serde_json::json!({
                "model_id": model_id,
                "model_name": model_name,
                "current": idx + 1,
                "total": total_models,
            }),
        );

        info!(
            "Transcribing pronunciation with model {}/{}: {} ({})",
            idx + 1,
            total_models,
            model_name,
            model_id
        );

        let tm_clone = Arc::clone(&tm);
        let model_id_clone = model_id.clone();
        let samples_clone = samples.clone();

        let transcription_result = tauri::async_runtime::spawn_blocking(move || {
            // Load the specific model
            if let Err(e) = tm_clone.lock().load_model(&model_id_clone) {
                return Err(format!("Failed to load model {}: {}", model_id_clone, e));
            }

            // Transcribe with this model
            tm_clone
                .lock()
                .transcribe(samples_clone)
                .map_err(|e| format!("Transcription failed with model {}: {}", model_id_clone, e))
        })
        .await
        .map_err(|e| format!("Transcription task failed for model {}: {}", model_id, e))?;

        match transcription_result {
            Ok(output) => {
                let transcription_text = output.text.trim().to_string();

                if transcription_text.is_empty() {
                    info!(
                        "Model {} ({}) produced empty transcription, skipping",
                        model_id, model_name
                    );
                    continue;
                }

                let normalized = normalize_for_comparison(&transcription_text);
                let matches_canonical = normalized == canonical_normalized;

                // Deduplicate: skip if we've already seen this exact transcription
                let transcription_lower = transcription_text.to_lowercase();
                if seen_transcriptions
                    .iter()
                    .any(|s| s.to_lowercase() == transcription_lower)
                {
                    info!(
                        "Model {} ({}) produced duplicate transcription '{}', skipping",
                        model_id, model_name, transcription_text
                    );
                    continue;
                }

                info!(
                    "Model {} ({}) transcribed: '{}' (normalized: '{}', matches_canonical: {})",
                    model_id, model_name, transcription_text, normalized, matches_canonical
                );

                seen_transcriptions.push(transcription_text.clone());

                results.push(PronunciationResult {
                    model_id: model_id.clone(),
                    model_name: model_name.clone(),
                    transcription: transcription_text,
                    matches_canonical,
                });
            }
            Err(e) => {
                warn!(
                    "Failed to transcribe pronunciation with model {}: {}",
                    model_id, e
                );
                // Continue to next model rather than failing entirely
            }
        }
    }

    // Restore the original model
    if !original_model.is_empty() {
        let tm_restore = Arc::clone(&tm);
        let original_model_clone = original_model.clone();
        let _ = tauri::async_runtime::spawn_blocking(move || {
            info!("Restoring original model: {}", original_model_clone);
            tm_restore.lock().load_model(&original_model_clone)
        })
        .await;
    }

    // Emit completion event
    let _ = app.emit(
        "pronunciation-model-progress",
        serde_json::json!({
            "model_id": "",
            "model_name": "",
            "current": total_models,
            "total": total_models,
            "completed": true,
        }),
    );

    info!(
        "Multi-model pronunciation transcription complete. {} unique results from {} models",
        results.len(),
        total_models
    );

    Ok(results)
}
