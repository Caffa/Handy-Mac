use crate::actions::ACTION_MAP;
use crate::managers::audio::AudioRecordingManager;
use log::{debug, error, info, warn};
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};

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
        self.flag.store(true, Ordering::SeqCst);
        info!("CancelSignal: cancel flag set");
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
}

/// Pipeline lifecycle, owned exclusively by the coordinator thread.
#[derive(Debug)]
enum Stage {
    Idle,
    Recording(String), // binding_id
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
            Stage::Recording(id) => AppState::Recording {
                binding_id: id.clone(),
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
    StartRecording { binding_id: String, hotkey_string: String },
    /// Call the `stop` action for this binding_id, then transition to Processing.
    StopRecording { binding_id: String, hotkey_string: String },
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
}

impl CoordinatorCore {
    fn new(processing_timeout: Duration) -> Self {
        Self {
            stage: Stage::Idle,
            last_press: None,
            processing_timeout,
        }
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
    /// if in Processing (to wake up for timeout check), `None` otherwise.
    /// Does NOT mutate state — the caller handles the timeout expiry separately.
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

    /// Clear the debounce timer so the next press event is not debounced.
    /// Used in tests to simulate realistic timing between key presses.
    #[cfg(test)]
    fn clear_debounce(&mut self) {
        self.last_press = None;
    }

    /// Process a command and return any action the coordinator thread should perform.
    /// Updates `stage` and `active_use`/`current_state` via the provided callbacks.
    fn process_command(
        &mut self,
        cmd: Command,
        active_use: &AtomicBool,
        current_state: &RwLock<AppState>,
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
                    if self.last_press.map_or(false, |t| now.duration_since(t) < DEBOUNCE) {
                        debug!("Debounced press for '{binding_id}'");
                        return StageAction::None;
                    }
                    self.last_press = Some(now);
                }

                if push_to_talk {
                    if is_pressed && matches!(self.stage, Stage::Idle) {
                        self.transition_to(Stage::Recording(binding_id.clone()));
                        self.sync_state(active_use, current_state);
                        StageAction::StartRecording {
                            binding_id,
                            hotkey_string,
                        }
                    } else if !is_pressed
                        && matches!(&self.stage, Stage::Recording(id) if id == &binding_id)
                    {
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
                    } else {
                        StageAction::None
                    }
                } else if is_pressed {
                    match &self.stage {
                        Stage::Idle => {
                            self.transition_to(Stage::Recording(binding_id.clone()));
                            self.sync_state(active_use, current_state);
                            StageAction::StartRecording {
                                binding_id,
                                hotkey_string,
                            }
                        }
                        Stage::Recording(id) if id == &binding_id => {
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
                            debug!("Ignoring press for '{binding_id}': pipeline busy");
                            StageAction::None
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
                info!(
                    "CoordinatorCore: cancel received, recording_was_active={}",
                    recording_was_active
                );
                if recording_was_active || matches!(self.stage, Stage::Recording(_)) {
                    self.transition_to(Stage::Idle);
                    self.sync_state(active_use, current_state);
                    info!("CoordinatorCore: cancelled, reset to Idle");
                } else if matches!(self.stage, Stage::Processing { .. }) {
                    warn!("Cancelling stuck processing stage");
                    self.transition_to(Stage::Idle);
                    self.sync_state(active_use, current_state);
                }
                StageAction::None
            }
            Command::ProcessingFinished => {
                if matches!(self.stage, Stage::Processing { .. }) {
                    info!("CoordinatorCore: processing finished, reset to Idle");
                    self.transition_to(Stage::Idle);
                    self.sync_state(active_use, current_state);
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
                    self.transition_to(Stage::Idle);
                    self.sync_state(active_use, current_state);
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

            loop {
                let panic_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    // Check cancel flag FIRST, before waiting for commands.
                    // This ensures the coordinator transitions to Idle immediately
                    // when cancel is requested, even if a command is pending.
                    if cancel_flag_clone.swap(false, Ordering::SeqCst) {
                        info!("Coordinator: cancel flag detected, resetting to Idle");
                        if core.check_cancel() {
                            set_stage(
                                &mut core.stage,
                                Stage::Idle,
                                &active_use_clone,
                                &current_state_clone,
                                &app,
                            );
                        }
                        return;
                    }

                    // Calculate recv timeout: if in Processing, wake up to check the timeout.
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

                    // Check cancel flag again after receiving a command.
                    // A cancel may have arrived while we were blocking on recv.
                    if cancel_flag_clone.swap(false, Ordering::SeqCst) {
                        info!("Coordinator: cancel flag detected after recv, resetting to Idle");
                        if core.check_cancel() {
                            set_stage(
                                &mut core.stage,
                                Stage::Idle,
                                &active_use_clone,
                                &current_state_clone,
                                &app,
                            );
                        }
                        return;
                    }

                    let action = core.process_command(cmd, &active_use_clone, &current_state_clone);

                    // Perform side effects based on the action returned by the core.
                    match action {
                        StageAction::StartRecording { binding_id, hotkey_string } => {
                            start(
                                &app,
                                &mut core.stage,
                                &binding_id,
                                &hotkey_string,
                                &active_use_clone,
                                &current_state_clone,
                            );
                        }
                        StageAction::StopRecording { binding_id, hotkey_string } => {
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
                            crate::utils::change_tray_icon(
                                &app,
                                crate::utils::TrayIconState::Idle,
                            );
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
    current_state: &RwLock<AppState>,
) {
    let Some(action) = ACTION_MAP.get(binding_id) else {
        warn!("No action in ACTION_MAP for '{binding_id}'");
        return;
    };
    action.start(app, binding_id, hotkey_string);
    if app
        .try_state::<Arc<AudioRecordingManager>>()
        .map_or(false, |a| a.is_recording())
    {
        set_stage(
            stage,
            Stage::Recording(binding_id.to_string()),
            active_use,
            current_state,
            app,
        );
        // Bump overlay session — any pending hide from previous session is now invalid
        crate::overlay::bump_overlay_session();
    } else {
        debug!("Start for '{binding_id}' did not begin recording; staying idle");
        // process_command optimistically transitioned to Recording before
        // dispatching StartRecording. If the audio device failed to start,
        // roll the stage back to Idle so the coordinator isn't stuck claiming
        // to record with no active capture. Without this, a failed start leaves
        // active_use=true and AppState=Recording with no way to recover.
        if matches!(*stage, Stage::Recording(_)) {
            set_stage(
                stage,
                Stage::Idle,
                active_use,
                current_state,
                app,
            );
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