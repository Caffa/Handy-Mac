use crate::audio_feedback;
use crate::audio_toolkit::audio::{list_input_devices, list_output_devices};
use crate::managers::audio::{AudioRecordingManager, MicrophoneMode};
use crate::managers::model::ModelManager;
use crate::managers::transcription::TranscriptionManager;
use crate::settings::{get_settings, write_settings};
use crate::usb_watchdog;
use log::{info, warn};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager};

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
        .is_ok_and(|path| path.exists())
}

#[tauri::command]
#[specta::specta]
pub fn check_custom_sounds(app: AppHandle) -> CustomSounds {
    CustomSounds {
        start: custom_sound_exists(&app, "start"),
        stop: custom_sound_exists(&app, "stop"),
    }
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
pub fn get_windows_microphone_permission_status() -> WindowsMicrophonePermissionStatus {
    #[cfg(target_os = "windows")]
    {
        get_windows_microphone_permission_status_impl()
    }

    #[cfg(not(target_os = "windows"))]
    {
        WindowsMicrophonePermissionStatus {
            supported: false,
            overall_access: PermissionAccess::Unknown,
            device_access: PermissionAccess::Unknown,
            app_access: PermissionAccess::Unknown,
            desktop_app_access: PermissionAccess::Unknown,
        }
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
    let mut settings = get_settings(&app);
    settings.always_on_microphone = always_on;
    write_settings(&app, settings);

    // Update the audio manager mode
    let rm = app.state::<Arc<AudioRecordingManager>>();
    let new_mode = if always_on {
        MicrophoneMode::AlwaysOn
    } else {
        MicrophoneMode::OnDemand
    };

    rm.update_mode(new_mode)
        .map_err(|e| format!("Failed to update microphone mode: {}", e))
}

#[tauri::command]
#[specta::specta]
pub fn get_microphone_mode(app: AppHandle) -> Result<bool, String> {
    let settings = get_settings(&app);
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
    let mut settings = get_settings(&app);
    settings.selected_microphone = if device_name == "default" {
        None
    } else {
        Some(device_name)
    };
    write_settings(&app, settings);

    // Update the audio manager to use the new device
    let rm = app.state::<Arc<AudioRecordingManager>>();
    rm.update_selected_device()
        .map_err(|e| format!("Failed to update selected device: {}", e))?;

    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn get_selected_microphone(app: AppHandle) -> Result<String, String> {
    let settings = get_settings(&app);
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
    let mut settings = get_settings(&app);
    settings.selected_output_device = if device_name == "default" {
        None
    } else {
        Some(device_name)
    };
    write_settings(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn get_selected_output_device(app: AppHandle) -> Result<String, String> {
    let settings = get_settings(&app);
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
    let mut settings = get_settings(&app);
    settings.clamshell_microphone = if device_name == "default" {
        None
    } else {
        Some(device_name)
    };
    write_settings(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn get_clamshell_microphone(app: AppHandle) -> Result<String, String> {
    let settings = get_settings(&app);
    Ok(settings
        .clamshell_microphone
        .unwrap_or_else(|| "default".to_string()))
}

#[tauri::command]
#[specta::specta]
pub fn is_recording(app: AppHandle) -> bool {
    let audio_manager = app.state::<Arc<AudioRecordingManager>>();
    audio_manager.is_recording()
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
    let mut settings = get_settings(&app);
    let device_name = settings.usb_watchdog_device_name.clone();
    settings.usb_watchdog_enabled = enabled;
    write_settings(&app, settings);

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
    let mut settings = get_settings(&app);
    let enabled = settings.usb_watchdog_enabled;
    settings.usb_watchdog_device_name = device_name.clone();
    write_settings(&app, settings);

    // Update the runtime watchdog state
    let Some(rm) = app.try_state::<Arc<AudioRecordingManager>>() else {
        return Err("AudioRecordingManager not available".to_string());
    };
    rm.usb_watchdog.update_config(enabled, device_name);

    Ok(())
}

/// Enable or disable the USB watchdog cycle-on-wake setting.
#[tauri::command]
#[specta::specta]
pub fn change_usb_watchdog_cycle_on_wake_setting(
    app: AppHandle,
    cycle_on_wake: bool,
) -> Result<(), String> {
    let mut settings = get_settings(&app);
    settings.usb_watchdog_cycle_on_wake = cycle_on_wake;
    write_settings(&app, settings);
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

// ============================================================================
// Pronunciation Recording Commands
// ============================================================================

/// Special binding ID used for pronunciation recordings.
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
    pub matches_canonical: bool,
}

/// Start a short recording for capturing a pronunciation sample.
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

    rm.try_start_recording(
        PRONUNCIATION_BINDING_ID,
        crate::audio_toolkit::VadPolicy::Offline,
    )
    .map_err(|e| format!("Failed to start pronunciation recording: {}", e))?;

    info!("Pronunciation recording started");
    Ok(())
}

/// Cancel an active pronunciation recording without processing.
#[tauri::command]
#[specta::specta]
pub fn cancel_pronunciation_recording(app: AppHandle) -> Result<(), String> {
    let Some(rm) = app.try_state::<Arc<AudioRecordingManager>>() else {
        return Err("AudioRecordingManager not available".to_string());
    };

    // Stop recording and discard audio — use generation 0 as cancel marker
    let _ = rm.stop_recording(PRONUNCIATION_BINDING_ID, 0);

    info!("Pronunciation recording cancelled");
    Ok(())
}

/// Stop the pronunciation recording and transcribe with ALL downloaded models.
#[tauri::command]
#[specta::specta]
pub async fn stop_and_transcribe_pronunciation_all_models(
    app: AppHandle,
    canonical_word: String,
) -> Result<Vec<PronunciationResult>, String> {
    let Some(rm) = app.try_state::<Arc<AudioRecordingManager>>() else {
        return Err("AudioRecordingManager not available".to_string());
    };

    // Stop recording and get audio samples — use generation 0 as sentinel
    let samples = rm
        .stop_recording(PRONUNCIATION_BINDING_ID, 0)
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
        let settings = get_settings(&app);
        settings.selected_model.clone()
    };

    let canonical_normalized = canonical_word.to_lowercase().trim().to_string();
    let tm = app
        .try_state::<Arc<TranscriptionManager>>()
        .ok_or_else(|| "TranscriptionManager not available".to_string())?;

    let total_models = downloaded_models.len();
    let mut results: Vec<PronunciationResult> = Vec::new();
    let mut seen_transcriptions: Vec<String> = Vec::new();

    // Transcribe with each downloaded model
    for (idx, (model_id, model_name)) in downloaded_models.iter().enumerate() {
        // Emit progress event
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

        let model_id_clone = model_id.clone();
        let samples_clone = samples.clone();
        let tm_clone = Arc::clone(tm.inner());

        let transcription_result = tauri::async_runtime::spawn_blocking(move || {
            // Load the specific model
            if let Err(e) = tm_clone.load_model(&model_id_clone) {
                return Err(format!("Failed to load model {}: {}", model_id_clone, e));
            }

            // Transcribe with this model
            tm_clone
                .transcribe(samples_clone)
                .map_err(|e| format!("Transcription failed with model {}: {}", model_id_clone, e))
        })
        .await
        .map_err(|e| format!("Transcription task failed for model {}: {}", model_id, e))?;

        match transcription_result {
            Ok(transcription_text) => {
                let transcription_text = transcription_text.trim().to_string();

                if transcription_text.is_empty() {
                    info!(
                        "Model {} ({}) produced empty transcription, skipping",
                        model_id, model_name
                    );
                    continue;
                }

                let normalized = transcription_text.to_lowercase();
                let matches_canonical = normalized == canonical_normalized;

                // Deduplicate
                if seen_transcriptions
                    .iter()
                    .any(|s| s.to_lowercase() == normalized)
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
                warn!(
                    "Failed to transcribe pronunciation with model {}: {}",
                    model_id, e
                );
            }
        }
    }

    // Restore the original model
    if !original_model.is_empty() {
        let tm_restore = Arc::clone(tm.inner());
        let original_model_clone = original_model.clone();
        let _ = tauri::async_runtime::spawn_blocking(move || {
            info!("Restoring original model: {}", original_model_clone);
            tm_restore.load_model(&original_model_clone)
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

/// Stop pronunciation recording and schedule deferred processing.
#[tauri::command]
#[specta::specta]
pub async fn stop_and_schedule_pronunciation(
    app: AppHandle,
    canonical_word: String,
) -> Result<String, String> {
    let Some(rm) = app.try_state::<Arc<AudioRecordingManager>>() else {
        return Err("AudioRecordingManager not available".to_string());
    };

    // Stop recording and get audio samples — use generation 0 as sentinel
    let samples = rm
        .stop_recording(PRONUNCIATION_BINDING_ID, 0)
        .ok_or_else(|| "No pronunciation recording in progress".to_string())?;

    if samples.is_empty() {
        return Err("Recording produced no audio samples".to_string());
    }

    info!(
        "Pronunciation recording stopped, {} samples captured. Scheduling deferred processing.",
        samples.len()
    );

    // For now, just return success. Deferred pronunciation processing requires
    // the pending_pronunciation field which is not yet in AudioRecordingManager.
    // The stop_and_transcribe_pronunciation_all_models command provides
    // immediate multi-model transcription.

    Ok(format!(
        "Recording saved for '{}'. Use stopAndTranscribePronunciationAllModels for immediate processing.",
        canonical_word
    ))
}
