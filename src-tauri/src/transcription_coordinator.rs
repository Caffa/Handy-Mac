use crate::actions::ACTION_MAP;
use crate::managers::audio::AudioRecordingManager;
use log::{debug, error, info, warn};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};

const DEBOUNCE: Duration = Duration::from_millis(30);

/// Maximum time the coordinator will stay in `Processing` before
/// auto-resetting to `Idle`. Prevents the app from becoming permanently
/// unresponsive when the async transcription pipeline hangs (e.g. dead
/// USB microphone, model load timeout, or engine panic that didn't fire
/// the `FinishGuard`).
const PROCESSING_TIMEOUT: Duration = Duration::from_secs(30);

/// Commands processed sequentially by the coordinator thread.
enum Command {
    Input {
        binding_id: String,
        hotkey_string: String,
        is_pressed: bool,
        push_to_talk: bool,
    },
    Cancel {
        recording_was_active: bool,
    },
    ProcessingFinished,
    /// Internal: the processing-timeout timer fired.
    ProcessingTimeout,
}

/// Pipeline lifecycle, owned exclusively by the coordinator thread.
enum Stage {
    Idle,
    Recording(String), // binding_id
    Processing { since: Instant },
}

impl Stage {
    /// Returns true if this stage represents active use (Recording or Processing)
    fn is_active(&self) -> bool {
        !matches!(self, Stage::Idle)
    }
}

/// Helper to update stage and sync the active_use flag
fn set_stage(stage: &mut Stage, new_stage: Stage, active_use: &AtomicBool) {
    *stage = new_stage;
    active_use.store(stage.is_active(), Ordering::SeqCst);
}

/// Serialises all transcription lifecycle events through a single thread
/// to eliminate race conditions between keyboard shortcuts, signals, and
/// the async transcribe-paste pipeline.
pub struct TranscriptionCoordinator {
    tx: Sender<Command>,
    /// Shared flag indicating whether Handy is in active use (Recording or Processing stage).
    /// This is used by the CLI `--is-active-use` flag to determine if the app is busy.
    active_use: Arc<AtomicBool>,
}

pub fn is_transcribe_binding(id: &str) -> bool {
    id == "transcribe" || id == "transcribe_with_post_process" || id == "transcribe_with_router"
}

impl TranscriptionCoordinator {
    pub fn new(app: AppHandle) -> Self {
        let (tx, rx) = mpsc::channel();
        let active_use = Arc::new(AtomicBool::new(false));
        let active_use_clone = Arc::clone(&active_use);
        info!("Starting transcription coordinator thread");

        thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut stage = Stage::Idle;
                // Sync active_use flag with initial stage
                active_use_clone.store(false, Ordering::SeqCst);
                let mut last_press: Option<Instant> = None;

                loop {
                    // Calculate recv timeout: if in Processing, wake up to check the timeout.
                    let timeout = match &stage {
                        Stage::Processing { since } => {
                            let elapsed = since.elapsed();
                            if elapsed >= PROCESSING_TIMEOUT {
                                // Already past the deadline — reset immediately.
                                warn!(
                                    "Processing stage exceeded {:?} timeout, auto-resetting to Idle",
                                    PROCESSING_TIMEOUT
                                );
                                set_stage(&mut stage, Stage::Idle, &active_use_clone);
                                continue; // re-evaluate in next iteration
                            }
                            Some(PROCESSING_TIMEOUT - elapsed)
                        }
                        _ => None,
                    };

                    let cmd = match timeout {
                        Some(dur) => {
                            match rx.recv_timeout(dur) {
                                Ok(c) => c,
                                Err(mpsc::RecvTimeoutError::Timeout) => Command::ProcessingTimeout,
                                Err(mpsc::RecvTimeoutError::Disconnected) => {
                                    info!("Transcription coordinator channel disconnected, shutting down");
                                    break;
                                }
                            }
                        }
                        None => {
                            match rx.recv() {
                                Ok(c) => c,
                                Err(_) => {
                                    info!("Transcription coordinator channel disconnected, shutting down");
                                    break;
                                }
                            }
                        }
                    };

                    match cmd {
                        Command::Input {
                            binding_id,
                            hotkey_string,
                            is_pressed,
                            push_to_talk,
                        } => {
                            // Debounce rapid-fire press events (key repeat / double-tap).
                            // Releases always pass through for push-to-talk.
                            if is_pressed {
                                let now = Instant::now();
                                if last_press.map_or(false, |t| now.duration_since(t) < DEBOUNCE) {
                                    debug!("Debounced press for '{binding_id}'");
                                    continue;
                                }
                                last_press = Some(now);
                            }

                            if push_to_talk {
                                if is_pressed && matches!(stage, Stage::Idle) {
                                    info!("Coordinator: starting recording for '{}'", binding_id);
                                    start(&app, &mut stage, &binding_id, &hotkey_string, &active_use_clone);
                                } else if !is_pressed
                                    && matches!(&stage, Stage::Recording(id) if id == &binding_id)
                                {
                                    info!("Coordinator: stopping recording for '{}'", binding_id);
                                    stop(&app, &mut stage, &binding_id, &hotkey_string, &active_use_clone);
                                }
                            } else if is_pressed {
                                match &stage {
                                    Stage::Idle => {
                                        info!(
                                            "Coordinator: starting recording for '{}'",
                                            binding_id
                                        );
                                        start(&app, &mut stage, &binding_id, &hotkey_string, &active_use_clone);
                                    }
                                    Stage::Recording(id) if id == &binding_id => {
                                        info!(
                                            "Coordinator: stopping recording for '{}'",
                                            binding_id
                                        );
                                        stop(&app, &mut stage, &binding_id, &hotkey_string, &active_use_clone);
                                    }
                                    _ => {
                                        debug!("Ignoring press for '{binding_id}': pipeline busy")
                                    }
                                }
                            }
                        }
                        Command::Cancel {
                            recording_was_active,
                        } => {
                            info!(
                                "Coordinator: cancel received, recording_was_active={}",
                                recording_was_active
                            );
                            if recording_was_active || matches!(stage, Stage::Recording(_)) {
                                set_stage(&mut stage, Stage::Idle, &active_use_clone);
                                info!("Coordinator: cancelled, reset to Idle");
                            } else if matches!(stage, Stage::Processing { .. }) {
                                // Allow cancel during processing too — if the
                                // transcription pipeline hangs, the user needs a
                                // way to unstick the app. The FinishGuard will
                                // still fire when (if) the pipeline completes.
                                warn!("Cancelling stuck processing stage");
                                set_stage(&mut stage, Stage::Idle, &active_use_clone);
                            }
                        }
                        Command::ProcessingFinished => {
                            if matches!(stage, Stage::Processing { .. }) {
                                info!("Coordinator: processing finished, reset to Idle");
                                set_stage(&mut stage, Stage::Idle, &active_use_clone);
                            }
                        }
                        Command::ProcessingTimeout => {
                            // Handled above in the timeout calculation, but
                            // also reachable if the timer fires exactly. Reset
                            // to Idle so the pipeline can be triggered again.
                            if matches!(stage, Stage::Processing { .. }) {
                                warn!(
                                    "Processing stage timed out after {:?}, auto-resetting to Idle",
                                    PROCESSING_TIMEOUT
                                );
                                set_stage(&mut stage, Stage::Idle, &active_use_clone);
                            }
                        }
                    }
                }
                debug!("Transcription coordinator exited");
            }));
            if let Err(e) = result {
                error!("Transcription coordinator panicked: {:?}", e);
                error!("This is likely the cause of sudden app crashes!");
            }
        });

        Self { tx, active_use }
    }

    /// Returns true if Handy is in active use (recording or processing).
    /// This is used by the CLI `--is-active-use` flag to determine if
    /// the app is busy and should not be quit.
    pub fn is_active_use(&self) -> bool {
        self.active_use.load(Ordering::SeqCst)
    }

    /// Returns a clone of the active_use Arc for external querying
    pub fn active_use_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.active_use)
    }

    /// Send a keyboard/signal input event for a transcribe binding.
    /// For signal-based toggles, use `is_pressed: true` and `push_to_talk: false`.
    pub fn send_input(
        &self,
        binding_id: &str,
        hotkey_string: &str,
        is_pressed: bool,
        push_to_talk: bool,
    ) {
        if self
            .tx
            .send(Command::Input {
                binding_id: binding_id.to_string(),
                hotkey_string: hotkey_string.to_string(),
                is_pressed,
                push_to_talk,
            })
            .is_err()
        {
            warn!("Transcription coordinator channel closed");
        }
    }

    pub fn notify_cancel(&self, recording_was_active: bool) {
        if self
            .tx
            .send(Command::Cancel {
                recording_was_active,
            })
            .is_err()
        {
            warn!("Transcription coordinator channel closed");
        }
    }

    pub fn notify_processing_finished(&self) {
        if self.tx.send(Command::ProcessingFinished).is_err() {
            warn!("Transcription coordinator channel closed");
        }
    }
}

fn start(
    app: &AppHandle,
    stage: &mut Stage,
    binding_id: &str,
    hotkey_string: &str,
    active_use: &AtomicBool,
) {
    let Some(action) = ACTION_MAP.get(binding_id) else {
        warn!("No action in ACTION_MAP for '{binding_id}'");
        return;
    };
    action.start(app, binding_id, hotkey_string);
    if app
        .try_state::<Arc<Mutex<AudioRecordingManager>>>()
        .map_or(false, |a| a.lock().unwrap().is_recording())
    {
        set_stage(stage, Stage::Recording(binding_id.to_string()), active_use);
    } else {
        debug!("Start for '{binding_id}' did not begin recording; staying idle");
    }
}

fn stop(
    app: &AppHandle,
    stage: &mut Stage,
    binding_id: &str,
    hotkey_string: &str,
    active_use: &AtomicBool,
) {
    let Some(action) = ACTION_MAP.get(binding_id) else {
        warn!("No action in ACTION_MAP for '{binding_id}'");
        return;
    };
    action.stop(app, binding_id, hotkey_string);
    set_stage(
        stage,
        Stage::Processing {
            since: Instant::now(),
        },
        active_use,
    );
}
