//! Tauri commands for the transcription retry queue.

use crate::managers::history::HistoryManager;
use crate::managers::transcription::TranscriptionManager;
use crate::managers::transcription_retry::{
    RetryableTranscription, TranscriptionFailure, TranscriptionRetryQueue,
};
use parking_lot::Mutex;
use std::sync::Arc;
use tauri::{AppHandle, Manager};

/// Get all pending retry entries.
#[tauri::command]
#[specta::specta]
pub async fn get_retry_queue(app: AppHandle) -> Result<Vec<RetryableTranscription>, String> {
    let Some(queue_state) = app.try_state::<Arc<Mutex<TranscriptionRetryQueue>>>() else {
        return Err("TranscriptionRetryQueue not available".to_string());
    };
    let queue = queue_state.inner().lock();
    Ok(queue.get_all_pending())
}

/// Manually trigger a retry for a specific entry.
#[tauri::command]
#[specta::specta]
pub async fn retry_transcription(app: AppHandle, entry_id: String) -> Result<(), String> {
    let Some(queue_state) = app.try_state::<Arc<Mutex<TranscriptionRetryQueue>>>() else {
        return Err("TranscriptionRetryQueue not available".to_string());
    };
    let queue_guard = queue_state.inner().lock();

    // Get the entry
    let entries = queue_guard.get_all_pending();
    let entry = entries
        .into_iter()
        .find(|e| e.id == entry_id)
        .ok_or_else(|| format!("Entry {} not found", entry_id))?;

    // Load audio samples from WAV file
    let audio_samples = crate::audio_toolkit::audio::read_wav_samples(&entry.audio_path)
        .map_err(|e| format!("Failed to load audio: {}", e))?;

    // Get transcription manager
    let tm_guard = app
        .try_state::<Arc<Mutex<TranscriptionManager>>>()
        .ok_or_else(|| "TranscriptionManager not available".to_string())?
        .inner()
        .lock();

    // Try transcription
    let result = tm_guard.transcribe(audio_samples);

    match result {
        Ok(transcription) => {
            // Success - update history if we have an entry
            if let Some(history_id) = entry.history_entry_id {
                let Some(hm_state) = app.try_state::<Arc<HistoryManager>>() else {
                    return Err("HistoryManager not available".to_string());
                };
                let hm = hm_state.inner();

                hm.update_transcription(
                    history_id,
                    transcription.text.clone(),
                    None,
                    None,
                    Some(transcription.model_id),
                )
                .map_err(|e| format!("Failed to update history: {}", e))?;
            }

            // Remove from retry queue
            queue_guard
                .mark_retry_complete(&entry_id)
                .map_err(|e| format!("Failed to mark complete: {}", e))?;

            Ok(())
        }
        Err(e) => {
            // Failed again - mark as failed
            let failure = TranscriptionFailure::Unknown {
                error: e.to_string(),
            };

            let can_retry = queue_guard
                .mark_retry_failed(&entry_id, failure)
                .map_err(|e| format!("Failed to mark retry failed: {}", e))?;

            if !can_retry {
                Err(format!("Max retries exceeded for entry {}", entry_id))
            } else {
                Err(format!("Retry failed: {}", e))
            }
        }
    }
}

/// Remove a specific entry from the retry queue.
#[tauri::command]
#[specta::specta]
pub async fn remove_from_retry_queue(app: AppHandle, entry_id: String) -> Result<bool, String> {
    let Some(queue_state) = app.try_state::<Arc<Mutex<TranscriptionRetryQueue>>>() else {
        return Err("TranscriptionRetryQueue not available".to_string());
    };
    let queue = queue_state.inner().lock();

    queue
        .remove_entry(&entry_id)
        .map_err(|e| format!("Failed to remove entry: {}", e))
}

/// Clear all pending retry entries.
#[tauri::command]
#[specta::specta]
pub async fn clear_retry_queue(app: AppHandle) -> Result<(), String> {
    let Some(queue_state) = app.try_state::<Arc<Mutex<TranscriptionRetryQueue>>>() else {
        return Err("TranscriptionRetryQueue not available".to_string());
    };
    let queue = queue_state.inner().lock();

    queue
        .clear_all()
        .map_err(|e| format!("Failed to clear queue: {}", e))
}

/// Get the count of pending retries.
#[tauri::command]
#[specta::specta]
pub async fn get_retry_queue_count(app: AppHandle) -> Result<usize, String> {
    let Some(queue_state) = app.try_state::<Arc<Mutex<TranscriptionRetryQueue>>>() else {
        return Err("TranscriptionRetryQueue not available".to_string());
    };
    let queue = queue_state.inner().lock();

    Ok(queue.count())
}
