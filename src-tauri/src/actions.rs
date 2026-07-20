#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
use crate::apple_intelligence;
use crate::audio_feedback::{play_feedback_sound, play_feedback_sound_blocking, SoundType};
use crate::audio_toolkit::{is_microphone_access_denied, is_no_input_device_error, VadPolicy};
use crate::managers::audio::AudioRecordingManager;
use crate::managers::history::HistoryManager;
use crate::managers::model::ModelManager;
use crate::managers::transcription::StreamWorkKind;
use crate::managers::transcription::TranscriptionManager;
use crate::settings::{get_settings, AppSettings, OverlayStyle, APPLE_INTELLIGENCE_PROVIDER_ID};
use crate::session;
use crate::shortcut;
use crate::tray::{change_tray_icon, TrayIconState};
use crate::utils::{
    self, show_processing_overlay, show_recording_overlay, show_transcribing_overlay,
};
use crate::TranscriptionCoordinator;
use ferrous_opencc::{config::BuiltinConfig, OpenCC};
use log::{debug, error, info, warn};
use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::Manager;
use tauri::{AppHandle, Emitter};

const CANCELLATION_POLL_INTERVAL: Duration = Duration::from_millis(25);

#[derive(Clone, serde::Serialize)]
struct RecordingErrorEvent {
    error_type: String,
    detail: Option<String>,
}

/// Drop guard that notifies the [`TranscriptionCoordinator`] when the
/// transcription pipeline finishes — whether it completes normally or panics.
struct FinishGuard(AppHandle);
impl Drop for FinishGuard {
    fn drop(&mut self) {
        if let Some(c) = self.0.try_state::<TranscriptionCoordinator>() {
            c.notify_processing_finished();
        }
    }
}

// Shortcut Action Trait
pub trait ShortcutAction: Send + Sync {
    fn start(&self, app: &AppHandle, binding_id: &str, shortcut_str: &str);
    fn stop(&self, app: &AppHandle, binding_id: &str, shortcut_str: &str);
}

// Transcribe Action
struct TranscribeAction {
    post_process: bool,
}

/// Field name for structured output JSON schema
const TRANSCRIPTION_FIELD: &str = "transcription";

/// Strip invisible Unicode characters that some LLMs may insert
fn strip_invisible_chars(s: &str) -> String {
    s.replace(['\u{200B}', '\u{200C}', '\u{200D}', '\u{FEFF}'], "")
}

/// Build a system prompt from the user's prompt template.
/// Removes `${output}` placeholder since the transcription is sent as the user message.
fn build_system_prompt(prompt_template: &str) -> String {
    prompt_template.replace("${output}", "").trim().to_string()
}

/// Returns `true` when a transcription has no meaningful content to
/// post-process (empty or whitespace-only). Used to skip the post-processing
/// LLM call when nothing was actually transcribed, which would otherwise make
/// the model reply with an error message such as "you need to provide the
/// transcription".
fn is_blank_transcription(transcription: &str) -> bool {
    transcription.trim().is_empty()
}

async fn complete_unless_cancelled<F, C>(operation: F, is_cancelled: C) -> Option<F::Output>
where
    F: Future,
    C: Fn() -> bool,
{
    tokio::pin!(operation);

    loop {
        if is_cancelled() {
            return None;
        }

        if let Ok(result) =
            tokio::time::timeout(CANCELLATION_POLL_INTERVAL, operation.as_mut()).await
        {
            return Some(result);
        }
    }
}

fn should_use_streaming_overlay(style: OverlayStyle, is_streaming: bool) -> bool {
    style == OverlayStyle::Live && is_streaming
}

async fn post_process_transcription(settings: &AppSettings, transcription: &str) -> Option<String> {
    if is_blank_transcription(transcription) {
        debug!("Post-processing skipped because the transcription is empty");
        return None;
    }

    let provider = match settings.active_post_process_provider().cloned() {
        Some(provider) => provider,
        None => {
            debug!("Post-processing enabled but no provider is selected");
            return None;
        }
    };

    let model = settings
        .post_process_models
        .get(&provider.id)
        .cloned()
        .unwrap_or_default();

    if model.trim().is_empty() {
        debug!(
            "Post-processing skipped because provider '{}' has no model configured",
            provider.id
        );
        return None;
    }

    let selected_prompt_id = match &settings.post_process_selected_prompt_id {
        Some(id) => id.clone(),
        None => {
            debug!("Post-processing skipped because no prompt is selected");
            return None;
        }
    };

    let prompt = match settings
        .post_process_prompts
        .iter()
        .find(|prompt| prompt.id == selected_prompt_id)
    {
        Some(prompt) => prompt.prompt.clone(),
        None => {
            debug!(
                "Post-processing skipped because prompt '{}' was not found",
                selected_prompt_id
            );
            return None;
        }
    };

    if prompt.trim().is_empty() {
        debug!("Post-processing skipped because the selected prompt is empty");
        return None;
    }

    debug!(
        "Starting LLM post-processing with provider '{}' (model: {})",
        provider.id, model
    );

    let api_key = settings
        .post_process_api_keys
        .get(&provider.id)
        .cloned()
        .unwrap_or_default();

    // Disable reasoning for providers where post-processing rarely benefits from it.
    // - custom: top-level reasoning_effort (works for local OpenAI-compat servers)
    // - openrouter: nested reasoning object; exclude:true also keeps reasoning text
    //   out of the response so it can't pollute structured-output JSON parsing
    let (reasoning_effort, reasoning) = match provider.id.as_str() {
        "custom" => (Some("none".to_string()), None),
        "openrouter" => (
            None,
            Some(crate::llm_client::ReasoningConfig {
                effort: Some("none".to_string()),
                exclude: Some(true),
            }),
        ),
        _ => (None, None),
    };

    if provider.supports_structured_output {
        debug!("Using structured outputs for provider '{}'", provider.id);

        let system_prompt = build_system_prompt(&prompt);
        let user_content = transcription.to_string();

        // Handle Apple Intelligence separately since it uses native Swift APIs
        if provider.id == APPLE_INTELLIGENCE_PROVIDER_ID {
            #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
            {
                if !apple_intelligence::check_apple_intelligence_availability() {
                    debug!(
                        "Apple Intelligence selected but not currently available on this device"
                    );
                    return None;
                }

                let token_limit = model.trim().parse::<i32>().unwrap_or(0);
                return match apple_intelligence::process_text_with_system_prompt(
                    &system_prompt,
                    &user_content,
                    token_limit,
                ) {
                    Ok(result) => {
                        if result.trim().is_empty() {
                            debug!("Apple Intelligence returned an empty response");
                            None
                        } else {
                            let result = strip_invisible_chars(&result);
                            debug!(
                                "Apple Intelligence post-processing succeeded. Output length: {} chars",
                                result.len()
                            );
                            Some(result)
                        }
                    }
                    Err(err) => {
                        error!("Apple Intelligence post-processing failed: {}", err);
                        None
                    }
                };
            }

            #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
            {
                debug!("Apple Intelligence provider selected on unsupported platform");
                return None;
            }
        }

        // Define JSON schema for transcription output
        let json_schema = serde_json::json!({
            "type": "object",
            "properties": {
                (TRANSCRIPTION_FIELD): {
                    "type": "string",
                    "description": "The cleaned and processed transcription text"
                }
            },
            "required": [TRANSCRIPTION_FIELD],
            "additionalProperties": false
        });

        match crate::llm_client::send_chat_completion_with_schema(
            &provider,
            api_key.clone(),
            &model,
            user_content,
            Some(system_prompt),
            Some(json_schema),
            reasoning_effort.clone(),
            reasoning.clone(),
        )
        .await
        {
            Ok(Some(content)) => {
                // Parse the JSON response to extract the transcription field
                match serde_json::from_str::<serde_json::Value>(&content) {
                    Ok(json) => {
                        if let Some(transcription_value) =
                            json.get(TRANSCRIPTION_FIELD).and_then(|t| t.as_str())
                        {
                            let result = strip_invisible_chars(transcription_value);
                            debug!(
                                "Structured output post-processing succeeded for provider '{}'. Output length: {} chars",
                                provider.id,
                                result.len()
                            );
                            return Some(result);
                        } else {
                            error!("Structured output response missing 'transcription' field");
                            return Some(strip_invisible_chars(&content));
                        }
                    }
                    Err(e) => {
                        error!(
                            "Failed to parse structured output JSON: {}. Returning raw content.",
                            e
                        );
                        return Some(strip_invisible_chars(&content));
                    }
                }
            }
            Ok(None) => {
                error!("LLM API response has no content");
                return None;
            }
            Err(e) => {
                warn!(
                    "Structured output failed for provider '{}': {}. Falling back to legacy mode.",
                    provider.id, e
                );
                // Fall through to legacy mode below
            }
        }
    }

    // Legacy mode: Replace ${output} variable in the prompt with the actual text
    let processed_prompt = prompt.replace("${output}", transcription);
    debug!("Processed prompt length: {} chars", processed_prompt.len());

    match crate::llm_client::send_chat_completion(
        &provider,
        api_key,
        &model,
        processed_prompt,
        reasoning_effort,
        reasoning,
    )
    .await
    {
        Ok(Some(content)) => {
            let content = strip_invisible_chars(&content);
            debug!(
                "LLM post-processing succeeded for provider '{}'. Output length: {} chars",
                provider.id,
                content.len()
            );
            Some(content)
        }
        Ok(None) => {
            error!("LLM API response has no content");
            None
        }
        Err(e) => {
            error!(
                "LLM post-processing failed for provider '{}': {}. Falling back to original transcription.",
                provider.id,
                e
            );
            None
        }
    }
}

async fn maybe_convert_chinese_variant(
    effective_language: &str,
    transcription: &str,
) -> Option<String> {
    // Gate on the language the model actually transcribed in (the effective
    // language), not the persisted intent. A leftover zh-Hans/zh-Hant intent
    // from a previously selected model must not run OpenCC S2T/T2S over output a
    // non-Chinese model produced — that would silently rewrite any shared CJK
    // characters (e.g. Japanese kanji) in the result.
    let is_simplified = effective_language == "zh-Hans";
    let is_traditional = effective_language == "zh-Hant";

    if !is_simplified && !is_traditional {
        debug!("effective language is not Simplified or Traditional Chinese; skipping conversion");
        return None;
    }

    debug!(
        "Starting Chinese variant conversion using OpenCC for language: {}",
        effective_language
    );

    // Use OpenCC to convert based on selected language
    let config = if is_simplified {
        // Convert Traditional Chinese to Simplified Chinese
        BuiltinConfig::Tw2sp
    } else {
        // Convert Simplified Chinese to Traditional Chinese
        BuiltinConfig::S2tw
    };

    match OpenCC::from_config(config) {
        Ok(converter) => {
            let converted = converter.convert(transcription);
            debug!(
                "OpenCC translation completed. Input length: {}, Output length: {}",
                transcription.len(),
                converted.len()
            );
            Some(converted)
        }
        Err(e) => {
            error!("Failed to initialize OpenCC converter: {}. Falling back to original transcription.", e);
            None
        }
    }
}

pub(crate) struct ProcessedTranscription {
    pub final_text: String,
    pub post_processed_text: Option<String>,
    pub post_process_prompt: Option<String>,
}

/// Resolve the persisted language *intent* into the language the currently-loaded
/// model will actually use — the same capability-aware coercion the transcription
/// paths apply (see [`crate::managers::model::effective_language`]). Post-processing
/// resolves it independently so it agrees with the language the transcription ran
/// in, without threading a value through the pipeline.
fn resolve_effective_language(app: &AppHandle, settings: &AppSettings) -> String {
    let tm = app.state::<Arc<TranscriptionManager>>();
    let model_manager = app.state::<Arc<ModelManager>>();
    let active_model = tm
        .get_current_model()
        .unwrap_or_else(|| settings.selected_model.clone());
    match model_manager.get_model_info(&active_model) {
        Some(info) => crate::managers::model::effective_language(
            &settings.selected_language,
            &info.supported_languages,
            info.supports_language_detection,
        ),
        None => settings.selected_language.clone(),
    }
}

pub(crate) async fn process_transcription_output(
    app: &AppHandle,
    transcription: &str,
    post_process: bool,
) -> ProcessedTranscription {
    let settings = get_settings(app);
    let mut final_text = transcription.to_string();
    let mut post_processed_text: Option<String> = None;
    let mut post_process_prompt: Option<String> = None;

    // Resolve the language the transcription actually ran in (the persisted
    // intent coerced against the loaded model's capabilities) so OpenCC keys off
    // the effective language rather than a possibly-stale intent.
    let effective_language = resolve_effective_language(app, &settings);
    if let Some(converted_text) =
        maybe_convert_chinese_variant(&effective_language, transcription).await
    {
        final_text = converted_text;
    }

    if post_process {
        if let Some(processed_text) = post_process_transcription(&settings, &final_text).await {
            post_processed_text = Some(processed_text.clone());
            final_text = processed_text;

            if let Some(prompt_id) = &settings.post_process_selected_prompt_id {
                if let Some(prompt) = settings
                    .post_process_prompts
                    .iter()
                    .find(|prompt| &prompt.id == prompt_id)
                {
                    post_process_prompt = Some(prompt.prompt.clone());
                }
            }
        }
    } else if final_text != transcription {
        post_processed_text = Some(final_text.clone());
    }

    ProcessedTranscription {
        final_text,
        post_processed_text,
        post_process_prompt,
    }
}

impl ShortcutAction for TranscribeAction {
    fn start(&self, app: &AppHandle, binding_id: &str, _shortcut_str: &str) {
        let start_time = Instant::now();
        debug!("TranscribeAction::start called for binding: {}", binding_id);

        // Load model in the background
        let tm = app.state::<Arc<TranscriptionManager>>();
        let rm = app.state::<Arc<AudioRecordingManager>>();

        // Load ASR model and VAD model in parallel
        let kickoff_started = Instant::now();
        tm.initiate_model_load();
        let rm_clone = Arc::clone(&rm);
        std::thread::spawn(move || {
            if let Err(e) = rm_clone.preload_vad() {
                debug!("VAD pre-load failed: {}", e);
            }
        });
        let kickoff_elapsed = kickoff_started.elapsed();

        let binding_id = binding_id.to_string();
        let tray_started = Instant::now();
        change_tray_icon(app, TrayIconState::Recording);
        let tray_elapsed = tray_started.elapsed();

        // Get the microphone mode to determine audio feedback timing
        let plan_started = Instant::now();
        let settings = get_settings(app);
        let is_always_on = settings.always_on_microphone;

        let selected_model_info = app
            .state::<Arc<ModelManager>>()
            .get_model_info(&settings.selected_model);

        // Use the app-facing model capability as the single pre-recording source
        // for live streaming decisions. Unknown support is represented as false
        // until the model registry is updated by discovery or runtime load.
        let model_supports_streaming = selected_model_info
            .as_ref()
            .map(|m| m.supports_streaming)
            .unwrap_or(false);
        let vad_policy = if !settings.vad_enabled {
            VadPolicy::Disabled
        } else if model_supports_streaming {
            VadPolicy::Streaming
        } else {
            VadPolicy::Offline
        };
        if model_supports_streaming {
            tm.start_stream();
        }
        let plan_elapsed = plan_started.elapsed();

        // Sizing the overlay follows the same advertised capability. A model that
        // doesn't stream (or whose capability is not known yet) gets the compact
        // pill instead of an oversized transparent live window.
        let overlay_started = Instant::now();
        match settings.overlay_style {
            OverlayStyle::Live if model_supports_streaming => utils::show_streaming_overlay(app),
            OverlayStyle::Live | OverlayStyle::Minimal => show_recording_overlay(app),
            OverlayStyle::None => {} // show_overlay_state no-ops on None anyway
        }
        // Everything above runs before capture can begin, so each span here is
        // added keypress->capture latency.
        debug!(
            "start-path pre-recording steps: model_kickoff={:?} tray={:?} settings+stream_plan={:?} overlay={:?}",
            kickoff_elapsed,
            tray_elapsed,
            plan_elapsed,
            overlay_started.elapsed()
        );
        debug!("Microphone mode - always_on: {}", is_always_on);

        let mut recording_error: Option<String> = None;
        if is_always_on {
            // Always-on mode: Play audio feedback immediately, then apply mute after sound finishes
            debug!("Always-on mode: Playing audio feedback immediately");
            let rm_clone = Arc::clone(&rm);
            let app_clone = app.clone();
            // The blocking helper exits immediately if audio feedback is disabled,
            // so we can always reuse this thread to ensure mute happens right after playback.
            std::thread::spawn(move || {
                play_feedback_sound_blocking(&app_clone, SoundType::Start);
                rm_clone.apply_mute();
            });

            if let Err(e) = rm.try_start_recording(&binding_id, vad_policy) {
                debug!("Recording failed: {}", e);
                recording_error = Some(e);
            }
        } else {
            // On-demand mode: Start recording first, then play audio feedback, then apply mute
            // This allows the microphone to be activated before playing the sound
            debug!("On-demand mode: Starting recording first, then audio feedback");
            let recording_start_time = Instant::now();
            match rm.try_start_recording(&binding_id, vad_policy) {
                Ok(()) => {
                    debug!("Recording started in {:?}", recording_start_time.elapsed());
                    // Small delay to ensure microphone stream is active
                    let app_clone = app.clone();
                    let rm_clone = Arc::clone(&rm);
                    std::thread::spawn(move || {
                        std::thread::sleep(std::time::Duration::from_millis(100));
                        debug!("Handling delayed audio feedback/mute sequence");
                        // Helper handles disabled audio feedback by returning early, so we reuse it
                        // to keep mute sequencing consistent in every mode.
                        play_feedback_sound_blocking(&app_clone, SoundType::Start);
                        rm_clone.apply_mute();
                    });
                }
                Err(e) => {
                    debug!("Failed to start recording: {}", e);
                    recording_error = Some(e);
                }
            }
        }

        if recording_error.is_none() {
            // Dynamically register the cancel shortcut in a separate task to avoid deadlock
            shortcut::register_cancel_shortcut(app);
        } else {
            // Starting failed (for example due to blocked microphone permissions).
            // Revert UI state so we don't stay stuck in the recording overlay.
            tm.cancel_stream();
            utils::hide_recording_overlay(app);
            change_tray_icon(app, TrayIconState::Idle);
            if let Some(err) = recording_error {
                let error_type = if is_microphone_access_denied(&err) {
                    "microphone_permission_denied"
                } else if is_no_input_device_error(&err) {
                    "no_input_device"
                } else {
                    "unknown"
                };
                let _ = app.emit(
                    "recording-error",
                    RecordingErrorEvent {
                        error_type: error_type.to_string(),
                        detail: Some(err),
                    },
                );
            }
        }

        debug!(
            "TranscribeAction::start completed in {:?}",
            start_time.elapsed()
        );
    }

    fn stop(&self, app: &AppHandle, binding_id: &str, _shortcut_str: &str) {
        // Unregister the cancel shortcut when transcription stops
        shortcut::unregister_cancel_shortcut(app);

        let stop_time = Instant::now();
        debug!("TranscribeAction::stop called for binding: {}", binding_id);

        let ah = app.clone();
        let rm = Arc::clone(&app.state::<Arc<AudioRecordingManager>>());
        let tm = Arc::clone(&app.state::<Arc<TranscriptionManager>>());
        let hm = Arc::clone(&app.state::<Arc<HistoryManager>>());

        change_tray_icon(app, TrayIconState::Transcribing);
        // Stop should give immediate visual feedback. Live streaming can keep
        // the larger panel, but it still switches from listening to a working
        // spinner while the stream finalizes. Non-streaming paths use the
        // compact transcribing pill (None no-ops in show_*).
        let style = get_settings(app).overlay_style;
        // Capture this before finalizing the stream so every later working state
        // targets the same overlay that was shown for this transcription.
        let use_streaming_overlay = should_use_streaming_overlay(style, tm.is_streaming());
        if use_streaming_overlay {
            tm.emit_stream_working(StreamWorkKind::Transcribing);
        } else {
            show_transcribing_overlay(app);
        }

        // Unmute before playing audio feedback so the stop sound is audible
        rm.remove_mute();

        // Play audio feedback for recording stop
        play_feedback_sound(app, SoundType::Stop);

        let binding_id = binding_id.to_string(); // Clone binding_id for the async task
        let post_process = self.post_process;
        let cancel_generation = rm.cancel_generation();

        tauri::async_runtime::spawn(async move {
            let _guard = FinishGuard(ah.clone());
            debug!(
                "Starting async transcription task for binding: {}",
                binding_id
            );

            let stop_recording_time = Instant::now();
            if let Some(samples) = rm.stop_recording(&binding_id, cancel_generation) {
                debug!(
                    "Recording stopped and samples retrieved in {:?}, sample count: {}",
                    stop_recording_time.elapsed(),
                    samples.len()
                );

                if rm.was_cancelled_since(cancel_generation) {
                    debug!("Transcription operation cancelled after recording stop");
                    tm.cancel_stream();
                    utils::hide_recording_overlay(&ah);
                    change_tray_icon(&ah, TrayIconState::Idle);
                    return;
                }

                if samples.is_empty() {
                    debug!("Recording produced no audio samples; skipping persistence");
                    // Tear down any streaming worker so its channel doesn't leak
                    // and block the next start_stream.
                    tm.cancel_stream();
                    utils::hide_recording_overlay(&ah);
                    change_tray_icon(&ah, TrayIconState::Idle);
                } else {
                    // Save WAV concurrently with transcription
                    let sample_count = samples.len();
                    let file_name = format!("handy-{}.wav", chrono::Utc::now().timestamp());
                    let wav_path = hm.recordings_dir().join(&file_name);
                    let wav_path_for_verify = wav_path.clone();
                    let samples_for_wav = samples.clone();
                    let wav_handle = tauri::async_runtime::spawn_blocking(move || {
                        crate::audio_toolkit::save_wav_file(&wav_path, &samples_for_wav)
                    });

                    // Transcribe concurrently with WAV save. If a live stream was
                    // running, finalize it and use its text (all audio was already
                    // fed to the stream); otherwise batch-transcribe the samples.
                    let transcription_time = Instant::now();
                    let transcription_result = match tm.finalize_stream() {
                        // A finalized stream with usable text wins. An empty result
                        // (no active stream, produced nothing, or a finalize error
                        // after the engine was returned) falls back to a full batch
                        // transcription of the same audio. A finalize timeout is
                        // surfaced instead — the worker may still hold the engine,
                        // so a batch fallback would contend with it.
                        Ok(Some(text)) if !text.trim().is_empty() => Ok(text),
                        Ok(_) => tm.transcribe(samples),
                        Err(err) => Err(err),
                    };

                    // Await WAV save and verify
                    let wav_saved = match wav_handle.await {
                        Ok(Ok(())) => {
                            match crate::audio_toolkit::verify_wav_file(
                                &wav_path_for_verify,
                                sample_count,
                            ) {
                                Ok(()) => true,
                                Err(e) => {
                                    error!("WAV verification failed: {}", e);
                                    false
                                }
                            }
                        }
                        Ok(Err(e)) => {
                            error!("Failed to save WAV file: {}", e);
                            false
                        }
                        Err(e) => {
                            error!("WAV save task panicked: {}", e);
                            false
                        }
                    };

                    if rm.was_cancelled_since(cancel_generation) {
                        debug!("Transcription operation cancelled before output handling");
                        utils::hide_recording_overlay(&ah);
                        change_tray_icon(&ah, TrayIconState::Idle);
                        return;
                    }

                    match transcription_result {
                        Ok(transcription) => {
                            debug!(
                                "Transcription completed in {:?}: '{}'",
                                transcription_time.elapsed(),
                                transcription
                            );

                            if post_process {
                                if use_streaming_overlay {
                                    tm.emit_stream_working(StreamWorkKind::Polishing);
                                } else {
                                    show_processing_overlay(&ah);
                                }
                            }
                            let Some(processed) = complete_unless_cancelled(
                                process_transcription_output(&ah, &transcription, post_process),
                                || rm.was_cancelled_since(cancel_generation),
                            )
                            .await
                            else {
                                debug!("Transcription operation cancelled during output handling");
                                utils::hide_recording_overlay(&ah);
                                change_tray_icon(&ah, TrayIconState::Idle);
                                return;
                            };

                            if rm.was_cancelled_since(cancel_generation) {
                                debug!("Transcription operation cancelled before paste");
                                utils::hide_recording_overlay(&ah);
                                change_tray_icon(&ah, TrayIconState::Idle);
                                return;
                            }

                            // Save to history if WAV was saved
                            if wav_saved {
                                if let Err(err) = hm.save_entry(
                                    file_name,
                                    transcription,
                                    post_process,
                                    processed.post_processed_text.clone(),
                                    processed.post_process_prompt.clone(),
                                    None,  // model_id
                                    false, // routed
                                ) {
                                    error!("Failed to save history entry: {}", err);
                                }
                            }

                            if processed.final_text.is_empty() {
                                utils::hide_recording_overlay(&ah);
                                change_tray_icon(&ah, TrayIconState::Idle);
                            } else {
                                let ah_clone = ah.clone();
                                let paste_time = Instant::now();
                                let final_text = processed.final_text;
                                let rm_for_paste = Arc::clone(&rm);
                                ah.run_on_main_thread(move || {
                                    if rm_for_paste.was_cancelled_since(cancel_generation) {
                                        debug!("Transcription operation cancelled before paste");
                                        utils::hide_recording_overlay(&ah_clone);
                                        change_tray_icon(&ah_clone, TrayIconState::Idle);
                                        return;
                                    }

                                    match utils::paste(final_text, ah_clone.clone()) {
                                        Ok(()) => debug!(
                                            "Text pasted successfully in {:?}",
                                            paste_time.elapsed()
                                        ),
                                        Err(e) => {
                                            error!("Failed to paste transcription: {}", e);
                                            let _ = ah_clone.emit("paste-error", ());
                                        }
                                    }
                                    utils::hide_recording_overlay(&ah_clone);
                                    change_tray_icon(&ah_clone, TrayIconState::Idle);
                                })
                                .unwrap_or_else(|e| {
                                    error!("Failed to run paste on main thread: {:?}", e);
                                    utils::hide_recording_overlay(&ah);
                                    change_tray_icon(&ah, TrayIconState::Idle);
                                });
                            }
                        }
                        Err(err) => {
                            if rm.was_cancelled_since(cancel_generation) {
                                debug!(
                                    "Transcription operation cancelled after transcription error"
                                );
                                utils::hide_recording_overlay(&ah);
                                change_tray_icon(&ah, TrayIconState::Idle);
                                return;
                            }

                            error!("Transcription failed: {}", err);
                            // Surface the failure to the UI (toast). The full
                            // message is also in handy.log via the line above.
                            let _ = ah.emit("transcription-error", err.to_string());
                            // Save entry with empty text so user can retry
                            if wav_saved {
                                if let Err(save_err) = hm.save_entry(
                                    file_name,
                                    String::new(),
                                    post_process,
                                    None,
                                    None,
                                    None,  // model_id
                                    false, // routed
                                ) {
                                    error!("Failed to save failed history entry: {}", save_err);
                                }
                            }
                            utils::hide_recording_overlay(&ah);
                            change_tray_icon(&ah, TrayIconState::Idle);
                        }
                    }
                }
            } else {
                debug!("No samples retrieved from recording stop");
                // Tear down any streaming worker so its channel doesn't leak.
                tm.cancel_stream();
                utils::hide_recording_overlay(&ah);
                change_tray_icon(&ah, TrayIconState::Idle);
            }
        });

        debug!(
            "TranscribeAction::stop completed in {:?}",
            stop_time.elapsed()
        );
    }
}

// Cancel Action
struct CancelAction;

impl ShortcutAction for CancelAction {
    fn start(&self, app: &AppHandle, _binding_id: &str, _shortcut_str: &str) {
        utils::cancel_current_operation(app);
    }

    fn stop(&self, _app: &AppHandle, _binding_id: &str, _shortcut_str: &str) {
        // Nothing to do on stop for cancel
    }
}

// Test Action
struct TestAction;

impl ShortcutAction for TestAction {
    fn start(&self, app: &AppHandle, binding_id: &str, shortcut_str: &str) {
        log::info!(
            "Shortcut ID '{}': Started - {} (App: {})", // Changed "Pressed" to "Started" for consistency
            binding_id,
            shortcut_str,
            app.package_info().name
        );
    }

    fn stop(&self, app: &AppHandle, binding_id: &str, shortcut_str: &str) {
        log::info!(
            "Shortcut ID '{}': Stopped - {} (App: {})", // Changed "Released" to "Stopped" for consistency
            binding_id,
            shortcut_str,
            app.package_info().name
        );
    }
}

// ============================================================================
// TranscribeWithRouterAction
// ============================================================================
// Records speech → transcribes → sends text to boss_router.py subprocess.
// The user gets a confirmation flow before routing. This is a fork-only
// feature gated behind the `router_script_path` setting.

/// Structured result from one boss_router.py handler.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct RouterHandlerData {
    /// Emoji status: "✅" for success, "❌" for failure.
    pub status: String,
    /// Human-readable handler name (e.g. "Daily", "Zettelkasten").
    pub handler: String,
    /// Internal classification (e.g. "diary_entry", "zettelkasten_entry").
    pub classification: String,
    /// Optional file path where content was saved.
    pub file_path: Option<String>,
}

/// Emitted when the router subprocess completes (success or failure).
#[derive(Clone, serde::Serialize)]
pub struct RouterResultEvent {
    pub success: bool,
    pub summary: Option<String>,
    pub error: Option<String>,
    pub transcription_text: String,
}

struct TranscribeWithRouterAction;

impl ShortcutAction for TranscribeWithRouterAction {
    fn start(&self, app: &AppHandle, binding_id: &str, _shortcut_str: &str) {
        let start_time = Instant::now();
        debug!(
            "TranscribeWithRouterAction::start called for binding: {}",
            binding_id
        );

        // Structured session tracking
        let settings = get_settings(app);
        let is_always_on = settings.always_on_microphone;
        let mic_name = settings
            .selected_microphone
            .clone()
            .unwrap_or_else(|| "default".to_string());

        if let Some(tracker) = app.try_state::<Arc<session::SessionTracker>>() {
            let sid = tracker.start_session(&mic_name, is_always_on);
            crate::logging::emit(crate::logging::AppEvent::ShortcutTriggered {
                binding_id: binding_id.to_string(),
                action: "transcribe_with_router".to_string(),
            });
            debug!("Session {} started", sid);
        }

        // Load model in the background
        let tm = app.state::<Arc<TranscriptionManager>>();
        let rm = app.state::<Arc<AudioRecordingManager>>();

        let kickoff_started = Instant::now();
        tm.initiate_model_load();
        let rm_clone = Arc::clone(&rm);
        std::thread::spawn(move || {
            if let Err(e) = rm_clone.preload_vad() {
                debug!("VAD pre-load failed: {}", e);
            }
        });
        let kickoff_elapsed = kickoff_started.elapsed();

        let binding_id = binding_id.to_string();
        change_tray_icon(app, TrayIconState::Recording);

        // Show recording overlay with Router mode
        crate::overlay::show_recording_overlay_with_mode(
            app,
            crate::overlay::OverlayMode::Router,
        );

        // Get the microphone mode to determine audio feedback timing
        let plan_started = Instant::now();
        let settings = get_settings(app);
        let model_supports_streaming = app
            .state::<Arc<ModelManager>>()
            .get_model_info(&settings.selected_model)
            .map(|m| m.supports_streaming)
            .unwrap_or(false);
        let vad_policy = if !settings.vad_enabled {
            VadPolicy::Disabled
        } else if model_supports_streaming {
            VadPolicy::Streaming
        } else {
            VadPolicy::Offline
        };
        if model_supports_streaming {
            tm.start_stream();
        }
        let plan_elapsed = plan_started.elapsed();

        debug!(
            "Router start-path pre-recording steps: model_kickoff={:?} settings+stream_plan={:?}",
            kickoff_elapsed, plan_elapsed
        );
        debug!("Microphone mode - always_on: {}", is_always_on);

        let mut recording_error: Option<String> = None;
        if is_always_on {
            debug!("Always-on mode: Playing audio feedback immediately");
            let rm_clone = Arc::clone(&rm);
            let app_clone = app.clone();
            std::thread::spawn(move || {
                play_feedback_sound_blocking(&app_clone, SoundType::Start);
                rm_clone.apply_mute();
            });

            if let Err(e) = rm.try_start_recording(&binding_id, vad_policy) {
                debug!("Recording failed: {}", e);
                recording_error = Some(e);
            }
        } else {
            debug!("On-demand mode: Starting recording first, then audio feedback");
            let recording_start_time = Instant::now();
            match rm.try_start_recording(&binding_id, vad_policy) {
                Ok(()) => {
                    debug!("Recording started in {:?}", recording_start_time.elapsed());
                    let app_clone = app.clone();
                    let rm_clone = Arc::clone(&rm);
                    std::thread::spawn(move || {
                        std::thread::sleep(std::time::Duration::from_millis(100));
                        play_feedback_sound_blocking(&app_clone, SoundType::Start);
                        rm_clone.apply_mute();
                    });
                }
                Err(e) => {
                    debug!("Failed to start recording: {}", e);
                    recording_error = Some(e);
                }
            }
        }

        if recording_error.is_none() {
            shortcut::register_cancel_shortcut(app);
        } else {
            tm.cancel_stream();
            crate::overlay::hide_recording_overlay(app);
            change_tray_icon(app, TrayIconState::Idle);
            if let Some(err) = recording_error {
                let error_type = if is_microphone_access_denied(&err) {
                    "microphone_permission_denied"
                } else if is_no_input_device_error(&err) {
                    "no_input_device"
                } else {
                    "unknown"
                };
                if let Some(tracker) = app.try_state::<Arc<session::SessionTracker>>() {
                    if let Some(sid) = tracker.current_session_id() {
                        tracker.fail_session(&sid, &err);
                    }
                }
                let _ = app.emit(
                    "recording-error",
                    RecordingErrorEvent {
                        error_type: error_type.to_string(),
                        detail: Some(err.clone()),
                    },
                );
                crate::error_events::emit_audio_device_error(
                    app,
                    error_type,
                    &err,
                    error_type == "microphone_permission_denied",
                );
            }
        }

        debug!(
            "TranscribeWithRouterAction::start completed in {:?}",
            start_time.elapsed()
        );
    }

    fn stop(&self, app: &AppHandle, binding_id: &str, _shortcut_str: &str) {
        shortcut::unregister_cancel_shortcut(app);

        let stop_time = Instant::now();
        debug!(
            "TranscribeWithRouterAction::stop called for binding: {}",
            binding_id
        );

        let ah = app.clone();
        let rm = Arc::clone(&app.state::<Arc<AudioRecordingManager>>());
        let tm = Arc::clone(&app.state::<Arc<TranscriptionManager>>());
        let hm = Arc::clone(&app.state::<Arc<HistoryManager>>());

        let sid: Option<String> = app
            .try_state::<Arc<session::SessionTracker>>()
            .and_then(|t| t.current_session_id());

        // Show transcribing overlay in Router mode
        change_tray_icon(app, TrayIconState::Transcribing);
        crate::overlay::show_transcribing_overlay_with_mode(
            app,
            crate::overlay::OverlayMode::Router,
        );

        // Unmute before playing audio feedback
        rm.remove_mute();
        play_feedback_sound(app, SoundType::Stop);

        let binding_id = binding_id.to_string();
        let cancel_generation = rm.cancel_generation();

        tauri::async_runtime::spawn(async move {
            let _guard = FinishGuard(ah.clone());
            debug!(
                "Starting async router task for binding: {}",
                binding_id
            );

            let stop_recording_time = Instant::now();
            if let Some(samples) = rm.stop_recording(&binding_id, cancel_generation) {
                debug!(
                    "Recording stopped, sample count: {}",
                    samples.len()
                );

                if rm.was_cancelled_since(cancel_generation) {
                    debug!("Router transcription cancelled after recording stop");
                    tm.cancel_stream();
                    crate::overlay::hide_recording_overlay(&ah);
                    change_tray_icon(&ah, TrayIconState::Idle);
                    return;
                }

                if samples.is_empty() {
                    debug!("Recording produced no audio samples; skipping");
                    if let (Some(ref s), Some(tracker)) =
                        (&sid, ah.try_state::<Arc<session::SessionTracker>>())
                    {
                        tracker.fail_session(s, "No audio samples from recording stop");
                    }
                    tm.cancel_stream();
                    crate::overlay::hide_recording_overlay(&ah);
                    change_tray_icon(&ah, TrayIconState::Idle);
                    return;
                }

                // Save WAV concurrently with transcription
                let sample_count = samples.len();
                let file_name = format!("handy-{}.wav", chrono::Utc::now().timestamp());
                let wav_path = hm.recordings_dir().join(&file_name);
                let wav_path_for_verify = wav_path.clone();
                let samples_for_wav = samples.clone();
                let wav_handle = tauri::async_runtime::spawn_blocking(move || {
                    crate::audio_toolkit::save_wav_file(&wav_path, &samples_for_wav)
                });

                // Transcribe using metadata variant to get model_id for history
                let transcription_time = Instant::now();
                let transcription_result = match tm.finalize_stream() {
                    Ok(Some(text)) if !text.trim().is_empty() => {
                        // Use the streaming result directly — we don't get
                        // metadata from a finalized stream, but that's fine
                        // for the router path.
                        Ok(crate::managers::transcription::TranscriptionOutput {
                            text,
                            model_id: None,
                            audio_duration_secs: sample_count as f64 / 16_000.0,
                            transcription_duration_secs: 0.0,
                            real_time_factor: 0.0,
                        })
                    }
                    Ok(_) => tm.transcribe_with_metadata(samples),
                    Err(err) => Err(err),
                };

                // Await WAV save
                let wav_saved = match wav_handle.await {
                    Ok(Ok(())) => {
                        match crate::audio_toolkit::verify_wav_file(
                            &wav_path_for_verify,
                            sample_count,
                        ) {
                            Ok(()) => true,
                            Err(e) => {
                                error!("WAV verification failed: {}", e);
                                false
                            }
                        }
                    }
                    Ok(Err(e)) => {
                        error!("Failed to save WAV file: {}", e);
                        false
                    }
                    Err(e) => {
                        error!("WAV save task panicked: {}", e);
                        false
                    }
                };

                if rm.was_cancelled_since(cancel_generation) {
                    debug!("Router transcription cancelled before output handling");
                    crate::overlay::hide_recording_overlay(&ah);
                    change_tray_icon(&ah, TrayIconState::Idle);
                    return;
                }

                match transcription_result {
                    Ok(transcription_output) => {
                        debug!(
                            "Transcription completed in {:?}: '{}'",
                            transcription_time.elapsed(),
                            transcription_output.text
                        );

                        let transcription_text = transcription_output.text.trim().to_string();

                        // Handle empty transcription in router mode
                        if transcription_text.is_empty() {
                            warn!("Router transcription returned empty text - skipping routing");
                            send_macos_notification("Handy Router", "No speech detected");
                            crate::overlay::hide_recording_overlay(&ah);
                            change_tray_icon(&ah, TrayIconState::Idle);

                            if let (Some(ref s), Some(tracker)) =
                                (&sid, ah.try_state::<Arc<session::SessionTracker>>())
                            {
                                tracker.fail_session(s, "Empty transcription - no speech detected");
                            }
                            return;
                        }

                        let model_id_for_history = transcription_output.model_id.clone();

                        // Structured session tracking
                        if let (Some(ref s), Some(tracker)) =
                            (&sid, ah.try_state::<Arc<session::SessionTracker>>())
                        {
                            tracker.advance_to_post_processing(
                                s,
                                transcription_output.text.len(),
                                transcription_time.elapsed().as_millis() as u64,
                            );
                        }

                        // Save to history with routed=true and capture entry ID
                        let history_entry_id: Option<i64> = if wav_saved {
                            match hm.save_entry(
                                file_name,
                                transcription_text.clone(),
                                false,  // post_process_requested
                                None,   // post_processed_text
                                None,   // post_process_prompt
                                model_id_for_history,
                                true,   // routed
                            ) {
                                Ok(entry) => Some(entry.id),
                                Err(err) => {
                                    error!("Failed to save history entry: {}", err);
                                    None
                                }
                            }
                        } else {
                            None
                        };

                        // Emit transcription preview for routing overlay
                        if let Some(overlay_window) =
                            ah.get_webview_window("recording_overlay")
                        {
                            crate::overlay::update_overlay_position_with_mode(
                                &ah,
                                "confirming",
                                &crate::overlay::OverlayMode::Router,
                            );
                            let _ = overlay_window
                                .emit("transcription-preview", &transcription_text);
                        }

                        // Set coordinator state to Confirming
                        if let Some(coordinator) =
                            ah.try_state::<TranscriptionCoordinator>()
                        {
                            coordinator.set_confirming(
                                &ah,
                                transcription_text.clone(),
                                Some("transcribe_with_router".to_string()),
                            );
                        }

                        // Wait for user confirmation (with countdown) before routing
                        let (confirm_tx, confirm_rx) =
                            tokio::sync::oneshot::channel::<String>();

                        let pending_state: crate::commands::PendingRoutingState =
                            ah.state::<crate::commands::PendingRoutingState>().inner().clone();
                        *pending_state.lock() =
                            Some(crate::commands::PendingRouting { confirm_tx });

                        // Wait for confirmation with timeout (30 seconds)
                        let confirmation_timeout =
                            std::time::Duration::from_secs(30);
                        let confirmed_text =
                            match tokio::time::timeout(confirmation_timeout, confirm_rx)
                                .await
                            {
                                Ok(Ok(edited_text)) => {
                                    debug!(
                                        "Router confirmation received, text length: {}",
                                        edited_text.len()
                                    );
                                    edited_text
                                }
                                Ok(Err(_)) => {
                                    debug!(
                                        "Router confirmation channel closed, using original text"
                                    );
                                    transcription_text.clone()
                                }
                                Err(_) => {
                                    debug!("Router confirmation timeout, using original text");
                                    transcription_text.clone()
                                }
                            };

                        // Show "Filing..." overlay while routing
                        crate::overlay::show_processing_overlay_with_mode(
                            &ah,
                            crate::overlay::OverlayMode::Router,
                        );

                        // Transition coordinator from Confirming to Processing
                        if let Some(coordinator) =
                            ah.try_state::<TranscriptionCoordinator>()
                        {
                            coordinator.set_processing_with_binding(
                                &ah,
                                Some("transcribe_with_router".to_string()),
                            );
                        }

                        let transcription_text = confirmed_text;

                        // Send transcription to boss_router
                        let settings = get_settings(&ah);
                        let router_path = settings.router_script_path.clone();
                        let env_file = settings.router_env_file.clone();

                        if let Some(router_script) = router_path {
                            let now = chrono::Local::now();
                            let datetime_str =
                                now.format("%Y-%m-%d %H:%M:%S").to_string();

                            info!(
                                "Sending transcription to router: {} chars, datetime={}",
                                transcription_text.len(),
                                datetime_str
                            );

                            let ah_for_router = ah.clone();
                            let sid_for_router = sid.clone();
                            let transcription_text_for_router =
                                transcription_text.clone();
                            let hm_for_router = if let Some(id) = history_entry_id {
                                Some((hm.clone(), id))
                            } else {
                                None
                            };

                            // Drop FinishGuard before spawning the router subprocess
                            // The router subprocess thread will call
                            // notify_processing_finished() when done.
                            drop(_guard);

                            // Spawn the router as a subprocess
                            std::thread::spawn(move || {
                                let result = run_router_subprocess(
                                    &router_script,
                                    &transcription_text_for_router,
                                    &datetime_str,
                                    env_file.as_deref(),
                                );

                                match result {
                                    Ok((summary_opt, handler_data)) => {
                                        let any_success = handler_data
                                            .iter()
                                            .any(|d| d.status == "✅");
                                        let summary_text = match &summary_opt {
                                            Some(s) => s.clone(),
                                            None => {
                                                if handler_data.is_empty() {
                                                    "No handlers".to_string()
                                                } else {
                                                    format!(
                                                        "{} handlers, none succeeded",
                                                        handler_data.len()
                                                    )
                                                }
                                            }
                                        };

                                        if !any_success && !handler_data.is_empty() {
                                            warn!(
                                                "Router completed but no handlers succeeded: {}",
                                                summary_text
                                            );
                                        }

                                        // Save routing result to history entry
                                        if let Some((ref hm_ref, entry_id)) =
                                            hm_for_router
                                        {
                                            let routing_json =
                                                serde_json::to_string(&handler_data)
                                                    .unwrap_or_else(|_| {
                                                        "[]".to_string()
                                                    });
                                            if let Err(e) = hm_ref
                                                .update_routing_result(
                                                    entry_id,
                                                    Some(routing_json),
                                                )
                                            {
                                                error!(
                                                    "Failed to update routing result: {}",
                                                    e
                                                );
                                            }
                                        }

                                        info!("Router completed: {}", summary_text);

                                        let event = RouterResultEvent {
                                            success: any_success
                                                || handler_data.is_empty(),
                                            summary: Some(summary_text.clone()),
                                            error: None,
                                            transcription_text:
                                                transcription_text_for_router,
                                        };
                                        let _ = ah_for_router
                                            .emit("router-result", &event);

                                        let notification_text =
                                            if summary_text.len() > 100 {
                                                format!(
                                                    "Route: {}...",
                                                    &summary_text[..100]
                                                )
                                            } else {
                                                format!(
                                                    "Route: {}",
                                                    summary_text
                                                )
                                            };
                                        send_macos_notification(
                                            "Handy Router",
                                            &notification_text,
                                        );
                                    }
                                    Err(e) => {
                                        error!("Router subprocess failed: {}", e);

                                        // Save routing failure to history
                                        if let Some((ref hm_ref, entry_id)) =
                                            hm_for_router
                                        {
                                            let failure_result =
                                                vec![RouterHandlerData {
                                                    status: "❌".to_string(),
                                                    handler: "Router Error"
                                                        .to_string(),
                                                    classification: "error"
                                                        .to_string(),
                                                    file_path: None,
                                                }];
                                            let routing_json =
                                                serde_json::to_string(
                                                    &failure_result,
                                                )
                                                .unwrap_or_else(|_| {
                                                    "[]".to_string()
                                                });
                                            if let Err(save_err) = hm_ref
                                                .update_routing_result(
                                                    entry_id,
                                                    Some(routing_json),
                                                )
                                            {
                                                error!(
                                                    "Failed to update routing error in history: {}",
                                                    save_err
                                                );
                                            }
                                        }

                                        let event = RouterResultEvent {
                                            success: false,
                                            summary: None,
                                            error: Some(e.clone()),
                                            transcription_text:
                                                transcription_text_for_router,
                                        };
                                        let _ = ah_for_router
                                            .emit("router-result", &event);

                                        let error_display = if e.len() > 150 {
                                            format!("{}...", &e[..150])
                                        } else {
                                            e.clone()
                                        };
                                        send_macos_notification(
                                            "Handy Router Error",
                                            &error_display,
                                        );
                                    }
                                }

                                // Free the coordinator immediately so the user
                                // can start a new recording right away
                                if let Some(coord) =
                                    ah_for_router.try_state::<TranscriptionCoordinator>(
                                    )
                                {
                                    coord.notify_processing_finished();
                                }

                                // Schedule delayed overlay hide (5 seconds)
                                {
                                    let app_delayed = ah_for_router.clone();
                                    std::thread::spawn(move || {
                                        std::thread::sleep(Duration::from_secs(5));
                                        crate::overlay::hide_recording_overlay(
                                            &app_delayed,
                                        );
                                        change_tray_icon(
                                            &app_delayed,
                                            TrayIconState::Idle,
                                        );
                                    });
                                }

                                // Finish session after routing
                                if let (Some(ref s), Some(tracker)) = (
                                    &sid_for_router,
                                    ah_for_router
                                        .try_state::<Arc<session::SessionTracker>>(),
                                ) {
                                    tracker.finish_session(s, 0);
                                }
                            });
                        } else {
                            warn!(
                                "No router_script_path configured; transcription not routed."
                            );

                            let event = RouterResultEvent {
                                success: false,
                                summary: None,
                                error: Some("No router_script_path configured. Set it in Settings to enable routing.".to_string()),
                                transcription_text: transcription_text.clone(),
                            };
                            let _ = ah.emit("router-result", &event);
                            send_macos_notification(
                                "Handy Router",
                                "No router path configured. Check Settings.",
                            );

                            // Fall back to paste if no router configured
                            if !transcription_text.is_empty() {
                                let ah_for_paste = ah.clone();
                                let _ = ah.run_on_main_thread(move || {
                                    let _ = crate::clipboard::paste(
                                        transcription_text,
                                        ah_for_paste.clone(),
                                    );
                                    crate::overlay::hide_recording_overlay(
                                        &ah_for_paste,
                                    );
                                    change_tray_icon(
                                        &ah_for_paste,
                                        TrayIconState::Idle,
                                    );
                                });
                            } else {
                                crate::overlay::hide_recording_overlay(&ah);
                                change_tray_icon(&ah, TrayIconState::Idle);
                            }
                        }
                    }
                    Err(err) => {
                        debug!("Router transcription error: {}", err);
                        if let (Some(ref s), Some(tracker)) =
                            (&sid, ah.try_state::<Arc<session::SessionTracker>>())
                        {
                            tracker.fail_session(s, &err.to_string());
                        }

                        let settings = get_settings(&ah);

                        let failure_type = if err
                            .to_string()
                            .contains("Model is not loaded")
                            || err
                                .to_string()
                                .contains("failed to load")
                            || err
                                .to_string()
                                .contains("Timed out waiting for model")
                        {
                            crate::managers::transcription_retry::TranscriptionFailure::ModelLoadFailure {
                                model_id: settings.selected_model.clone(),
                                error: err.to_string(),
                            }
                        } else if err.to_string().contains("timed out") {
                            crate::managers::transcription_retry::TranscriptionFailure::Timeout {
                                model_id: settings.selected_model.clone(),
                                duration_secs: 120,
                            }
                        } else {
                            crate::managers::transcription_retry::TranscriptionFailure::Unknown {
                                error: err.to_string(),
                            }
                        };

                        let fallback_models = {
                            let mut models = Vec::new();
                            if settings.hybrid_mode_enabled {
                                if let Some(short_model) =
                                    &settings.hybrid_short_audio_model
                                {
                                    if short_model != &settings.selected_model {
                                        models.push(short_model.clone());
                                    }
                                }
                                if let Some(long_model) =
                                    &settings.hybrid_long_audio_model
                                {
                                    if long_model != &settings.selected_model
                                        && !models.contains(long_model)
                                    {
                                        models.push(long_model.clone());
                                    }
                                }
                            }
                            models
                        };

                        let history_entry_id = if wav_saved {
                            match hm.save_entry(
                                file_name.clone(),
                                String::new(),
                                false,
                                None,
                                None,
                                None,
                                true, // routed
                            ) {
                                Ok(entry) => {
                                    info!(
                                        "Saved history entry {} for failed router transcription",
                                        entry.id
                                    );
                                    Some(entry.id)
                                }
                                Err(save_err) => {
                                    error!(
                                        "Failed to save failed history entry: {}",
                                        save_err
                                    );
                                    None
                                }
                            }
                        } else {
                            None
                        };

                        if wav_saved {
                            if let Some(retry_queue) = ah
                                .try_state::<Arc<
                                    parking_lot::Mutex<
                                        crate::managers::transcription_retry::TranscriptionRetryQueue,
                                    >,
                                >>()
                            {
                                let wav_path =
                                    hm.recordings_dir().join(&file_name);
                                let model_id =
                                    settings.selected_model.clone();

                                if let Err(retry_err) = retry_queue
                                    .lock()
                                    .add_failed_transcription(
                                        wav_path,
                                        model_id,
                                        fallback_models,
                                        failure_type,
                                        false,
                                        None,
                                        history_entry_id,
                                    )
                                {
                                    error!(
                                        "Failed to add transcription to retry queue: {}",
                                        retry_err
                                    );
                                } else {
                                    info!("Added failed router transcription to retry queue");
                                }
                            }
                        }

                        send_macos_notification(
                            "Handy Router",
                            "Transcription failed. Will retry automatically.",
                        );

                        crate::overlay::hide_recording_overlay(&ah);
                        change_tray_icon(&ah, TrayIconState::Idle);

                        let router_model_id =
                            settings.selected_model.clone();
                        crate::error_events::emit_transcription_error(
                            &ah,
                            &err.to_string(),
                            Some(&router_model_id),
                            true,
                        );
                    }
                }
            } else {
                debug!("No samples retrieved from recording stop");
                if let (Some(ref s), Some(tracker)) =
                    (&sid, ah.try_state::<Arc<session::SessionTracker>>())
                {
                    tracker.fail_session(s, "No audio samples from recording stop");
                }

                tm.cancel_stream();
                crate::overlay::hide_recording_overlay(&ah);
                change_tray_icon(&ah, TrayIconState::Idle);
            }
        });

        debug!(
            "TranscribeWithRouterAction::stop completed in {:?}",
            stop_time.elapsed()
        );
    }
}

/// Run boss_router.py as a subprocess and return the parsed output.
/// Returns (summary_string, handler_data_vec) on success, or an error String.
fn run_router_subprocess(
    router_script: &str,
    transcription_text: &str,
    datetime_str: &str,
    env_file: Option<&str>,
) -> Result<(Option<String>, Vec<RouterHandlerData>), String> {
    use std::process::Command;

    let python_bin = "/Users/caffae/miniforge3/bin/python3";
    let mut cmd = Command::new(python_bin);
    cmd.arg(router_script)
        .arg("--text")
        .arg(transcription_text)
        .arg("--datetime")
        .arg(datetime_str)
        .arg("--json")
        .arg("--handy");

    let current_path = std::env::var("PATH").unwrap_or_default();
    cmd.env(
        "PATH",
        format!("/Users/caffae/miniforge3/bin:{}", current_path),
    );

    // Load environment variables from .env file if provided
    if let Some(env_path) = env_file {
        let env_path = std::path::Path::new(env_path);
        if env_path.exists() {
            if let Ok(contents) = std::fs::read_to_string(env_path) {
                for line in contents.lines() {
                    let line = line.trim();
                    if line.is_empty() || line.starts_with('#') {
                        continue;
                    }
                    if let Some((key, value)) = line.split_once('=') {
                        let key = key.trim();
                        let value = value
                            .trim()
                            .strip_prefix('"')
                            .and_then(|v| v.strip_suffix('"'))
                            .unwrap_or(value.trim());
                        cmd.env(key, value);
                    }
                }
            } else {
                warn!("Router env file not readable: {}", env_path.display());
            }
        } else {
            warn!(
                "Router env file does not exist: {}",
                env_path.display()
            );
        }
    }

    // Set working directory to the router script's directory
    if let Some(parent) = std::path::Path::new(router_script).parent() {
        cmd.current_dir(parent);
    }

    debug!("Running router command: {:?}", cmd);

    let output = cmd
        .output()
        .map_err(|e| format!("Failed to execute router: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "Router exited with code {}: {}",
            output.status.code().unwrap_or(-1),
            stderr.trim()
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let (handler_summary, handler_data) = match parse_router_json_output(&stdout) {
        Some(output) => (Some(output.summary), output.handler_data),
        None => (None, Vec::new()),
    };
    Ok((handler_summary, handler_data))
}

/// Parsed output returned by the boss_router subprocess.
struct RouterOutput {
    summary: String,
    handler_data: Vec<RouterHandlerData>,
}

/// Parse the JSON output from boss_router.py --json.
fn parse_router_json_output(stdout: &str) -> Option<RouterOutput> {
    for line in stdout.lines().rev() {
        let line = line.trim();
        if line.starts_with('{') {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(line) {
                let handlers = json.get("handlers").and_then(|h| h.as_array());
                if let Some(handlers) = handlers {
                    let mut handler_data: Vec<RouterHandlerData> = Vec::new();
                    let mut summaries: Vec<String> = Vec::new();

                    for h in handlers {
                        let status = h
                            .get("status")
                            .and_then(|v| v.as_str())
                            .unwrap_or("?");
                        let handler_name = h
                            .get("handler")
                            .and_then(|v| v.as_str())
                            .unwrap_or("?");
                        let classification = h
                            .get("classification")
                            .and_then(|v| v.as_str())
                            .unwrap_or("?");
                        let file_path = h
                            .get("file_path")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string());

                        handler_data.push(RouterHandlerData {
                            status: status.to_string(),
                            handler: handler_name.to_string(),
                            classification: classification.to_string(),
                            file_path: file_path.clone(),
                        });

                        if let Some(ref path) = file_path {
                            let filename = std::path::Path::new(path)
                                .file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or(path);
                            summaries.push(format!(
                                "{} {} ({})",
                                status, handler_name, filename
                            ));
                        } else {
                            summaries.push(format!(
                                "{} {}",
                                status, handler_name
                            ));
                        }
                    }

                    if handler_data.is_empty() {
                        return None;
                    }

                    return Some(RouterOutput {
                        summary: summaries.join(" | "),
                        handler_data,
                    });
                }
            }
        }
    }
    None
}

/// Send a macOS notification via osascript.
fn send_macos_notification(title: &str, message: &str) {
    let escaped_message = message
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', " ");
    let escaped_title = title.replace('\\', "\\\\").replace('"', "\\\"");
    let script = format!(
        "display notification \"{}\" with title \"{}\"",
        escaped_message, escaped_title
    );
    match std::process::Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .spawn()
    {
        Ok(_) => debug!("macOS notification sent: {} - {}", title, message),
        Err(e) => warn!("Failed to send macOS notification: {}", e),
    }
}

// Static Action Map
pub static ACTION_MAP: Lazy<HashMap<String, Arc<dyn ShortcutAction>>> = Lazy::new(|| {
    let mut map = HashMap::new();
    map.insert(
        "transcribe".to_string(),
        Arc::new(TranscribeAction {
            post_process: false,
        }) as Arc<dyn ShortcutAction>,
    );
    map.insert(
        "transcribe_with_post_process".to_string(),
        Arc::new(TranscribeAction { post_process: true }) as Arc<dyn ShortcutAction>,
    );
    map.insert(
        "cancel".to_string(),
        Arc::new(CancelAction) as Arc<dyn ShortcutAction>,
    );
    map.insert(
        "test".to_string(),
        Arc::new(TestAction) as Arc<dyn ShortcutAction>,
    );
    map.insert(
        "transcribe_with_router".to_string(),
        Arc::new(TranscribeWithRouterAction) as Arc<dyn ShortcutAction>,
    );
    map
});

#[cfg(test)]
mod tests {
    use super::{complete_unless_cancelled, is_blank_transcription, should_use_streaming_overlay};
    use crate::settings::OverlayStyle;
    use std::future;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn blank_transcription_is_detected() {
        assert!(is_blank_transcription(""));
        assert!(is_blank_transcription("   "));
        assert!(is_blank_transcription("\t\n  \r\n"));
    }

    #[test]
    fn non_blank_transcription_is_kept() {
        assert!(!is_blank_transcription("hello"));
        assert!(!is_blank_transcription("  hello  "));
    }

    #[test]
    fn completed_operation_returns_its_output() {
        let result = tauri::async_runtime::block_on(complete_unless_cancelled(
            future::ready("done"),
            || false,
        ));

        assert_eq!(result, Some("done"));
    }

    #[test]
    fn pending_operation_stops_after_cancellation() {
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancelled_for_thread = Arc::clone(&cancelled);
        let cancel_thread = thread::spawn(move || {
            thread::sleep(Duration::from_millis(10));
            cancelled_for_thread.store(true, Ordering::Release);
        });

        let result = tauri::async_runtime::block_on(complete_unless_cancelled(
            future::pending::<()>(),
            || cancelled.load(Ordering::Acquire),
        ));

        cancel_thread.join().unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn live_overlay_uses_streaming_states_only_for_streaming_models() {
        assert!(should_use_streaming_overlay(OverlayStyle::Live, true));
        assert!(!should_use_streaming_overlay(OverlayStyle::Live, false));
        assert!(!should_use_streaming_overlay(OverlayStyle::Minimal, true));
        assert!(!should_use_streaming_overlay(OverlayStyle::None, true));
    }
}
