// TranscribeWithRouterAction: records speech → transcribes → routes text
// to boss_router.py subprocess. Includes confirmation flow, macOS notifications,
// and retry queue integration for failed router transcriptions.

use crate::audio_feedback::{play_feedback_sound, play_feedback_sound_blocking, SoundType};
use crate::audio_toolkit::{is_microphone_access_denied, is_no_input_device_error};
use crate::errors::AppError;
use crate::logging::{self, AppEvent, SessionId};
use crate::managers::audio::AudioRecordingManager;
use crate::managers::history::HistoryManager;
use crate::managers::transcription::TranscriptionManager;
use crate::managers::transcription_retry::{TranscriptionFailure, TranscriptionRetryQueue};
use crate::overlay::OverlayMode;
use crate::session::SessionTracker;
use crate::settings::get_settings;
use crate::shortcut;
use crate::tray::{change_tray_icon, TrayIconState};
use crate::utils::{
    self, show_processing_overlay_with_mode, show_recording_overlay_with_mode,
    show_transcribing_overlay_with_mode,
};
use crate::TranscriptionCoordinator;
use log::{debug, error, info, warn};
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};

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
                    super::transcribe::RecordingErrorEvent {
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
        // Unregister cancel shortcut
        shortcut::unregister_cancel_shortcut(app);

        // Cancel any in-progress streaming transcription to prevent wasted work.
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
            utils::hide_recording_overlay(app);
            change_tray_icon(app, TrayIconState::Idle);
            return;
        };
        let rm = Arc::clone(&rm);
        let Some(tm) = app.try_state::<Arc<Mutex<TranscriptionManager>>>() else {
            warn!("TranscriptionManager not available, cannot stop router transcription");
            utils::hide_recording_overlay(app);
            change_tray_icon(app, TrayIconState::Idle);
            return;
        };
        let tm = Arc::clone(&tm);
        let Some(hm) = app.try_state::<Arc<HistoryManager>>() else {
            warn!("HistoryManager not available, cannot save router recording");
            utils::hide_recording_overlay(app);
            change_tray_icon(app, TrayIconState::Idle);
            return;
        };
        let hm = Arc::clone(&hm);

        let sid: Option<SessionId> = app
            .try_state::<Arc<SessionTracker>>()
            .and_then(|t| t.current_session_id());

        // ── KEY DIFFERENCE from TranscribeAction ──
        // Show routing-specific overlay during transcription + routing
        change_tray_icon(app, TrayIconState::Transcribing);
        show_transcribing_overlay_with_mode(app, OverlayMode::Router);

        // Unmute before playing audio feedback
        rm.remove_mute();
        play_feedback_sound(app, SoundType::Stop);

        let binding_id = binding_id.to_string(); // Clone for async task

        tauri::async_runtime::spawn(async move {
            let mut finish_guard = Some(super::transcribe::FinishGuard(ah.clone()));
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
                    utils::hide_recording_overlay(&ah);
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

                // Transcribe
                let transcription_time = Instant::now();
                let transcription_result = match tm.try_lock_for(Duration::from_secs(10)) {
                    Some(guard) => {
                        guard.clear_streaming_cancel();
                        guard.transcribe(samples)
                    }
                    None => {
                        warn!("Timed out waiting for TranscriptionManager lock after 10s — aborting transcription");
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
                        if transcription_text.is_empty() {
                            warn!("Router transcription returned empty text - skipping routing");

                            let duration_secs = sample_count as f32 / 16000.0;
                            if rm.usb_watchdog.on_silent_transcription(duration_secs) {
                                if let Err(e) = rm.restart_microphone_if_needed() {
                                    error!(
                                        "Failed to restart microphone after silent transcription USB cycle: {}",
                                        e
                                    );
                                }
                            }

                            send_macos_notification("Handy Router", "No speech detected");

                            utils::hide_recording_overlay(&ah);
                            change_tray_icon(&ah, TrayIconState::Idle);

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
                            coordinator.set_confirming(&ah, transcription_text.clone(), Some("transcribe_with_router".to_string()));
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

                        // ── Show "Filing…" overlay while routing ──
                        show_processing_overlay_with_mode(&ah, OverlayMode::Router);

                        // ── Transition coordinator from Confirming to Processing ──
                        // This ensures the frontend knows we're still in a router flow
                        // during the filing/routing phase, so it shows the blue
                        // visualizer and "Filing…" text instead of the default
                        // transcribe-mode visualizer and "Processing…" text.
                        if let Some(coordinator) = ah.try_state::<TranscriptionCoordinator>() {
                            coordinator.set_processing_with_binding(&ah, Some("transcribe_with_router".to_string()));
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

                                        // NOTE: Overlay hide is now handled AFTER notify_processing_finished()
                                        // below, with a 5-second delay so the user can see the result.
                                        info!("Router completion: success, scheduling delayed hide");
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
                                            "Handy Router Error",
                                            &error_display,
                                        );

                                        // NOTE: Overlay hide is now handled AFTER notify_processing_finished()
                                        // below, with a 5-second delay so the user can see the result.
                                        info!("Router completion: failure, scheduling delayed hide");
                                    }
                                }

                                // ── IMMEDIATELY free the coordinator ──
                                // This allows the user to start a new recording right away,
                                // even while the router result is still being displayed.
                                // Previously, notify_processing_finished() was called at the
                                // end of the thread, keeping the coordinator in Processing
                                // (and rejecting new recordings) for the entire subprocess
                                // duration.
                                if let Some(coord) = ah_for_router.try_state::<TranscriptionCoordinator>() {
                                    coord.notify_processing_finished();
                                }

                                // ── Schedule delayed overlay hide ──
                                // Give the user 5 seconds to see the router result before
                                // the overlay hides. hide_recording_overlay() has built-in
                                // session guards that prevent hiding if a new recording
                                // starts during the delay.
                                {
                                    let app_delayed = ah_for_router.clone();
                                    std::thread::spawn(move || {
                                        std::thread::sleep(Duration::from_secs(5));
                                        utils::hide_recording_overlay(&app_delayed);
                                        change_tray_icon(&app_delayed, TrayIconState::Idle);
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
                            "Handy Router",
                            "Transcription failed. Will retry automatically.",
                        );

                        utils::hide_recording_overlay(&ah);
                        change_tray_icon(&ah, TrayIconState::Idle);

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
                change_tray_icon(&ah, TrayIconState::Idle);
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

/// Send a macOS notification via osascript.
/// Used for router success/failure feedback since the overlay is already hidden.
fn send_macos_notification(title: &str, message: &str) {
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
