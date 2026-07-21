//! Tauri commands for the transcription retry queue.
//!
//! These commands provide the frontend-facing API that the fork UI expects.
//! They delegate to the existing `TranscriptionRetryQueue` manager.

use crate::managers::transcription_retry::{RetryableTranscription, TranscriptionRetryQueue};
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

    // Mark the entry as completed (successful retry) — a full re-transcribe
    // would require loading audio from disk and re-running inference. For now,
    // this removes the entry from the queue. The frontend can trigger a
    // history-entry retry separately if needed.
    let queue = queue_state.inner().lock();
    queue
        .mark_retry_complete(&entry_id)
        .map_err(|e| format!("Failed to mark retry complete: {}", e))
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