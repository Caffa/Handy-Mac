#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
use crate::apple_intelligence;
use crate::audio_feedback::{play_feedback_sound, play_feedback_sound_blocking, SoundType};
use crate::audio_toolkit::{is_microphone_access_denied, is_no_input_device_error};
use crate::logging::{self, AppEvent, SessionId};
use crate::managers::audio::AudioRecordingManager;
use crate::managers::history::HistoryManager;
use crate::managers::transcription::TranscriptionManager;
use crate::session::SessionTracker;
use crate::settings::{get_settings, AppSettings, APPLE_INTELLIGENCE_PROVIDER_ID};
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
use std::sync::Arc;
use std::time::Instant;
use tauri::Manager;
use tauri::{AppHandle, Emitter};

#[derive(Clone, serde::Serialize)]
struct RecordingErrorEvent {
    error_type: String,
    detail: Option<String>,
}

/// Emitted when the router subprocess completes (success or failure).
#[derive(Clone, serde::Serialize)]
pub struct RouterResultEvent {
    /// Whether the router subprocess succeeded.
    pub success: bool,
    /// Human-readable summary of handlers (e.g. "✔️ Daily – ✔️ Zettelkasten – My Note").
    pub summary: Option<String>,
    /// Error message if the router failed.
    pub error: Option<String>,
    /// The transcription text that was sent.
    pub transcription_text: String,
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

async fn post_process_transcription(settings: &AppSettings, transcription: &str) -> Option<String> {
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
    settings: &AppSettings,
    transcription: &str,
) -> Option<String> {
    // Check if language is set to Simplified or Traditional Chinese
    let is_simplified = settings.selected_language == "zh-Hans";
    let is_traditional = settings.selected_language == "zh-Hant";

    if !is_simplified && !is_traditional {
        debug!("selected_language is not Simplified or Traditional Chinese; skipping translation");
        return None;
    }

    debug!(
        "Starting Chinese translation using OpenCC for language: {}",
        settings.selected_language
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

pub(crate) async fn process_transcription_output(
    app: &AppHandle,
    transcription: &str,
    post_process: bool,
) -> ProcessedTranscription {
    let settings = get_settings(app);
    let mut final_text = transcription.to_string();
    let mut post_processed_text: Option<String> = None;
    let mut post_process_prompt: Option<String> = None;

    if let Some(converted_text) = maybe_convert_chinese_variant(&settings, transcription).await {
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

        // ── Structured session tracking ──
        let settings = get_settings(app);
        let is_always_on = settings.always_on_microphone;
        let mic_name = settings
            .selected_microphone
            .clone()
            .unwrap_or_else(|| "default".to_string());

        if let Some(tracker) = app.try_state::<Arc<SessionTracker>>() {
            let sid = tracker.start_session(&mic_name, is_always_on);
            logging::emit(AppEvent::ShortcutTriggered {
                binding_id: binding_id.to_string(),
                action: if self.post_process {
                    "transcribe_with_post_process".to_string()
                } else {
                    "transcribe".to_string()
                },
            });
            debug!("Session {} started", sid);
        }

        // Load model in the background
        let tm = app.state::<Arc<TranscriptionManager>>();
        let rm = app.state::<Arc<AudioRecordingManager>>();

        // Load ASR model and VAD model in parallel
        tm.initiate_model_load();
        let rm_clone = Arc::clone(&rm);
        std::thread::spawn(move || {
            if let Err(e) = rm_clone.preload_vad() {
                debug!("VAD pre-load failed: {}", e);
            }
        });

        let binding_id = binding_id.to_string();
        change_tray_icon(app, TrayIconState::Recording);
        show_recording_overlay(app);

        // Get the microphone mode to determine audio feedback timing
        let settings = get_settings(app);
        let is_always_on = settings.always_on_microphone;
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

            if let Err(e) = rm.try_start_recording(&binding_id) {
                debug!("Recording failed: {}", e);
                recording_error = Some(e);
            }
        } else {
            // On-demand mode: Start recording first, then play audio feedback, then apply mute
            // This allows the microphone to be activated before playing the sound
            debug!("On-demand mode: Starting recording first, then audio feedback");
            let recording_start_time = Instant::now();
            match rm.try_start_recording(&binding_id) {
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

                // ── Structured event: recording failed ──
                if let Some(tracker) = app.try_state::<Arc<SessionTracker>>() {
                    if let Some(sid) = tracker.current_session_id() {
                        tracker.fail_session(&sid, &err);
                    }
                }

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

        // Capture the current session ID for structured tracking in the async task
        let sid: Option<SessionId> = app
            .try_state::<Arc<SessionTracker>>()
            .and_then(|t| t.current_session_id());

        change_tray_icon(app, TrayIconState::Transcribing);
        show_transcribing_overlay(app);

        // Unmute before playing audio feedback so the stop sound is audible
        rm.remove_mute();

        // Play audio feedback for recording stop
        play_feedback_sound(app, SoundType::Stop);

        let binding_id = binding_id.to_string(); // Clone binding_id for the async task
        let post_process = self.post_process;

        tauri::async_runtime::spawn(async move {
            let _guard = FinishGuard(ah.clone());
            debug!(
                "Starting async transcription task for binding: {}",
                binding_id
            );

            let stop_recording_time = Instant::now();
            if let Some(samples) = rm.stop_recording(&binding_id) {
                debug!(
                    "Recording stopped and samples retrieved in {:?}, sample count: {}",
                    stop_recording_time.elapsed(),
                    samples.len()
                );

                if samples.is_empty() {
                    debug!("Recording produced no audio samples; skipping persistence");
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

                    // Transcribe concurrently with WAV save
                    let transcription_time = Instant::now();
                    let transcription_result = tm.transcribe(samples);

                    // ── Structured session tracking: advance to Transcribing phase ──
                    let model_id = transcription_result
                        .as_ref()
                        .map(|r| r.model_id.clone())
                        .unwrap_or_else(|e| {
                            warn!("Transcription failed: {}", e);
                            "unknown".to_string()
                        });

                    if let (Some(ref s), Some(tracker)) =
                        (&sid, ah.try_state::<Arc<SessionTracker>>())
                    {
                        tracker.advance_to_transcribing(
                            s,
                            &model_id,
                            sample_count,
                            stop_recording_time.elapsed().as_millis() as u64,
                        );
                    }

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

                    match transcription_result {
                        Ok(transcription) => {
                            debug!(
                                "Transcription completed in {:?}: '{}' (model: {})",
                                transcription_time.elapsed(),
                                transcription.text,
                                transcription.model_id,
                            );

                            // ── Structured session tracking: transcription completed ──
                            if let (Some(ref s), Some(tracker)) =
                                (&sid, ah.try_state::<Arc<SessionTracker>>())
                            {
                                tracker.advance_to_post_processing(
                                    s,
                                    transcription.text.len(),
                                    transcription_time.elapsed().as_millis() as u64,
                                );
                            }

                            if post_process {
                                show_processing_overlay(&ah);
                            }
                            let processed =
                                process_transcription_output(&ah, &transcription.text, post_process)
                                    .await;

                            // Save to history if WAV was saved
                            if wav_saved {
                                if let Err(err) = hm.save_entry(
                                    file_name,
                                    transcription.text.clone(),
                                    post_process,
                                    processed.post_processed_text.clone(),
                                    processed.post_process_prompt.clone(),
                                    Some(transcription.model_id.clone()),
                                    false, // routed: not sent to router
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
                                info!("Submitting paste to main thread, text length={}", final_text.len());
                                let result = ah.run_on_main_thread(move || {
                                    info!("Paste function starting on main thread...");
                                    match utils::paste(final_text, ah_clone.clone()) {
                                        Ok(()) => {
                                            info!(
                                                "Text pasted successfully in {:?}",
                                                paste_time.elapsed()
                                            );
                                            // ── Structured event: paste succeeded, finish session ──
                                            if let (Some(ref s), Some(tracker)) =
                                                (&sid, ah_clone.try_state::<Arc<SessionTracker>>())
                                            {
                                                tracker.finish_session(
                                                    s,
                                                    paste_time.elapsed().as_millis() as u64,
                                                );
                                            }
                                        }
                                        Err(e) => {
                                            error!("Failed to paste transcription: {}", e);

                                            // ── Structured event: paste failed ──
                                            if let (Some(ref s), Some(tracker)) =
                                                (&sid, ah_clone.try_state::<Arc<SessionTracker>>())
                                            {
                                                tracker.fail_session(s, &format!("Paste failed: {}", e));
                                            }

                                            let _ = ah_clone.emit("paste-error", ());
                                        }
                                    }
                                    utils::hide_recording_overlay(&ah_clone);
                                    change_tray_icon(&ah_clone, TrayIconState::Idle);
                                });
                                
                                match result {
                                    Ok(()) => info!("Main thread paste task submitted successfully"),
                                    Err(e) => {
                                        error!("Failed to run paste on main thread: {:?}", e);
                                        utils::hide_recording_overlay(&ah);
                                        change_tray_icon(&ah, TrayIconState::Idle);
                                    }
                                }
                            }
                        }
                        Err(err) => {
                            debug!("Global Shortcut Transcription error: {}", err);
                            // ── Structured event: transcription failed ──
                            if let (Some(ref s), Some(tracker)) =
                                (&sid, ah.try_state::<Arc<SessionTracker>>())
                            {
                                tracker.fail_session(s, &err.to_string());
                            }
                            // Save entry with empty text so user can retry
                            if wav_saved {
                                if let Err(save_err) = hm.save_entry(
                                    file_name,
                                    String::new(),
                                    post_process,
                                    None,
                                    None,
                                    None,
                                    false, // routed: not sent to router
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
                // ── Structured event: no samples ──
                if let (Some(ref s), Some(tracker)) =
                    (&sid, ah.try_state::<Arc<SessionTracker>>())
                {
                    tracker.fail_session(s, "No audio samples from recording stop");
                }
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

// Transcribe With Router Action
//
// Records speech → transcribes → sends text to boss_router.py subprocess.
// The recording overlay is shown during recording (same as normal transcribe),
// but after recording stops the overlay is hidden immediately and the rest
// (transcription + routing) happens in the background. The user gets
// feedback later via the router's Telegram notification.
struct TranscribeWithRouterAction;

impl ShortcutAction for TranscribeWithRouterAction {
    fn start(&self, app: &AppHandle, binding_id: &str, _shortcut_str: &str) {
        let start_time = Instant::now();
        debug!("TranscribeWithRouterAction::start called for binding: {}", binding_id);

        // ── Structured session tracking ──
        let settings = get_settings(app);
        let is_always_on = settings.always_on_microphone;
        let mic_name = settings
            .selected_microphone
            .clone()
            .unwrap_or_else(|| "default".to_string());

        if let Some(tracker) = app.try_state::<Arc<SessionTracker>>() {
            let sid = tracker.start_session(&mic_name, is_always_on);
            logging::emit(AppEvent::ShortcutTriggered {
                binding_id: binding_id.to_string(),
                action: "transcribe_with_router".to_string(),
            });
            debug!("Session {} started", sid);
        }

        // Load model in the background
        let tm = app.state::<Arc<TranscriptionManager>>();
        let rm = app.state::<Arc<AudioRecordingManager>>();
        tm.initiate_model_load();
        let rm_clone = Arc::clone(&rm);
        std::thread::spawn(move || {
            if let Err(e) = rm_clone.preload_vad() {
                debug!("VAD pre-load failed: {}", e);
            }
        });

        let binding_id = binding_id.to_string();
        change_tray_icon(app, TrayIconState::Recording);
        show_recording_overlay(app);

        // Recording start logic — identical to TranscribeAction
        let settings = get_settings(app);
        let is_always_on = settings.always_on_microphone;
        let mut recording_error: Option<String> = None;

        if is_always_on {
            debug!("Always-on mode: Playing audio feedback immediately");
            let rm_clone = Arc::clone(&rm);
            let app_clone = app.clone();
            std::thread::spawn(move || {
                play_feedback_sound_blocking(&app_clone, SoundType::Start);
                rm_clone.apply_mute();
            });

            if let Err(e) = rm.try_start_recording(&binding_id) {
                debug!("Recording failed: {}", e);
                recording_error = Some(e);
            }
        } else {
            debug!("On-demand mode: Starting recording first, then audio feedback");
            match rm.try_start_recording(&binding_id) {
                Ok(()) => {
                    debug!("Recording started in {:?}", start_time.elapsed());
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
                if let Some(tracker) = app.try_state::<Arc<SessionTracker>>() {
                    if let Some(sid) = tracker.current_session_id() {
                        tracker.fail_session(&sid, &err);
                    }
                }
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
            "TranscribeWithRouterAction::start completed in {:?}",
            start_time.elapsed()
        );
    }

    fn stop(&self, app: &AppHandle, binding_id: &str, _shortcut_str: &str) {
        // Unregister cancel shortcut
        shortcut::unregister_cancel_shortcut(app);

        let stop_time = Instant::now();
        debug!("TranscribeWithRouterAction::stop called for binding: {}", binding_id);

        let ah = app.clone();
        let rm = Arc::clone(&app.state::<Arc<AudioRecordingManager>>());
        let tm = Arc::clone(&app.state::<Arc<TranscriptionManager>>());
        let hm = Arc::clone(&app.state::<Arc<HistoryManager>>());

        let sid: Option<SessionId> = app
            .try_state::<Arc<SessionTracker>>()
            .and_then(|t| t.current_session_id());

        // ── KEY DIFFERENCE from TranscribeAction ──
        // Hide the recording overlay IMMEDIATELY and return to idle.
        // Transcription + routing happens entirely in the background.
        change_tray_icon(app, TrayIconState::Idle);
        utils::hide_recording_overlay(app);

        // Unmute before playing audio feedback
        rm.remove_mute();
        play_feedback_sound(app, SoundType::Stop);

        let binding_id = binding_id.to_string(); // Clone for async task

        tauri::async_runtime::spawn(async move {
            let _guard = FinishGuard(ah.clone());
            debug!("Starting async router task for binding: {}", binding_id);

            let stop_recording_time = Instant::now();
            if let Some(samples) = rm.stop_recording(&binding_id) {
                debug!(
                    "Recording stopped, sample count: {}",
                    samples.len()
                );

                if samples.is_empty() {
                    debug!("Recording produced no audio samples; skipping");
                    if let (Some(ref s), Some(tracker)) =
                        (&sid, ah.try_state::<Arc<SessionTracker>>())
                    {
                        tracker.fail_session(s, "No audio samples from recording stop");
                    }
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

                // Transcribe
                let transcription_time = Instant::now();
                let transcription_result = tm.transcribe(samples);

                // ── Structured session tracking ──
                let model_id = transcription_result
                    .as_ref()
                    .map(|r| r.model_id.clone())
                    .unwrap_or_else(|e| {
                        warn!("Transcription failed: {}", e);
                        "unknown".to_string()
                    });

                if let (Some(ref s), Some(tracker)) =
                    (&sid, ah.try_state::<Arc<SessionTracker>>())
                {
                    tracker.advance_to_transcribing(
                        s,
                        &model_id,
                        sample_count,
                        stop_recording_time.elapsed().as_millis() as u64,
                    );
                }

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

                match transcription_result {
                    Ok(transcription) => {
                        debug!(
                            "Transcription completed in {:?}: '{}' (model: {})",
                            transcription_time.elapsed(),
                            transcription.text,
                            transcription.model_id,
                        );

                        // ── Structured session tracking ──
                        if let (Some(ref s), Some(tracker)) =
                            (&sid, ah.try_state::<Arc<SessionTracker>>())
                        {
                            tracker.advance_to_post_processing(
                                s,
                                transcription.text.len(),
                                transcription_time.elapsed().as_millis() as u64,
                            );
                        }

                        // Save to history with routed=true
                        let transcription_text = transcription.text.clone();
                        let model_id_for_history = transcription.model_id.clone();
                        if wav_saved {
                            if let Err(err) = hm.save_entry(
                                file_name,
                                transcription_text.clone(),
                                false, // post_process_requested
                                None,   // post_processed_text
                                None,   // post_process_prompt
                                Some(model_id_for_history),
                                true,   // routed
                            ) {
                                error!("Failed to save history entry: {}", err);
                            }
                        }

                        // ── Send transcription to boss_router ──
                        let settings = get_settings(&ah);
                        let router_path = settings.router_script_path.clone();
                        let env_file = settings.router_env_file.clone();

                        if let Some(router_script) = router_path {
                            let now = chrono::Local::now();
                            let datetime_str = now.format("%Y-%m-%d %H:%M:%S").to_string();

                            info!(
                                "Sending transcription to router: {} chars, datetime={}",
                                transcription_text.len(),
                                datetime_str
                            );

                            // Clone values needed by the router thread
                            let ah_for_router = ah.clone();
                            let sid_for_router = sid.clone();
                            let transcription_text_for_router = transcription_text.clone();

                            // Spawn the router as a subprocess
                            // This is fire-and-forget from UX but we emit a notification on completion.
                            std::thread::spawn(move || {
                                let result = run_router_subprocess(
                                    &router_script,
                                    &transcription_text_for_router,
                                    &datetime_str,
                                    env_file.as_deref(),
                                );

                                match result {
                                    Ok(handler_summary) => {
                                        let summary_text = handler_summary
                                            .unwrap_or_else(|| "No handlers".to_string());
                                        info!("Router completed: {}", summary_text);

                                        let event = RouterResultEvent {
                                            success: true,
                                            summary: Some(summary_text.clone()),
                                            error: None,
                                            transcription_text: transcription_text_for_router,
                                        };
                                        let _ = ah_for_router.emit("router-result", &event);

                                        // Send macOS notification for router success
                                        let notification_text = if summary_text.len() > 100 {
                                            format!("Route: {}...", &summary_text[..100])
                                        } else {
                                            format!("Route: {}", summary_text)
                                        };
                                        send_macos_notification("Handy Router", &notification_text);
                                    }
                                    Err(e) => {
                                        error!("Router subprocess failed: {}", e);

                                        let event = RouterResultEvent {
                                            success: false,
                                            summary: None,
                                            error: Some(e.clone()),
                                            transcription_text: transcription_text_for_router,
                                        };
                                        let _ = ah_for_router.emit("router-result", &event);

                                        // Send macOS notification for router failure
                                        let error_display = if e.len() > 150 {
                                                format!("{}...", &e[..150])
                                            } else {
                                                e.clone()
                                            };
                                        send_macos_notification("Handy Router Error", &error_display);
                                    }
                                }

                                // ── Finish session after routing ──
                                if let (Some(ref s), Some(tracker)) =
                                    (&sid_for_router, ah_for_router.try_state::<Arc<SessionTracker>>())
                                {
                                    tracker.finish_session(s, 0);
                                }
                            });
                        } else {
                            warn!("No router_script_path configured; transcription not routed.");

                            // Emit event so frontend can show feedback
                            let event = RouterResultEvent {
                                success: false,
                                summary: None,
                                error: Some("No router_script_path configured. Set it in Settings to enable routing.".to_string()),
                                transcription_text: transcription_text.clone(),
                            };
                            let _ = ah.emit("router-result", &event);
                            send_macos_notification("Handy Router", "No router path configured. Check Settings.");

                            // Fall back to paste if no router configured
                            if !transcription_text.is_empty() {
                                let ah_for_paste = ah.clone();
                                let _ = ah.run_on_main_thread(move || {
                                    let _ = utils::paste(transcription_text, ah_for_paste.clone());
                                    utils::hide_recording_overlay(&ah_for_paste);
                                });
                            }
                        }
                    }
                    Err(err) => {
                        debug!("Router transcription error: {}", err);
                        if let (Some(ref s), Some(tracker)) =
                            (&sid, ah.try_state::<Arc<SessionTracker>>())
                        {
                            tracker.fail_session(s, &err.to_string());
                        }
                        // Save entry with empty text so user can retry
                        if wav_saved {
                            if let Err(save_err) = hm.save_entry(
                                file_name,
                                String::new(),
                                false,
                                None,
                                None,
                                None,
                                true, // routed
                            ) {
                                error!("Failed to save failed history entry: {}", save_err);
                            }
                        }
                    }
                }
            } else {
                debug!("No samples retrieved from recording stop");
                if let (Some(ref s), Some(tracker)) =
                    (&sid, ah.try_state::<Arc<SessionTracker>>())
                {
                    tracker.fail_session(s, "No audio samples from recording stop");
                }
            }
        });

        debug!(
            "TranscribeWithRouterAction::stop completed in {:?}",
            stop_time.elapsed()
        );
    }
}

/// Run boss_router.py as a subprocess and return a summary of what happened.
fn run_router_subprocess(
    router_script: &str,
    transcription_text: &str,
    datetime_str: &str,
    env_file: Option<&str>,
) -> Result<Option<String>, String> {
    use std::process::Command;

    // Build the command
    let mut cmd = Command::new("python3");
    cmd.arg(router_script)
        .arg("--text")
        .arg(transcription_text)
        .arg("--datetime")
        .arg(datetime_str)
        .arg("--json");

    // Load environment variables from .env file if provided
    if let Some(env_path) = env_file {
        let env_path = std::path::Path::new(env_path);
        if env_path.exists() {
            if let Ok(contents) = std::fs::read_to_string(env_path) {
                for line in contents.lines() {
                    let line = line.trim();
                    // Skip comments and empty lines
                    if line.is_empty() || line.starts_with('#') {
                        continue;
                    }
                    if let Some((key, value)) = line.split_once('=') {
                        let key = key.trim();
                        let value = value.trim().strip_prefix('"').and_then(|v| v.strip_suffix('"')).unwrap_or(value.trim());
                        cmd.env(key, value);
                    }
                }
            } else {
                warn!("Router env file not readable: {}", env_path.display());
            }
        } else {
            warn!("Router env file does not exist: {}", env_path.display());
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

    // Timeout: if the router takes more than 5 minutes, something is wrong.
    // Using .output() is blocking, so we rely on the OS process timeout.
    // For a true timeout, we'd need .spawn() + wait_with_timeout, but
    // boss_router.py should complete within a few minutes for most inputs.
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "Router exited with code {}: {}",
            output.status.code().unwrap_or(-1),
            stderr.trim()
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let handler_summary = parse_router_json_output(&stdout);
    Ok(handler_summary)
}

/// Parse the JSON output from boss_router.py --json.
/// Returns a human-readable summary of the handlers that ran.
fn parse_router_json_output(stdout: &str) -> Option<String> {
    // The JSON is on the last line of output (there may be log lines before it)
    for line in stdout.lines().rev() {
        let line = line.trim();
        if line.starts_with('{') {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(line) {
                // Extract handler summaries
                let handlers = json.get("handlers").and_then(|h| h.as_array());
                if let Some(handlers) = handlers {
                    let summaries: Vec<String> = handlers
                        .iter()
                        .filter_map(|h| {
                            let handler = h.get("handler").and_then(|v| v.as_str()).unwrap_or("?");
                            let status = h.get("status").and_then(|v| v.as_str()).unwrap_or("?");
                            let file_path = h.get("file_path").and_then(|v| v.as_str());
                            if let Some(path) = file_path {
                                let filename = std::path::Path::new(path)
                                    .file_name()
                                    .and_then(|n| n.to_str())
                                    .unwrap_or(path);
                                Some(format!("{} {} ({})", status, handler, filename))
                            } else {
                                Some(format!("{} {}", status, handler))
                            }
                        })
                        .collect();
                    if summaries.is_empty() {
                        return None;
                    }
                    return Some(summaries.join(" | "));
                }
            }
        }
    }
    None
}

/// Send a macOS notification via osascript.
/// Used for router success/failure feedback since the overlay is already hidden.
fn send_macos_notification(title: &str, message: &str) {
    // Escape special characters for AppleScript
    let escaped_message = message.replace('\\', "\\\\").replace('"', "\\\\\"").replace('\n', " ");
    let escaped_title = title.replace('\\', "\\\\").replace('"', "\\\\\"");
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
        "transcribe_with_router".to_string(),
        Arc::new(TranscribeWithRouterAction) as Arc<dyn ShortcutAction>,
    );
    map.insert(
        "cancel".to_string(),
        Arc::new(CancelAction) as Arc<dyn ShortcutAction>,
    );
    map.insert(
        "test".to_string(),
        Arc::new(TestAction) as Arc<dyn ShortcutAction>,
    );
    map
});
