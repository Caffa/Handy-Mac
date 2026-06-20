//! Tauri commands for the transcription retry queue.

use crate::managers::history::HistoryManager;
use crate::managers::transcription::TranscriptionManager;
use crate::managers::transcription_retry::{RetryableTranscription, TranscriptionFailure, TranscriptionRetryQueue};
use tauri::{AppHandle, Manager};
use std::sync::Arc;

/// Get all pending retry entries.
#[tauri::command]
#[specta::specta]
pub async fn get_retry_queue(app: AppHandle) -> Result<Vec<RetryableTranscription>, String> {
    let queue = app
        .state::<Arc<TranscriptionRetryQueue>>()
        .inner()
        .clone();
    
    Ok(queue.get_all_pending())
}

/// Manually trigger a retry for a specific entry.
#[tauri::command]
#[specta::specta]
pub async fn retry_transcription(app: AppHandle, entry_id: String) -> Result<(), String> {
    let queue = app
        .state::<Arc<TranscriptionRetryQueue>>()
        .inner()
        .clone();
    
    // Get the entry
    let entries = queue.get_all_pending();
    let entry = entries
        .into_iter()
        .find(|e| e.id == entry_id)
        .ok_or_else(|| format!("Entry {} not found", entry_id))?;
    
    // Load audio samples from WAV file
    let audio_samples = crate::audio_toolkit::audio::read_wav_samples(&entry.audio_path)
        .map_err(|e| format!("Failed to load audio: {}", e))?;
    
    // Get transcription manager
    let tm = app
        .state::<Arc<TranscriptionManager>>()
        .inner()
        .clone();
    
    // Try transcription
    let result = tm.transcribe(audio_samples);
    
    match result {
        Ok(transcription) => {
            // Success - update history if we have an entry
            if let Some(history_id) = entry.history_entry_id {
                let hm = app
                    .state::<Arc<HistoryManager>>()
                    .inner()
                    .clone();
                
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
            queue.mark_retry_complete(&entry_id)
                .map_err(|e| format!("Failed to mark complete: {}", e))?;
            
            Ok(())
        }
        Err(e) => {
            // Failed again - mark as failed
            let failure = TranscriptionFailure::Unknown {
                error: e.to_string(),
            };
            
            let can_retry = queue
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
    let queue = app
        .state::<Arc<TranscriptionRetryQueue>>()
        .inner()
        .clone();
    
    queue
        .remove_entry(&entry_id)
        .map_err(|e| format!("Failed to remove entry: {}", e))
}

/// Clear all pending retry entries.
#[tauri::command]
#[specta::specta]
pub async fn clear_retry_queue(app: AppHandle) -> Result<(), String> {
    let queue = app
        .state::<Arc<TranscriptionRetryQueue>>()
        .inner()
        .clone();
    
    queue
        .clear_all()
        .map_err(|e| format!("Failed to clear queue: {}", e))
}

/// Get the count of pending retries.
#[tauri::command]
#[specta::specta]
pub async fn get_retry_queue_count(app: AppHandle) -> Result<usize, String> {
    let queue = app
        .state::<Arc<TranscriptionRetryQueue>>()
        .inner()
        .clone();
    
    Ok(queue.count())
}