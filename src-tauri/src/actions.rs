#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
use crate::apple_intelligence;
use crate::audio_feedback::{play_feedback_sound, play_feedback_sound_blocking, SoundType};
use crate::audio_toolkit::{is_microphone_access_denied, is_no_input_device_error};
use crate::logging::{self, AppEvent, SessionId};
use crate::managers::audio::AudioRecordingManager;
use crate::managers::history::HistoryManager;
use crate::managers::transcription::TranscriptionManager;
use crate::managers::transcription_retry::{TranscriptionFailure, TranscriptionRetryQueue};
use crate::overlay::OverlayMode;
use crate::session::SessionTracker;
use crate::settings::{get_settings, AppSettings, APPLE_INTELLIGENCE_PROVIDER_ID};
use crate::shortcut;
use crate::tray::{change_tray_icon, TrayIconState};
use crate::utils::{
    self, show_processing_overlay, show_processing_overlay_with_mode, show_recording_overlay,
    show_recording_overlay_with_mode, show_transcribing_overlay,
    show_transcribing_overlay_with_mode,
};
use crate::TranscriptionCoordinator;
use ferrous_opencc::{config::BuiltinConfig, OpenCC};
use log::{debug, error, info, warn};
use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use parking_lot::Mutex;
use std::sync::Arc;
use std::time::Instant;
use tauri::Manager;
use tauri::{AppHandle, Emitter};

#[derive(Clone, serde::Serialize)]
struct RecordingErrorEvent {
    error_type: String,
    detail: Option<String>,
}

/// Structured result from one boss_router.py handler.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct RouterHandlerData {
    /// Emoji status: "✅" for success, "❌" for failure.
    pub status: String,
    /// Human-readable handler name (e.g. "Daily", "Zettelkasten", "Project Devlog").
    pub handler: String,
    /// Internal classification (e.g. "diary_entry", "zettelkasten_entry").
    pub classification: String,
    /// Optional file path where content was saved (None for diary entries).
    pub file_path: Option<String>,
}

/// Emitted when the router subprocess completes (success or failure).
/// Detailed handler results are persisted separately via update_routing_result() in the history entry.
#[derive(Clone, serde::Serialize)]
pub struct RouterResultEvent {
    /// Whether the router subprocess succeeded (at least one handler completed).
    pub success: bool,
    /// Human-readable summary of handlers (e.g. "✅ Daily | ✅ Zettelkasten (my-note)").
    pub summary: Option<String>,
    /// Error message if the router failed.
    pub error: Option<String>,
    /// The transcription text that was sent.
    pub transcription_text: String,
}

/// Drop guard that notifies the [`TranscriptionCoordinator`] when the
/// transcription pipeline finishes — whether it completes normally or panics.
/// 
/// FIXED: Only notifies if we're still in the Processing stage. This prevents
/// race conditions where:
/// 1. User starts recording → stage = Recording
/// 2. User stops → stage = Processing, async task starts
/// 3. User cancels → stage = Idle
/// 4. User starts new recording → stage = Recording
/// 5. Async task from step 2 finishes → FinishGuard fires
/// 6. stage transitions to Idle (wrong!)
/// 
/// With this fix, FinishGuard only fires if stage is still Processing,
/// preventing the race condition.
struct FinishGuard(AppHandle);
impl Drop for FinishGuard {
    fn drop(&mut self) {
        // The coordinator will only transition from Processing to Idle,
        // so if we're already Idle (due to cancel), this is a no-op.
        // This prevents race conditions between cancel and finish.
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
        let Some(tm) = app.try_state::<Arc<Mutex<TranscriptionManager>>>() else {
            warn!("TranscriptionManager not available, skipping recording start");
            return;
        };
        let Some(rm) = app.try_state::<Arc<AudioRecordingManager>>() else {
            warn!("AudioRecordingManager not available, skipping recording start");
            return;
        };
        
        // Clear any previous streaming cancellation flag when starting a new recording.
        // Use the Arc<AtomicBool> directly to avoid blocking on the TM lock.
        // The streaming callback may be holding the TM lock during transcription (seconds),
        // and we want immediate feedback when starting a new recording.
        if let Some(cancel_flag) = app.try_state::<Arc<AtomicBool>>() {
            cancel_flag.store(false, Ordering::Release);
            debug!("Cleared streaming cancel flag for new recording");
        } else {
            // Fallback to TM lock if Arc<AtomicBool> not available (shouldn't happen)
            tm.lock().clear_streaming_cancel();
        }

        // Load ASR model and VAD model in parallel
        tm.lock().initiate_model_load();
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
                        detail: Some(err.clone()),
                    },
                );

                // Emit a recoverable error for the error dialog system
                crate::error_events::emit_audio_device_error(
                    app,
                    error_type,
                    &err,
                    error_type == "microphone_permission_denied",
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

        // Cancel any in-progress streaming transcription to prevent wasted work.
        // IMPORTANT: Use the streaming_cancel_flag managed separately in app state
        // instead of tm.lock().cancel_streaming() to avoid blocking.
        // The streaming callback holds the TM lock during transcription (seconds),
        // and this stop handler would block waiting for that lock, freezing the UI.
        // By using the Arc<AtomicBool> directly, we can cancel without waiting.
        if let Some(cancel_flag) = app.try_state::<Arc<AtomicBool>>() {
            cancel_flag.store(true, Ordering::Release);
            debug!("Cancelled streaming transcription via Arc<AtomicBool>");
        } else {
            warn!("Streaming cancel flag not available in app state");
        }

        let stop_time = Instant::now();
        debug!("TranscribeAction::stop called for binding: {}", binding_id);

        let ah = app.clone();
        let Some(rm) = app.try_state::<Arc<AudioRecordingManager>>() else {
            warn!("AudioRecordingManager not available, cannot stop recording");
            return;
        };
        let rm = Arc::clone(&rm);
        let Some(tm) = app.try_state::<Arc<Mutex<TranscriptionManager>>>() else {
            warn!("TranscriptionManager not available, cannot stop transcription");
            return;
        };
        let tm = Arc::clone(&tm);
        let Some(hm) = app.try_state::<Arc<HistoryManager>>() else {
            warn!("HistoryManager not available, cannot save recording");
            return;
        };
        let hm = Arc::clone(&hm);

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
            let samples = rm.stop_recording(&binding_id);
            if let Some(samples) = samples {
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
                    let transcription_result = tm.lock().transcribe(samples);

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
                            let processed = process_transcription_output(
                                &ah,
                                &transcription.text,
                                post_process,
                            )
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
                                // Transcription returned empty text - may indicate dead mic
                                warn!("Transcription returned empty text - checking if USB watchdog should cycle");
                                // Calculate duration from sample count (16000 Hz sample rate)
                                let duration_secs = sample_count as f32 / 16000.0;
                                if rm.usb_watchdog.on_silent_transcription(duration_secs) {
                                    // USB cycle was triggered - restart mic stream if needed
                                    if let Err(e) = rm.restart_microphone_if_needed() {
                                        error!("Failed to restart microphone after silent transcription USB cycle: {}", e);
                                    }
                                }
                                utils::hide_recording_overlay(&ah);
                                change_tray_icon(&ah, TrayIconState::Idle);
                            } else {
                                let ah_clone = ah.clone();
                                let paste_time = Instant::now();
                                let final_text = processed.final_text;
                                info!(
                                    "Submitting paste to main thread, text length={}",
                                    final_text.len()
                                );
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
                                                tracker.fail_session(
                                                    s,
                                                    &format!("Paste failed: {}", e),
                                                );
                                            }

                                            let _ = ah_clone.emit("paste-error", ());
                                        }
                                    }
                                    utils::hide_recording_overlay(&ah_clone);
                                    change_tray_icon(&ah_clone, TrayIconState::Idle);
                                });

                                match result {
                                    Ok(()) => {
                                        info!("Main thread paste task submitted successfully")
                                    }
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
                            
                            // Get settings for error classification and fallback models
                            let settings = get_settings(&ah);
                            
                            // Classify the error for retry handling
                            let failure_type = if err.to_string().contains("Model is not loaded") 
                                || err.to_string().contains("failed to load")
                                || err.to_string().contains("Timed out waiting for model") {
                                TranscriptionFailure::ModelLoadFailure {
                                    model_id: settings.selected_model.clone(),
                                    error: err.to_string(),
                                }
                            } else if err.to_string().contains("timed out") {
                                TranscriptionFailure::Timeout {
                                    model_id: settings.selected_model.clone(),
                                    duration_secs: 120,
                                }
                            } else {
                                TranscriptionFailure::Unknown {
                                    error: err.to_string(),
                                }
                            };
                            
                            // Get fallback models from settings (hybrid mode models)
                            let fallback_models = {
                                let mut models = Vec::new();
                                if settings.hybrid_mode_enabled {
                                    if let Some(short_model) = &settings.hybrid_short_audio_model {
                                        if short_model != &settings.selected_model {
                                            models.push(short_model.clone());
                                        }
                                    }
                                    if let Some(long_model) = &settings.hybrid_long_audio_model {
                                        if long_model != &settings.selected_model 
                                            && !models.contains(long_model) {
                                            models.push(long_model.clone());
                                        }
                                    }
                                }
                                models
                            };
                            
                            // Save entry with empty text so user can retry
                            let history_entry_id = if wav_saved {
                                let entry_result = hm.save_entry(
                                    file_name.clone(),
                                    String::new(),
                                    post_process,
                                    None,
                                    None,
                                    None,
                                    false, // routed: not sent to router
                                );
                                
                                match entry_result {
                                    Ok(entry) => {
                                        info!("Saved history entry {} for failed transcription", entry.id);
                                        Some(entry.id)
                                    }
                                    Err(save_err) => {
                                        error!("Failed to save failed history entry: {}", save_err);
                                        None
                                    }
                                }
                            } else {
                                None
                            };
                            
                            // Add to retry queue for automatic retry
                            if wav_saved {
                                if let Some(retry_queue) = ah.try_state::<Arc<Mutex<TranscriptionRetryQueue>>>() {
                                    let wav_path = hm.recordings_dir().join(&file_name);
                                    let model_id = {
                                        let settings = get_settings(&ah);
                                        settings.selected_model.clone()
                                    };
                                    
                                    if let Err(retry_err) = retry_queue.lock().add_failed_transcription(
                                        wav_path,
                                        model_id,
                                        fallback_models,
                                        failure_type,
                                        post_process,
                                        None, // post_process_prompt
                                        history_entry_id,
                                    ) {
                                        error!("Failed to add transcription to retry queue: {}", retry_err);
                                    } else {
                                        info!("Added failed transcription to retry queue for automatic retry");
                                    }
                                }
                            }
                            
                            utils::hide_recording_overlay(&ah);
                            change_tray_icon(&ah, TrayIconState::Idle);
                            
                            // Emit a recoverable transcription error for the error dialog system
                            let model_id_for_error = {
                                let settings = get_settings(&ah);
                                settings.selected_model.clone()
                            };
                            crate::error_events::emit_transcription_error(
                                &ah,
                                &err.to_string(),
                                Some(&model_id_for_error),
                                true, // Transcription errors are generally retriable
                            );
                        }
                    }
                }
            } else {
                debug!("No samples retrieved from recording stop");
                // ── Structured event: no samples ──
                if let (Some(ref s), Some(tracker)) = (&sid, ah.try_state::<Arc<SessionTracker>>())
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
        debug!(
            "TranscribeWithRouterAction::start called for binding: {}",
            binding_id
        );

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
        let Some(tm) = app.try_state::<Arc<Mutex<TranscriptionManager>>>() else {
            warn!("TranscriptionManager not available, skipping router recording start");
            return;
        };
        let Some(rm) = app.try_state::<Arc<AudioRecordingManager>>() else {
            warn!("AudioRecordingManager not available, skipping router recording start");
            return;
        };
        
        // Clear any previous streaming cancellation flag when starting a new recording.
        // Use the Arc<AtomicBool> directly to avoid blocking on the TM lock.
        // The streaming callback may be holding the TM lock during transcription (seconds),
        // and we want immediate feedback when starting a new recording.
        if let Some(cancel_flag) = app.try_state::<Arc<AtomicBool>>() {
            cancel_flag.store(false, Ordering::Release);
            debug!("Cleared streaming cancel flag for new router recording");
        } else {
            // Fallback to TM lock if Arc<AtomicBool> not available (shouldn't happen)
            tm.lock().clear_streaming_cancel();
        }
        
        tm.lock().initiate_model_load();
        let rm_clone = Arc::clone(&rm);
        std::thread::spawn(move || {
            if let Err(e) = rm_clone.preload_vad() {
                debug!("VAD pre-load failed: {}", e);
            }
        });

        let binding_id = binding_id.to_string();
        change_tray_icon(app, TrayIconState::Recording);
        show_recording_overlay_with_mode(app, OverlayMode::Router);

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
                        detail: Some(err.clone()),
                    },
                );

                // Emit a recoverable error for the error dialog system
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
        // Unregister cancel shortcut
        shortcut::unregister_cancel_shortcut(app);

        // Cancel any in-progress streaming transcription to prevent wasted work.
        // IMPORTANT: Use the streaming_cancel_flag managed separately in app state
        // instead of tm.lock().cancel_streaming() to avoid blocking.
        // The streaming callback holds the TM lock during transcription (seconds),
        // and this stop handler would block waiting for that lock, freezing the UI.
        // By using the Arc<AtomicBool> directly, we can cancel without waiting.
        if let Some(cancel_flag) = app.try_state::<Arc<AtomicBool>>() {
            cancel_flag.store(true, Ordering::Release);
            debug!("Cancelled streaming transcription via Arc<AtomicBool>");
        } else {
            warn!("Streaming cancel flag not available in app state");
        }

        let stop_time = Instant::now();
        debug!(
            "TranscribeWithRouterAction::stop called for binding: {}",
            binding_id
        );

        let ah = app.clone();
        let Some(rm) = app.try_state::<Arc<AudioRecordingManager>>() else {
            warn!("AudioRecordingManager not available, cannot stop router recording");
            return;
        };
        let rm = Arc::clone(&rm);
        let Some(tm) = app.try_state::<Arc<Mutex<TranscriptionManager>>>() else {
            warn!("TranscriptionManager not available, cannot stop router transcription");
            return;
        };
        let tm = Arc::clone(&tm);
        let Some(hm) = app.try_state::<Arc<HistoryManager>>() else {
            warn!("HistoryManager not available, cannot save router recording");
            return;
        };
        let hm = Arc::clone(&hm);

        let sid: Option<SessionId> = app
            .try_state::<Arc<SessionTracker>>()
            .and_then(|t| t.current_session_id());

        // ── KEY DIFFERENCE from TranscribeAction ──
        // Show routing-specific overlay during transcription + routing
        // instead of hiding immediately. The overlay transitions through
        // "Routing..." (transcribing) then "Filing..." (processing)
        // states, then is hidden after the router subprocess completes.
        change_tray_icon(app, TrayIconState::Transcribing);
        show_transcribing_overlay_with_mode(app, OverlayMode::Router);

        // Unmute before playing audio feedback
        rm.remove_mute();
        play_feedback_sound(app, SoundType::Stop);

        let binding_id = binding_id.to_string(); // Clone for async task

        tauri::async_runtime::spawn(async move {
            let _guard = FinishGuard(ah.clone());
            debug!("Starting async router task for binding: {}", binding_id);

            let stop_recording_time = Instant::now();
            let samples = rm.stop_recording(&binding_id);
            if let Some(samples) = samples {
                debug!("Recording stopped, sample count: {}", samples.len());

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
                let transcription_result = tm.lock().transcribe(samples);

                // ── Structured session tracking ──
                let model_id = transcription_result
                    .as_ref()
                    .map(|r| r.model_id.clone())
                    .unwrap_or_else(|e| {
                        warn!("Transcription failed: {}", e);
                        "unknown".to_string()
                    });

                if let (Some(ref s), Some(tracker)) = (&sid, ah.try_state::<Arc<SessionTracker>>())
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

                        let transcription_text = transcription.text.trim().to_string();

                        // ── Handle empty transcription in router mode ──
                        // If the user didn't speak anything (silence), skip the routing flow
                        // and show a notification instead of trying to route an empty message.
                        if transcription_text.is_empty() {
                            warn!("Router transcription returned empty text - skipping routing");

                            // Trigger USB watchdog check (same as normal transcription)
                            // Calculate duration from sample count (16000 Hz sample rate)
                            let duration_secs = sample_count as f32 / 16000.0;
                                if rm.usb_watchdog.on_silent_transcription(duration_secs) {
                                    if let Err(e) = rm.restart_microphone_if_needed() {
                                    error!(
                                        "Failed to restart microphone after silent transcription USB cycle: {}",
                                        e
                                    );
                                }
                            }

                            // Show notification about empty transcription
                            send_macos_notification("Handy Router", "No speech detected");

                            // Hide overlay and reset tray
                            utils::hide_recording_overlay(&ah);
                            change_tray_icon(&ah, TrayIconState::Idle);

                            // End session tracking if present
                            if let (Some(ref s), Some(tracker)) =
                                (&sid, ah.try_state::<Arc<SessionTracker>>())
                            {
                                tracker.fail_session(s, "Empty transcription - no speech detected");
                            }

                            return;
                        }

                        let model_id_for_history = transcription.model_id.clone();

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

                        // Save to history with routed=true and capture entry ID
                        let history_entry_id: Option<i64> = if wav_saved {
                            match hm.save_entry(
                                file_name,
                                transcription_text.clone(),
                                false, // post_process_requested
                                None,  // post_processed_text
                                None,  // post_process_prompt
                                Some(model_id_for_history),
                                true, // routed
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

                        // ── Emit transcription preview for routing overlay ──
                        // The overlay transitions to "confirming" state. Update the window
                        // position (fixed max-height, content managed by CSS).
                        if let Some(overlay_window) = ah.get_webview_window("recording_overlay") {
                            crate::overlay::update_overlay_position(&ah, "confirming", &OverlayMode::Router);
                            let _ = overlay_window.emit("transcription-preview", &transcription_text);
                        }

                        // ── Wait for user confirmation (with countdown) before routing ──
                        // Create a oneshot channel for the frontend to send confirmation
                        let (confirm_tx, confirm_rx) = tokio::sync::oneshot::channel::<String>();
                        
                        // Store the pending routing state so the frontend can trigger it
                        let pending_state: crate::commands::PendingRoutingState = std::sync::Arc::new(
                            parking_lot::Mutex::new(Some(crate::commands::PendingRouting { confirm_tx }))
                        );
                        ah.manage(pending_state);

                        // Wait for confirmation with timeout (30 seconds)
                        let confirmation_timeout = std::time::Duration::from_secs(30);
                        let confirmed_text = match tokio::time::timeout(confirmation_timeout, confirm_rx).await {
                            Ok(Ok(edited_text)) => {
                                debug!("Router confirmation received, text length: {}", edited_text.len());
                                edited_text
                            }
                            Ok(Err(_)) => {
                                debug!("Router confirmation channel closed, using original text");
                                transcription_text.clone()
                            }
                            Err(_) => {
                                debug!("Router confirmation timeout, using original text");
                                transcription_text.clone()
                            }
                        };

                        // ── Show "Filing…" overlay while routing ──
                        show_processing_overlay_with_mode(&ah, OverlayMode::Router);
                        
                        // Use the confirmed (possibly edited) text for routing
                        let transcription_text = confirmed_text;

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
                            let hm_for_router = if let Some(id) = history_entry_id {
                                Some((hm.clone(), id))
                            } else {
                                None
                            };

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
                                    Ok((summary_opt, handler_data)) => {
                                        let any_success =
                                            handler_data.iter().any(|d| d.status == "✅");
                                        // Build summary text
                                        let summary_text = match &summary_opt {
                                            Some(s) => s.clone(),
                                            None => {
                                                if handler_data.is_empty() {
                                                    "No handlers".to_string()
                                                } else {
                                                    // Shouldn't happen: summary_opt is always Some when handler_data is non-empty
                                                    format!(
                                                        "{} handlers, none succeeded",
                                                        handler_data.len()
                                                    )
                                                }
                                            }
                                        };

                                        // ── DETECTION: Verify at least one handler succeeded ──
                                        if !any_success && !handler_data.is_empty() {
                                            warn!(
                                                "Router subprocess completed but no handlers succeeded (exit code 0): {}",
                                                summary_text
                                            );
                                        }

                                        // Save routing result to history entry
                                        if let Some((ref hm, entry_id)) = hm_for_router {
                                            let routing_json = serde_json::to_string(&handler_data)
                                                .unwrap_or_else(|_| "[]".to_string());
                                            if let Err(e) = hm
                                                .update_routing_result(entry_id, Some(routing_json))
                                            {
                                                error!("Failed to update routing result in history: {}", e);
                                            }
                                        }

                                        info!("Router completed: {}", summary_text);

                                        let event = RouterResultEvent {
                                            success: any_success || handler_data.is_empty(),
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

                                        // ── Clean up overlay after routing succeeds ──
                                        // ============================================================================
                                        // BUGFIX (2026-06-15): Router Filing Race Condition
                                        // ============================================================================
                                        // PROBLEM: When router finishes filing and user has already started a new
                                        // transcription, hide_overlay would dismiss the overlay mid-recording.
                                        //
                                        // ROOT CAUSE: Router thread is fire-and-forget. It runs after FinishGuard
                                        // drops, so coordinator is already Idle when router thread finishes. User
                                        // can start new transcription while router thread is still running.
                                        //
                                        // FIX: Check is_active_use() before hiding. If true, keep overlay visible.
                                        // This guard catches the case where router finishes BEFORE user starts new
                                        // recording. The frontend has TWO matching guards:
                                        // 1. RecordingOverlay.tsx hide-overlay handler (lines 548-588): checks state
                                        //    before hiding on event emission race condition.
                                        // 2. RecordingOverlay.tsx router-result timeout (lines 414-441): checks state
                                        //    before hiding after 5-second result display timeout.
                                        //
                                        // See learning-log.md "Router Filing Race Condition — Overlay Dismissal Bug"
                                        // for full documentation.
                                        // ============================================================================
                                        let is_active = ah_for_router
                                            .try_state::<Arc<TranscriptionCoordinator>>()
                                            .map_or(false, |coord| coord.is_active_use());
                                        if !is_active {
                                            utils::hide_recording_overlay(&ah_for_router);
                                            change_tray_icon(&ah_for_router, TrayIconState::Idle);
                                        } else {
                                            info!("Router finished but transcription pipeline is active — keeping overlay");
                                        }
                                    }
                                    Err(e) => {
                                        error!("Router subprocess failed: {}", e);

                                        // Save routing failure to history entry
                                        if let Some((ref hm, entry_id)) = hm_for_router {
                                            let failure_result = vec![RouterHandlerData {
                                                status: "❌".to_string(),
                                                handler: "Router Error".to_string(),
                                                classification: "error".to_string(),
                                                file_path: None,
                                            }];
                                            let routing_json =
                                                serde_json::to_string(&failure_result)
                                                    .unwrap_or_else(|_| "[]".to_string());
                                            if let Err(save_err) = hm
                                                .update_routing_result(entry_id, Some(routing_json))
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
                                            transcription_text: transcription_text_for_router,
                                        };
                                        let _ = ah_for_router.emit("router-result", &event);

                                        // Send macOS notification for router failure
                                        let error_display = if e.len() > 150 {
                                            format!("{}...", &e[..150])
                                        } else {
                                            e.clone()
                                        };
                                        send_macos_notification(
                                            "Handy Router Error",
                                            &error_display,
                                        );

                                        // ── Clean up overlay after routing fails ──
                                        // Same BUGFIX (2026-06-15) as success case above — see comment there.
                                        // Don't hide overlay if another transcription is active.
                                        // The frontend also guards against hide-overlay event races.
                                        let is_other_active = ah_for_router
                                            .try_state::<Arc<TranscriptionCoordinator>>()
                                            .map_or(false, |coord| coord.is_active_use());
                                        if !is_other_active {
                                            utils::hide_recording_overlay(&ah_for_router);
                                            change_tray_icon(&ah_for_router, TrayIconState::Idle);
                                        } else {
                                            info!("Router failed but transcription pipeline is active — keeping overlay visible");
                                        }
                                    }
                                }

                                // ── Finish session after routing ──
                                if let (Some(ref s), Some(tracker)) = (
                                    &sid_for_router,
                                    ah_for_router.try_state::<Arc<SessionTracker>>(),
                                ) {
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
                            send_macos_notification(
                                "Handy Router",
                                "No router path configured. Check Settings.",
                            );

                            // Fall back to paste if no router configured
                            if !transcription_text.is_empty() {
                                let ah_for_paste = ah.clone();
                                let _ = ah.run_on_main_thread(move || {
                                    let _ = utils::paste(transcription_text, ah_for_paste.clone());
                                    utils::hide_recording_overlay(&ah_for_paste);
                                    change_tray_icon(&ah_for_paste, TrayIconState::Idle);
                                });
                            } else {
                                utils::hide_recording_overlay(&ah);
                                change_tray_icon(&ah, TrayIconState::Idle);
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

                        // Get settings for error classification and fallback models
                        let settings = get_settings(&ah);

                        // Classify the error for retry handling
                        let failure_type = if err.to_string().contains("Model is not loaded") 
                            || err.to_string().contains("failed to load")
                            || err.to_string().contains("Timed out waiting for model") {
                            TranscriptionFailure::ModelLoadFailure {
                                model_id: settings.selected_model.clone(),
                                error: err.to_string(),
                            }
                        } else if err.to_string().contains("timed out") {
                            TranscriptionFailure::Timeout {
                                model_id: settings.selected_model.clone(),
                                duration_secs: 120,
                            }
                        } else {
                            TranscriptionFailure::Unknown {
                                error: err.to_string(),
                            }
                        };

                        // Get fallback models from settings (hybrid mode models)
                        let fallback_models = {
                            let mut models = Vec::new();
                            if settings.hybrid_mode_enabled {
                                if let Some(short_model) = &settings.hybrid_short_audio_model {
                                    if short_model != &settings.selected_model {
                                        models.push(short_model.clone());
                                    }
                                }
                                if let Some(long_model) = &settings.hybrid_long_audio_model {
                                    if long_model != &settings.selected_model
                                        && !models.contains(long_model)
                                    {
                                        models.push(long_model.clone());
                                    }
                                }
                            }
                            models
                        };

                        // Save entry with empty text so user can retry, capture ID for retry queue
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
                                    info!("Saved history entry {} for failed router transcription", entry.id);
                                    Some(entry.id)
                                }
                                Err(save_err) => {
                                    error!("Failed to save failed history entry: {}", save_err);
                                    None
                                }
                            }
                        } else {
                            None
                        };

                        // Add to retry queue for automatic retry
                        if wav_saved {
                            if let Some(retry_queue) = ah.try_state::<Arc<Mutex<TranscriptionRetryQueue>>>() {
                                let wav_path = hm.recordings_dir().join(&file_name);
                                let model_id = {
                                    let settings = get_settings(&ah);
                                    settings.selected_model.clone()
                                };

                                if let Err(retry_err) = retry_queue.lock().add_failed_transcription(
                                    wav_path,
                                    model_id,
                                    fallback_models,
                                    failure_type,
                                    false, // post_process (router doesn't post-process)
                                    None,  // post_process_prompt
                                    history_entry_id,
                                ) {
                                    error!("Failed to add transcription to retry queue: {}", retry_err);
                                } else {
                                    info!("Added failed router transcription to retry queue for automatic retry");
                                }
                            }
                        }

                        // ── Notify user of failure with retry intent ──
                        send_macos_notification("Handy Router", "Transcription failed. Will retry automatically.");

                        // ── Clean up overlay on transcription failure ──
                        utils::hide_recording_overlay(&ah);
                        change_tray_icon(&ah, TrayIconState::Idle);

                        // Emit a recoverable transcription error for the error dialog system
                        let router_model_id = {
                            let settings = get_settings(&ah);
                            settings.selected_model.clone()
                        };
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
                if let (Some(ref s), Some(tracker)) = (&sid, ah.try_state::<Arc<SessionTracker>>())
                {
                    tracker.fail_session(s, "No audio samples from recording stop");
                }

                // ── Clean up overlay when no samples ──
                utils::hide_recording_overlay(&ah);
                change_tray_icon(&ah, TrayIconState::Idle);

                // ── Clean up overlay on empty samples ──
                utils::hide_recording_overlay(&ah);
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

    // Build the command
    // IMPORTANT: Use the miniforge3 Python path because the system /usr/bin/python3
    // (found when launched as a GUI app) lacks required deps (soundfile, etc.).
    // The decision_integration.py module already uses this full path internally.
    let python_bin = "/Users/caffae/miniforge3/bin/python3";
    let mut cmd = Command::new(python_bin);
    cmd.arg(router_script)
        .arg("--text")
        .arg(transcription_text)
        .arg("--datetime")
        .arg(datetime_str)
        .arg("--json")
        .arg("--handy");

    // Prepend miniforge3 to PATH so the subprocess and any children (e.g.
    // decision_router.py → llm_client.py) find the correct python + deps.
    // GUI-launched apps get a minimal PATH from macOS that excludes conda.
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
                    // Skip comments and empty lines
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
    let (handler_summary, handler_data) = match parse_router_json_output(&stdout) {
        Some(output) => (Some(output.summary), output.handler_data),
        None => (None, Vec::new()),
    };
    Ok((handler_summary, handler_data))
}

/// Parsed output returned by the boss_router subprocess.
struct RouterOutput {
    /// Human-readable summary string (e.g. "✅ Daily | ✅ Zettelkasten (my-note)").
    summary: String,
    /// Structured handler results for persistence and verification.
    handler_data: Vec<RouterHandlerData>,
}

/// Parse the JSON output from boss_router.py --json.
/// Returns structured handler data and a human-readable summary.
fn parse_router_json_output(stdout: &str) -> Option<RouterOutput> {
    // The JSON is on the last line of output (there may be log lines before it)
    for line in stdout.lines().rev() {
        let line = line.trim();
        if line.starts_with('{') {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(line) {
                // Extract handler summaries
                let handlers = json.get("handlers").and_then(|h| h.as_array());
                if let Some(handlers) = handlers {
                    let mut handler_data: Vec<RouterHandlerData> = Vec::new();
                    let mut summaries: Vec<String> = Vec::new();

                    for h in handlers {
                        let status = h.get("status").and_then(|v| v.as_str()).unwrap_or("?");
                        let handler_name = h.get("handler").and_then(|v| v.as_str()).unwrap_or("?");
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

                        // Build summary line
                        if let Some(ref path) = file_path {
                            let filename = std::path::Path::new(path)
                                .file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or(path);
                            summaries.push(format!("{} {} ({})", status, handler_name, filename));
                        } else {
                            summaries.push(format!("{} {}", status, handler_name));
                        }
                    }

                    if handler_data.is_empty() {
                        return None;
                    }

                    let _any_success = handler_data.iter().any(|d| d.status == "✅");
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
/// Used for router success/failure feedback since the overlay is already hidden.
fn send_macos_notification(title: &str, message: &str) {
    // Escape special characters for AppleScript
    let escaped_message = message
        .replace('\\', "\\\\")
        .replace('"', "\\\\\"")
        .replace('\n', " ");
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
