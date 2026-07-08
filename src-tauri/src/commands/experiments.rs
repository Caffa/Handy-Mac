use crate::audio_toolkit::read_wav_samples;
use crate::managers::history::{ExperimentGroup, HistoryManager, TranscriptionVariant};
use crate::managers::transcription::TranscriptionManager;
use parking_lot::Mutex;
use std::sync::Arc;
use tauri::{AppHandle, State};

#[tauri::command]
#[specta::specta]
pub async fn create_experiment_group(
    _app: AppHandle,
    history_manager: State<'_, Arc<HistoryManager>>,
    recording_id: i64,
) -> Result<ExperimentGroup, String> {
    // Get the recording's transcription text
    let entry = history_manager
        .get_entry_by_id(recording_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("Recording {} not found", recording_id))?;

    // Check if experiment group already exists
    if let Some(existing) = history_manager
        .get_experiment_group_by_recording(recording_id)
        .await
        .map_err(|e| e.to_string())?
    {
        return Ok(existing);
    }

    // Create new experiment group
    history_manager
        .create_experiment_group(recording_id, entry.transcription_text)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
#[specta::specta]
pub async fn get_experiment_group(
    _app: AppHandle,
    history_manager: State<'_, Arc<HistoryManager>>,
    recording_id: i64,
) -> Result<Option<ExperimentGroup>, String> {
    history_manager
        .get_experiment_group_by_recording(recording_id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
#[specta::specta]
pub async fn update_experiment_group(
    _app: AppHandle,
    history_manager: State<'_, Arc<HistoryManager>>,
    id: i64,
    ground_truth: Option<String>,
    speech_speed: Option<String>,
    recording_quality: Option<String>,
    notes: Option<String>,
    is_complete: Option<bool>,
) -> Result<ExperimentGroup, String> {
    history_manager
        .update_experiment_group(
            id,
            ground_truth,
            speech_speed,
            recording_quality,
            notes,
            is_complete,
        )
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
#[specta::specta]
pub async fn add_transcription_variant(
    _app: AppHandle,
    history_manager: State<'_, Arc<HistoryManager>>,
    experiment_group_id: i64,
    model_id: String,
    parameters: String,
    transcription_text: String,
) -> Result<TranscriptionVariant, String> {
    history_manager
        .add_variant(
            experiment_group_id,
            model_id,
            parameters,
            transcription_text,
        )
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
#[specta::specta]
pub async fn get_variants_for_experiment(
    _app: AppHandle,
    history_manager: State<'_, Arc<HistoryManager>>,
    experiment_group_id: i64,
) -> Result<Vec<TranscriptionVariant>, String> {
    history_manager
        .get_variants_for_experiment(experiment_group_id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
#[specta::specta]
pub async fn update_transcription_variant(
    _app: AppHandle,
    history_manager: State<'_, Arc<HistoryManager>>,
    id: i64,
    ranking: Option<i32>,
    is_acceptable: Option<bool>,
    notes: Option<String>,
    match_score: Option<f32>,
) -> Result<TranscriptionVariant, String> {
    history_manager
        .update_variant(id, ranking, is_acceptable, notes, match_score)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
#[specta::specta]
pub async fn get_complete_experiments(
    _app: AppHandle,
    history_manager: State<'_, Arc<HistoryManager>>,
) -> Result<Vec<ExperimentGroup>, String> {
    history_manager
        .get_complete_experiment_groups()
        .await
        .map_err(|e| e.to_string())
}

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

/// Generate transcription variants using multiple models and parameter configurations.
/// This runs the same audio through different transcription settings to compare accuracy.
#[tauri::command]
#[specta::specta]
pub async fn generate_variants(
    _app: AppHandle,
    history_manager: State<'_, Arc<HistoryManager>>,
    transcription_manager: State<'_, Arc<Mutex<TranscriptionManager>>>,
    experiment_group_id: i64,
    models: Vec<String>,
) -> Result<Vec<GeneratedVariant>, String> {
    // Get the experiment group to find the recording
    let experiment = history_manager
        .get_experiment_group_by_id(experiment_group_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("Experiment {} not found", experiment_group_id))?;

    // Get the history entry to find the audio file
    let entry = history_manager
        .get_entry_by_id(experiment.recording_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("Recording {} not found", experiment.recording_id))?;

    // Load the audio file
    let audio_path = history_manager.get_audio_file_path(&entry.file_name);
    let audio_samples = read_wav_samples(&audio_path)
        .map_err(|e| format!("Failed to load audio file {}: {}", audio_path.display(), e))?;

    if audio_samples.is_empty() {
        return Err("Audio file is empty".to_string());
    }

    // Generate variants
    let mut variants = Vec::new();
    let original_model_id = transcription_manager.lock().get_current_model();

    for model_id in &models {
        // Load model (synchronous call)
        if let Err(e) = transcription_manager.lock().load_model(model_id) {
            log::warn!("Failed to load model {}: {}", model_id, e);
            continue;
        }

        // Wait for model to finish loading
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // Transcribe with this model
        match transcription_manager
            .lock()
            .transcribe_for_benchmark(audio_samples.clone())
        {
            Ok(text) => {
                variants.push(GeneratedVariant {
                    model_id: model_id.clone(),
                    parameters: "{}".to_string(), // Default params for now
                    transcription_text: text,
                });
            }
            Err(e) => {
                log::warn!("Failed to transcribe with model {}: {}", model_id, e);
            }
        }
    }

    // Restore original model if it was set
    if let Some(original) = original_model_id {
        if let Err(e) = transcription_manager.lock().load_model(&original) {
            log::warn!("Failed to restore original model: {}", e);
        }
    }

    if variants.is_empty() {
        return Err("No variants could be generated - all models failed".to_string());
    }

    Ok(variants)
}
