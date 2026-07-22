// TranscribeAction: recording start/stop for standard transcription
// (with and without post-processing). Includes session tracking, audio
// feedback, WAV saving, error handling, and retry queue integration.

use crate::audio_feedback::{play_feedback_sound, play_feedback_sound_blocking, SoundType};
use crate::audio_toolkit::{is_microphone_access_denied, is_no_input_device_error};
use crate::errors::AppError;
use crate::logging::{self, AppEvent, SessionId};
use crate::managers::audio::AudioRecordingManager;
use crate::managers::history::HistoryManager;
use crate::managers::transcription::TranscriptionManager;
use crate::managers::transcription_retry::{TranscriptionFailure, TranscriptionRetryQueue};
use crate::session::SessionTracker;
use crate::settings::get_settings;
use crate::shortcut;
use crate::tray::{change_tray_icon, TrayIconState};
use crate::utils::{
    self, show_processing_overlay, show_recording_overlay, show_transcribing_overlay,
};
use crate::TranscriptionCoordinator;
use log::{debug, error, info, warn};
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};

use super::post_process::process_transcription_output;

/// Emitted when recording fails to start.
#[derive(Clone, serde::Serialize)]
pub(super) struct RecordingErrorEvent {
    pub error_type: String,
    pub detail: Option<String>,
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
pub(crate) struct FinishGuard(pub AppHandle);
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

// Transcribe Action
pub(super) struct TranscribeAction {
    pub post_process: bool,
}

impl super::ShortcutAction for TranscribeAction {
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
            utils::hide_recording_overlay(app);
            change_tray_icon(app, TrayIconState::Idle);
            return;
        };
        let rm = Arc::clone(&rm);
        let Some(tm) = app.try_state::<Arc<Mutex<TranscriptionManager>>>() else {
            warn!("TranscriptionManager not available, cannot stop transcription");
            utils::hide_recording_overlay(app);
            change_tray_icon(app, TrayIconState::Idle);
            return;
        };
        let tm = Arc::clone(&tm);
        let Some(hm) = app.try_state::<Arc<HistoryManager>>() else {
            warn!("HistoryManager not available, cannot save recording");
            utils::hide_recording_overlay(app);
            change_tray_icon(app, TrayIconState::Idle);
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
                    // Use a bounded lock timeout to avoid indefinite blocking when a streaming
                    // transcription is in-flight holding the TM lock (1-5s of GPU work).
                    let transcription_result = match tm.try_lock_for(Duration::from_secs(10)) {
                        Some(guard) => {
                            // Clear the streaming cancel flag before the final transcription.
                            guard.clear_streaming_cancel();
                            guard.transcribe(samples)
                        }
                        None => {
                            warn!("Timed out waiting for TranscriptionManager lock after 10s — aborting transcription");
                            Err(AppError::TranscriptionBusy)
                        }
                    };

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
                                let duration_secs = sample_count as f32 / 16000.0;
                                if rm.usb_watchdog.on_silent_transcription(duration_secs) {
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
                                            error!("Failed to paste transcription (clipboard fallback also failed): {}", e);

                                            // ── Structured event: paste failed ──
                                            if let (Some(ref s), Some(tracker)) =
                                                (&sid, ah_clone.try_state::<Arc<SessionTracker>>())
                                            {
                                                tracker.fail_session(
                                                    s,
                                                    &format!("Paste failed: {}", e),
                                                );
                                            }

                                            // Both paste and clipboard fallback failed.
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
                                        info!(
                                            "Saved history entry {} for failed transcription",
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

                            // Add to retry queue for automatic retry
                            if wav_saved {
                                if let Some(retry_queue) =
                                    ah.try_state::<Arc<Mutex<TranscriptionRetryQueue>>>()
                                {
                                    let wav_path = hm.recordings_dir().join(&file_name);
                                    let model_id = {
                                        let settings = get_settings(&ah);
                                        settings.selected_model.clone()
                                    };

                                    if let Err(retry_err) =
                                        retry_queue.lock().add_failed_transcription(
                                            wav_path,
                                            model_id,
                                            fallback_models,
                                            failure_type,
                                            post_process,
                                            None, // post_process_prompt
                                            history_entry_id,
                                        )
                                    {
                                        error!(
                                            "Failed to add transcription to retry queue: {}",
                                            retry_err
                                        );
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
