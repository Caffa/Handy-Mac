use crate::managers::transcription::TranscriptionManager;
use crate::mutex_util::lock_mutex;
use crate::settings::{get_settings, write_settings, ModelUnloadTimeout};
use serde::Serialize;
use specta::Type;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, State};

#[derive(Serialize, Type)]
pub struct ModelLoadStatus {
    is_loaded: bool,
    current_model: Option<String>,
}

#[tauri::command]
#[specta::specta]
pub fn set_model_unload_timeout(app: AppHandle, timeout: ModelUnloadTimeout) {
    let mut settings = get_settings(&app);
    settings.model_unload_timeout = timeout;
    write_settings(&app, settings);
}

#[tauri::command]
#[specta::specta]
pub fn set_repetition_suppression_level(app: AppHandle, level: u8) {
    let mut settings = get_settings(&app);
    // Clamp level to valid range (0-3)
    settings.repetition_suppression_level = level.min(3);
    write_settings(&app, settings);
}

#[tauri::command]
#[specta::specta]
pub fn get_model_load_status(
    transcription_manager: State<'_, Arc<Mutex<TranscriptionManager>>>,
) -> Result<ModelLoadStatus, String> {
    let tm = lock_mutex(&transcription_manager, "TranscriptionManager");
    Ok(ModelLoadStatus {
        is_loaded: tm.is_model_loaded(),
        current_model: tm.get_current_model(),
    })
}

#[tauri::command]
#[specta::specta]
pub fn unload_model_manually(
    transcription_manager: State<'_, Arc<Mutex<TranscriptionManager>>>,
) -> Result<(), String> {
    transcription_manager
        .lock()
        .unwrap()
        .unload_model()
        .map_err(|e| format!("Failed to unload model: {}", e))
}