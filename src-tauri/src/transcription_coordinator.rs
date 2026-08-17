use crate::actions::ACTION_MAP;
use crate::managers::audio::AudioRecordingManager;
use log::{debug, error, info, warn};
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, Runtime};

/// Non-blocking cancel signal for the coordinator thread.
///
/// Uses an `AtomicBool` so the sender (cancel hotkey handler) never blocks,
/// even when the GPU transcription callback holds the TM mutex. The coordinator
/// thread polls this flag on every loop iteration and immediately transitions
/// to Idle when a cancel is detected.
#[derive(Clone)]
pub struct CancelSignal {
    flag: Arc<AtomicBool>,
}

impl CancelSignal {
    pub fn new() -> Self {
        Self {
            flag: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Non-blocking cancel signal. Returns immediately.
    pub fn send_cancel(&self) {
        let t = Instant::now();
        self.flag.store(true, Ordering::SeqCst);
        let dt = t.elapsed();
        if dt > Duration::from_millis(100) {
            warn!("CancelSignal::send_cancel took {:?}", dt);
        } else {
            debug!("CancelSignal: cancel flag set (took {:?})", dt);
        }
    }

    /// Check and consume the cancel flag. Returns true if a cancel was pending.
    pub fn consume_cancel(&self) -> bool {
        self.flag.swap(false, Ordering::SeqCst)
    }

    /// Create a separate flag handle for passing to the coordinator thread.
    pub fn flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.flag)
    }
}

impl Default for CancelSignal {
    fn default() -> Self {
        Self::new()
    }
}

const DEBOUNCE: Duration = Duration::from_millis(30);

/// Maximum time the coordinator will stay in `Recording` before it is
/// forcibly cancelled and returned to `Idle`. This is a safety net for the
/// rare case where every hotkey path fails (missed key-release after
/// sleep/wake, tap disabled, etc.) so a recording cannot stay stuck
/// forever and the microphone does not remain live.
const RECORDING_TIMEOUT: Duration = Duration::from_secs(300);

/// Hard ceiling on total recording duration. The sliding VAD-based timeout can
/// extend a continuous-speech session, but it can never grow beyond this cap.
/// This prevents an unbounded recording if the user leaves the mic on with a
/// looping audio source, or if VAD gets stuck in Speech.
const MAX_RECORDING_HARD_CAP: Duration = Duration::from_secs(3600);

/// How long before a recording deadline we start showing the UI warning.
/// The overlay shows a countdown + extend button inside this window.
const RECORDING_TIMEOUT_WARN_LEAD: Duration = Duration::from_secs(30);

/// How much time each "Extend" click adds to both the silence window and the
/// hard cap. Currently 5 minutes per click; both deadlines are bumped equally.
#[allow(dead_code)]
const RECORDING_TIMEOUT_EXTEND_STEP: Duration = Duration::from_secs(300);

/// Maximum time the coordinator will stay in `Processing` before
/// auto-resetting to `Idle`. Prevents the app from becoming permanently
/// unresponsive when the async transcription pipeline hangs (e.g. dead
/// USB microphone, model load timeout, or engine panic that didn't fire
/// the `FinishGuard`).
const PROCESSING_TIMEOUT: Duration = Duration::from_secs(30);

/// Unified application state. This is the single source of truth
/// for the frontend to render the overlay. Emitted via `app-state` events
/// alongside existing `show-overlay`/`hide-overlay` events for backward
/// compatibility during migration.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(tag = "state", content = "data")]
pub enum AppState {
    Idle,
    Recording {
        binding_id: String,
    },
    Processing {
        binding_id: Option<String>,
    },
    UsbCycling {
        stage: String,
    },
    Confirming {
        text: String,
        binding_id: Option<String>,
    },
}

/// Emit an `app-state` event to both the overlay window and the main window.
/// This provides a single source of truth for frontend state, supplementing
/// the existing `show-overlay`/`hide-overlay` events during the migration period.
pub fn emit_app_state(app: &AppHandle, state: &AppState) {
    // Emit to overlay window (primary consumer)
    if let Some(overlay) = app.get_webview_window("recording_overlay") {
        let _ = overlay.emit("app-state", state);
    }
    // Also emit to main window for settings UI and other consumers
    let _ = app.emit("app-state", state);
}

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
    /// Transition from Processing to a new Processing stage with fresh timer
    /// and binding_id. Used by the router action after user confirmation so
    /// the coordinator's internal Stage stays in sync with the shared AppState.
    SetProcessingWithBinding {
        binding_id: Option<String>,
    },
    /// Internal: the processing-timeout timer fired.
    ProcessingTimeout,
    /// Extend the recording deadlines by `seconds`. Sent by the overlay when
    /// the user clicks the "Extend" button on the timeout warning banner.
    ExtendRecording {
        seconds: u64,
    },
}

/// Pipeline lifecycle, owned exclusively by the coordinator thread.
#[derive(Debug)]
enum Stage {
    Idle,
    Recording {
        binding_id: String,
        since: Instant,
        /// Additional silence tolerance accumulated via "Extend" clicks.
        silence_extension: Duration,
        /// Whether we've already emitted the 30s warning for the current deadline.
        /// Reset to false whenever the silence deadline slides (VAD or extend)
        /// so the warning re-arms for the new deadline.
        warned: bool,
    },
    Processing {
        since: Instant,
        binding_id: Option<String>,
    },
}

impl Stage {
    /// Returns true if this stage represents active use (Recording or Processing)
    fn is_active(&self) -> bool {
        !matches!(self, Stage::Idle)
    }

    /// Convert to the corresponding AppState for sharing with the frontend.
    fn to_app_state(&self) -> AppState {
        match self {
            Stage::Idle => AppState::Idle,
            Stage::Recording { binding_id, .. } => AppState::Recording {
                binding_id: binding_id.clone(),
            },
            Stage::Processing { binding_id, .. } => AppState::Processing {
                binding_id: binding_id.clone(),
            },
        }
    }
}

/// Result of processing a single command through the state machine.
/// Returned by `CoordinatorCore::process_command` so the coordinator thread
/// can perform side effects (emit events, start/stop actions) based on
/// what changed, without the core logic depending on AppHandle.
#[derive(Debug, PartialEq)]
enum StageAction {
    /// No action needed (command was ignored, e.g. debounce or wrong state).
    None,
    /// Call the `start` action for this binding_id, then check if recording began.
    StartRecording {
        binding_id: String,
        hotkey_string: String,
    },
    /// Call the `stop` action for this binding_id, then transition to Processing.
    StopRecording {
        binding_id: String,
        hotkey_string: String,
    },
    /// Processing timeout fired — the thread must hide the overlay and reset
    /// the tray icon to Idle. Mirrors the pre-refactor ProcessingTimeout arm
    /// which called `hide_recording_overlay()` + `change_tray_icon(Idle)`.
    ProcessingTimeoutExpired,
}

/// Pure state machine for the transcription coordinator pipeline.
///
/// Handles stage transitions, cancel logic, debouncing, and timeout
/// computation without any dependency on AppHandle or Tauri. This makes
/// it fully testable in isolation.
///
/// The coordinator thread owns a `CoordinatorCore` instance and performs
/// side effects (event emission, action dispatch) based on the transitions
/// and actions returned by `process_command`.
struct CoordinatorCore {
    stage: Stage,
    last_press: Option<Instant>,
    /// Processing timeout duration. In production this is 30s; in tests
    /// it can be set to a much shorter value via `new_for_test()`.
    processing_timeout: Duration,
    /// Additional time added to the 1-hour hard cap by "Extend" clicks.
    /// Reset to zero whenever the stage leaves Recording so each recording
    /// session starts with a fresh hard cap.
    hard_cap_extension: Duration,
}

impl CoordinatorCore {
    fn new(processing_timeout: Duration) -> Self {
        Self {
            stage: Stage::Idle,
            last_press: None,
            processing_timeout,
            hard_cap_extension: Duration::ZERO,
        }
    }

    /// Returns true if a stop input for `binding_id` should be honored even
    /// when the internal stage doesn't currently match `Recording(binding_id)`.
    /// This detects cases where the coordinator's state has drifted from the
    /// audio recorder's real state (e.g. after sleep/wake with HandyKeys, or a
    /// missed/failed transition), and recovers by forcing the stage to match.
    fn try_recover_stop<R: Runtime>(
        &mut self,
        binding_id: String,
        active_use: &AtomicBool,
        current_state: &RwLock<AppState>,
        recorder_is_recording: bool,
        app: &AppHandle<R>,
    ) -> bool {
        info!(
            "try_recover_stop: entry, stage={:?}, passed_recorder_is_recording={}",
            self.stage, recorder_is_recording
        );

        // Re-sample the recorder state inside this diagnostic function so we
        // can detect when the passed-in value has become stale (e.g. due to
        // blocking on a lock or an async gap between recv and processing).
        let resampled_recorder_is_recording = app
            .try_state::<Arc<AudioRecordingManager>>()
            .is_some_and(|a| a.is_recording());
        if resampled_recorder_is_recording != recorder_is_recording {
            warn!(
                "try_recover_stop: recorder state drifted since passed value (passed={}, resampled={})",
                recorder_is_recording, resampled_recorder_is_recording
            );
        }

        // We can only recover if we're not already in the middle of a real
        // Processing stop (in that case the recorder should be stopping on
        // its own, and forcing it again could double-stop).
        if matches!(self.stage, Stage::Processing { .. }) {
            warn!("try_recover_stop: false, reason=already_in_processing_stage");
            return false;
        }

        // Defensive: only force a recovery when we have external evidence that
        // the recorder is actually still capturing. If it isn't, the stage is
        // most likely correct and the stop event is stale/spurious.
        // NOTE: We use the passed-in `recorder_is_recording` value for the
        // recovery decision, not the re-sampled value, so the re-sampled value
        // remains purely informational (diagnostics only).
        if !recorder_is_recording {
            warn!("try_recover_stop: false, reason=passed_recorder_not_recording");
            return false;
        }

        warn!(
            "Coordinator stage drift detected: stage={:?}, forcing Recording('{}') so stop can proceed",
            self.stage, binding_id
        );
        self.transition_to(Stage::Recording {
            binding_id,
            since: Instant::now(),
            silence_extension: Duration::ZERO,
            warned: false,
        });
        self.sync_state(active_use, current_state);
        true
    }

    /// Production constructor with the default 30s timeout.
    fn new_production() -> Self {
        Self::new(PROCESSING_TIMEOUT)
    }

    /// Test constructor with a custom processing timeout.
    #[cfg(test)]
    fn new_for_test(processing_timeout: Duration) -> Self {
        Self::new(processing_timeout)
    }

    /// Process a cancel flag. Returns true if the coordinator should reset to Idle.
    fn check_cancel(&mut self) -> bool {
        if self.stage.is_active() {
            self.transition_to(Stage::Idle);
            true
        } else {
            false
        }
    }

    /// Calculate the recv timeout for the next command. Returns `Some(duration)`
    /// if in Processing or Recording (to wake up for timeout check), `None`
    /// otherwise. Does NOT mutate state — the caller handles the timeout expiry
    /// separately.
    #[cfg(test)]
    fn recv_timeout(&self) -> Option<Duration> {
        match &self.stage {
            Stage::Processing { since, .. } => {
                let elapsed = since.elapsed();
                if elapsed >= self.processing_timeout {
                    // Already past the deadline — return zero so the caller
                    // knows to handle it immediately.
                    Some(Duration::ZERO)
                } else {
                    Some(self.processing_timeout - elapsed)
                }
            }
            Stage::Recording {
                since,
                silence_extension,
                ..
            } => {
                let elapsed = since.elapsed();
                let deadline = *silence_extension + RECORDING_TIMEOUT;
                if elapsed >= deadline {
                    Some(Duration::ZERO)
                } else {
                    Some(deadline - elapsed)
                }
            }
            _ => None,
        }
    }

    /// Returns true if the Processing timeout has expired (i.e., we've been
    /// in the Processing stage longer than the configured timeout).
    #[cfg(test)]
    fn is_processing_expired(&self) -> bool {
        match &self.stage {
            Stage::Processing { since, .. } => since.elapsed() >= self.processing_timeout,
            _ => false,
        }
    }

    /// Returns true if the Recording safety timeout has expired.
    #[cfg(test)]
    fn is_recording_expired(&self) -> bool {
        match &self.stage {
            Stage::Recording {
                since,
                silence_extension,
                ..
            } => since.elapsed() >= RECORDING_TIMEOUT + *silence_extension,
            _ => false,
        }
    }

    /// Test-only accessor for the current recording silence extension.
    #[cfg(test)]
    fn recording_silence_extension(&self) -> Duration {
        match &self.stage {
            Stage::Recording {
                silence_extension, ..
            } => *silence_extension,
            _ => Duration::ZERO,
        }
    }

    /// Test-only accessor for the current hard-cap extension.
    #[cfg(test)]
    fn hard_cap_extension(&self) -> Duration {
        self.hard_cap_extension
    }

    /// Clear the debounce timer so the next press event is not debounced.
    /// Used in tests to simulate realistic timing between key presses.
    #[cfg(test)]
    fn clear_debounce(&mut self) {
        self.last_press = None;
    }

    /// Process a command and return any action the coordinator thread should perform.
    /// Updates `stage` and `active_use`/`current_state` via the provided callbacks.
    fn process_command<R: Runtime>(
        &mut self,
        cmd: Command,
        active_use: &AtomicBool,
        current_state: &RwLock<AppState>,
        recorder_is_recording: bool,
        app: &AppHandle<R>,
    ) -> StageAction {
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
                    if self
                        .last_press
                        .is_some_and(|t| now.duration_since(t) < DEBOUNCE)
                    {
                        debug!("Debounced press for '{binding_id}'");
                        return StageAction::None;
                    }
                    self.last_press = Some(now);
                }

                if push_to_talk {
                    if is_pressed && matches!(self.stage, Stage::Idle) {
                        self.transition_to(Stage::Recording {
                            binding_id: binding_id.clone(),
                            since: Instant::now(),
                            silence_extension: Duration::ZERO,
                            warned: false,
                        });
                        self.sync_state(active_use, current_state);
                        StageAction::StartRecording {
                            binding_id,
                            hotkey_string,
                        }
                    } else if !is_pressed
                        && matches!(
                            &self.stage,
                            Stage::Recording { binding_id: id, .. } if id == &binding_id
                        )
                    {
                        info!(
                            "Stop input received: stage={:?} recorder_is_recording={}",
                            self.stage, recorder_is_recording
                        );
                        self.hard_cap_extension = Duration::ZERO;
                        // Transition to Processing first so active_use stays true,
                        // then tell the thread to call stop which may fire FinishGuard.
                        self.transition_to(Stage::Processing {
                            since: Instant::now(),
                            binding_id: Some(binding_id.clone()),
                        });
                        self.sync_state(active_use, current_state);
                        StageAction::StopRecording {
                            binding_id,
                            hotkey_string,
                        }
                    } else if !is_pressed
                        && self.try_recover_stop(
                            binding_id.clone(),
                            active_use,
                            current_state,
                            recorder_is_recording,
                            app,
                        )
                    {
                        info!(
                            "Stop input received (PTT recovery): stage={:?} recorder_is_recording={}",
                            self.stage, recorder_is_recording
                        );
                        self.hard_cap_extension = Duration::ZERO;
                        // Push-to-talk release arrived while the recorder is still
                        // active but the coordinator stage drifted (e.g. missed
                        // release during sleep/wake). Force Recording and then
                        // perform the normal stop transition.
                        self.transition_to(Stage::Processing {
                            since: Instant::now(),
                            binding_id: Some(binding_id.clone()),
                        });
                        self.sync_state(active_use, current_state);
                        StageAction::StopRecording {
                            binding_id,
                            hotkey_string,
                        }
                    } else {
                        debug!(
                            "Ignoring push-to-talk release for '{}': stage={:?}, pressed={}",
                            binding_id, self.stage, is_pressed
                        );
                        StageAction::None
                    }
                } else if is_pressed {
                    match &self.stage {
                        Stage::Idle => {
                            self.transition_to(Stage::Recording {
                                binding_id: binding_id.clone(),
                                since: Instant::now(),
                                silence_extension: Duration::ZERO,
                                warned: false,
                            });
                            self.sync_state(active_use, current_state);
                            StageAction::StartRecording {
                                binding_id,
                                hotkey_string,
                            }
                        }
                        Stage::Recording { binding_id: id, .. } if id == &binding_id => {
                            info!(
                                "Stop input received: stage={:?} recorder_is_recording={}",
                                self.stage, recorder_is_recording
                            );
                            self.hard_cap_extension = Duration::ZERO;
                            self.transition_to(Stage::Processing {
                                since: Instant::now(),
                                binding_id: Some(binding_id.clone()),
                            });
                            self.sync_state(active_use, current_state);
                            StageAction::StopRecording {
                                binding_id,
                                hotkey_string,
                            }
                        }
                        _ => {
                            // Defensive recovery: if a stop press arrives while the
                            // recorder is still active but the coordinator stage has
                            // drifted, force the stage to Recording so the stop can
                            // proceed. This fixes missed transitions after sleep/wake
                            // or HandyKeys stale state.
                            if self.try_recover_stop(
                                binding_id.clone(),
                                active_use,
                                current_state,
                                recorder_is_recording,
                                app,
                            ) {
                                info!(
                                    "Stop input received (toggle recovery): stage={:?} recorder_is_recording={}",
                                    self.stage, recorder_is_recording
                                );
                                self.hard_cap_extension = Duration::ZERO;
                                self.transition_to(Stage::Processing {
                                    since: Instant::now(),
                                    binding_id: Some(binding_id.clone()),
                                });
                                self.sync_state(active_use, current_state);
                                StageAction::StopRecording {
                                    binding_id,
                                    hotkey_string,
                                }
                            } else {
                                warn!(
                                    "Ignoring stop press for '{}': pipeline busy (stage={:?})",
                                    binding_id, self.stage
                                );
                                StageAction::None
                            }
                        }
                    }
                } else {
                    // Key release without push-to-talk: no-op
                    StageAction::None
                }
            }
            Command::Cancel {
                recording_was_active,
            } => {
                let cancel_start = Instant::now();
                info!(
                    "CoordinatorCore: cancel received, recording_was_active={}",
                    recording_was_active
                );
                let was_active = self.stage.is_active();
                if recording_was_active || matches!(self.stage, Stage::Recording { .. }) {
                    self.hard_cap_extension = Duration::ZERO;
                    self.transition_to(Stage::Idle);
                    self.sync_state(active_use, current_state);
                    info!("CoordinatorCore: cancelled, reset to Idle");
                } else if matches!(self.stage, Stage::Processing { .. }) {
                    warn!("Cancelling stuck processing stage");
                    self.transition_to(Stage::Idle);
                    self.sync_state(active_use, current_state);
                }
                if was_active {
                    info!("Cancel received, resetting handy-keys pressed-hotkey state");
                    let t = Instant::now();
                    crate::shortcut::handy_keys::reset_all_pressed_hotkeys(app);
                    let dt = t.elapsed();
                    if dt > Duration::from_millis(100) {
                        warn!("reset_all_pressed_hotkeys during cancel took {:?}", dt);
                    }
                }
                let dt = cancel_start.elapsed();
                if dt > Duration::from_millis(100) {
                    warn!("CoordinatorCore cancel command processing took {:?}", dt);
                }
                StageAction::None
            }
            Command::ProcessingFinished => {
                if matches!(self.stage, Stage::Processing { .. }) {
                    info!("CoordinatorCore: processing finished, reset to Idle");
                    self.hard_cap_extension = Duration::ZERO;
                    self.transition_to(Stage::Idle);
                    self.sync_state(active_use, current_state);
                    info!("Session complete, resetting handy-keys pressed-hotkey state");
                    crate::shortcut::handy_keys::reset_all_pressed_hotkeys(app);
                }
                StageAction::None
            }
            Command::ExtendRecording { seconds } => {
                if let Stage::Recording {
                    ref mut silence_extension,
                    ref mut warned,
                    ..
                } = self.stage
                {
                    let delta = Duration::from_secs(seconds);
                    *silence_extension += delta;
                    self.hard_cap_extension += delta;
                    *warned = false;
                    info!(
                        "Recording extended by {:?}: silence_extension={:?}, hard_cap_extension={:?}",
                        delta, silence_extension, self.hard_cap_extension
                    );
                } else {
                    debug!(
                        "Ignoring ExtendRecording({}) command: not currently Recording",
                        seconds
                    );
                }
                StageAction::None
            }
            Command::SetProcessingWithBinding { binding_id } => {
                info!(
                    "CoordinatorCore: set processing with binding_id={:?}, resetting timer",
                    binding_id
                );
                self.transition_to(Stage::Processing {
                    since: Instant::now(),
                    binding_id,
                });
                self.sync_state(active_use, current_state);
                StageAction::None
            }
            Command::ProcessingTimeout => {
                if matches!(self.stage, Stage::Processing { .. }) {
                    warn!(
                        "Processing stage timed out after {:?}, auto-resetting to Idle",
                        self.processing_timeout
                    );
                    self.hard_cap_extension = Duration::ZERO;
                    self.transition_to(Stage::Idle);
                    self.sync_state(active_use, current_state);
                    info!("Processing timeout, resetting handy-keys pressed-hotkey state");
                    crate::shortcut::handy_keys::reset_all_pressed_hotkeys(app);
                    StageAction::ProcessingTimeoutExpired
                } else {
                    StageAction::None
                }
            }
        }
    }

    /// Transition to a new stage. Does NOT sync the shared state —
    /// call `sync_state` separately after this.
    fn transition_to(&mut self, new_stage: Stage) {
        self.stage = new_stage;
    }

    /// Sync the internal stage to the shared active_use flag and AppState.
    fn sync_state(&self, active_use: &AtomicBool, current_state: &RwLock<AppState>) {
        active_use.store(self.stage.is_active(), Ordering::SeqCst);
        if let Ok(mut guard) = current_state.write() {
            *guard = self.stage.to_app_state();
        }
    }
}

/// Helper to update stage and sync the active_use flag.
/// Also updates the shared AppState and emits an `app-state` event.
fn set_stage(
    stage: &mut Stage,
    new_stage: Stage,
    active_use: &AtomicBool,
    current_state: &RwLock<AppState>,
    app: &AppHandle,
) {
    *stage = new_stage;
    active_use.store(stage.is_active(), Ordering::SeqCst);

    // Update the shared AppState to reflect the new stage
    let new_app_state = stage.to_app_state();

    if let Ok(mut guard) = current_state.write() {
        *guard = new_app_state.clone();
    }
    emit_app_state(app, &new_app_state);
}

/// Payload for the `recording-timeout-warning` event emitted ~30s before a
/// recording deadline (silence window or 1-hour hard cap). Mirrors the Rust
/// deadline calculation so the overlay can show a countdown without recomputing
/// it from absolute timestamps.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct RecordingTimeoutWarning {
    /// Seconds remaining until the closest deadline (silence or hard cap).
    pub seconds_remaining: u64,
    /// Which deadline is driving the warning.
    pub kind: RecordingTimeoutWarningKind,
    /// Approximate Unix-epoch milliseconds when the deadline will hit. This is
    /// a wall-clock estimate for the frontend countdown; small drift between
    /// Rust Instant and JS Date is acceptable because we re-emit on every loop
    /// wake while inside the warning window.
    pub deadline_at_epoch_ms: u64,
}

/// Which recording deadline is about to expire.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum RecordingTimeoutWarningKind {
    /// The silence-window deadline (5 minutes + any extensions).
    Silence,
    /// The absolute 1-hour hard cap (+ any extensions).
    HardCap,
}

/// Emit a `recording-timeout-warning` event to both the overlay window and the
/// main window. The overlay uses it to show the countdown + extend button; the
/// main window gets it so any future settings UI can react if needed.
pub fn emit_recording_timeout_warning(app: &AppHandle, warning: &RecordingTimeoutWarning) {
    // Emit to overlay window (primary consumer)
    if let Some(overlay) = app.get_webview_window("recording_overlay") {
        let _ = overlay.emit("recording-timeout-warning", warning);
    }
    // Also emit to main window for settings UI and other consumers
    let _ = app.emit("recording-timeout-warning", warning);
}

/// Serialises all transcription lifecycle events through a single thread
/// to eliminate race conditions between keyboard shortcuts, signals, and
/// the async transcribe-paste pipeline.
pub struct TranscriptionCoordinator {
    tx: Sender<Command>,
    /// Shared flag indicating whether Handy is in active use (Recording or Processing stage).
    /// This is used by the CLI `--is-active-use` flag to determine if the app is busy.
    active_use: Arc<AtomicBool>,
    /// Current application state (shared for reads from any thread).
    /// Updated after every state transition and emitted via `app-state` events.
    current_state: Arc<RwLock<AppState>>,
    /// Non-blocking cancel flag. When set, the coordinator thread transitions
    /// to Idle on the next loop iteration, ensuring the UI never blocks.
    /// Stored here for ownership; actual reads happen through the clone passed
    /// to the coordinator thread.
    #[allow(dead_code)]
    cancel_flag: Arc<AtomicBool>,
}

pub fn is_transcribe_binding(id: &str) -> bool {
    id == "transcribe" || id == "transcribe_with_post_process" || id == "transcribe_with_router"
}

impl TranscriptionCoordinator {
    pub fn new(app: AppHandle, cancel_flag: Arc<AtomicBool>) -> Self {
        let (tx, rx) = mpsc::channel();
        let active_use = Arc::new(AtomicBool::new(false));
        let active_use_clone = Arc::clone(&active_use);
        let current_state = Arc::new(RwLock::new(AppState::Idle));
        let current_state_clone = Arc::clone(&current_state);
        let cancel_flag_clone = Arc::clone(&cancel_flag);
        info!("Starting transcription coordinator thread");

        // Emit initial Idle state
        emit_app_state(&app, &AppState::Idle);

        thread::spawn(move || {
            let mut core = CoordinatorCore::new_production();
            let mut should_exit = false;
            // Total elapsed-time tracking for the hard recording cap. Set when
            // we first observe Recording and cleared when we leave it, so the
            // 1-hour ceiling measures wall-clock recording duration regardless
            // of how often VAD resets the 5-minute silence deadline.
            let mut recording_start: Option<Instant> = None;

            loop {
                let panic_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    // Check cancel flag FIRST, before waiting for commands.
                    // This ensures the coordinator transitions to Idle immediately
                    // when cancel is requested, even if a command is pending.
                    if cancel_flag_clone.swap(false, Ordering::SeqCst) {
                        let t = Instant::now();
                        info!("Coordinator: cancel flag detected, resetting to Idle");
                        let did_cancel = core.check_cancel();
                        if did_cancel {
                            set_stage(
                                &mut core.stage,
                                Stage::Idle,
                                &active_use_clone,
                                &current_state_clone,
                                &app,
                            );
                        }
                        let dt = t.elapsed();
                        if dt > Duration::from_millis(100) {
                            warn!("Coordinator cancel flag handling took {:?}", dt);
                        }
                        return;
                    }

                    // Synchronise the total-recording clock with the core stage.
                    // We start the clock when the core enters Recording and stop
                    // it when it leaves, so the hard cap measures real recording
                    // time even though the core's `since` may be reset by VAD.
                    match &core.stage {
                        Stage::Recording { .. } if recording_start.is_none() => {
                            recording_start = Some(Instant::now());
                        }
                        Stage::Recording { .. } => {}
                        _ => recording_start = None,
                    }

                    // Calculate recv timeout: wake up to check Processing and
                    // Recording timeouts.
                    let timeout = match &core.stage {
                        Stage::Processing { since, .. } => {
                            let elapsed = since.elapsed();
                            if elapsed >= core.processing_timeout {
                                // Already past the deadline — reset immediately.
                                // Hide the overlay so the user doesn't see a stuck "Transcribing..." state.
                                warn!(
                                    "Processing stage exceeded {:?} timeout, auto-resetting to Idle",
                                    core.processing_timeout
                                );
                                crate::utils::hide_recording_overlay(&app);
                                crate::utils::change_tray_icon(
                                    &app,
                                    crate::utils::TrayIconState::Idle,
                                );
                                set_stage(
                                    &mut core.stage,
                                    Stage::Idle,
                                    &active_use_clone,
                                    &current_state_clone,
                                    &app,
                                );
                                return; // exit this iteration cleanly, outer loop will re-enter
                            }
                            Some(core.processing_timeout - elapsed)
                        }
                        Stage::Recording {
                            since,
                            binding_id,
                            silence_extension,
                            warned,
                        } => {
                            let elapsed = since.elapsed();

                            // VAD-based sliding timeout: if the user is still
                            // talking, reset the silence deadline so continuous
                            // speech can extend the session. Silence for longer
                            // than RECORDING_TIMEOUT + extensions still triggers
                            // emergency cancel. We use the AudioRecordingManager's
                            // shared atomic value, which is elapsed ms since
                            // recording start, to avoid cross-thread Instant
                            // arithmetic.
                            let mut since = *since;
                            let mut warned_local = *warned;
                            let silence_extension_local = *silence_extension;
                            let binding_id_local = binding_id.clone();
                            if let Some(rm) = app.try_state::<Arc<AudioRecordingManager>>() {
                                let voice_elapsed =
                                    Duration::from_millis(rm.last_voice_activity_ms());
                                // Two-clock design: `elapsed` is the coordinator's
                                // local time since it entered the Recording stage,
                                // while `voice_elapsed` is a monotonic stamp from
                                // the recorder (ms since its own recording start).
                                // `since` resets whenever recent voice is detected,
                                // so the two values can diverge. `saturating_sub`
                                // keeps the combination safe regardless of which
                                // value is currently larger.
                                let silence = elapsed.saturating_sub(voice_elapsed);
                                if silence < RECORDING_TIMEOUT + silence_extension_local {
                                    // User spoke within the timeout window. Reset
                                    // the silence deadline to now, giving a fresh
                                    // RECORDING_TIMEOUT of silence tolerance.
                                    since = Instant::now();
                                    warned_local = false;
                                }
                            }

                            // Hard cap on total recording time, independent of
                            // voice activity. Prevents runaway recordings even if
                            // VAD gets stuck or a continuous audio source loops.
                            let hard_cap_elapsed =
                                recording_start.map_or(Duration::ZERO, |start| start.elapsed());
                            let hard_cap_deadline =
                                MAX_RECORDING_HARD_CAP + core.hard_cap_extension;
                            if hard_cap_elapsed >= hard_cap_deadline {
                                warn!(
                                    "Recording exceeded hard cap of {:?}, forcing emergency cancel",
                                    hard_cap_deadline
                                );
                                if let Some(rm) = app.try_state::<Arc<AudioRecordingManager>>() {
                                    rm.cancel_recording();
                                }
                                // AudioRecordingManager::cancel_recording() already
                                // cancels the emergency backup, so don't duplicate
                                // that work here.
                                crate::utils::hide_recording_overlay(&app);
                                crate::utils::change_tray_icon(
                                    &app,
                                    crate::utils::TrayIconState::Idle,
                                );
                                crate::shortcut::handy_keys::reset_all_pressed_hotkeys(&app);
                                core.hard_cap_extension = Duration::ZERO;
                                set_stage(
                                    &mut core.stage,
                                    Stage::Idle,
                                    &active_use_clone,
                                    &current_state_clone,
                                    &app,
                                );
                                return;
                            }

                            let silence_deadline = RECORDING_TIMEOUT + silence_extension_local;
                            let silence_deadline_remaining =
                                silence_deadline.saturating_sub(since.elapsed());
                            let hard_cap_remaining =
                                hard_cap_deadline.saturating_sub(hard_cap_elapsed);

                            // The effective deadline is the sooner of the two.
                            // We report that kind to the frontend so the warning
                            // text can say "Recording ending in Ns" vs
                            // "Recording hard limit in Ns".
                            let (effective_remaining, kind) =
                                if silence_deadline_remaining <= hard_cap_remaining {
                                    (
                                        silence_deadline_remaining,
                                        RecordingTimeoutWarningKind::Silence,
                                    )
                                } else {
                                    (hard_cap_remaining, RecordingTimeoutWarningKind::HardCap)
                                };

                            // Warning emission: fire once when we enter the 30s
                            // window, then re-emit on every loop wake while still
                            // inside the window so a freshly opened overlay stays
                            // in sync. The frontend handles deduplication by
                            // resetting its local countdown from the new payload.
                            if effective_remaining <= RECORDING_TIMEOUT_WARN_LEAD {
                                if !warned_local {
                                    info!(
                                        "Recording timeout warning: {:?} remaining ({:?})",
                                        effective_remaining, kind
                                    );
                                }
                                let now_epoch_ms = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or(Duration::ZERO)
                                    .as_millis()
                                    as u64;
                                let warning = RecordingTimeoutWarning {
                                    seconds_remaining: effective_remaining.as_secs(),
                                    kind: kind.clone(),
                                    deadline_at_epoch_ms: now_epoch_ms
                                        + effective_remaining.as_millis() as u64,
                                };
                                emit_recording_timeout_warning(&app, &warning);
                                warned_local = true;
                            }

                            // Push the re-armed `warned` flag back into the stage
                            // so the warning only re-fires after a VAD slide or
                            // an extend resets it.
                            core.stage = Stage::Recording {
                                binding_id: binding_id_local,
                                since,
                                silence_extension: silence_extension_local,
                                warned: warned_local,
                            };

                            if effective_remaining == Duration::ZERO {
                                // Recording has exceeded the maximum allowed
                                // duration. This is an emergency safety net for
                                // the rare case where the stop hotkey never
                                // reached us (e.g. after sleep/wake). Cancel the
                                // recorder directly, reset UI, and return to
                                // Idle so the microphone does not stay live.
                                warn!(
                                    "Recording stage exceeded {:?} timeout, forcing emergency cancel",
                                    silence_deadline
                                );
                                if let Some(rm) = app.try_state::<Arc<AudioRecordingManager>>() {
                                    rm.cancel_recording();
                                }
                                // AudioRecordingManager::cancel_recording() already
                                // cancels the emergency backup, so don't duplicate
                                // that work here.
                                crate::utils::hide_recording_overlay(&app);
                                crate::utils::change_tray_icon(
                                    &app,
                                    crate::utils::TrayIconState::Idle,
                                );
                                crate::shortcut::handy_keys::reset_all_pressed_hotkeys(&app);
                                core.hard_cap_extension = Duration::ZERO;
                                set_stage(
                                    &mut core.stage,
                                    Stage::Idle,
                                    &active_use_clone,
                                    &current_state_clone,
                                    &app,
                                );
                                return;
                            }

                            // Choose the next recv_timeout so we wake up at the
                            // 30s warning mark (if not yet warned) and then stay
                            // on every loop iteration while inside the warning
                            // window to keep re-emitting the countdown.
                            if warned_local {
                                Some(effective_remaining)
                            } else if effective_remaining > RECORDING_TIMEOUT_WARN_LEAD {
                                Some(std::cmp::min(
                                    effective_remaining,
                                    RECORDING_TIMEOUT_WARN_LEAD,
                                ))
                            } else {
                                // Should not happen because we're outside the
                                // warning window, but stay safe.
                                Some(effective_remaining)
                            }
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
                                    should_exit = true;
                                    return;
                                }
                            }
                        }
                        None => {
                            match rx.recv() {
                                Ok(c) => c,
                                Err(_) => {
                                    info!("Transcription coordinator channel disconnected, shutting down");
                                    should_exit = true;
                                    return;
                                }
                            }
                        }
                    };
                    let recv_at = Instant::now();
                    // Capture the command name before process_command consumes `cmd`.
                    let cmd_name = match cmd {
                        Command::Input { .. } => "Input",
                        Command::Cancel { .. } => "Cancel",
                        Command::ProcessingFinished => "ProcessingFinished",
                        Command::ProcessingTimeout => "ProcessingTimeout",
                        Command::SetProcessingWithBinding { .. } => "SetProcessingWithBinding",
                        Command::ExtendRecording { .. } => "ExtendRecording",
                    };

                    // Check cancel flag again after receiving a command.
                    // A cancel may have arrived while we were blocking on recv.
                    if cancel_flag_clone.swap(false, Ordering::SeqCst) {
                        let t = Instant::now();
                        info!("Coordinator: cancel flag detected after recv, resetting to Idle");
                        let did_cancel = core.check_cancel();
                        if did_cancel {
                            set_stage(
                                &mut core.stage,
                                Stage::Idle,
                                &active_use_clone,
                                &current_state_clone,
                                &app,
                            );
                        }
                        let dt = t.elapsed();
                        if dt > Duration::from_millis(100) {
                            warn!(
                                "Coordinator cancel flag handling (after recv) took {:?}",
                                dt
                            );
                        }
                        return;
                    }

                    let recorder_is_recording = app
                        .try_state::<Arc<AudioRecordingManager>>()
                        .is_some_and(|a| a.is_recording_with_log("coordinator"));
                    let action = core.process_command(
                        cmd,
                        &active_use_clone,
                        &current_state_clone,
                        recorder_is_recording,
                        &app,
                    );
                    let process_dt = recv_at.elapsed();
                    if process_dt > Duration::from_millis(250) {
                        warn!(
                            "[CMD-CHAN] processed command {} recv→process took {:?} (stage now {:?})",
                            cmd_name, process_dt, core.stage
                        );
                    } else {
                        debug!(
                            "[CMD-CHAN] processed command {} recv→process took {:?} (stage now {:?})",
                            cmd_name, process_dt, core.stage
                        );
                    }

                    // Perform side effects based on the action returned by the core.
                    match action {
                        StageAction::StartRecording {
                            binding_id,
                            hotkey_string,
                        } => {
                            start(
                                &app,
                                &mut core.stage,
                                &binding_id,
                                &hotkey_string,
                                &active_use_clone,
                                &current_state_clone,
                            );
                        }
                        StageAction::StopRecording {
                            binding_id,
                            hotkey_string,
                        } => {
                            stop(
                                &app,
                                &mut core.stage,
                                &binding_id,
                                &hotkey_string,
                                &active_use_clone,
                                &current_state_clone,
                            );
                        }
                        StageAction::ProcessingTimeoutExpired => {
                            // Mirrors the pre-refactor ProcessingTimeout arm:
                            // the stage has already been reset to Idle by
                            // process_command, but the overlay window and tray
                            // icon side effects must run on the thread loop.
                            crate::utils::hide_recording_overlay(&app);
                            crate::utils::change_tray_icon(&app, crate::utils::TrayIconState::Idle);
                        }
                        StageAction::None => {}
                    }

                    // Emit app-state event for the current stage. Non-action
                    // commands (Cancel, ProcessingFinished, SetProcessingWithBinding)
                    // have already updated the stage via process_command. The
                    // ProcessingTimeoutExpired arm ran its own side effects above
                    // but still benefits from this emit to broadcast AppState::Idle.
                    let new_app_state = core.stage.to_app_state();
                    emit_app_state(&app, &new_app_state);
                }));

                if let Err(e) = panic_result {
                    error!("Transcription coordinator iteration panicked: {:?}", e);
                    error!("Recovering: resetting state to Idle and continuing");

                    // Wrap ALL recovery work in catch_unwind so a secondary panic
                    // (e.g. in change_tray_icon) doesn't kill the coordinator thread.
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        // Reset audio recorder if it's stuck in Recording state.
                        // This is the key fix for "stop doesn't work after a panic" —
                        // without this, the recorder keeps recording while the coordinator
                        // thinks it's Idle, so stop commands are ignored.
                        if let Some(rm) = app.try_state::<Arc<AudioRecordingManager>>() {
                            if rm.is_recording() {
                                warn!("Coordinator panic recovery: cancelling stale recording");
                                rm.cancel_recording();
                            }
                        }
                        crate::utils::hide_recording_overlay(&app);
                        crate::utils::change_tray_icon(&app, crate::utils::TrayIconState::Idle);
                    }));

                    active_use_clone.store(false, Ordering::SeqCst);
                    if let Ok(mut guard) = current_state_clone.write() {
                        *guard = AppState::Idle;
                    }
                    emit_app_state(&app, &AppState::Idle);
                    core.stage = Stage::Idle;
                    info!("Panic recovery, resetting handy-keys pressed-hotkey state");
                    crate::shortcut::handy_keys::reset_all_pressed_hotkeys(&app);
                    // Brief pause to avoid hot-looping if panic is deterministic
                    thread::sleep(Duration::from_millis(100));
                }

                if should_exit {
                    info!("Transcription coordinator exiting (channel disconnected)");
                    break;
                }
            }
        });

        Self {
            tx,
            active_use,
            current_state,
            cancel_flag,
        }
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

    /// Get the current application state (thread-safe).
    /// This is the single source of truth for frontend state.
    pub fn get_state(&self) -> AppState {
        match self.current_state.read() {
            Ok(guard) => guard.clone(),
            Err(_) => AppState::Idle, // fallback if lock is poisoned
        }
    }

    /// Set the application state to UsbCycling and emit an app-state event.
    /// Called from the USB watchdog when cycling a USB port.
    pub fn set_usb_cycling(&self, app: &AppHandle, stage: String) {
        let new_state = AppState::UsbCycling { stage };
        if let Ok(mut guard) = self.current_state.write() {
            *guard = new_state.clone();
        }
        emit_app_state(app, &new_state);
    }

    /// Set the application state to Confirming and emit an app-state event.
    /// Called when the router preview is shown for user confirmation.
    /// The `binding_id` identifies the originating action (e.g. "transcribe_with_router").
    pub fn set_confirming(&self, app: &AppHandle, text: String, binding_id: Option<String>) {
        let new_state = AppState::Confirming { text, binding_id };
        if let Ok(mut guard) = self.current_state.write() {
            *guard = new_state.clone();
        }
        emit_app_state(app, &new_state);
    }

    /// Transition the coordinator's internal Stage to Processing with a fresh
    /// timer and the given binding_id. This is critical for the router flow:
    /// after user confirmation, the router subprocess runs asynchronously, so
    /// we need the coordinator's Stage to reflect Processing with a fresh
    /// timeout timer (otherwise the 30s timeout from the initial stop() would
    /// fire too early). Also updates the shared AppState and emits the event.
    pub fn set_processing_with_binding(&self, app: &AppHandle, binding_id: Option<String>) {
        let _ = self.tx.send(Command::SetProcessingWithBinding {
            binding_id: binding_id.clone(),
        });
        // Also update the shared AppState immediately so the frontend
        // transitions to the correct visualizer without waiting for the
        // coordinator thread to process the command.
        let new_state = AppState::Processing { binding_id };
        if let Ok(mut guard) = self.current_state.write() {
            *guard = new_state.clone();
        }
        emit_app_state(app, &new_state);
    }

    /// Set the application state to Idle and emit an app-state event.
    /// Can be called to explicitly reset state from any thread.
    pub fn set_idle(&self, app: &AppHandle) {
        if let Ok(mut guard) = self.current_state.write() {
            *guard = AppState::Idle;
        }
        emit_app_state(app, &AppState::Idle);
    }

    /// Send a request to extend the current recording's deadlines by `seconds`.
    /// Has no effect if the coordinator is not currently Recording.
    pub fn extend_recording(&self, seconds: u64) {
        let _ = self.tx.send(Command::ExtendRecording { seconds });
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
        let send_start = Instant::now();
        let result = self.tx.send(Command::Input {
            binding_id: binding_id.to_string(),
            hotkey_string: hotkey_string.to_string(),
            is_pressed,
            push_to_talk,
        });
        let dt = send_start.elapsed();
        if dt > Duration::from_millis(100) {
            warn!(
                "[CMD-CHAN] send_input enqueued for '{}' (send took {:?})",
                binding_id, dt
            );
        } else {
            debug!(
                "[CMD-CHAN] send_input enqueued for '{}' (send took {:?})",
                binding_id, dt
            );
        }
        if result.is_err() {
            warn!("Transcription coordinator channel closed");
        }
    }

    pub fn notify_cancel(&self, recording_was_active: bool) {
        let t = Instant::now();
        let result = self.tx.send(Command::Cancel {
            recording_was_active,
        });
        let dt = t.elapsed();
        if dt > Duration::from_millis(100) {
            warn!(
                "notify_cancel took {:?} (recording_was_active={})",
                dt, recording_was_active
            );
        }
        if result.is_err() {
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
    current_state: &RwLock<AppState>,
) {
    let Some(action) = ACTION_MAP.get(binding_id) else {
        warn!("No action in ACTION_MAP for '{binding_id}'");
        return;
    };
    action.start(app, binding_id, hotkey_string);
    if app
        .try_state::<Arc<AudioRecordingManager>>()
        .is_some_and(|a| a.is_recording())
    {
        set_stage(
            stage,
            Stage::Recording {
                binding_id: binding_id.to_string(),
                since: Instant::now(),
                silence_extension: Duration::ZERO,
                warned: false,
            },
            active_use,
            current_state,
            app,
        );
        // Bump overlay session — any pending hide from previous session is now invalid
        crate::overlay::bump_overlay_session();
    } else {
        warn!("[START-ROLLBACK] start dispatched for '{}' but is_recording()=false — rolling stage back to Idle (start hotkey may appear dead if this repeats)", binding_id);
        // process_command optimistically transitioned to Recording before
        // dispatching StartRecording. If the audio device failed to start,
        // roll the stage back to Idle so the coordinator isn't stuck claiming
        // to record with no active capture. Without this, a failed start leaves
        // active_use=true and AppState=Recording with no way to recover.
        if matches!(*stage, Stage::Recording { .. }) {
            set_stage(stage, Stage::Idle, active_use, current_state, app);
        }
    }
}

fn stop(
    app: &AppHandle,
    stage: &mut Stage,
    binding_id: &str,
    hotkey_string: &str,
    active_use: &AtomicBool,
    current_state: &RwLock<AppState>,
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
            binding_id: Some(binding_id.to_string()),
        },
        active_use,
        current_state,
        app,
    );
}

// Tests are in a separate file to keep this module under 500 lines.
// The test module exercises `CoordinatorCore`, the pure state-machine logic
// extracted from `TranscriptionCoordinator`, without needing a real AppHandle.
#[cfg(test)]
mod tests {
    include!("transcription_coordinator_tests.rs");
}
