use crate::managers::model::{BenchmarkModelFailure, BenchmarkResult, BenchmarkScore, ModelInfo, ModelManager};
use crate::managers::transcription::{ModelStateEvent, TranscriptionManager};
use crate::settings::{get_settings, write_settings, ModelUnloadTimeout};
use log::{info, warn};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, State};

#[tauri::command]
#[specta::specta]
pub async fn get_available_models(
    model_manager: State<'_, Arc<ModelManager>>,
) -> Result<Vec<ModelInfo>, String> {
    Ok(model_manager.get_available_models())
}

#[tauri::command]
#[specta::specta]
pub async fn get_model_info(
    model_manager: State<'_, Arc<ModelManager>>,
    model_id: String,
) -> Result<Option<ModelInfo>, String> {
    Ok(model_manager.get_model_info(&model_id))
}

#[tauri::command]
#[specta::specta]
pub async fn download_model(
    app_handle: AppHandle,
    model_manager: State<'_, Arc<ModelManager>>,
    model_id: String,
) -> Result<(), String> {
    let result = model_manager
        .download_model(&model_id)
        .await
        .map_err(|e| e.to_string());

    if let Err(ref error) = result {
        let _ = app_handle.emit(
            "model-download-failed",
            serde_json::json!({ "model_id": &model_id, "error": error }),
        );
    }

    result
}

#[tauri::command]
#[specta::specta]
pub async fn delete_model(
    app_handle: AppHandle,
    model_manager: State<'_, Arc<ModelManager>>,
    transcription_manager: State<'_, Arc<TranscriptionManager>>,
    model_id: String,
) -> Result<(), String> {
    // If deleting the active model, unload it and clear the setting
    let settings = get_settings(&app_handle);
    if settings.selected_model == model_id {
        transcription_manager
            .unload_model()
            .map_err(|e| format!("Failed to unload model: {}", e))?;

        let mut settings = get_settings(&app_handle);
        settings.selected_model = String::new();
        write_settings(&app_handle, settings);
    }

    model_manager
        .delete_model(&model_id)
        .map_err(|e| e.to_string())
}

/// Shared logic for switching the active model, used by both the Tauri command
/// and the tray menu handler.
///
/// Validates the model, updates the persisted setting, and loads the model
/// unless the unload timeout is set to "Immediately" (in which case the model
/// will be loaded on-demand during the next transcription).
pub fn switch_active_model(app: &AppHandle, model_id: &str) -> Result<(), String> {
    let model_manager = app.state::<Arc<ModelManager>>();
    let transcription_manager = app.state::<Arc<TranscriptionManager>>();

    // Atomically claim the loading slot — prevents concurrent model loads
    // from tray double-clicks or overlapping commands. The guard resets the
    // flag on drop (including early returns, errors, and panics).
    let _loading_guard = transcription_manager
        .try_start_loading()
        .ok_or_else(|| "Model load already in progress".to_string())?;

    // Check if model exists and is available
    let model_info = model_manager
        .get_model_info(model_id)
        .ok_or_else(|| format!("Model not found: {}", model_id))?;

    if !model_info.is_downloaded {
        return Err(format!("Model not downloaded: {}", model_id));
    }

    let settings = get_settings(app);
    let unload_timeout = settings.model_unload_timeout;
    let old_model = settings.selected_model.clone();

    // Persist the new selection early so the frontend sees the correct model
    // when it reacts to events emitted by load_model.
    let mut settings = settings;
    settings.selected_model = model_id.to_string();

    // Reset language to auto if the new model doesn't support the currently selected language.
    // This prevents stale language settings from causing errors (e.g. Canary receiving zh-Hans)
    // and stops downstream processing (e.g. OpenCC) from running on an irrelevant language.
    if settings.selected_language != "auto"
        && !model_info.supported_languages.is_empty()
        && !model_info
            .supported_languages
            .contains(&settings.selected_language)
    {
        log::info!(
            "Resetting language from '{}' to 'auto' (not supported by {})",
            settings.selected_language,
            model_id
        );
        settings.selected_language = "auto".to_string();
    }

    write_settings(app, settings);

    // Skip eager loading if unload is set to "Immediately" — the model
    // will be loaded on-demand during the next transcription.
    if unload_timeout == ModelUnloadTimeout::Immediately {
        // Notify frontend — load_model won't be called so no events
        // would otherwise be emitted.
        let _ = app.emit(
            "model-state-changed",
            ModelStateEvent {
                event_type: "selection_changed".to_string(),
                model_id: Some(model_id.to_string()),
                model_name: Some(model_info.name.clone()),
                error: None,
            },
        );
        log::info!(
            "Model selection changed to {} (not loading — unload set to Immediately).",
            model_id
        );
        return Ok(());
    }

    // Load the model. On failure, revert the persisted selection.
    if let Err(e) = transcription_manager.load_model(model_id) {
        let mut settings = get_settings(app);
        settings.selected_model = old_model;
        write_settings(app, settings);
        return Err(e.to_string());
    }

    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn set_active_model(
    app_handle: AppHandle,
    _model_manager: State<'_, Arc<ModelManager>>,
    _transcription_manager: State<'_, Arc<TranscriptionManager>>,
    model_id: String,
) -> Result<(), String> {
    switch_active_model(&app_handle, &model_id)
}

#[tauri::command]
#[specta::specta]
pub async fn get_current_model(app_handle: AppHandle) -> Result<String, String> {
    let settings = get_settings(&app_handle);
    Ok(settings.selected_model)
}

#[tauri::command]
#[specta::specta]
pub async fn get_transcription_model_status(
    transcription_manager: State<'_, Arc<TranscriptionManager>>,
) -> Result<Option<String>, String> {
    Ok(transcription_manager.get_current_model())
}

#[tauri::command]
#[specta::specta]
pub async fn is_model_loading(
    transcription_manager: State<'_, Arc<TranscriptionManager>>,
) -> Result<bool, String> {
    // Check if transcription manager has a loaded model
    let current_model = transcription_manager.get_current_model();
    Ok(current_model.is_none())
}

#[tauri::command]
#[specta::specta]
pub async fn has_any_models_available(
    model_manager: State<'_, Arc<ModelManager>>,
) -> Result<bool, String> {
    let models = model_manager.get_available_models();
    Ok(models.iter().any(|m| m.is_downloaded))
}

#[tauri::command]
#[specta::specta]
pub async fn has_any_models_or_downloads(
    model_manager: State<'_, Arc<ModelManager>>,
) -> Result<bool, String> {
    let models = model_manager.get_available_models();
    // Return true if any models are downloaded OR if any downloads are in progress
    Ok(models.iter().any(|m| m.is_downloaded))
}

#[tauri::command]
#[specta::specta]
pub async fn cancel_download(
    model_manager: State<'_, Arc<ModelManager>>,
    model_id: String,
) -> Result<(), String> {
    model_manager
        .cancel_download(&model_id)
        .map_err(|e| e.to_string())
}

/// Check whether benchmarking is available (requires downloaded models and history audio clips).
#[tauri::command]
#[specta::specta]
pub async fn can_benchmark_models(
    app_handle: AppHandle,
) -> Result<bool, String> {
    let model_manager = app_handle.state::<Arc<ModelManager>>();
    let history_manager = app_handle.state::<Arc<crate::managers::history::HistoryManager>>();

    let has_downloaded = model_manager.get_available_models().iter().any(|m| m.is_downloaded);
    let has_clips = history_manager.get_history_count().map_err(|e| e.to_string())? >= 20;

    Ok(has_downloaded && has_clips)
}

/// Count the number of history audio clips available for benchmarking.
#[tauri::command]
#[specta::specta]
pub async fn get_benchmark_clip_count(
    app_handle: AppHandle,
) -> Result<usize, String> {
    let history_manager = app_handle.state::<Arc<crate::managers::history::HistoryManager>>();
    history_manager
        .get_history_count()
        .map_err(|e| e.to_string())
}

/// Run a benchmark of all downloaded models against the user's history audio clips.
/// Models that already have valid (non-failed) benchmark data are skipped.
/// Returns benchmark scores with measured transcription times, plus info about
/// any models that failed or were skipped.
/// Emits `benchmark-progress` events as each model is tested.
#[tauri::command]
#[specta::specta]
pub async fn benchmark_models(
    app_handle: AppHandle,
) -> Result<BenchmarkResult, String> {
    use crate::managers::transcription::TranscriptionManager;

    let model_manager = app_handle.state::<Arc<ModelManager>>();
    let history_manager = app_handle.state::<Arc<crate::managers::history::HistoryManager>>();
    let transcription_manager = app_handle.state::<Arc<TranscriptionManager>>();

    // Get downloaded models
    let models = model_manager.get_available_models();
    let downloaded_models: Vec<ModelInfo> = models
        .into_iter()
        .filter(|m| m.is_downloaded && !m.is_custom)
        .collect();

    if downloaded_models.is_empty() {
        return Err("No downloaded models to benchmark".to_string());
    }

    // Determine which models already have valid (non-failed) benchmark data
    let already_benchmarked = model_manager.get_benchmarked_model_ids();

    // Filter to models that need benchmarking (no score or failed)
    let models_to_benchmark: Vec<&ModelInfo> = downloaded_models
        .iter()
        .filter(|m| !already_benchmarked.contains(&m.id))
        .collect();

    let skipped_model_ids: Vec<String> = already_benchmarked
        .iter()
        .filter(|id| downloaded_models.iter().any(|m| m.id == **id))
        .cloned()
        .collect();

    if models_to_benchmark.is_empty() && !already_benchmarked.is_empty() {
        // All models already benchmarked — just re-score them and return
        info!("All models already benchmarked, re-scoring with existing data");
        model_manager.set_benchmark_scores(vec![]); // triggers re-scoring of all existing

        // Collect re-scored results
        let all_models = model_manager.get_available_models();
        let scores: Vec<BenchmarkScore> = all_models
            .iter()
            .filter_map(|m| m.dynamic_score.clone())
            .collect();

        let _ = app_handle.emit(
            "benchmark-progress",
            serde_json::json!({
                "stage": "completed",
                "results_count": scores.len(),
                "skipped_count": skipped_model_ids.len()
            }),
        );

        return Ok(BenchmarkResult {
            scores,
            failed_models: vec![],
            skipped_model_ids,
        });
    }

    // Get history audio clips (up to 20 for benchmarking)
    let entries = history_manager
        .get_recent_entries(20)
        .await
        .map_err(|e| e.to_string())?;

    if entries.is_empty() {
        return Err("No audio clips in history for benchmarking. Record at least 20 clips first.".to_string());
    }

    // Load audio samples from history
    let mut audio_clips: Vec<Vec<f32>> = Vec::new();
    for entry in &entries {
        let audio_path = history_manager.get_audio_file_path(&entry.file_name);
        if let Ok(samples) = crate::audio_toolkit::read_wav_samples(&audio_path) {
            if !samples.is_empty() {
                audio_clips.push(samples);
            }
        }
    }

    if audio_clips.len() < 20 {
        return Err(format!(
            "Need at least 20 audio clips with valid audio for benchmarking, found {}",
            audio_clips.len()
        ));
    }

    // Emit benchmark started event
    let _ = app_handle.emit(
        "benchmark-progress",
        serde_json::json!({
            "stage": "started",
            "model_count": models_to_benchmark.len(),
            "clip_count": audio_clips.len(),
            "skipped_count": skipped_model_ids.len()
        }),
    );

    let total_models = models_to_benchmark.len();
    let mut results: Vec<BenchmarkScore> = Vec::new();
    let mut failed_models: Vec<BenchmarkModelFailure> = Vec::new();

    for (idx, model) in models_to_benchmark.iter().enumerate() {
        // Emit progress for this model
        let _ = app_handle.emit(
            "benchmark-progress",
            serde_json::json!({
                "stage": "loading",
                "model_id": model.id,
                "model_name": model.name,
                "progress": (idx as f64 / total_models as f64 * 100.0) as u32
            }),
        );

        // Load the model
        if let Err(e) = transcription_manager.load_model(&model.id) {
            warn!("Skipping model {} in benchmark: failed to load: {}", model.id, e);
            failed_models.push(BenchmarkModelFailure {
                model_id: model.id.clone(),
                model_name: model.name.clone(),
                reason: "load".to_string(),
                error: e.to_string(),
            });
            continue;
        }

        // Emit transcription stage
        let _ = app_handle.emit(
            "benchmark-progress",
            serde_json::json!({
                "stage": "transcribing",
                "model_id": model.id,
                "model_name": model.name,
                "progress": ((idx as f64 + 0.5) / total_models as f64 * 100.0) as u32
            }),
        );

        // Benchmark: transcribe each clip and measure time
        let mut total_ms: f64 = 0.0;
        let mut clip_count: u32 = 0;
        let mut clip_errors: u32 = 0;

        for (clip_idx, clip) in audio_clips.iter().enumerate() {
            let start = std::time::Instant::now();
            match transcription_manager.transcribe_for_benchmark(clip.clone()) {
                Ok(text) => {
                    // Check if transcription produced actual content (not empty/whitespace)
                    if text.trim().is_empty() {
                        warn!(
                            "Benchmark: model {} produced empty transcription on clip {}",
                            model.id, clip_idx
                        );
                        clip_errors += 1;
                    } else {
                        total_ms += start.elapsed().as_secs_f64() * 1000.0;
                        clip_count += 1;
                    }
                }
                Err(e) => {
                    warn!(
                        "Benchmark: model {} failed on clip {}: {}",
                        model.id, clip_idx, e
                    );
                    clip_errors += 1;
                }
            }
        }

        if clip_count > 0 {
            let avg_ms = total_ms / clip_count as f64;
            results.push(BenchmarkScore {
                model_id: model.id.clone(),
                avg_ms,
                speed_score: 0.0, // Will be computed after all models are done
                clip_count,
                benchmarked_at: chrono::Utc::now().timestamp(),
                failed: false,
            });
        } else {
            // Model loaded but couldn't transcribe any clips
            failed_models.push(BenchmarkModelFailure {
                model_id: model.id.clone(),
                model_name: model.name.clone(),
                reason: "transcribe".to_string(),
                error: format!(
                    "Failed to transcribe any of the {} clips ({} errors)",
                    audio_clips.len(),
                    clip_errors
                ),
            });
            // Also store a failed score so we know this model was benchmarked but failed
            results.push(BenchmarkScore {
                model_id: model.id.clone(),
                avg_ms: 0.0,
                speed_score: 0.0,
                clip_count: 0,
                benchmarked_at: chrono::Utc::now().timestamp(),
                failed: true,
            });
        }

        // Unload the model to free memory before loading the next one
        let _ = transcription_manager.unload_model();
    }

    // Update the model manager with all new scores (which triggers re-scoring across ALL models)
    model_manager.set_benchmark_scores(results.clone());

    // Collect all final scores (including previously benchmarked + newly scored)
    let all_models = model_manager.get_available_models();
    let final_scores: Vec<BenchmarkScore> = all_models
        .iter()
        .filter_map(|m| m.dynamic_score.clone())
        .collect();

    // Emit completion event
    let _ = app_handle.emit(
        "benchmark-progress",
        serde_json::json!({
            "stage": "completed",
            "results_count": final_scores.len(),
            "failed_count": failed_models.len(),
            "skipped_count": skipped_model_ids.len()
        }),
    );

    Ok(BenchmarkResult {
        scores: final_scores,
        failed_models,
        skipped_model_ids,
    })
}
