// TranscribeWithRouterAction: records speech → transcribes → routes text
// to boss_router.py subprocess. Includes confirmation flow, macOS notifications,
// and retry queue integration for failed router transcriptions.

use crate::audio_feedback::{play_feedback_sound, play_feedback_sound_blocking, SoundType};
use crate::audio_toolkit::VadPolicy;
use crate::errors::AppError;
use crate::logging::{self, AppEvent, SessionId};
use crate::managers::audio::recording_error_type;
use crate::managers::audio::AudioRecordingManager;
use crate::managers::history::HistoryManager;
use crate::managers::transcription::{TranscriptionManager, TRANSCRIBING_SLOT_WAIT_TIMEOUT};
use crate::managers::transcription_retry::{TranscriptionFailure, TranscriptionRetryQueue};
use crate::overlay::OverlayMode;
use crate::session::SessionTracker;
use crate::settings::get_settings;
use crate::shortcut;
use crate::streaming_cancel_flag::StreamingCancelFlag;
use crate::tray::{set_tray_state, TrayIconState};
use crate::utils::{
    self, show_processing_overlay_with_mode, show_recording_overlay_with_mode,
    show_transcribing_overlay_with_mode,
};
use crate::TranscriptionCoordinator;
use log::{debug, error, info, warn};
use parking_lot::Mutex;
use std::io::{BufRead, BufReader, Read};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_notification::NotificationExt;
use wait_timeout::ChildExt;

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

/// One route classification reported early by boss_router.py via a
/// `"event":"classified"` stdout line, long before the handlers finish.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClassifiedRoute {
    /// Internal classification (e.g. "direct_order").
    pub classification: String,
    /// Human-readable handler name (e.g. "Direct Order").
    pub handler_name: String,
    /// Emoji prefix ("" when the classification has none).
    pub emoji: String,
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

// Transcribe With Router Action
//
// Records speech → transcribes → sends text to boss_router.py subprocess.
// The recording overlay is shown during recording (same as normal transcribe),
// but after recording stops the overlay is hidden immediately and the rest
// (transcription + routing) happens in the background. The user gets
// feedback later via the router's Telegram notification.
pub(super) struct TranscribeWithRouterAction;

impl super::ShortcutAction for TranscribeWithRouterAction {
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
        // The cancel flag fast path is intentionally kept lock-free so it can never
        // block the coordinator thread even when the TM mutex is held by a GPU callback.
        if let Some(cancel_flag) = app.try_state::<StreamingCancelFlag>() {
            cancel_flag.set(false, "router_start");
        }

        // Safely kick model load without blocking the coordinator thread on a contended
        // TranscriptionManager mutex. If the lock is unavailable for 2s, skip the kick
        // rather than stalling the start path — the next audio chunk will retry loading.
        if let Some(tm_guard) = tm.try_lock_for(Duration::from_secs(2)) {
            let lock_dt = start_time.elapsed();
            tm_guard.clear_streaming_cancel();
            let before_init = Instant::now();
            tm_guard.initiate_model_load();
            let init_dt = before_init.elapsed();
            debug!(
                "[START-LOCK] TM lock acquired for '{}' (router) (lock wait+init took {:?}, clear+initiate took {:?})",
                binding_id, lock_dt, init_dt
            );
            if init_dt > Duration::from_millis(100) {
                warn!(
                    "[START-LOCK] clear_streaming_cancel + initiate_model_load took {:?} for '{}' (router)",
                    init_dt, binding_id
                );
            }
        } else {
            warn!(
                "[START-LOCK] TM lock NOT acquired within 2s on start for '{}' — skipping model-load kick (router)",
                binding_id
            );
        }
        let rm_clone = Arc::clone(&rm);
        std::thread::spawn(move || {
            if let Err(e) = rm_clone.preload_vad() {
                debug!("VAD pre-load failed: {}", e);
            }
        });

        let binding_id = binding_id.to_string();
        set_tray_state(app, TrayIconState::Recording);
        show_recording_overlay_with_mode(app, OverlayMode::Router);

        // Recording start logic — identical to TranscribeAction
        let settings = get_settings(app);
        let is_always_on = settings.always_on_microphone;
        let mut recording_error: Option<anyhow::Error> = None;

        if is_always_on {
            debug!("Always-on mode: Playing audio feedback immediately");
            let rm_clone = Arc::clone(&rm);
            let app_clone = app.clone();
            std::thread::spawn(move || {
                play_feedback_sound_blocking(&app_clone, SoundType::Start);
                rm_clone.apply_mute();
            });

            match rm.try_start_recording(&binding_id, VadPolicy::Offline) {
                Ok(readiness) => {
                    // The chime is a readiness cue: it follows the first real
                    // microphone chunk rather than a fixed delay (upstream).
                    let generation = readiness.generation();
                    let app_clone = app.clone();
                    let rm_clone = Arc::clone(&rm);
                    std::thread::spawn(move || {
                        if !readiness.wait() {
                            debug!("Microphone readiness wait ended without receiving samples");
                            return;
                        }
                        if !rm_clone.is_recording_readiness_current(generation) {
                            debug!("Microphone became ready for an inactive recording");
                            return;
                        }
                        debug!("Microphone is receiving samples; recording is ready");
                        play_feedback_sound_blocking(&app_clone, SoundType::Start);
                        if rm_clone.is_recording_readiness_current(generation) {
                            rm_clone.apply_mute();
                        }
                    });
                }
                Err(e) => {
                    debug!("Recording failed: {e:#}");
                    recording_error = Some(e);
                }
            }
        } else {
            debug!("On-demand mode: Starting recording first, then audio feedback");
            match rm.try_start_recording(&binding_id, VadPolicy::Offline) {
                Ok(readiness) => {
                    debug!(
                        "Recording requested in {:?}; waiting for first microphone samples",
                        start_time.elapsed()
                    );
                    let generation = readiness.generation();
                    let app_clone = app.clone();
                    let rm_clone = Arc::clone(&rm);
                    std::thread::spawn(move || {
                        if !readiness.wait() {
                            debug!("Microphone readiness wait ended without receiving samples");
                            return;
                        }
                        if !rm_clone.is_recording_readiness_current(generation) {
                            debug!("Microphone became ready for an inactive recording");
                            return;
                        }
                        debug!("Microphone is receiving samples; recording is ready");
                        play_feedback_sound_blocking(&app_clone, SoundType::Start);
                        if rm_clone.is_recording_readiness_current(generation) {
                            rm_clone.apply_mute();
                        }
                    });
                }
                Err(e) => {
                    debug!("Failed to start recording: {e:#}");
                    recording_error = Some(e);
                }
            }
        }

        if recording_error.is_none() {
            shortcut::register_cancel_shortcut(app);
        } else {
            utils::hide_recording_overlay(app);
            set_tray_state(app, TrayIconState::Idle);
            if let Some(err) = recording_error {
                if let Some(tracker) = app.try_state::<Arc<SessionTracker>>() {
                    if let Some(sid) = tracker.current_session_id() {
                        tracker.fail_session(&sid, &format!("{err:#}"));
                    }
                }
                let error_type = recording_error_type(&err);
                let _ = app.emit(
                    "recording-error",
                    super::transcribe::RecordingErrorEvent {
                        error_type: error_type.to_string(),
                        detail: Some(format!("{err:#}")),
                    },
                );

                crate::error_events::emit_audio_device_error(
                    app,
                    error_type,
                    &format!("{err:#}"),
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
        if let Some(cancel_flag) = app.try_state::<StreamingCancelFlag>() {
            cancel_flag.set(true, "router_stop");
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
            utils::hide_recording_overlay(app);
            set_tray_state(app, TrayIconState::Idle);
            return;
        };
        let rm = Arc::clone(&rm);
        // Keep a second Arc handle for the fire-and-forget mute thread used after spawning.
        let rm_mute = Arc::clone(&rm);
        let Some(tm) = app.try_state::<Arc<Mutex<TranscriptionManager>>>() else {
            warn!("TranscriptionManager not available, cannot stop router transcription");
            utils::hide_recording_overlay(app);
            set_tray_state(app, TrayIconState::Idle);
            return;
        };
        let tm = Arc::clone(&tm);
        let Some(hm) = app.try_state::<Arc<HistoryManager>>() else {
            warn!("HistoryManager not available, cannot save router recording");
            utils::hide_recording_overlay(app);
            set_tray_state(app, TrayIconState::Idle);
            return;
        };
        let hm = Arc::clone(&hm);

        let sid: Option<SessionId> = app
            .try_state::<Arc<SessionTracker>>()
            .and_then(|t| t.current_session_id());

        let binding_id_spawn = binding_id.to_string(); // Clone for async task

        // CRITICAL: Spawn the recording-stop work IMMEDIATELY, before any UI/feedback
        // call that might synchronously block (e.g. AppleScript mute, overlay JS).
        // See the matching comment in TranscribeAction::stop for full rationale.
        tauri::async_runtime::spawn(async move {
            // Capture the session that owns this router flow before any newer
            // recording can bump the overlay session. A's teardown events are
            // tagged with this id so they cannot clobber a later session.
            let owner_session: u64 = crate::overlay::current_overlay_session();

            let mut finish_guard = Some(super::transcribe::FinishGuard(
                ah.clone(),
                Arc::clone(&tm),
                Some(owner_session),
            ));
            debug!(
                "Starting async router task for binding: {}",
                binding_id_spawn
            );

            let stop_recording_time = Instant::now();
            let recording_result = rm.stop_recording(&binding_id_spawn);
            if let Some((processed_samples, raw_samples)) = recording_result {
                debug!(
                    "Recording stopped, processed: {}, raw: {}",
                    processed_samples.len(),
                    raw_samples.len()
                );

                if processed_samples.is_empty() {
                    debug!("Recording produced no audio samples; skipping");
                    if let (Some(ref s), Some(tracker)) =
                        (&sid, ah.try_state::<Arc<SessionTracker>>())
                    {
                        tracker.fail_session(s, "No audio samples from recording stop");
                    }
                    utils::hide_recording_overlay(&ah);
                    set_tray_state(&ah, TrayIconState::Idle);
                    return;
                }

                // Save RAW WAV (what the mic actually captured — unfiltered)
                let raw_sample_count = raw_samples.len();
                let file_name = format!("handy-{}.wav", chrono::Utc::now().timestamp());
                let wav_path = hm.recordings_dir().join(&file_name);
                let wav_path_for_verify = wav_path.clone();
                // Capture the opening for the gated trigger-phrase fallback scan
                // (see actions/trigger_fallback.rs) before raw_samples is moved
                // into the WAV writer. Only allocate when the feature is enabled,
                // to keep normal recordings on the fast (zero-extra) path.
                let trigger_samples: Vec<f32> = if get_settings(&ah).trigger_phrase_fallback_enabled
                {
                    let scan_seconds = get_settings(&ah).trigger_scan_seconds.clamp(3, 30) as usize;
                    raw_samples
                        .iter()
                        .take(scan_seconds * 16_000)
                        .cloned()
                        .collect()
                } else {
                    Vec::new()
                };
                let samples_for_wav = raw_samples;
                let wav_handle = tauri::async_runtime::spawn_blocking(move || {
                    crate::audio_toolkit::save_wav_file(&wav_path, &samples_for_wav)
                });

                // Save VAD-filtered WAV (for debugging VAD quality)
                let vad_file_name = format!("handy-{}-vad.wav", chrono::Utc::now().timestamp());
                let vad_wav_path = hm.recordings_dir().join(&vad_file_name);
                let vad_samples = processed_samples.clone();
                let vad_wav_handle = tauri::async_runtime::spawn_blocking(move || {
                    crate::audio_toolkit::save_wav_file(&vad_wav_path, &vad_samples)
                });

                // Transcribe using VAD-filtered samples
                let transcription_time = Instant::now();
                let transcription_result = match tm.try_lock_for(TRANSCRIBING_SLOT_WAIT_TIMEOUT) {
                    Some(guard) => {
                        // Capture the shared streaming cancel flag so it can be cleared
                        // only after the final transcription acquires the `is_transcribing`
                        // slot. Clearing it before the slot is owned lets queued streaming
                        // callbacks race ahead and steal the slot, causing
                        // `AppError::TranscriptionBusy`.
                        let cancel_flag = guard.streaming_cancel_flag();
                        guard.transcribe_with_hook(processed_samples, move || {
                            cancel_flag.store(false, Ordering::Release);
                            debug!(
                                "Cleared streaming cancel flag after acquiring transcription slot"
                            );
                        })
                    }
                    None => {
                        warn!("Timed out waiting for TranscriptionManager lock after {:?} — aborting transcription", TRANSCRIBING_SLOT_WAIT_TIMEOUT);
                        Err(AppError::TranscriptionBusy)
                    }
                };

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
                        raw_sample_count,
                        stop_recording_time.elapsed().as_millis() as u64,
                    );
                }

                // Await RAW WAV save and verify
                let wav_saved = match wav_handle.await {
                    Ok(Ok(())) => {
                        match crate::audio_toolkit::verify_wav_file(
                            &wav_path_for_verify,
                            raw_sample_count,
                        ) {
                            Ok(()) => true,
                            Err(e) => {
                                error!("RAW WAV verification failed: {}", e);
                                false
                            }
                        }
                    }
                    Ok(Err(e)) => {
                        error!("Failed to save RAW WAV file: {}", e);
                        false
                    }
                    Err(e) => {
                        error!("RAW WAV save task panicked: {}", e);
                        false
                    }
                };

                // Await VAD WAV save (best-effort, non-critical)
                match vad_wav_handle.await {
                    Ok(Ok(())) => {
                        debug!("VAD-filtered WAV saved: {}", vad_file_name);
                    }
                    Ok(Err(e)) => {
                        error!("Failed to save VAD WAV file: {}", e);
                    }
                    Err(e) => {
                        error!("VAD WAV save task panicked: {}", e);
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

                        let mut transcription_text = transcription.text.trim().to_string();

                        // ── Handle empty transcription in router mode ──
                        if transcription_text.is_empty() {
                            warn!("Router transcription returned empty text - skipping routing");

                            let duration_secs = raw_sample_count as f32 / 16000.0;
                            if rm.usb_watchdog.on_silent_transcription(duration_secs) {
                                if let Err(e) = rm.restart_microphone_if_needed() {
                                    error!(
                                        "Failed to restart microphone after silent transcription USB cycle: {}",
                                        e
                                    );
                                }
                            }

                            send_macos_notification(&ah, "Handy Router", "No speech detected");

                            utils::hide_recording_overlay(&ah);
                            set_tray_state(&ah, TrayIconState::Idle);

                            if let (Some(ref s), Some(tracker)) =
                                (&sid, ah.try_state::<Arc<SessionTracker>>())
                            {
                                tracker.fail_session(s, "Empty transcription - no speech detected");
                            }

                            return;
                        }

                        // ── Gated trigger-phrase fallback ──
                        // When the primary model mangled a leading trigger phrase
                        // (e.g. "hey million, write for me …"), re-scan the first
                        // `trigger_scan_seconds` of raw audio with the dedicated
                        // scan model and canonicalize the opening when the scan
                        // candidate confirms a configured trigger.
                        // See actions/trigger_fallback.rs for the pipeline contract.
                        let settings = get_settings(&ah);
                        let phrases = if settings.trigger_phrases.is_empty() {
                            crate::settings::default_trigger_phrases()
                        } else {
                            settings.trigger_phrases.clone()
                        };
                        let suspicious = crate::actions::trigger_fallback::suspicious_opening(
                            &transcription_text,
                        );
                        // Step 1 of the documented pipeline: if a canonical
                        // trigger phrase is already present, there is nothing to
                        // recover — skip the (paid) whisper rescan.
                        let already_canonical =
                            crate::actions::trigger_fallback::detect_canonical_trigger(
                                &transcription_text,
                                &phrases,
                            )
                            .is_some();
                        let scan_model_downloadable = !settings.trigger_scan_model.is_empty()
                            && ah
                                .try_state::<std::sync::Arc<crate::managers::model::ModelManager>>()
                                .and_then(|mm| mm.get_model_info(&settings.trigger_scan_model))
                                .map(|info| info.is_downloaded)
                                .unwrap_or(false);
                        let gate = settings.trigger_phrase_fallback_enabled
                            && (settings.trigger_fallback_force_scan || suspicious)
                            && !already_canonical
                            && write_for_me_prompt_with_phrases(&transcription_text, &phrases)
                                .is_none();

                        if gate {
                            if let Some(trigger_state) =
                                ah.try_state::<crate::TriggerTranscriptionManager>()
                            {
                                let trigger_arc = trigger_state.0.clone();
                                let model = settings.trigger_scan_model.clone();
                                let samples = trigger_samples.clone();
                                let downloadable = scan_model_downloadable;
                                let handle = tokio::task::spawn_blocking(move || {
                                    let Some(guard) =
                                        trigger_arc.try_lock_for(std::time::Duration::from_secs(2))
                                    else {
                                        return Ok::<Option<String>, ()>(None);
                                    };
                                    // Never attempt to initiate a load for a model that
                                    // is not downloaded — `load_model` would emit a
                                    // spurious `loading_failed` UI toast per utterance.
                                    if !downloadable {
                                        return Ok(None);
                                    }
                                    if !guard.is_model_loaded() || guard.is_model_loading() {
                                        guard.initiate_model_load_for(&model);
                                        return Ok(None);
                                    }
                                    let out = guard.transcribe(samples).ok().map(|o| o.text);
                                    Ok(out)
                                });
                                match handle.await {
                                    Ok(Ok(Some(candidate))) => {
                                        if let Some(canonical) = crate::actions::trigger_fallback::detect_canonical_trigger(&candidate, &phrases) {
                                            if let Some(canonicalized) = crate::actions::trigger_fallback::canonicalize_leading_trigger(&transcription_text, &canonical) {
                                                info!(
                                                    "Trigger fallback replaced leading token using Whisper scan candidate: {}",
                                                    candidate
                                                );
                                                transcription_text = canonicalized;
                                            }
                                        }
                                    }
                                    _ => {
                                        debug!("Trigger fallback scan skipped/not available");
                                    }
                                }
                            }
                        }

                        // ── Fast path: "Hey Minion/Hey Assistant, write for me …" ──
                        // Single-shot Ollama Cloud reply, pasted back after a short
                        // countdown with a sound. Skips kimaki/router filing.
                        if let Some(write_prompt) =
                            write_for_me_prompt_with_phrases(&transcription_text, &phrases)
                        {
                            info!(
                                "Fast path: write-for-me prompt ({} chars)",
                                write_prompt.len()
                            );

                            let settings = get_settings(&ah);

                            // API key: stored setting first, then env fallback.
                            let mut api_key = settings.write_for_me_api_key.0.clone();
                            if api_key.trim().is_empty() {
                                api_key = std::env::var("OLLAMA_API_KEY").unwrap_or_default();
                            }
                            if api_key.trim().is_empty() {
                                send_macos_notification(
                                    &ah,
                                    "Handy Write",
                                    "No Ollama API key set (Settings or OLLAMA_API_KEY).",
                                );
                                utils::hide_recording_overlay(&ah);
                                set_tray_state(&ah, TrayIconState::Idle);
                                return;
                            }

                            let provider = crate::settings::PostProcessProvider {
                                id: "ollama-cloud".to_string(),
                                label: "Ollama Cloud".to_string(),
                                base_url: "https://ollama.com/v1".to_string(),
                                allow_base_url_edit: false,
                                models_endpoint: None,
                                supports_structured_output: false,
                                api_format: crate::settings::PostProcessApiFormat::OpenAi,
                            };

                            // Model and system prompt from settings, with defaults
                            // for blank values.
                            let model = {
                                let stored = settings.write_for_me_model.trim();
                                if stored.is_empty() {
                                    crate::settings::default_write_for_me_model()
                                } else {
                                    stored.to_string()
                                }
                            };
                            let system_prompt = {
                                let stored = settings.write_for_me_system_prompt.trim();
                                if stored.is_empty() {
                                    crate::settings::default_write_for_me_system_prompt()
                                } else {
                                    stored.to_string()
                                }
                            };

                            // Tell the overlay to show a "writing" label while the
                            // model generates.
                            let _ = ah.emit(
                                "write-progress",
                                serde_json::json!({ "phase": "generating" }),
                            );

                            let result = crate::llm_client::send_chat_completion_with_schema(
                                &provider,
                                api_key,
                                &model,
                                write_prompt.clone(),
                                Some(system_prompt),
                                None,
                                true,
                            )
                            .await;

                            match result {
                                Ok(Some(output)) => {
                                    let output = output.trim().to_string();
                                    if output.is_empty() {
                                        send_macos_notification(
                                            &ah,
                                            "Handy Write",
                                            "Model returned empty output.",
                                        );
                                    } else {
                                        // Warning sound + short countdown before
                                        // pasting; the overlay shows the countdown
                                        // driven by the write-progress event.
                                        play_feedback_sound(&ah, SoundType::Stop);
                                        let _ = ah.emit(
                                            "write-progress",
                                            serde_json::json!({ "phase": "pasting", "ms": 500 }),
                                        );
                                        tokio::time::sleep(Duration::from_millis(500)).await;
                                        if let Err(e) = utils::paste(output, ah.clone()) {
                                            error!("Paste failed: {}", e);
                                            send_macos_notification(
                                                &ah,
                                                "Handy Write",
                                                "Paste failed; output copied? Check clipboard.",
                                            );
                                        } else {
                                            send_macos_notification(
                                                &ah,
                                                "Handy Write",
                                                "Wrote and pasted output.",
                                            );
                                        }
                                    }
                                }
                                Ok(None) => {
                                    send_macos_notification(
                                        &ah,
                                        "Handy Write",
                                        "Model returned no content.",
                                    );
                                }
                                Err(e) => {
                                    error!("Ollama write-for-me failed: {}", e);
                                    let disp = if e.len() > 150 { &e[..150] } else { &e };
                                    send_macos_notification(&ah, "Handy Write Error", disp);
                                }
                            }

                            utils::hide_recording_overlay(&ah);
                            set_tray_state(&ah, TrayIconState::Idle);

                            if let (Some(ref s), Some(tracker)) =
                                (&sid, ah.try_state::<Arc<SessionTracker>>())
                            {
                                tracker.finish_session(s, 0);
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
                            match hm.save_entry_with_vad(
                                file_name,
                                transcription_text.clone(),
                                false, // post_process_requested
                                None,  // post_processed_text
                                None,  // post_process_prompt
                                Some(model_id_for_history),
                                true, // routed
                                Some(vad_file_name),
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
                        if let Some(overlay_window) = ah.get_webview_window("recording_overlay") {
                            crate::overlay::update_overlay_position(
                                &ah,
                                "confirming",
                                &OverlayMode::Router,
                            );
                            let _ =
                                overlay_window.emit("transcription-preview", &transcription_text);
                        }

                        // ── Set coordinator state to Confirming ──
                        if let Some(coordinator) = ah.try_state::<TranscriptionCoordinator>() {
                            coordinator.set_confirming(
                                &ah,
                                transcription_text.clone(),
                                Some("transcribe_with_router".to_string()),
                                owner_session,
                            );
                        }

                        // ── Wait for user confirmation (with countdown) before routing ──
                        let (confirm_tx, confirm_rx) = tokio::sync::oneshot::channel::<String>();

                        let pending_state: crate::commands::PendingRoutingState =
                            std::sync::Arc::new(parking_lot::Mutex::new(Some(
                                crate::commands::PendingRouting { confirm_tx },
                            )));
                        ah.manage(pending_state);

                        // Wait for confirmation with timeout (30 seconds)
                        let confirmation_timeout = std::time::Duration::from_secs(30);
                        let confirmed_text =
                            match tokio::time::timeout(confirmation_timeout, confirm_rx).await {
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

                        // ── Hide overlay during filing — the user should have full
                        // screen control while the router subprocess runs. ──
                        utils::hide_recording_overlay(&ah);

                        // ── Transition coordinator from Confirming to Processing ──
                        // The coordinator stays in Processing (blocking new recordings)
                        // but the overlay is hidden so the user can interact freely.
                        if let Some(coordinator) = ah.try_state::<TranscriptionCoordinator>() {
                            coordinator.set_processing_with_binding(
                                &ah,
                                Some("transcribe_with_router".to_string()),
                                owner_session,
                            );
                        }

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

                            // Drop the FinishGuard BEFORE spawning the router subprocess.
                            // The async block is about to exit (the subprocess runs in a
                            // separate thread), so FinishGuard would fire immediately and
                            // reset the coordinator to Idle — hiding the overlay before
                            // the router result is shown. Instead, the router subprocess
                            // thread will call notify_processing_finished() when done.
                            finish_guard.take();

                            // Spawn the router as a subprocess
                            std::thread::spawn(move || {
                                let result = run_router_subprocess(
                                    &ah_for_router,
                                    &router_script,
                                    &transcription_text_for_router,
                                    &datetime_str,
                                    env_file.as_deref(),
                                );

                                match result {
                                    Ok((summary_opt, handler_data)) => {
                                        let any_success =
                                            handler_data.iter().any(|d| d.status == "✅");
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
                                                "Router subprocess completed but no handlers succeeded (exit code 0): {}",
                                                summary_text
                                            );
                                        }

                                        // Save routing result to history entry
                                        let mut existing_routing_json_for_verify: Option<String> =
                                            None;
                                        if let Some((ref hm, entry_id)) = hm_for_router {
                                            let routing_json = serde_json::to_string(&handler_data)
                                                .unwrap_or_else(|_| "[]".to_string());
                                            existing_routing_json_for_verify =
                                                Some(routing_json.clone());
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
                                        send_macos_notification(
                                            &ah_for_router,
                                            "Handy Router",
                                            &notification_text,
                                        );

                                        // Schedule delayed verification of pending diary entries
                                        // in case Obsidian clobbered an external append.
                                        if any_success {
                                            run_pending_entry_verify(
                                                ah_for_router.clone(),
                                                router_script.clone(),
                                                env_file.clone(),
                                                hm_for_router.clone(),
                                                existing_routing_json_for_verify.clone(),
                                            );
                                        }

                                        // NOTE: Overlay hide is now handled AFTER notify_processing_finished()
                                        // below, with a 5-second delay so the user can see the result.
                                        info!(
                                            "Router completion: success, scheduling delayed hide"
                                        );
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

                                        let error_display = if e.len() > 150 {
                                            format!("{}...", &e[..150])
                                        } else {
                                            e.clone()
                                        };
                                        send_macos_notification(
                                            &ah_for_router,
                                            "Handy Router Error",
                                            &error_display,
                                        );

                                        // NOTE: Overlay hide is now handled AFTER notify_processing_finished()
                                        // below, with a 5-second delay so the user can see the result.
                                        info!(
                                            "Router completion: failure, scheduling delayed hide"
                                        );
                                    }
                                }

                                // ── IMMEDIATELY free the coordinator ──
                                // This allows the user to start a new recording right away,
                                // even while the router result is still being displayed.
                                // Previously, notify_processing_finished() was called at the
                                // end of the thread, keeping the coordinator in Processing
                                // (and rejecting new recordings) for the entire subprocess
                                // duration.
                                if let Some(coord) =
                                    ah_for_router.try_state::<TranscriptionCoordinator>()
                                {
                                    coord.notify_processing_finished(Some(owner_session));
                                }

                                // ── Show result overlay if no new recording started ──
                                // After notify_processing_finished(), the coordinator is Idle.
                                // If a new recording started, is_active_use() will be true
                                // and we skip showing — the new recording's overlay takes
                                // priority, and the macOS notification provides feedback.
                                let should_show_result = {
                                    let coord =
                                        ah_for_router.try_state::<TranscriptionCoordinator>();
                                    !coord.map_or(false, |c| c.is_active_use())
                                };

                                if should_show_result {
                                    show_processing_overlay_with_mode(
                                        &ah_for_router,
                                        OverlayMode::Router,
                                    );
                                }

                                // ── Schedule delayed overlay hide ──
                                // Give the user 5 seconds to see the router result before
                                // the overlay hides. Re-check the session inside the delayed
                                // thread so A's teardown cannot flip the tray to Idle or hide
                                // the overlay while a newer recording (B) is active.
                                {
                                    let app_delayed = ah_for_router.clone();
                                    let hide_session = owner_session;
                                    std::thread::spawn(move || {
                                        std::thread::sleep(Duration::from_secs(5));
                                        if crate::overlay::current_overlay_session() != hide_session
                                        {
                                            debug!(
                                                "Skipping delayed hide/tray reset — newer recording session active"
                                            );
                                        } else {
                                            utils::hide_recording_overlay(&app_delayed);
                                            set_tray_state(&app_delayed, TrayIconState::Idle);
                                        }
                                    });
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

                            let event = RouterResultEvent {
                                success: false,
                                summary: None,
                                error: Some("No router_script_path configured. Set it in Settings to enable routing.".to_string()),
                                transcription_text: transcription_text.clone(),
                            };
                            let _ = ah.emit("router-result", &event);
                            send_macos_notification(
                                &ah,
                                "Handy Router",
                                "No router path configured. Check Settings.",
                            );

                            // Fall back to paste if no router configured
                            if !transcription_text.is_empty() {
                                let ah_for_paste = ah.clone();
                                let _ = ah.run_on_main_thread(move || {
                                    let _ = utils::paste(transcription_text, ah_for_paste.clone());
                                    utils::hide_recording_overlay(&ah_for_paste);
                                    set_tray_state(&ah_for_paste, TrayIconState::Idle);
                                });
                            } else {
                                utils::hide_recording_overlay(&ah);
                                set_tray_state(&ah, TrayIconState::Idle);
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

                        let settings = get_settings(&ah);

                        let failure_type = if err.to_string().contains("Model is not loaded")
                            || err.to_string().contains("failed to load")
                            || err.to_string().contains("Timed out waiting for model")
                        {
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

                        let fallback_models: Vec<String> = Vec::new();

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
                                    error!("Failed to save failed history entry: {}", save_err);
                                    None
                                }
                            }
                        } else {
                            None
                        };

                        if wav_saved {
                            if let Some(retry_queue) =
                                ah.try_state::<Arc<Mutex<TranscriptionRetryQueue>>>()
                            {
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
                                    error!(
                                        "Failed to add transcription to retry queue: {}",
                                        retry_err
                                    );
                                } else {
                                    info!("Added failed router transcription to retry queue for automatic retry");
                                }
                            }
                        }

                        send_macos_notification(
                            &ah,
                            "Handy Router",
                            "Transcription failed. Will retry automatically.",
                        );

                        utils::hide_recording_overlay(&ah);
                        set_tray_state(&ah, TrayIconState::Idle);

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

                utils::hide_recording_overlay(&ah);
                set_tray_state(&ah, TrayIconState::Idle);
                utils::hide_recording_overlay(&ah);
                set_tray_state(&ah, TrayIconState::Idle);
            }
        });

        // UI/feedback updates are intentionally dispatched AFTER spawning the stop work.
        // They are best-effort: if any of them hangs (e.g. AppleScript mute after
        // sleep/wake, or overlay JS evaluation), it must not prevent the recording
        // from stopping and the visualizer from eventually hiding.
        // ── KEY DIFFERENCE from TranscribeAction ──
        // Show routing-specific overlay during transcription + routing
        set_tray_state(app, TrayIconState::Transcribing);
        show_transcribing_overlay_with_mode(app, OverlayMode::Router);

        // Unmute in a detached thread so a stuck osascript/System Events cannot block
        // the stop path.
        std::thread::spawn(move || {
            rm_mute.remove_mute();
        });

        play_feedback_sound(app, SoundType::Stop);

        debug!(
            "TranscribeWithRouterAction::stop UI/feedback dispatched in {:?}",
            stop_time.elapsed()
        );
    }
}

const ROUTER_SUBPROCESS_TIMEOUT: Duration = Duration::from_secs(120);

const ROUTER_VERIFY_DELAY: Duration = Duration::from_secs(60);
const STARTUP_VERIFY_DELAY: Duration = Duration::from_secs(30);
static ROUTER_VERIFY_INFLIGHT: AtomicBool = AtomicBool::new(false);

/// Per-line stdout callback passed to `spawn_and_wait_with_drain`; invoked
/// from the reading thread as each line arrives, before process exit.
type OnRouterStdoutLine = Box<dyn FnMut(&str) + Send>;

/// Build the base router subprocess command with python bin, PATH prefix, env file, and cwd.
fn build_router_command(router_script: &str, env_file: Option<&str>) -> Command {
    let python_bin = "/Users/caffae/miniforge3/bin/python3";
    let mut cmd = Command::new(python_bin);
    cmd.arg(router_script);

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
            warn!("Router env file does not exist: {}", env_path.display());
        }
    }

    // Set working directory to the router script's directory
    if let Some(parent) = std::path::Path::new(router_script).parent() {
        cmd.current_dir(parent);
    }

    cmd
}

/// Spawn a child process with piped stdout/stderr, stream stdout line by line
/// with `BufReader::lines`, drain stderr concurrently, and wait up to
/// `ROUTER_SUBPROCESS_TIMEOUT`. `on_stdout_line`, when provided, is invoked
/// from the reading thread as each line arrives — before process exit — which
/// is what makes early "classified" notifications possible. On timeout the
/// child is killed and waited on.
/// Returns `(stdout, stderr)` on success; on failure returns an error string
/// including stderr contents when available.
fn spawn_and_wait_with_drain(
    cmd: &mut Command,
    mut on_stdout_line: Option<OnRouterStdoutLine>,
) -> Result<(String, String), String> {
    debug!("Running router command: {:?}", cmd);

    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to execute router: {}", e))?;

    // Stream stdout incrementally (lines) instead of read_to_end so events can
    // be acted on while the router is still running. Collected lines are
    // joined so the previous (whole document, stderr) return contract and the
    // downstream parse_router_json_output call are unchanged.
    let stdout_handle = child.stdout.take().map(|stdout| {
        std::thread::spawn(move || {
            let mut collected: Vec<String> = Vec::new();
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                let Ok(line) = line else {
                    debug!("Router stdout line was not valid UTF-8; skipping");
                    continue;
                };
                if let Some(on_line) = on_stdout_line.as_mut() {
                    on_line(&line);
                }
                collected.push(line);
            }
            collected
        })
    });
    let stderr_handle = child.stderr.take().map(|mut stderr| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = stderr.read_to_end(&mut buf);
            buf
        })
    });

    let status = match child.wait_timeout(ROUTER_SUBPROCESS_TIMEOUT) {
        Ok(Some(status)) => status,
        Ok(None) => {
            warn!(
                "Router subprocess timed out after {:?}, killing it",
                ROUTER_SUBPROCESS_TIMEOUT
            );
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "Router subprocess timed out after {:?}",
                ROUTER_SUBPROCESS_TIMEOUT
            ));
        }
        Err(e) => return Err(format!("Failed to wait on router: {}", e)),
    };

    let stdout_lines = stdout_handle.map_or_else(Vec::new, |h| h.join().unwrap_or_default());
    let stdout = stdout_lines.join("\n");
    let stderr_buf = stderr_handle.map_or_else(Vec::new, |h| h.join().unwrap_or_default());

    if !status.success() {
        let stderr = String::from_utf8_lossy(&stderr_buf);
        return Err(format!(
            "Router exited with code {}: {}",
            status.code().unwrap_or(-1),
            stderr.trim()
        ));
    }

    let stderr = String::from_utf8_lossy(&stderr_buf).to_string();
    Ok((stdout, stderr))
}

/// Detect a "write for me" fast-path trigger and return the cleaned user
/// request, or `None` if the transcription is not one of these.
///
/// Test-only convenience wrapper using the default trigger phrase list; the
/// router call site passes `AppSettings::trigger_phrases` to
/// [`write_for_me_prompt_with_phrases`] instead.
///
/// Two triggers are accepted by default:
/// - "Hey Assistant …" — everything after the trigger is the prompt.
/// - "Hey Minion, write for me …" — requires a write sub-trigger prefix.
#[cfg(test)]
fn write_for_me_prompt(text: &str) -> Option<String> {
    write_for_me_prompt_with_phrases(text, &crate::settings::default_trigger_phrases())
}

/// Settings-aware variant of [`write_for_me_prompt`]: `phrases` is
/// `AppSettings::trigger_phrases`, checked in order (first match wins).
///
/// Roles are kept per phrase:
/// - A phrase containing "minion" (case-insensitive) requires a write
///   sub-trigger ("write for me" / "write me" / "write to me") after the
///   trigger, otherwise it does not match.
/// - Every other phrase hands the whole remainder through as the prompt.
fn write_for_me_prompt_with_phrases(text: &str, phrases: &[String]) -> Option<String> {
    let t = text.trim();
    if t.is_empty() {
        return None;
    }
    let low = t.to_lowercase();

    for phrase in phrases {
        let phrase_trimmed = phrase.trim();
        if phrase_trimmed.is_empty() {
            continue;
        }
        let phrase_low = phrase_trimmed.to_lowercase();
        if !low.starts_with(&phrase_low) {
            continue;
        }

        // Offset in the ORIGINAL text just past the matched phrase, counted
        // by chars so Unicode case-folding can't desync the slice.
        let phrase_chars = phrase_trimmed.chars().count();
        let Some((offset, _)) = t.char_indices().nth(phrase_chars) else {
            return None;
        };
        let after_trigger =
            t[offset..].trim_start_matches(|c: char| c.is_whitespace() || ",.!?-:".contains(c));

        if phrase_low.contains("minion") {
            // "Hey minion"-style phrases gate on an explicit write sub-trigger.
            const PREFIXES: &[&str] = &["write for me", "write me", "write to me"];
            let after_low = after_trigger.to_lowercase();
            let mut rest: Option<String> = None;
            for prefix in PREFIXES {
                if after_low.starts_with(prefix) {
                    let r = after_trigger[prefix.len()..]
                        .trim_start_matches(|c: char| c.is_whitespace() || ",.!?-:".contains(c));
                    rest = Some(r.to_string());
                    break;
                }
            }
            let prompt = rest?.trim().to_string();
            return if prompt.is_empty() {
                None
            } else {
                Some(prompt)
            };
        }

        let prompt = after_trigger.trim().to_string();
        return if prompt.is_empty() {
            None
        } else {
            Some(prompt)
        };
    }
    None
}

#[cfg(test)]
mod write_for_me_tests {
    use super::write_for_me_prompt;

    #[test]
    fn detects_write_for_me() {
        let p = write_for_me_prompt("Hey Minion, write for me an email to my boss").unwrap();
        assert_eq!(p, "an email to my boss");
    }

    #[test]
    fn rejects_non_trigger() {
        assert!(write_for_me_prompt("write an email please").is_none());
    }

    #[test]
    fn rejects_generic_direct_order() {
        assert!(write_for_me_prompt("Hey Minion, restart kimaki").is_none());
    }

    #[test]
    fn hey_assistant_takes_rest_as_prompt() {
        let p = write_for_me_prompt("Hey Assistant, write a polite decline to a meeting invite")
            .unwrap();
        assert_eq!(p, "write a polite decline to a meeting invite");
    }

    #[test]
    fn hey_assistant_is_case_insensitive() {
        let p = write_for_me_prompt("hey assistant draft a tweet").unwrap();
        assert_eq!(p, "draft a tweet");
    }

    #[test]
    fn hey_assistant_without_request_is_none() {
        assert!(write_for_me_prompt("hey assistant").is_none());
        assert!(write_for_me_prompt("Hey Assistant!").is_none());
    }

    // ── write_for_me_prompt_with_phrases (settings-aware) ──

    use super::write_for_me_prompt_with_phrases;

    fn phrase_list(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn custom_non_minion_phrase_takes_whole_request() {
        let phrases = phrase_list(&["Hey Jarvis"]);
        // No write sub-trigger needed — the whole request is the prompt.
        assert_eq!(
            write_for_me_prompt_with_phrases("hey jarvis restart kimaki", &phrases).unwrap(),
            "restart kimaki"
        );
        assert_eq!(
            write_for_me_prompt_with_phrases("Hey Jarvis, write me a poem", &phrases).unwrap(),
            "write me a poem"
        );
        assert!(write_for_me_prompt_with_phrases("hey jarvis", &phrases).is_none());
    }

    #[test]
    fn custom_minion_phrase_requires_write_trigger() {
        let phrases = phrase_list(&["Hey Minion"]);
        assert_eq!(
            write_for_me_prompt_with_phrases("hey minion write me a poem", &phrases).unwrap(),
            "a poem"
        );
        assert!(write_for_me_prompt_with_phrases("hey minion restart kimaki", &phrases).is_none());
    }

    #[test]
    fn minion_role_is_keyed_on_phrase_containing_minion() {
        // "Minions" (plural) still carries the minion role.
        let phrases = phrase_list(&["Hey Minions"]);
        assert!(write_for_me_prompt_with_phrases("hey minions restart kimaki", &phrases).is_none());
        assert_eq!(
            write_for_me_prompt_with_phrases("hey minions write for me a haiku", &phrases).unwrap(),
            "a haiku"
        );
    }

    #[test]
    fn phrases_checked_in_configured_order() {
        // "Hey Assistant Pro" is a prefix-superset of "Hey Assistant"; the
        // first configured match wins, so the configured order changes the
        // extracted prompt for the same input.
        let broad_first = phrase_list(&["Hey Assistant Pro", "Hey Assistant"]);
        assert_eq!(
            write_for_me_prompt_with_phrases("hey assistant pro draft a tweet", &broad_first)
                .unwrap(),
            "draft a tweet"
        );
        let narrow_first = phrase_list(&["Hey Assistant", "Hey Assistant Pro"]);
        assert_eq!(
            write_for_me_prompt_with_phrases("hey assistant pro draft a tweet", &narrow_first)
                .unwrap(),
            "pro draft a tweet"
        );
    }

    #[test]
    fn empty_or_blank_phrase_list_is_none() {
        assert!(write_for_me_prompt_with_phrases("hey minion write me a poem", &[]).is_none());
        let blanks = phrase_list(&["", "   "]);
        assert!(write_for_me_prompt_with_phrases("hey minion write me a poem", &blanks).is_none());
    }

    #[test]
    fn default_list_preserves_legacy_behavior() {
        let default = crate::settings::default_trigger_phrases();
        assert_eq!(
            write_for_me_prompt_with_phrases(
                "Hey Minion, write for me an email to my boss",
                &default
            )
            .unwrap(),
            "an email to my boss"
        );
        assert!(write_for_me_prompt_with_phrases("Hey Minion, restart kimaki", &default).is_none());
        assert_eq!(
            write_for_me_prompt_with_phrases("hey assistant draft a tweet", &default).unwrap(),
            "draft a tweet"
        );
    }
}

/// One route classification reported early by boss_router.py arrives as a
/// single-line JSON document. Parse one stdout line into those routes, or
/// `None` for any other line (log noise, malformed JSON, the final document).
fn parse_classified_event_line(line: &str) -> Option<Vec<ClassifiedRoute>> {
    let trimmed = line.trim();
    if !trimmed.starts_with('{') {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(trimmed).ok()?;
    if value.get("event").and_then(|v| v.as_str()) != Some("classified") {
        return None;
    }
    let routes = value.get("routes")?.as_array()?;
    Some(
        routes
            .iter()
            .filter_map(|route| {
                let classification = route.get("classification")?.as_str()?.to_string();
                Some(ClassifiedRoute {
                    classification,
                    handler_name: route
                        .get("handler_name")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    emoji: route
                        .get("emoji")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string(),
                })
            })
            .collect(),
    )
}

/// Split raw router stdout lines into classified-route events and the
/// remaining lines of the final JSON document. Lines that parse as
/// `"event":"classified"` contribute their routes; all other lines (including
/// malformed or empty ones) are joined with `\n` and still go through
/// `parse_router_json_output` downstream, unchanged.
fn split_router_stdout(lines: &[String]) -> (Vec<ClassifiedRoute>, String) {
    let mut classified = Vec::new();
    let mut document = String::new();
    for line in lines {
        if let Some(routes) = parse_classified_event_line(line) {
            classified.extend(routes);
        } else {
            if !document.is_empty() {
                document.push('\n');
            }
            document.push_str(line);
        }
    }
    (classified, document)
}

/// Immediate notification when the router reports its classification, long
/// before the (possibly slow) handlers finish. Body joins routes as
/// "🫡 Direct Order", skipping the emoji when a route has none.
fn notify_routes_classified(app: &AppHandle, routes: &[ClassifiedRoute]) {
    if routes.is_empty() {
        return;
    }
    let body = routes
        .iter()
        .map(|route| {
            if route.emoji.is_empty() {
                route.handler_name.clone()
            } else {
                format!("{} {}", route.emoji, route.handler_name)
            }
        })
        .collect::<Vec<_>>()
        .join(" · ");
    send_macos_notification(app, "Handy Router", &body);
}

/// Run boss_router.py as a subprocess and return the parsed output.
/// Returns (summary_string, handler_data_vec) on success, or an error String.
/// Lines carrying an early `"event":"classified"` document trigger an
/// immediate "Handy Router" notification while the subprocess is still
/// running; the final document is parsed after exit as before.
fn run_router_subprocess(
    app: &AppHandle,
    router_script: &str,
    transcription_text: &str,
    datetime_str: &str,
    env_file: Option<&str>,
) -> Result<(Option<String>, Vec<RouterHandlerData>), String> {
    let mut cmd = build_router_command(router_script, env_file);
    cmd.arg("--text")
        .arg(transcription_text)
        .arg("--datetime")
        .arg(datetime_str)
        .arg("--json")
        .arg("--handy");

    let app_for_events = app.clone();
    let (stdout, _stderr) = spawn_and_wait_with_drain(
        &mut cmd,
        Some(Box::new(move |line: &str| {
            if let Some(routes) = parse_classified_event_line(line) {
                notify_routes_classified(&app_for_events, &routes);
            }
        })),
    )?;

    // Classified events were already notified on arrival; the remaining lines
    // form the final document for the existing JSON parser.
    let lines: Vec<String> = stdout.lines().map(str::to_string).collect();
    let (_classified, final_document) = split_router_stdout(&lines);
    let (handler_summary, handler_data) = match parse_router_json_output(&final_document) {
        Some(output) => (Some(output.summary), output.handler_data),
        None => (None, Vec::new()),
    };
    Ok((handler_summary, handler_data))
}

/// Run the router in pending-entry verification mode.
/// Returns the subprocess stdout on success, or an error String.
fn run_router_verify(router_script: &str, env_file: Option<&str>) -> Result<String, String> {
    let mut cmd = build_router_command(router_script, env_file);
    cmd.arg("--verify-pending").arg("--json");

    let (stdout, stderr) = spawn_and_wait_with_drain(&mut cmd, None)?;
    if !stderr.is_empty() {
        let preview: String = stderr.chars().take(500).collect();
        debug!("router verify stderr (first 500 chars): {}", preview);
    }
    Ok(stdout)
}

/// Try to claim the single-flight verify slot. Returns `true` if acquired.
fn try_begin_verify() -> bool {
    ROUTER_VERIFY_INFLIGHT
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_ok()
}

fn end_verify() {
    ROUTER_VERIFY_INFLIGHT.store(false, Ordering::SeqCst);
}

/// Ensures `end_verify()` runs when the guard drops, covering every exit path.
struct VerifyInFlightGuard;

impl Drop for VerifyInFlightGuard {
    fn drop(&mut self) {
        end_verify();
    }
}

/// Merge new verification handler rows into an existing routing_result JSON string.
/// Only rows with status "🔁" (re-added) or "❌" (failed) are appended; "✅"
/// delivered rows are deliberately dropped to avoid history noise.
fn append_verify_rows_to_routing_result(
    existing_json: Option<&str>,
    new_handler_data: &[RouterHandlerData],
) -> String {
    let mut existing: Vec<RouterHandlerData> = existing_json
        .and_then(|s| serde_json::from_str::<Vec<RouterHandlerData>>(s).ok())
        .unwrap_or_default();

    for row in new_handler_data {
        if row.status == "🔁" || row.status == "❌" {
            existing.push(row.clone());
        }
    }

    serde_json::to_string(&existing).unwrap_or_else(|_| "[]".to_string())
}

/// Body of the delayed pending-entry verification thread. Claims the single-
/// flight slot, spawns one inner thread that sleeps for `ROUTER_VERIFY_DELAY`,
/// runs the router in verify mode, and surfaces any re-added/failed rows in
/// history and macOS notifications.
fn verify_thread_body(
    app: AppHandle,
    router_script: String,
    env_file: Option<String>,
    hm_and_entry: Option<(Arc<HistoryManager>, i64)>,
    existing_routing_json: Option<String>,
) {
    if !try_begin_verify() {
        debug!("verify already in flight, skipping");
        return;
    }
    let guard = VerifyInFlightGuard;

    std::thread::spawn(move || {
        let _guard = guard;

        std::thread::sleep(ROUTER_VERIFY_DELAY);

        match run_router_verify(&router_script, env_file.as_deref()) {
            Err(e) => {
                error!("Pending-entry verification failed: {}", e);
            }
            Ok(stdout) => {
                let Some(output) = parse_router_json_output(&stdout) else {
                    debug!("no verified pending entries");
                    return;
                };

                if output.handler_data.is_empty() {
                    debug!("no verified pending entries");
                    return;
                }

                let any_readded = output.handler_data.iter().any(|d| d.status == "🔁");
                let any_failed = output.handler_data.iter().any(|d| d.status == "❌");

                if any_readded || any_failed {
                    if let (Some((ref hm, entry_id)), Some(ref existing)) =
                        (&hm_and_entry, &existing_routing_json)
                    {
                        let merged = append_verify_rows_to_routing_result(
                            Some(existing.as_str()),
                            &output.handler_data,
                        );
                        if let Err(e) = hm.update_routing_result(*entry_id, Some(merged)) {
                            error!(
                                "Failed to update routing result with verification rows: {}",
                                e
                            );
                        }
                    }
                } else {
                    debug!(
                        "All {} pending handlers delivered; skipping routing-result DB write",
                        output.handler_data.len()
                    );
                }

                if any_readded {
                    send_macos_notification(
                        &app,
                        "Handy Router",
                        "Journal entry re-added after conflict",
                    );
                }
                if any_failed {
                    send_macos_notification(
                        &app,
                        "Handy Router",
                        "Journal verify failed — see history",
                    );
                }
                if !any_readded && !any_failed {
                    debug!("All pending journal entries verified as delivered");
                }

                let summary = output
                    .handler_data
                    .iter()
                    .map(|d| format!("{} {}", d.status, d.handler))
                    .collect::<Vec<_>>()
                    .join(" | ");
                info!("Pending-entry verification completed: {}", summary);
            }
        }
    });
}

/// Spawn a non-blocking pending-entry verification for a recent routing.
fn run_pending_entry_verify(
    app: AppHandle,
    router_script: String,
    env_file: Option<String>,
    hm_and_entry: Option<(Arc<HistoryManager>, i64)>,
    existing_routing_json: Option<String>,
) {
    // The caller (router result thread) is already backgrounded; verify_thread_body
    // returns immediately after spawning the single sleeping/verify thread.
    verify_thread_body(
        app,
        router_script,
        env_file,
        hm_and_entry,
        existing_routing_json,
    );
}

/// Schedule a one-time startup sweep to verify any pending entries from
/// previous runs or races. Runs after `STARTUP_VERIFY_DELAY`.
pub(crate) fn schedule_startup_pending_entry_verify(app: &AppHandle) {
    let settings = get_settings(app);
    let Some(router_script) = settings.router_script_path.clone() else {
        debug!("No router_script_path configured; skipping startup pending-entry verification");
        return;
    };
    let env_file = settings.router_env_file.clone();

    let app_handle = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(STARTUP_VERIFY_DELAY);
        verify_thread_body(app_handle, router_script, env_file, None, None);
    });
}

/// Parsed output returned by the boss_router subprocess.
struct RouterOutput {
    /// Human-readable summary string.
    summary: String,
    /// Structured handler results for persistence and verification.
    handler_data: Vec<RouterHandlerData>,
}

/// Parse the JSON output from boss_router.py --json.
/// Returns structured handler data and a human-readable summary.
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

/// Send a macOS notification via the tauri notification plugin.
/// Used for router success/failure feedback since the overlay is already hidden.
/// Posting through the plugin keeps the notification attributed to the Handy
/// process itself (app icon) instead of osascript's Script Editor.
pub(crate) fn send_macos_notification(app: &AppHandle, title: &str, message: &str) {
    match app
        .notification()
        .builder()
        .title(title)
        .body(message)
        .show()
    {
        Ok(()) => debug!("macOS notification sent: {} - {}", title, message),
        Err(e) => warn!("Failed to send macOS notification: {}", e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_split_router_stdout_classified_and_final() {
        let lines = vec![
            r#"{"event":"classified","routes":[{"classification":"direct_order","handler_name":"Direct Order","emoji":"🫡"}]}"#.to_string(),
            r#"{"handlers": [{"status": "✅", "handler": "Direct Order", "classification": "direct_order", "file_path": null}]}"#.to_string(),
        ];
        let (classified, document) = split_router_stdout(&lines);
        assert_eq!(classified.len(), 1);
        assert_eq!(
            classified[0],
            ClassifiedRoute {
                classification: "direct_order".to_string(),
                handler_name: "Direct Order".to_string(),
                emoji: "🫡".to_string(),
            }
        );
        // The classified line is excluded from the final document.
        assert_eq!(document, lines[1]);
        // The final document still flows through the existing parser unchanged.
        let output = parse_router_json_output(&document).expect("final document parses");
        assert_eq!(output.handler_data.len(), 1);
        assert_eq!(output.handler_data[0].classification, "direct_order");
    }

    #[test]
    fn test_split_router_stdout_no_classified_lines() {
        // Today's stdout (no classified events) must pass through unchanged.
        let lines = vec![
            "log line".to_string(),
            r#"{"handlers": [{"status": "✅", "handler": "Daily", "classification": "diary_entry", "file_path": "/path/to/note.md"}]}"#.to_string(),
        ];
        let (classified, document) = split_router_stdout(&lines);
        assert!(classified.is_empty());
        assert_eq!(document, lines.join("\n"));
        assert!(parse_router_json_output(&document).is_some());
    }

    #[test]
    fn test_split_router_stdout_garbage_ignored() {
        // Malformed/empty lines and non-classified JSON are ignored for
        // classification purposes but kept in the final document.
        let lines = vec![
            "not json at all".to_string(),
            "".to_string(),
            "{broken json".to_string(),
            r#"{"event":"other","routes":[1,2]}"#.to_string(),
        ];
        let (classified, document) = split_router_stdout(&lines);
        assert!(classified.is_empty());
        assert_eq!(document, lines.join("\n"));
    }

    #[test]
    fn test_parse_verify_mode_output() {
        let stdout = "Checking pending entries...\n{\"handlers\": [{\"status\": \"🔁\", \"handler\": \"Daily\", \"classification\": \"diary_entry_verify\", \"file_path\": \"/path/to/note.md\"}, {\"status\": \"❌\", \"handler\": \"Daily\", \"classification\": \"diary_entry_verify\", \"file_path\": \"/path/to/note2.md\"}]}";
        let output = parse_router_json_output(stdout).expect("should parse verify output");
        assert_eq!(output.handler_data.len(), 2);
        assert_eq!(output.handler_data[0].classification, "diary_entry_verify");
        assert_eq!(output.handler_data[0].status, "🔁");
        assert!(output.handler_data[0].file_path.is_some());
        assert_eq!(output.handler_data[1].status, "❌");
        assert!(output.summary.contains("🔁"));
        assert!(output.summary.contains("❌"));
    }

    #[test]
    fn test_parse_verify_mode_empty() {
        let stdout = "log line\n{\"handlers\": []}";
        assert!(parse_router_json_output(stdout).is_none());
    }

    #[test]
    fn test_parse_verify_mode_noise() {
        let stdout = "log line 1\nanother log\n{\"handlers\": [{\"status\": \"✅\", \"handler\": \"Daily\", \"classification\": \"diary_entry_verify\", \"file_path\": \"/path/to/note.md\"}]}";
        let output = parse_router_json_output(stdout).expect("should parse despite noise");
        assert_eq!(output.handler_data.len(), 1);
        assert_eq!(output.handler_data[0].classification, "diary_entry_verify");
    }

    #[test]
    fn test_append_verify_rows_preserves_existing() {
        let existing = r#"[{"status": "✅", "handler": "Daily", "classification": "diary_entry", "file_path": "/path/to/note.md"}]"#;
        let new_rows = vec![
            RouterHandlerData {
                status: "✅".to_string(),
                handler: "Daily".to_string(),
                classification: "diary_entry_verify".to_string(),
                file_path: Some("/path/to/note.md".to_string()),
            },
            RouterHandlerData {
                status: "🔁".to_string(),
                handler: "Daily".to_string(),
                classification: "diary_entry_verify".to_string(),
                file_path: Some("/path/to/note.md".to_string()),
            },
            RouterHandlerData {
                status: "❌".to_string(),
                handler: "Daily".to_string(),
                classification: "diary_entry_verify".to_string(),
                file_path: Some("/path/to/note2.md".to_string()),
            },
        ];
        let merged = append_verify_rows_to_routing_result(Some(existing), &new_rows);
        let parsed: Vec<RouterHandlerData> = serde_json::from_str(&merged).unwrap();
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0].classification, "diary_entry");
        assert_eq!(parsed[1].status, "🔁");
        assert_eq!(parsed[2].status, "❌");
    }

    #[test]
    fn test_append_verify_rows_none_input() {
        let new_rows = vec![RouterHandlerData {
            status: "🔁".to_string(),
            handler: "Daily".to_string(),
            classification: "diary_entry_verify".to_string(),
            file_path: Some("/path/to/note.md".to_string()),
        }];
        let merged = append_verify_rows_to_routing_result(None, &new_rows);
        let parsed: Vec<RouterHandlerData> = serde_json::from_str(&merged).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].status, "🔁");
    }

    #[test]
    fn test_append_verify_rows_unparseable() {
        let new_rows = vec![RouterHandlerData {
            status: "🔁".to_string(),
            handler: "Daily".to_string(),
            classification: "diary_entry_verify".to_string(),
            file_path: Some("/path/to/note.md".to_string()),
        }];
        let merged = append_verify_rows_to_routing_result(Some("garbage"), &new_rows);
        let parsed: Vec<RouterHandlerData> = serde_json::from_str(&merged).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].status, "🔁");
    }

    #[test]
    fn test_append_verify_rows_all_delivered() {
        let existing = r#"[{"status": "✅", "handler": "Daily", "classification": "diary_entry", "file_path": "/path/to/note.md"}]"#;
        let new_rows = vec![RouterHandlerData {
            status: "✅".to_string(),
            handler: "Daily".to_string(),
            classification: "diary_entry_verify".to_string(),
            file_path: Some("/path/to/note.md".to_string()),
        }];
        let merged = append_verify_rows_to_routing_result(Some(existing), &new_rows);
        let parsed: Vec<RouterHandlerData> = serde_json::from_str(&merged).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].classification, "diary_entry");
    }
}
