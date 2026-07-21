//! Tauri commands for the experiment system (A/B accuracy testing).
//!
//! Most backend methods are not yet implemented on this branch. Commands return
//! descriptive errors so the frontend can display "coming soon" or degrade
//! gracefully. The command signatures and types are fully defined here so
//! TypeScript bindings are generated correctly.

use crate::managers::history::{ExperimentGroup, HistoryManager, TranscriptionVariant};
use std::sync::Arc;
use tauri::{AppHandle, State};

/// Variant configuration for experiment generation
#[allow(dead_code)]
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct VariantConfig {
    pub model_id: String,
    pub parameters: String,
    pub display_name: String,
}

/// Result of generating variants for an experiment
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct GeneratedVariant {
    pub model_id: String,
    pub parameters: String,
    pub transcription_text: String,
}

#[tauri::command]
#[specta::specta]
pub async fn create_experiment_group(
    _app: AppHandle,
    _history_manager: State<'_, Arc<HistoryManager>>,
    recording_id: i64,
) -> Result<ExperimentGroup, String> {
    Err(format!(
        "Experiment system not yet available on this branch (recording_id={})",
        recording_id
    ))
}

#[tauri::command]
#[specta::specta]
pub async fn get_experiment_group(
    _app: AppHandle,
    _history_manager: State<'_, Arc<HistoryManager>>,
    recording_id: i64,
) -> Result<Option<ExperimentGroup>, String> {
    Err(format!(
        "Experiment system not yet available on this branch (recording_id={})",
        recording_id
    ))
}

#[tauri::command]
#[specta::specta]
pub async fn update_experiment_group(
    _app: AppHandle,
    _history_manager: State<'_, Arc<HistoryManager>>,
    id: i64,
    ground_truth: Option<String>,
    speech_speed: Option<String>,
    recording_quality: Option<String>,
    notes: Option<String>,
    is_complete: Option<bool>,
) -> Result<ExperimentGroup, String> {
    let _ = (ground_truth, speech_speed, recording_quality, notes, is_complete);
    Err(format!(
        "Experiment system not yet available on this branch (id={})",
        id
    ))
}

#[tauri::command]
#[specta::specta]
pub async fn add_transcription_variant(
    _app: AppHandle,
    _history_manager: State<'_, Arc<HistoryManager>>,
    experiment_group_id: i64,
    model_id: String,
    parameters: String,
    transcription_text: String,
) -> Result<TranscriptionVariant, String> {
    let _ = (model_id, parameters, transcription_text);
    Err(format!(
        "Experiment system not yet available on this branch (experiment_group_id={})",
        experiment_group_id
    ))
}

#[tauri::command]
#[specta::specta]
pub async fn get_variants_for_experiment(
    _app: AppHandle,
    _history_manager: State<'_, Arc<HistoryManager>>,
    experiment_group_id: i64,
) -> Result<Vec<TranscriptionVariant>, String> {
    Err(format!(
        "Experiment system not yet available on this branch (experiment_group_id={})",
        experiment_group_id
    ))
}

#[tauri::command]
#[specta::specta]
pub async fn update_transcription_variant(
    _app: AppHandle,
    _history_manager: State<'_, Arc<HistoryManager>>,
    id: i64,
    ranking: Option<i32>,
    is_acceptable: Option<bool>,
    notes: Option<String>,
    match_score: Option<f32>,
) -> Result<TranscriptionVariant, String> {
    let _ = (ranking, is_acceptable, notes, match_score);
    Err(format!(
        "Experiment system not yet available on this branch (id={})",
        id
    ))
}

#[tauri::command]
#[specta::specta]
pub async fn get_complete_experiments(
    _app: AppHandle,
    _history_manager: State<'_, Arc<HistoryManager>>,
) -> Result<Vec<ExperimentGroup>, String> {
    Err("Experiment system not yet available on this branch".to_string())
}

/// Generate transcription variants using multiple models and parameter configurations.
#[tauri::command]
#[specta::specta]
pub async fn generate_variants(
    _app: AppHandle,
    _history_manager: State<'_, Arc<HistoryManager>>,
    experiment_group_id: i64,
    models: Vec<String>,
) -> Result<Vec<GeneratedVariant>, String> {
    let _ = models;
    Err(format!(
        "Experiment system not yet available on this branch (experiment_group_id={})",
        experiment_group_id
    ))
}