use crate::audio_toolkit::audio::AudioQualityMetrics;
use crate::audio_toolkit::{
    process_transcription_text, trim_trailing_silence,
};
use crate::errors::{AppError, AppResult};
use crate::managers::audio::AudioRecordingManager;
use crate::managers::model::{EngineType, ModelManager};
use crate::settings::{
    get_settings, ModelUnloadTimeout, OrtAcceleratorSetting, WhisperAcceleratorSetting,
    WordCorrectionMode,
};
use anyhow::Result;
use log::{debug, error, info, warn};
use serde::Serialize;
use specta::Type;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::thread;
use std::time::{Duration, SystemTime};
use tauri::{AppHandle, Emitter, Manager};
use transcribe_rs::{
    onnx::{
        canary::CanaryModel,
        cohere::CohereModel,
        gigaam::GigaAMModel,
        moonshine::{MoonshineModel, MoonshineVariant, StreamingModel},
        parakeet::{ParakeetModel, ParakeetParams, TimestampGranularity},
        sense_voice::{SenseVoiceModel, SenseVoiceParams},
        Quantization,
    },
    whisper_cpp::{WhisperEngine, WhisperInferenceParams},
    SpeechModel, TranscribeOptions,
};

/// Result of a transcription call, including metadata about which model was used.
#[derive(Clone, Debug, Serialize)]
pub struct TranscriptionOutput {
    /// The transcribed text.
    pub text: String,
    /// The model ID that produced this transcription (e.g. "turbo", "parakeet-tdt-0.6b-v2").
    pub model_id: String,
    /// Number of tokens suppressed by the decoder's confidence thresholding,
    /// if the engine supports this (currently only Parakeet). `None` for
    /// engines that don't track suppression or when no tokens were suppressed.
    pub suppressed_token_count: Option<usize>,
    /// Raw segments with timestamps from the transcription engine.
    /// Only populated for engines that support timestamps (Whisper, Parakeet).
    /// `None` for engines like Moonshine that don't produce timestamps.
    pub segments: Option<Vec<transcribe_rs::TranscriptionSegment>>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ModelStateEvent {
    pub event_type: String,
    pub model_id: Option<String>,
    pub model_name: Option<String>,
    pub error: Option<String>,
}

enum LoadedEngine {
    Whisper(WhisperEngine),
    Parakeet(ParakeetModel),
    Moonshine(MoonshineModel),
    MoonshineStreaming(StreamingModel),
    SenseVoice(SenseVoiceModel),
    GigaAM(GigaAMModel),
    Canary(CanaryModel),
    Cohere(CohereModel),
}

/// RAII guard that clears the `is_loading` flag and notifies waiters on drop.
/// Ensures the loading flag is always reset, even on early returns or panics.
pub struct LoadingGuard {
    is_loading: Arc<Mutex<bool>>,
    loading_condvar: Arc<Condvar>,
}

impl Drop for LoadingGuard {
    fn drop(&mut self) {
        let mut is_loading = self.is_loading.lock().unwrap();
        *is_loading = false;
        self.loading_condvar.notify_all();
    }
}

#[derive(Clone)]
pub struct TranscriptionManager {
    engine: Arc<Mutex<Option<LoadedEngine>>>,
    model_manager: Arc<ModelManager>,
    app_handle: AppHandle,
    current_model_id: Arc<Mutex<Option<String>>>,
    last_activity: Arc<AtomicU64>,
    shutdown_signal: Arc<AtomicBool>,
    watcher_handle: Arc<Mutex<Option<thread::JoinHandle<()>>>>,
    is_loading: Arc<Mutex<bool>>,
    loading_condvar: Arc<Condvar>,
    /// Flag to prevent concurrent transcription calls (streaming vs final).
    /// When streaming transcription is enabled, partial transcriptions run
    /// every 2.5s during recording. When recording stops, the final transcription
    /// must wait for any in-progress streaming transcription to complete.
    is_transcribing: Arc<AtomicBool>,
    /// Flag to cancel streaming transcription when recording stops.
    /// When set, the streaming callback should skip transcription and return early.
    /// This prevents wasted work when the user stops recording mid-streaming-transcription.
    cancel_streaming: Arc<AtomicBool>,
}

impl TranscriptionManager {
    pub fn new(app_handle: &AppHandle, model_manager: Arc<ModelManager>) -> Result<Self> {
        let manager = Self {
            engine: Arc::new(Mutex::new(None)),
            model_manager,
            app_handle: app_handle.clone(),
            current_model_id: Arc::new(Mutex::new(None)),
            last_activity: Arc::new(AtomicU64::new(Self::now_ms())),
            shutdown_signal: Arc::new(AtomicBool::new(false)),
            watcher_handle: Arc::new(Mutex::new(None)),
            is_loading: Arc::new(Mutex::new(false)),
            loading_condvar: Arc::new(Condvar::new()),
            is_transcribing: Arc::new(AtomicBool::new(false)),
            cancel_streaming: Arc::new(AtomicBool::new(false)),
        };

        // Start the idle watcher
        {
            let app_handle_cloned = app_handle.clone();
            let manager_cloned = manager.clone();
            let shutdown_signal = manager.shutdown_signal.clone();
            let handle = thread::spawn(move || {
                debug!("Idle watcher thread started");
                while !shutdown_signal.load(Ordering::Relaxed) {
                    thread::sleep(Duration::from_secs(10)); // Check every 10 seconds

                    // Check shutdown signal again after sleep
                    if shutdown_signal.load(Ordering::Relaxed) {
                        break;
                    }

                    let settings = get_settings(&app_handle_cloned);
                    let timeout = settings.model_unload_timeout;

                    // Skip Immediately — that variant is handled by
                    // maybe_unload_immediately() after each transcription.
                    // Treating it as 0s here would unload the model mid-recording.
                    if timeout == ModelUnloadTimeout::Immediately {
                        continue;
                    }

                    // While recording, keep the idle timer fresh so the
                    // model is never unloaded mid-session.
                    let is_recording = app_handle_cloned
                        .try_state::<Arc<AudioRecordingManager>>()
                        .map_or(false, |a| a.is_recording());
                    if is_recording {
                        manager_cloned.touch_activity();
                        continue;
                    }

                    if let Some(limit_seconds) = timeout.to_seconds() {
                        let last = manager_cloned.last_activity.load(Ordering::Relaxed);
                        let now_ms = TranscriptionManager::now_ms();
                        let idle_ms = now_ms.saturating_sub(last);
                        let limit_ms = limit_seconds * 1000;

                        if idle_ms > limit_ms {
                            // idle -> unload
                            if manager_cloned.is_model_loaded() {
                                let unload_start = std::time::Instant::now();
                                info!(
                                    "Model idle for {}s (limit: {}s), unloading",
                                    idle_ms / 1000,
                                    limit_seconds
                                );
                                match manager_cloned.unload_model() {
                                    Ok(()) => {
                                        let unload_duration = unload_start.elapsed();
                                        info!(
                                            "Model unloaded due to inactivity (took {}ms)",
                                            unload_duration.as_millis()
                                        );
                                    }
                                    Err(e) => {
                                        error!("Failed to unload idle model: {}", e);
                                    }
                                }
                            }
                        }
                    }
                }
                debug!("Idle watcher thread shutting down gracefully");
            });
            *manager.watcher_handle.lock().unwrap() = Some(handle);
        }

        Ok(manager)
    }

    /// Lock the engine mutex, recovering from poison if a previous transcription panicked.
    fn lock_engine(&self) -> MutexGuard<'_, Option<LoadedEngine>> {
        self.engine.lock().unwrap_or_else(|poisoned| {
            warn!("Engine mutex was poisoned by a previous panic, recovering");
            poisoned.into_inner()
        })
    }

    pub fn is_model_loaded(&self) -> bool {
        let engine = self.lock_engine();
        engine.is_some()
    }

    /// Atomically check whether a model load is in progress and, if not, mark
    /// one as starting. Returns a [`LoadingGuard`] whose [`Drop`] impl will
    /// clear the flag and wake waiters. Returns `None` if a load is already in
    /// progress.
    pub fn try_start_loading(&self) -> Option<LoadingGuard> {
        let mut is_loading = self.is_loading.lock().unwrap();
        if *is_loading {
            return None;
        }
        *is_loading = true;
        Some(LoadingGuard {
            is_loading: self.is_loading.clone(),
            loading_condvar: self.loading_condvar.clone(),
        })
    }

    pub fn unload_model(&self) -> AppResult<()> {
        let unload_start = std::time::Instant::now();
        debug!("Starting to unload model");

        {
            let mut engine = self.lock_engine();
            // Dropping the engine frees all resources
            *engine = None;
        }
        {
            let mut current_model = self.current_model_id.lock().unwrap();
            *current_model = None;
        }

        // Emit unloaded event
        let _ = self.app_handle.emit(
            "model-state-changed",
            ModelStateEvent {
                event_type: "unloaded".to_string(),
                model_id: None,
                model_name: None,
                error: None,
            },
        );

        let unload_duration = unload_start.elapsed();
        debug!(
            "Model unloaded manually (took {}ms)",
            unload_duration.as_millis()
        );
        Ok(())
    }

    /// Request cancellation of any in-progress streaming transcription.
    /// Called when recording stops to prevent wasted work on partial audio.
    pub fn cancel_streaming(&self) {
        self.cancel_streaming.store(true, Ordering::Release);
        debug!("Streaming cancellation requested");
    }

    /// Clear the streaming cancellation flag.
    /// Should be called when starting a new recording session.
    pub fn clear_streaming_cancel(&self) {
        self.cancel_streaming.store(false, Ordering::Release);
    }

    /// Check if streaming cancellation has been requested.
    /// Streaming callbacks should check this and abort early if true.
    pub fn is_streaming_cancelled(&self) -> bool {
        self.cancel_streaming.load(Ordering::Acquire)
    }

    fn now_ms() -> u64 {
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
    }

    /// Reset the idle timer to now.
    fn touch_activity(&self) {
        self.last_activity.store(Self::now_ms(), Ordering::Relaxed);
    }

    /// Unloads the model immediately if the setting is enabled and the model is loaded
    pub fn maybe_unload_immediately(&self, context: &str) {
        let settings = get_settings(&self.app_handle);
        if settings.model_unload_timeout == ModelUnloadTimeout::Immediately
            && self.is_model_loaded()
        {
            info!("Immediately unloading model after {}", context);
            if let Err(e) = self.unload_model() {
                warn!("Failed to immediately unload model: {}", e);
            }
        }
    }

    pub fn load_model(&self, model_id: &str) -> AppResult<()> {
        let load_start = std::time::Instant::now();
        debug!("Starting to load model: {}", model_id);

        // Emit loading started event
        let _ = self.app_handle.emit(
            "model-state-changed",
            ModelStateEvent {
                event_type: "loading_started".to_string(),
                model_id: Some(model_id.to_string()),
                model_name: None,
                error: None,
            },
        );

        let model_info = self
            .model_manager
            .get_model_info(model_id)
            .ok_or_else(|| AppError::ModelNotFound(model_id.to_string()))?;

        if !model_info.is_downloaded {
            let error_msg = "Model not downloaded";
            let _ = self.app_handle.emit(
                "model-state-changed",
                ModelStateEvent {
                    event_type: "loading_failed".to_string(),
                    model_id: Some(model_id.to_string()),
                    model_name: Some(model_info.name.clone()),
                    error: Some(error_msg.to_string()),
                },
            );
            return Err(AppError::ModelNotDownloaded(model_id.to_string()));
        }

        let model_path = self.model_manager.get_model_path(model_id)?;

        // Create appropriate engine based on model type
        let emit_loading_failed = |error_msg: &str| {
            let _ = self.app_handle.emit(
                "model-state-changed",
                ModelStateEvent {
                    event_type: "loading_failed".to_string(),
                    model_id: Some(model_id.to_string()),
                    model_name: Some(model_info.name.clone()),
                    error: Some(error_msg.to_string()),
                },
            );
        };

        let loaded_engine = match model_info.engine_type {
            EngineType::Whisper => {
                let engine = WhisperEngine::load(&model_path).map_err(|e| {
                    let error_msg = format!("Failed to load whisper model {}: {}", model_id, e);
                    emit_loading_failed(&error_msg);
                    AppError::model_load("Whisper", model_id, error_msg, anyhow::anyhow!("{}", e))
                })?;
                LoadedEngine::Whisper(engine)
            }
            EngineType::Parakeet => {
                let engine =
                    ParakeetModel::load(&model_path, &Quantization::Int8).map_err(|e| {
                        let error_msg =
                            format!("Failed to load parakeet model {}: {}", model_id, e);
                        emit_loading_failed(&error_msg);
                        AppError::model_load("Parakeet", model_id, error_msg, anyhow::anyhow!("{}", e))
                    })?;
                LoadedEngine::Parakeet(engine)
            }
            EngineType::Moonshine => {
                let engine = MoonshineModel::load(
                    &model_path,
                    MoonshineVariant::Base,
                    &Quantization::default(),
                )
                .map_err(|e| {
                    let error_msg = format!("Failed to load moonshine model {}: {}", model_id, e);
                    emit_loading_failed(&error_msg);
                    AppError::model_load("Moonshine", model_id, error_msg, anyhow::anyhow!("{}", e))
                })?;
                LoadedEngine::Moonshine(engine)
            }
            EngineType::MoonshineStreaming => {
                let engine = StreamingModel::load(&model_path, 0, &Quantization::default())
                    .map_err(|e| {
                        let error_msg = format!(
                            "Failed to load moonshine streaming model {}: {}",
                            model_id, e
                        );
                        emit_loading_failed(&error_msg);
                        AppError::model_load("MoonshineStreaming", model_id, error_msg, anyhow::anyhow!("{}", e))
                    })?;
                LoadedEngine::MoonshineStreaming(engine)
            }
            EngineType::SenseVoice => {
                let engine =
                    SenseVoiceModel::load(&model_path, &Quantization::Int8).map_err(|e| {
                        let error_msg =
                            format!("Failed to load SenseVoice model {}: {}", model_id, e);
                        emit_loading_failed(&error_msg);
                        AppError::model_load("SenseVoice", model_id, error_msg, anyhow::anyhow!("{}", e))
                    })?;
                LoadedEngine::SenseVoice(engine)
            }
            EngineType::GigaAM => {
                let engine = GigaAMModel::load(&model_path, &Quantization::Int8).map_err(|e| {
                    let error_msg = format!("Failed to load gigaam model {}: {}", model_id, e);
                    emit_loading_failed(&error_msg);
                    AppError::model_load("GigaAM", model_id, error_msg, anyhow::anyhow!("{}", e))
                })?;
                LoadedEngine::GigaAM(engine)
            }
            EngineType::Canary => {
                let engine = CanaryModel::load(&model_path, &Quantization::Int8).map_err(|e| {
                    let error_msg = format!("Failed to load canary model {}: {}", model_id, e);
                    emit_loading_failed(&error_msg);
                    AppError::model_load("Canary", model_id, error_msg, anyhow::anyhow!("{}", e))
                })?;
                LoadedEngine::Canary(engine)
            }
            EngineType::Cohere => {
                let engine = CohereModel::load(&model_path, &Quantization::Int8).map_err(|e| {
                    let error_msg = format!("Failed to load cohere model {}: {}", model_id, e);
                    emit_loading_failed(&error_msg);
                    AppError::model_load("Cohere", model_id, error_msg, anyhow::anyhow!("{}", e))
                })?;
                LoadedEngine::Cohere(engine)
            }
        };

        // Update the current engine and model ID
        {
            let mut engine = self.lock_engine();
            *engine = Some(loaded_engine);
        }
        {
            let mut current_model = self.current_model_id.lock().unwrap();
            *current_model = Some(model_id.to_string());
        }

        // Reset idle timer so the watcher doesn't immediately unload a just-loaded model
        self.touch_activity();

        // Emit loading completed event
        let _ = self.app_handle.emit(
            "model-state-changed",
            ModelStateEvent {
                event_type: "loading_completed".to_string(),
                model_id: Some(model_id.to_string()),
                model_name: Some(model_info.name.clone()),
                error: None,
            },
        );

        let load_duration = load_start.elapsed();
        debug!(
            "Successfully loaded transcription model: {} (took {}ms)",
            model_id,
            load_duration.as_millis()
        );
        Ok(())
    }

    /// Kicks off the model loading in a background thread if it's not already loaded.
    /// Uses LoadingGuard to ensure `is_loading` is always reset even if the thread panics.
    pub fn initiate_model_load(&self) {
        let guard = match self.try_start_loading() {
            Some(g) => g,
            None => return, // already loading or loaded
        };

        let self_clone = self.clone();
        thread::spawn(move || {
            // LoadingGuard's Drop impl will reset is_loading and notify waiters
            let _guard = guard;
            let settings = get_settings(&self_clone.app_handle);
            if let Err(e) = self_clone.load_model(&settings.selected_model) {
                error!("Failed to load model: {}", e);
            }
        });
    }

    pub fn get_current_model(&self) -> Option<String> {
        let current_model = self.current_model_id.lock().unwrap();
        current_model.clone()
    }

    /// Returns true if a model is currently being loaded in the background.
    pub fn is_model_loading(&self) -> bool {
        let is_loading = self.is_loading.lock().unwrap();
        *is_loading
    }

    pub fn transcribe(&self, audio: Vec<f32>) -> AppResult<TranscriptionOutput> {
        #[cfg(debug_assertions)]
        if std::env::var("HANDY_FORCE_TRANSCRIPTION_FAILURE").is_ok() {
            return Err(AppError::Transcription {
                message: "Simulated transcription failure (HANDY_FORCE_TRANSCRIPTION_FAILURE)"
                    .to_string(),
                source: anyhow::anyhow!("Simulated failure for testing"),
            });
        }

        // Wait for any in-progress transcription to complete.
        // This prevents race conditions between streaming transcription (running
        // every 2.5s during recording) and the final transcription (after recording stops).
        // We use a spin-wait with yield to avoid blocking the thread indefinitely.
        let wait_start = std::time::Instant::now();
        let max_wait = Duration::from_secs(30);
        while self.is_transcribing.load(Ordering::Relaxed) {
            if wait_start.elapsed() > max_wait {
                warn!("Timed out waiting for previous transcription to complete");
                return Err(AppError::TranscriptionBusy);
            }
            std::thread::yield_now();
        }

        // Set transcribing flag for the duration of this transcription
        self.is_transcribing.store(true, Ordering::Relaxed);
        struct TranscribingGuard {
            flag: Arc<AtomicBool>,
        }
        impl Drop for TranscribingGuard {
            fn drop(&mut self) {
                self.flag.store(false, Ordering::Relaxed);
            }
        }
        let _guard = TranscribingGuard {
            flag: self.is_transcribing.clone(),
        };

        // Update last activity timestamp
        self.touch_activity();

        let st = std::time::Instant::now();

        debug!("Audio vector length: {}", audio.len());

        // Check if model is loaded, if not try to load it
        {
            // If the model is loading, wait for it to complete (with timeout).
            // A previous bug caused hangs when the loading thread panicked without
            // resetting the is_loading flag, blocking transcribe() forever.
            let mut is_loading = self.is_loading.lock().unwrap();
            let wait_deadline = std::time::Instant::now() + Duration::from_secs(120);
            while *is_loading {
                let remaining = wait_deadline.saturating_duration_since(std::time::Instant::now());
                if remaining.is_zero() {
                    // Timed out waiting for model to load. Force-reset the flag
                    // so subsequent calls don't also hang, and return an error.
                    warn!("Timed out waiting for model to load after 120s — transcription aborted");
                    *is_loading = false;
                    self.loading_condvar.notify_all();
                    return Err(AppError::TranscriptionLoadTimeout);
                }
                let result = self
                    .loading_condvar
                    .wait_timeout(is_loading, remaining)
                    .unwrap();
                is_loading = result.0;
            }

            let engine_guard = self.lock_engine();
            if engine_guard.is_none() {
                return Err(AppError::ModelNotLoaded);
            }
        }

        // Get current settings for configuration
        let settings = get_settings(&self.app_handle);

        // Determine which model to use (hybrid mode or standard).
        // Hybrid mode picks a different model based on audio length:
        // short audio uses the "short audio model", long audio uses the "long audio model".
        let effective_model_id = if settings.hybrid_mode_enabled {
            let audio_duration_secs = audio.len() as f64 / 16000.0;
            if audio_duration_secs < settings.hybrid_threshold_secs {
                debug!(
                    "Hybrid mode: audio is {:.1}s (< {}s threshold), using short audio model",
                    audio_duration_secs, settings.hybrid_threshold_secs
                );
                settings
                    .hybrid_short_audio_model
                    .clone()
                    .unwrap_or(settings.selected_model.clone())
            } else {
                debug!(
                    "Hybrid mode: audio is {:.1}s (>= {}s threshold), using long audio model",
                    audio_duration_secs, settings.hybrid_threshold_secs
                );
                settings
                    .hybrid_long_audio_model
                    .clone()
                    .unwrap_or(settings.selected_model.clone())
            }
        } else {
            settings.selected_model.clone()
        };

        // If hybrid mode selected a different model than what's loaded, load it.
        // This handles the case where the user switches between short/long models.
        // IMPORTANT: We load the model BEFORE taking the engine out of the mutex
        // to avoid a race where load_model() writes a new engine but the old engine
        // is put back at the end of transcription, overwriting the new one.
        let current_loaded = self.current_model_id.lock().unwrap().clone();
        if effective_model_id != current_loaded.clone().unwrap_or_default() {
            debug!(
                "Loading effective model '{}' for transcription (currently loaded: {:?})",
                effective_model_id, current_loaded
            );
            // Release any locks before loading
            drop(current_loaded);
            if let Err(e) = self.load_model(&effective_model_id) {
                error!(
                    "Failed to load effective model '{}': {}",
                    effective_model_id, e
                );
                return Err(AppError::model_load(
                    "effective",
                    &effective_model_id,
                    format!("Failed to load model '{}': {}", effective_model_id, e),
                    anyhow::anyhow!("{}", e),
                ));
            }
        } else {
            drop(current_loaded);
        }

        // Trim trailing silence from audio before transcription.
        // Critical for Whisper (hallucinates on silence) AND for autoregressive
        // transducer models (Parakeet TDT) whose decoder free-runs language
        // model continuations into trailing silence.
        let _effective_model_info = self.model_manager.get_model_info(&effective_model_id);

        // Use the same VAD threshold as recording to avoid dropping audio that was
        // captured during recording but then trimmed away before transcription.
        // Previously used hardcoded 0.5 which was more aggressive than the recording
        // VAD (default 0.30), causing first/last words to be dropped.
        let vad_threshold = settings.vad_sensitivity.threshold();

        let audio = match self.app_handle.path().resolve(
            "resources/models/silero_vad_v4.onnx",
            tauri::path::BaseDirectory::Resource,
        ) {
            Ok(vad_path) => {
                let path_str = vad_path.to_str().unwrap_or("");
                trim_trailing_silence(&audio, path_str, vad_threshold)
            }
            Err(e) => {
                warn!(
                    "Could not resolve VAD model path for trimming ({}), skipping",
                    e
                );
                audio
            }
        };

        // Re-check empty after possible trim
        if audio.is_empty() {
            debug!("Audio became empty after VAD trim");
            self.maybe_unload_immediately("empty audio after trim");
            return Ok(TranscriptionOutput {
                text: String::new(),
                model_id: effective_model_id,
                suppressed_token_count: None,
                segments: None,
            });
        }

        // Validate selected language against the effective model's supported languages.
        // If the language isn't supported, fall back to "auto" to prevent errors.
        let validated_language = if settings.selected_language == "auto" {
            "auto".to_string()
        } else {
            let is_supported = self
                .model_manager
                .get_model_info(&effective_model_id)
                .map(|info| {
                    info.supported_languages.is_empty()
                        || info
                            .supported_languages
                            .contains(&settings.selected_language)
                })
                .unwrap_or(true);

            if is_supported {
                settings.selected_language.clone()
            } else {
                warn!(
                    "Language '{}' not supported by current model, falling back to auto-detect",
                    settings.selected_language
                );
                "auto".to_string()
            }
        };

        // Perform transcription with the appropriate engine.
        // We use catch_unwind to prevent engine panics from poisoning the mutex,
        // which would make the app hang indefinitely on subsequent operations.
        let result = {
            let mut engine_guard = self.lock_engine();

            // Take the engine out so we own it during transcription.
            // If the engine panics, we simply don't put it back (effectively unloading it)
            // instead of poisoning the mutex.
            let mut engine = match engine_guard.take() {
                Some(e) => {
                    // Release the lock before transcribing — no mutex held during the engine call
                    drop(engine_guard);
                    e
                }
                None => {
                    warn!("Engine not loaded when transcribe() started - attempting emergency load");
                    drop(engine_guard);
                    
                    // Try to load the model immediately
                    let model_id = settings.selected_model.clone();
                    
                    // Try loading primary model
                    if let Err(e) = self.load_model(&model_id) {
                        warn!("Primary model load failed: {} - trying fallback models", e);
                        
                        // Try fallback models from hybrid mode
                        let mut fallback_tried = false;
                        if settings.hybrid_mode_enabled {
                            for fallback in [&settings.hybrid_short_audio_model, &settings.hybrid_long_audio_model]
                                .into_iter()
                                .filter_map(|m| m.as_ref())
                                .filter(|m| *m != &model_id) 
                            {
                                info!("Trying fallback model: {}", fallback);
                                if self.load_model(fallback).is_ok() {
                                    fallback_tried = true;
                                    break;
                                }
                            }
                        }
                        
                        if !fallback_tried {
                            return Err(AppError::ModelLoadFailed {
                                engine: "fallback".to_string(),
                                model_id: model_id,
                                message: format!("Model failed to load and no fallback available: {}", e),
                                source: anyhow::anyhow!("{}", e),
                            });
                        }
                    }
                    
                    // Re-acquire engine after load
                    let mut engine_guard = self.lock_engine();
                    match engine_guard.take() {
                        Some(e) => {
                            // Release the lock before transcribing
                            drop(engine_guard);
                            e
                        }
                        None => {
                            return Err(AppError::ModelNotLoaded);
                        }
                    }
                }
            };

            let transcribe_result = catch_unwind(AssertUnwindSafe(
                || -> Result<transcribe_rs::TranscriptionResult, AppError> {
                    match &mut engine {
                        LoadedEngine::Whisper(whisper_engine) => {
                            let whisper_language = if validated_language == "auto" {
                                None
                            } else {
                                let normalized = if validated_language == "zh-Hans"
                                    || validated_language == "zh-Hant"
                                {
                                    "zh".to_string()
                                } else {
                                    validated_language.clone()
                                };
                                Some(normalized)
                            };

                            // Optimize Whisper inference params based on audio length.
                            //
                            // Short audio (< 30s at 16kHz = 480000 samples):
                            //   - single_segment: skips whisper.cpp's internal segmentation,
                            //     treating the entire clip as one utterance (faster, simpler).
                            //   - greedy decoding: fastest strategy, negligible accuracy loss
                            //     for short, clear speech.
                            //   - no_context: true (default) — each segment starts fresh,
                            //     which is fine for single-segment audio.
                            //
                            // Long audio (>= 30s):
                            //   - multi-segment: whisper.cpp splits audio into 30-second windows.
                            //   - beam search (beam_size=3): more robust decoding across segment
                            //     boundaries, reduces hallucination and dropped text.
                            //   - no_context: false — preserves decoder state across segments so
                            //     the model carries context from one 30-second window to the next.
                            //     This prevents mid-sentence chunk drops at segment boundaries.
                            let audio_sample_count = audio.len();
                            // 30 seconds at 16kHz
                            let short_audio_threshold = 16000 * 30;
                            let is_short_audio = audio_sample_count < short_audio_threshold;

                            // Reduce CPU threads when GPU acceleration is active.
                            // On GPU the encoder runs on the GPU; extra CPU threads only
                            // help the decoder and can cause sync overhead.
                            let gpu_threads: i32 = if settings.whisper_accelerator
                                != crate::settings::WhisperAcceleratorSetting::Cpu
                            {
                                4 // GPU handles heavy lifting, 4 CPU threads for decoder
                            } else {
                                0 // 0 = whisper.cpp default (min(4, num_cores))
                            };

                            let params = WhisperInferenceParams {
                                language: whisper_language,
                                translate: settings.translate_to_english,
                                initial_prompt: match settings.word_correction_mode {
                                    WordCorrectionMode::Pronunciation
                                        if !settings.advanced_custom_words.is_empty() =>
                                    {
                                        // Advanced mode: use only canonical words
                                        Some(
                                            settings
                                                .advanced_custom_words
                                                .iter()
                                                .map(|cw| cw.word.as_str())
                                                .collect::<Vec<_>>()
                                                .join(", "),
                                        )
                                    }
                                    WordCorrectionMode::WordBias
                                        if !settings.custom_words.is_empty() =>
                                    {
                                        Some(settings.custom_words.join(", "))
                                    }
                                    WordCorrectionMode::Replacement
                                        if !settings.word_replacements.is_empty() =>
                                    {
                                        // Replacement mode: use corrections as prompt
                                        Some(
                                            settings
                                                .word_replacements
                                                .iter()
                                                .map(|r| r.correction.as_str())
                                                .collect::<Vec<_>>()
                                                .join(", "),
                                        )
                                    }
                                    _ => None,
                                },
                                single_segment: is_short_audio,
                                use_greedy: is_short_audio,
                                // For long audio, carry decoder state across 30-second segments
                                // to prevent mid-sentence text drops at segment boundaries.
                                // Short audio doesn't need this (single segment = no boundaries).
                                no_context: is_short_audio,
                                n_threads: gpu_threads,
                                ..Default::default()
                            };

                            whisper_engine
                                .transcribe_with(&audio, &params)
                                .map_err(|e| AppError::transcription(
                                    format!("Whisper transcription failed: {}", e),
                                    anyhow::anyhow!("{}", e),
                                ))
                        }
                        LoadedEngine::Parakeet(parakeet_engine) => {
                            // Use library defaults by not specifying thresholds.
                            // transcribe-rs defaults are (0.30, 0.45) which are
                            // tuned for the Parakeet TDT decoder.
                            // Adaptive thresholds based on audio quality caused
                            // regressions for fast speech in quiet environments.
                            let params = ParakeetParams {
                                language: None,
                                timestamp_granularity: Some(TimestampGranularity::Segment),
                                confidence_threshold: None,  // Use library default (0.30)
                                post_gap_confidence: None,  // Use library default (0.45)
                            };
                            parakeet_engine
                                .transcribe_with(&audio, &params)
                                .map_err(|e| {
                                    AppError::transcription(
                                        format!("Parakeet transcription failed: {}", e),
                                        anyhow::anyhow!("{}", e),
                                    )
                                })
                        }
                        LoadedEngine::Moonshine(moonshine_engine) => moonshine_engine
                            .transcribe(&audio, &TranscribeOptions::default())
                            .map_err(|e| AppError::transcription(
                                format!("Moonshine transcription failed: {}", e),
                                anyhow::anyhow!("{}", e),
                            )),
                        LoadedEngine::MoonshineStreaming(streaming_engine) => streaming_engine
                            .transcribe(&audio, &TranscribeOptions::default())
                            .map_err(|e| {
                                AppError::transcription(
                                    format!("Moonshine streaming transcription failed: {}", e),
                                    anyhow::anyhow!("{}", e),
                                )
                            }),
                        LoadedEngine::SenseVoice(sense_voice_engine) => {
                            let language = match validated_language.as_str() {
                                "zh" | "zh-Hans" | "zh-Hant" => Some("zh".to_string()),
                                "en" => Some("en".to_string()),
                                "ja" => Some("ja".to_string()),
                                "ko" => Some("ko".to_string()),
                                "yue" => Some("yue".to_string()),
                                _ => None,
                            };
                            let params = SenseVoiceParams {
                                language,
                                use_itn: Some(true),
                            };
                            sense_voice_engine
                                .transcribe_with(&audio, &params)
                                .map_err(|e| {
                                    AppError::transcription(
                                        format!("SenseVoice transcription failed: {}", e),
                                        anyhow::anyhow!("{}", e),
                                    )
                                })
                        }
                        LoadedEngine::GigaAM(gigaam_engine) => gigaam_engine
                            .transcribe(&audio, &TranscribeOptions::default())
                            .map_err(|e| AppError::transcription(
                                format!("GigaAM transcription failed: {}", e),
                                anyhow::anyhow!("{}", e),
                            )),
                        LoadedEngine::Canary(canary_engine) => {
                            let lang = if validated_language == "auto" {
                                None
                            } else {
                                Some(validated_language.clone())
                            };
                            let options = TranscribeOptions {
                                language: lang,
                                translate: settings.translate_to_english,
                                ..Default::default()
                            };
                            canary_engine
                                .transcribe(&audio, &options)
                                .map_err(|e| AppError::transcription(
                                    format!("Canary transcription failed: {}", e),
                                    anyhow::anyhow!("{}", e),
                                ))
                        }
                        LoadedEngine::Cohere(cohere_engine) => {
                            let lang = if validated_language == "auto" {
                                None
                            } else if validated_language == "zh-Hans"
                                || validated_language == "zh-Hant"
                            {
                                Some("zh".to_string())
                            } else {
                                Some(validated_language.clone())
                            };
                            let options = TranscribeOptions {
                                language: lang,
                                ..Default::default()
                            };
                            cohere_engine
                                .transcribe(&audio, &options)
                                .map_err(|e| AppError::transcription(
                                    format!("Cohere transcription failed: {}", e),
                                    anyhow::anyhow!("{}", e),
                                ))
                        }
                    }
                },
            ));

            match transcribe_result {
                Ok(inner_result) => {
                    // Success or normal error — put the engine back
                    let mut engine_guard = self.lock_engine();
                    *engine_guard = Some(engine);
                    inner_result?
                }
                Err(panic_payload) => {
                    // Engine panicked — do NOT put it back (it's in an unknown state).
                    // The engine is dropped here, effectively unloading it.
                    let panic_msg = if let Some(s) = panic_payload.downcast_ref::<&str>() {
                        s.to_string()
                    } else if let Some(s) = panic_payload.downcast_ref::<String>() {
                        s.clone()
                    } else {
                        "unknown panic".to_string()
                    };
                    error!(
                        "Transcription engine panicked: {}. Model has been unloaded.",
                        panic_msg
                    );

                    // Clear the model ID so it will be reloaded on next attempt
                    {
                        let mut current_model = self
                            .current_model_id
                            .lock()
                            .unwrap_or_else(|e| e.into_inner());
                        *current_model = None;
                    }

                    let _ = self.app_handle.emit(
                        "model-state-changed",
                        ModelStateEvent {
                            event_type: "unloaded".to_string(),
                            model_id: None,
                            model_name: None,
                            error: Some(format!("Engine panicked: {}", panic_msg)),
                        },
                    );

                    return Err(AppError::TranscriptionPanic(panic_msg));
                }
            }
        };

        // Apply word correction if custom words are configured.
        // For WordBias and Pronunciation modes, skip for Whisper models since custom words
        // are already passed as initial_prompt which biases the model's vocabulary.
        // For Replacement mode, ALWAYS apply word replacements as a post-processing step
        // because exact word substitutions should be guaranteed, not just hinted.
        let is_whisper = self
            .model_manager
            .get_model_info(&effective_model_id)
            .map(|info| matches!(info.engine_type, EngineType::Whisper))
            .unwrap_or(false);

        let has_words = match settings.word_correction_mode {
            WordCorrectionMode::WordBias => !settings.custom_words.is_empty(),
            WordCorrectionMode::Pronunciation => !settings.advanced_custom_words.is_empty(),
            WordCorrectionMode::Replacement => !settings.word_replacements.is_empty(),
        };

        // Save raw text for logging before processing
        let raw_text = result.text.clone();

        info!(
            "Word correction check: mode={:?}, has_words={}, is_whisper={}, num_replacements={}, raw_text='{}'",
            settings.word_correction_mode,
            has_words,
            is_whisper,
            settings.word_replacements.len(),
            raw_text
        );

        // Save suppressed token count before result.text is consumed below
        let suppressed_token_count = result.suppressed_token_count;

        // Process transcription text through the full pipeline in a single call:
        // 1. Word correction (custom words, pronunciation, or word replacements)
        // 2. Filler word removal and hallucination cleanup
        // 3. US → British spelling conversion (if enabled)
        // 4. Repetition suppression
        let final_result = process_transcription_text(
            &result.text,
            settings.word_correction_mode,
            &settings.custom_words,
            &settings.advanced_custom_words,
            &settings.word_replacements,
            settings.word_correction_threshold,
            is_whisper,
            &settings.app_language,
            &settings.custom_filler_words,
            settings.convert_us_to_british,
            settings.spelling_dictionary,
            settings.repetition_suppression_level,
        );

        // Log if processing changed the text
        if final_result != raw_text {
            info!("Text processing applied: '{}' -> '{}'", raw_text, final_result);
        }

        let et = std::time::Instant::now();
        let translation_note = if settings.translate_to_english {
            " (translated)"
        } else {
            ""
        };
        info!(
            "Transcription completed in {}ms{}",
            (et - st).as_millis(),
            translation_note
        );

        // In verification mode, log audio quality metrics for debugging
        // but NEVER append to the transcription text
        if settings.verification_mode {
            let quality = AudioQualityMetrics::compute(&audio);
            info!(
                "Verification mode - Audio: peak={:.0} dBFS, SNR={:.0} dB, dur={:.1}s",
                quality.peak_dbfs, quality.estimated_snr_db, quality.duration_secs,
            );
            if let Some(count) = suppressed_token_count {
                if count > 0 {
                    info!(
                        "Verification mode - Suppressed {} low-confidence tokens",
                        count
                    );
                }
            }
        }

        if final_result.is_empty() {
            info!("Transcription result is empty");
        } else {
            info!("Transcription result: {}", final_result);
        }

        self.maybe_unload_immediately("transcription");

        Ok(TranscriptionOutput {
            text: final_result,
            model_id: effective_model_id,
            suppressed_token_count: suppressed_token_count,
            segments: result.segments,
        })
    }

    /// Transcribe audio for benchmarking purposes.
    /// Similar to `transcribe()` but uses default settings (no custom words, no post-processing)
    /// and always uses greedy+single_segment for consistency.
    pub fn transcribe_for_benchmark(&self, audio: Vec<f32>) -> AppResult<String> {
        if audio.is_empty() {
            return Err(AppError::Transcription {
                message: "Empty audio for benchmark".to_string(),
                source: anyhow::anyhow!("Empty audio for benchmark"),
            });
        }

        // Wait for model to be loaded (if loading)
        {
            let is_loading = self.is_loading.lock().unwrap();
            if *is_loading {
                return Err(AppError::Transcription {
                    message: "Model is still loading".to_string(),
                    source: anyhow::anyhow!("Model is still loading"),
                });
            }
        }

        let result = {
            let mut engine_guard = self.lock_engine();
            let mut engine = match engine_guard.take() {
                Some(e) => e,
                None => {
                    return Err(AppError::ModelNotLoaded);
                }
            };
            drop(engine_guard);

            // Use greedy + single_segment for consistent benchmarking
            let transcribe_result = catch_unwind(AssertUnwindSafe(|| -> Result<transcribe_rs::TranscriptionResult, AppError> {
                match &mut engine {
                    LoadedEngine::Whisper(whisper_engine) => {
                        let params = WhisperInferenceParams {
                            language: None, // auto-detect
                            translate: false,
                            single_segment: true,
                            use_greedy: true,
                            ..Default::default()
                        };
                        whisper_engine
                            .transcribe_with(&audio, &params)
                            .map_err(|e| AppError::transcription(
                                format!("Whisper benchmark failed: {}", e),
                                anyhow::anyhow!("{}", e),
                            ))
                    }
                    LoadedEngine::Parakeet(parakeet_engine) => parakeet_engine
                        .transcribe(&audio, &TranscribeOptions::default())
                        .map_err(|e| AppError::transcription(
                            format!("Parakeet benchmark failed: {}", e),
                            anyhow::anyhow!("{}", e),
                        )),
                    LoadedEngine::Moonshine(moonshine_engine) => moonshine_engine
                        .transcribe(&audio, &TranscribeOptions::default())
                        .map_err(|e| AppError::transcription(
                            format!("Moonshine benchmark failed: {}", e),
                            anyhow::anyhow!("{}", e),
                        )),
                    LoadedEngine::MoonshineStreaming(streaming_engine) => streaming_engine
                        .transcribe(&audio, &TranscribeOptions::default())
                        .map_err(|e| {
                            AppError::transcription(
                                format!("Moonshine streaming benchmark failed: {}", e),
                                anyhow::anyhow!("{}", e),
                            )
                        }),
                    LoadedEngine::SenseVoice(sense_voice_engine) => {
                        let params = SenseVoiceParams {
                            language: None,
                            use_itn: Some(true),
                        };
                        sense_voice_engine
                            .transcribe_with(&audio, &params)
                            .map_err(|e| AppError::transcription(
                                format!("SenseVoice benchmark failed: {}", e),
                                anyhow::anyhow!("{}", e),
                            ))
                    }
                    LoadedEngine::GigaAM(gigaam_engine) => gigaam_engine
                        .transcribe(&audio, &TranscribeOptions::default())
                        .map_err(|e| AppError::transcription(
                            format!("GigaAM benchmark failed: {}", e),
                            anyhow::anyhow!("{}", e),
                        )),
                    LoadedEngine::Canary(canary_engine) => canary_engine
                        .transcribe(&audio, &TranscribeOptions::default())
                        .map_err(|e| AppError::transcription(
                            format!("Canary benchmark failed: {}", e),
                            anyhow::anyhow!("{}", e),
                        )),
                    LoadedEngine::Cohere(cohere_engine) => cohere_engine
                        .transcribe(&audio, &TranscribeOptions::default())
                        .map_err(|e| AppError::transcription(
                            format!("Cohere benchmark failed: {}", e),
                            anyhow::anyhow!("{}", e),
                        )),
                }
            }));

            match transcribe_result {
                Ok(inner_result) => {
                    // Success or normal error — put the engine back
                    let mut engine_guard = self.lock_engine();
                    *engine_guard = Some(engine);
                    inner_result?
                }
                Err(panic_payload) => {
                    // Engine panicked — do NOT put it back
                    let panic_msg = if let Some(s) = panic_payload.downcast_ref::<&str>() {
                        s.to_string()
                    } else if let Some(s) = panic_payload.downcast_ref::<String>() {
                        s.clone()
                    } else {
                        "unknown panic".to_string()
                    };
                    warn!("Benchmark engine panicked: {}", panic_msg);
                    // Clear model ID
                    {
                        let mut current_model = self
                            .current_model_id
                            .lock()
                            .unwrap_or_else(|e| e.into_inner());
                        *current_model = None;
                    }
                    return Err(AppError::TranscriptionPanic(format!(
                        "Benchmark engine panicked: {}",
                        panic_msg
                    )));
                }
            }
        };

        Ok(result.text.trim().to_string())
    }
}

/// Apply the user's accelerator preferences to the transcribe-rs global atomics.
/// Called on startup and whenever the user changes the setting.
pub fn apply_accelerator_settings(app: &tauri::AppHandle) {
    use transcribe_rs::accel;

    let settings = get_settings(app);

    let whisper_pref = match settings.whisper_accelerator {
        WhisperAcceleratorSetting::Auto => accel::WhisperAccelerator::Auto,
        WhisperAcceleratorSetting::Cpu => accel::WhisperAccelerator::CpuOnly,
        WhisperAcceleratorSetting::Gpu => accel::WhisperAccelerator::Gpu,
    };
    accel::set_whisper_accelerator(whisper_pref);
    accel::set_whisper_gpu_device(settings.whisper_gpu_device);
    info!(
        "Whisper accelerator set to: {}, gpu_device: {}",
        whisper_pref,
        if settings.whisper_gpu_device == accel::GPU_DEVICE_AUTO {
            "auto".to_string()
        } else {
            settings.whisper_gpu_device.to_string()
        }
    );

    let ort_pref = match settings.ort_accelerator {
        OrtAcceleratorSetting::Auto => accel::OrtAccelerator::Auto,
        OrtAcceleratorSetting::Cpu => accel::OrtAccelerator::CpuOnly,
        OrtAcceleratorSetting::Cuda => accel::OrtAccelerator::Cuda,
        OrtAcceleratorSetting::DirectMl => accel::OrtAccelerator::DirectMl,
        OrtAcceleratorSetting::Rocm => accel::OrtAccelerator::Rocm,
    };
    accel::set_ort_accelerator(ort_pref);
    info!("ORT accelerator set to: {}", ort_pref);
}

#[derive(Serialize, Clone, Debug, Type)]
pub struct GpuDeviceOption {
    pub id: i32,
    pub name: String,
    pub total_vram_mb: usize,
}

static GPU_DEVICES: OnceLock<Vec<GpuDeviceOption>> = OnceLock::new();

fn cached_gpu_devices() -> &'static [GpuDeviceOption] {
    use transcribe_rs::whisper_cpp::gpu::list_gpu_devices;

    GPU_DEVICES.get_or_init(|| {
        // ggml's Vulkan backend uses FMA3 instructions internally.
        // On older CPUs without FMA3 (e.g. Sandy Bridge Xeons) this causes
        // a SIGILL crash that cannot be caught. Skip enumeration entirely
        // on those CPUs — GPU-accelerated whisper won't work there anyway.
        #[cfg(target_arch = "x86_64")]
        if !std::arch::is_x86_feature_detected!("fma") {
            warn!("CPU lacks FMA3 support — skipping GPU device enumeration");
            return Vec::new();
        }

        list_gpu_devices()
            .into_iter()
            .map(|d| GpuDeviceOption {
                id: d.id,
                name: d.name,
                total_vram_mb: d.total_vram / (1024 * 1024),
            })
            .collect()
    })
}

#[derive(Serialize, Clone, Debug, Type)]
pub struct AvailableAccelerators {
    pub whisper: Vec<String>,
    pub ort: Vec<String>,
    pub gpu_devices: Vec<GpuDeviceOption>,
}

/// Return which accelerators are compiled into this build.
pub fn get_available_accelerators() -> AvailableAccelerators {
    use transcribe_rs::accel::OrtAccelerator;

    let ort_options: Vec<String> = OrtAccelerator::available()
        .into_iter()
        .map(|a| a.to_string())
        .collect();

    let whisper_options = vec!["auto".to_string(), "cpu".to_string(), "gpu".to_string()];

    AvailableAccelerators {
        whisper: whisper_options,
        ort: ort_options,
        gpu_devices: cached_gpu_devices().to_vec(),
    }
}

impl Drop for TranscriptionManager {
    fn drop(&mut self) {
        // Skip shutdown unless this is the very last clone. TranscriptionManager
        // is cloned by initiate_model_load() and the watcher thread — those
        // clones dropping must not kill the watcher. The watcher thread holds
        // its own clone, so engine's strong_count is always >= 2 while the
        // watcher is alive. When it reaches 1, only this instance remains
        // and we can safely shut down.
        if Arc::strong_count(&self.engine) > 1 {
            return;
        }

        // Signal the watcher thread to shutdown
        self.shutdown_signal.store(true, Ordering::Relaxed);

        // Wait for the thread to finish gracefully
        if let Some(handle) = self.watcher_handle.lock().unwrap().take() {
            if let Err(e) = handle.join() {
                warn!("Failed to join idle watcher thread: {:?}", e);
            } else {
                debug!("Idle watcher thread joined successfully");
            }
        }
    }
}
