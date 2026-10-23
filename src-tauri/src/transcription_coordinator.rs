use crate::actions::ACTION_MAP;
use crate::managers::audio::AudioRecordingManager;
use crate::settings::ShortcutActivation;
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
    Idle {
        session_id: u64,
    },
    Recording {
        binding_id: String,
        session_id: u64,
    },
    Processing {
        binding_id: Option<String>,
        session_id: u64,
    },
    UsbCycling {
        stage: String,
        session_id: u64,
    },
    Confirming {
        text: String,
        binding_id: Option<String>,
        session_id: u64,
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
        /// How this binding's key events drive the recording (upstream
        /// c62a5fcd). Keyboard sends the configured setting; external
        /// triggers always send Toggle.
        activation: ShortcutActivation,
        /// Hold-or-toggle only: a hold at least this long stops the
        /// recording on release; anything shorter is a tap that locks the
        /// recording on. Push-to-talk resolves as if the threshold were zero.
        hold_threshold: Duration,
        /// True for non-keyboard triggers (signals, CLI flags). External
        /// triggers are exempt from the keyboard debounce: each one is a
        /// deliberate edge from the user's own integration, and dropping it
        /// desyncs toggle parity. (upstream c6fa60da)
        external: bool,
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

/// A press that arrived while the pipeline was still busy processing the
/// previous transcription. Toggle-style triggers (signals, CLI flags, pedals
/// that signal on both edges) flip state on every edge, so dropping a busy
/// press desyncs the parity: the next edge starts a recording nobody will
/// ever stop. (upstream c6fa60da)
struct PendingPress {
    binding_id: String,
    hotkey_string: String,
    /// The real key-down time, so a hold that straddles the drain is still
    /// measured from when the user pressed, not from when recording began
    /// (upstream c62a5fcd).
    pressed_at: Instant,
    /// The recording will start locked on when the pipeline drains: set from
    /// the start for toggle, and for hold-or-toggle once the key came back up
    /// within the threshold (a tap). An unlocked pending press is a key we
    /// believe is still held.
    locked: bool,
}

impl PendingPress {
    fn remembered(&self) -> Remembered {
        if self.locked {
            Remembered::Locked
        } else {
            Remembered::Held
        }
    }
}

/// Whether a remembered press will start a locked (outlives-the-key) session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Remembered {
    Locked,
    Held,
}

/// What to do with an input that arrives while the pipeline is busy
/// (`Stage::Processing`). `remembered` is whether a press for the same binding
/// is already waiting for the pipeline to drain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BusyAction {
    /// Ignore the input entirely.
    Ignore,
    /// Remember the press; start recording when the pipeline finishes.
    Remember,
    /// This input cancels a previously remembered press (toggle parity: two
    /// presses during one busy window net to no-op; hold modes: a second
    /// press on a tap-locked pending session cancels it).
    Forget,
}

fn classify_busy_input(
    activation: ShortcutActivation,
    is_pressed: bool,
    remembered: Option<Remembered>,
) -> BusyAction {
    use ShortcutActivation::*;
    match (activation, is_pressed, remembered) {
        // Toggle: presses alternate remember/forget to preserve parity.
        (Toggle, true, None) => BusyAction::Remember,
        (Toggle, true, Some(_)) => BusyAction::Forget,
        // Toggle mode ignores releases.
        (Toggle, false, _) => BusyAction::Ignore,
        // Hold modes: a press while busy means the user is holding the key —
        // start as soon as the pipeline drains.
        (PushToTalk | HoldOrToggle, true, None) => BusyAction::Remember,
        // A second press on a tap-locked pending session cancels it.
        (HoldOrToggle, true, Some(Remembered::Locked)) => BusyAction::Forget,
        // Press while a held press is pending: auto-repeat — the key never
        // went up, so there is nothing to cancel or re-remember. (A
        // push-to-talk pending press is never locked, but be exhaustive.)
        (PushToTalk | HoldOrToggle, true, Some(Remembered::Held)) => BusyAction::Ignore,
        (PushToTalk, true, Some(Remembered::Locked)) => BusyAction::Ignore,
        // Releases in hold modes are resolved against the pending press by
        // the caller (tap → lock / hold → drop), or ignored if none.
        (PushToTalk | HoldOrToggle, false, _) => BusyAction::Ignore,
    }
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
        /// The session outlives the key: a release does not stop it, the next
        /// press does. Set from the start for toggle mode, and for
        /// hold-or-toggle when a release within the hold threshold counts as
        /// a tap that locks recording on. (upstream c62a5fcd)
        locked: bool,
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
    fn to_app_state(&self, session_id: u64) -> AppState {
        match self {
            Stage::Idle => AppState::Idle { session_id },
            Stage::Recording { binding_id, .. } => AppState::Recording {
                binding_id: binding_id.clone(),
                session_id,
            },
            Stage::Processing { binding_id, .. } => AppState::Processing {
                binding_id: binding_id.clone(),
                session_id,
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
    /// which called `hide_recording_overlay()` + `set_tray_state(Idle)`.
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
    /// Press remembered while the pipeline was busy (`Stage::Processing`).
    /// At most one press is remembered; it starts recording as soon as the
    /// pipeline drains (`Command::ProcessingFinished`). An explicit cancel
    /// or a processing timeout abandons it.
    pending_press: Option<PendingPress>,
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
            pending_press: None,
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
            // A recovered session is managed by the recovery stop flow, not
            // by hold semantics.
            locked: false,
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
            self.hard_cap_extension = Duration::ZERO;
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
                activation,
                hold_threshold,
                external,
            } => {
                // Debounce rapid-fire press events (key repeat / double-tap).
                // Releases always pass through for push-to-talk. External
                // triggers are exempt: each one is a deliberate edge from the
                // user's own integration, and dropping it desyncs toggle
                // parity (upstream c6fa60da).
                if is_pressed && !external {
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

                // A busy pipeline can't accept lifecycle changes now: classify
                // the input against any already-remembered press instead of
                // dropping it silently (upstream c6fa60da). The fork's
                // try_recover_stop drift recovery below is unaffected — it
                // only fires for non-Processing stages.
                if matches!(self.stage, Stage::Processing { .. }) {
                    // Only one press can be remembered. Once a binding has
                    // claimed it, inputs for a different binding are ignored —
                    // the same rule as a different binding pressed while
                    // recording — rather than silently replacing the
                    // remembered press and breaking its parity.
                    if let Some(pending) = &self.pending_press {
                        if pending.binding_id != binding_id {
                            debug!(
                                "Ignoring input for '{}': '{}' is already pending",
                                binding_id, pending.binding_id
                            );
                            return StageAction::None;
                        }
                    }

                    // Hold-mode releases resolve against the remembered press
                    // immediately (fork divergence: upstream defers via a 50ms
                    // RELEASE_GRACE to absorb X11 auto-repeat; macOS key-repeat
                    // timing plus the fork's debounce cover the common cases
                    // without the extra machinery). A completed hold has
                    // nothing left to start; a tap locks the drained session on.
                    if !is_pressed && activation != ShortcutActivation::Toggle {
                        let mut drop_pending = false;
                        if let Some(pending) = self.pending_press.as_mut() {
                            if pending.binding_id == binding_id && !pending.locked {
                                let held =
                                    Instant::now().saturating_duration_since(pending.pressed_at);
                                if activation == ShortcutActivation::PushToTalk
                                    || held >= hold_threshold
                                {
                                    debug!(
                                        "Hold completed before drain for '{}': dropping pending press",
                                        binding_id
                                    );
                                    drop_pending = true;
                                } else {
                                    debug!(
                                        "Tap before drain for '{}': drained session starts locked",
                                        binding_id
                                    );
                                    pending.locked = true;
                                }
                            }
                        }
                        if drop_pending {
                            self.pending_press = None;
                        }
                        return StageAction::None;
                    }

                    let remembered = self.pending_press.as_ref().map(|p| p.remembered());
                    match classify_busy_input(activation, is_pressed, remembered) {
                        BusyAction::Remember => {
                            debug!("Remembering press for '{}': pipeline busy", binding_id);
                            self.pending_press = Some(PendingPress {
                                binding_id,
                                hotkey_string,
                                pressed_at: Instant::now(),
                                locked: activation == ShortcutActivation::Toggle,
                            });
                        }
                        BusyAction::Forget => {
                            debug!("Forgetting remembered press for '{}'", binding_id);
                            self.pending_press = None;
                        }
                        BusyAction::Ignore => {
                            debug!("Ignoring input for '{}': pipeline busy", binding_id);
                        }
                    }
                    return StageAction::None;
                }

                if is_pressed {
                    match &self.stage {
                        Stage::Idle => {
                            // Toggle never ends on a release: locked from the
                            // start (upstream c62a5fcd).
                            let locked = activation == ShortcutActivation::Toggle;
                            self.transition_to(Stage::Recording {
                                binding_id: binding_id.clone(),
                                since: Instant::now(),
                                silence_extension: Duration::ZERO,
                                warned: false,
                                locked,
                            });
                            self.sync_state(active_use, current_state);
                            StageAction::StartRecording {
                                binding_id,
                                hotkey_string,
                            }
                        }
                        Stage::Recording {
                            binding_id: id,
                            locked,
                            ..
                        } if id == &binding_id => {
                            // A locked session ends on the next press. In toggle
                            // mode every press ends it, even if the recording
                            // began under a hold mode (the setting changed
                            // mid-recording) — otherwise nothing but Escape
                            // could stop it. An unlocked recording means the key
                            // is still held (auto-repeat): a repeated press is
                            // meaningless.
                            if *locked || activation == ShortcutActivation::Toggle {
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
                            } else {
                                debug!("Ignoring press for '{}': key is held", binding_id);
                                StageAction::None
                            }
                        }
                        Stage::Recording { binding_id: id, .. } => {
                            // Cross-binding press while a recording is active:
                            // treat it as a universal "stop + complete" of the
                            // ACTIVE session. Keep the owner's binding id so the
                            // stop flow (transcribe → paste, router → confirming)
                            // matches the recorder's active binding; the pressed
                            // hotkey's identity must neither rebind the stage nor
                            // start a new recording.
                            let owner_id = id.clone();
                            info!(
                                "Cross-binding stop: owner '{}', pressed '{}'",
                                owner_id, binding_id
                            );
                            self.hard_cap_extension = Duration::ZERO;
                            self.transition_to(Stage::Processing {
                                since: Instant::now(),
                                binding_id: Some(owner_id.clone()),
                            });
                            self.sync_state(active_use, current_state);
                            StageAction::StopRecording {
                                binding_id: owner_id,
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
                } else if activation != ShortcutActivation::Toggle {
                    // Hold-mode release (push-to-talk or hold-or-toggle).
                    match &self.stage {
                        Stage::Recording {
                            binding_id: id,
                            since,
                            silence_extension,
                            warned,
                            locked,
                        } if id == &binding_id => {
                            if *locked {
                                // A tap-locked session outlives the key: the
                                // release that locked it was already consumed;
                                // later releases do nothing.
                                debug!(
                                    "Ignoring release for '{}': session is tap-locked",
                                    binding_id
                                );
                                StageAction::None
                            } else {
                                let held = Instant::now().duration_since(*since);
                                if activation == ShortcutActivation::PushToTalk
                                    || held >= hold_threshold
                                {
                                    info!(
                                        "Stop input received (hold release): stage={:?} recorder_is_recording={}",
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
                                    // Tap: lock the recording on until the next
                                    // press (upstream c62a5fcd).
                                    info!(
                                        "Tap detected for '{}' (held {:?} < {:?}): locking recording on",
                                        binding_id, held, hold_threshold
                                    );
                                    let (since_c, se_c, warned_c) =
                                        (*since, *silence_extension, *warned);
                                    self.transition_to(Stage::Recording {
                                        binding_id,
                                        since: since_c,
                                        silence_extension: se_c,
                                        warned: warned_c,
                                        locked: true,
                                    });
                                    StageAction::None
                                }
                            }
                        }
                        Stage::Recording { binding_id: id, .. } => {
                            // Cross-binding hold release while another binding
                            // owns the active recording: ignore it so the
                            // owner's release completes the session (fork rule).
                            debug!(
                                "Ignoring cross-binding hold release for '{}': owner '{}'",
                                binding_id, id
                            );
                            StageAction::None
                        }
                        _ => {
                            // Drift recovery (fork): a hold-mode release
                            // arrived while the recorder is still active but
                            // the coordinator stage drifted (e.g. missed
                            // release during sleep/wake).
                            if self.try_recover_stop(
                                binding_id.clone(),
                                active_use,
                                current_state,
                                recorder_is_recording,
                                app,
                            ) {
                                info!(
                                    "Stop input received (hold recovery): stage={:?} recorder_is_recording={}",
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
                                debug!(
                                    "Ignoring hold-mode release for '{}': stage={:?}, pressed={}",
                                    binding_id, self.stage, is_pressed
                                );
                                StageAction::None
                            }
                        }
                    }
                } else {
                    // Toggle-mode release: no-op
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
                // An explicit cancel abandons any remembered start too — the
                // user asked for silence, not a deferred recording
                // (upstream c6fa60da).
                self.pending_press = None;
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
                    // Pipeline drained: start a press that arrived while the
                    // pipeline was busy, restoring toggle parity
                    // (upstream c6fa60da). The optimistic Recording transition
                    // mirrors the normal input path; the thread's start()
                    // rolls it back if the recorder fails to begin. `since`
                    // uses the real press time so a hold straddling the drain
                    // is measured from the actual key-down (upstream c62a5fcd).
                    if let Some(pending) = self.pending_press.take() {
                        debug!(
                            "Pipeline drained; starting remembered press for '{}'",
                            pending.binding_id
                        );
                        self.transition_to(Stage::Recording {
                            binding_id: pending.binding_id.clone(),
                            since: pending.pressed_at,
                            silence_extension: Duration::ZERO,
                            warned: false,
                            locked: pending.locked,
                        });
                        self.sync_state(active_use, current_state);
                        return StageAction::StartRecording {
                            binding_id: pending.binding_id,
                            hotkey_string: pending.hotkey_string,
                        };
                    }
                }
                StageAction::None
            }
            Command::ExtendRecording { seconds } => {
                // Clamp extension to a 1-hour ceiling to prevent unbounded values.
                let seconds = seconds.min(3600);
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
                if matches!(self.stage, Stage::Idle) {
                    warn!(
                        "CoordinatorCore: ignoring SetProcessingWithBinding {:?} — stage is Idle (stale router handoff)",
                        binding_id
                    );
                    return StageAction::None;
                }
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
                    // The timeout path abandons the session (overlay hidden,
                    // tray reset), so any remembered press is stale by now —
                    // drop it rather than auto-starting a recording the user
                    // asked for before a 30s hang. (Fork divergence from
                    // upstream c6fa60da, which has no timeout concept; here
                    // the timeout is deliberate "reset to known-good Idle".)
                    self.pending_press = None;
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
    ///
    /// NOTE on session_id: this method is called from the pure state machine
    /// `CoordinatorCore`, which does not track the monotonic overlay session
    /// id. The shared `current_state` is therefore written with `session_id: 0`.
    /// That is fine for its current consumers (they only care about `state`
    /// and `binding_id`). The authoritative session-aware state is emitted via
    /// `emit_app_state` from the coordinator thread, which supplies the real
    /// `current_session` id.
    fn sync_state(&self, active_use: &AtomicBool, current_state: &RwLock<AppState>) {
        active_use.store(self.stage.is_active(), Ordering::SeqCst);
        if let Ok(mut guard) = current_state.write() {
            *guard = self.stage.to_app_state(0);
        }
    }
}

/// Helper to update stage and sync the active_use flag.
/// Also updates the shared AppState and emits an `app-state` event.
fn set_stage(
    stage: &mut Stage,
    new_stage: Stage,
    session_id: u64,
    active_use: &AtomicBool,
    current_state: &RwLock<AppState>,
    app: &AppHandle,
) {
    *stage = new_stage;
    active_use.store(stage.is_active(), Ordering::SeqCst);

    // Update the shared AppState to reflect the new stage
    let new_app_state = stage.to_app_state(session_id);

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
        let current_state = Arc::new(RwLock::new(AppState::Idle { session_id: 0 }));
        let current_state_clone = Arc::clone(&current_state);
        let cancel_flag_clone = Arc::clone(&cancel_flag);
        info!("Starting transcription coordinator thread");

        // Emit initial Idle state
        emit_app_state(&app, &AppState::Idle { session_id: 0 });

        // Named thread: sample/spindump showed the wedged coordinator only as
        // an anonymous parked thread. The name makes future stacks readable.
        thread::Builder::new()
            .name("transcription-coordinator".into())
            .spawn(move || {
            let mut core = CoordinatorCore::new_production();
            let mut should_exit = false;
            // Monotonic session id for the current recording/processing session.
            // `start()` pre-allocates the next overlay session id and stores it
            // here; all subsequent state transitions for this session emit it.
            let mut current_session: u64 = 0;
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
                                current_session,
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
                                // Delegate to the single Command::ProcessingTimeout path so all
                                // timeout cleanup (emit Idle, hide overlay, reset tray icon) runs
                                // in one place and no queued commands are skipped.
                                Some(Duration::ZERO)
                            } else {
                                Some(core.processing_timeout - elapsed)
                            }
                        }
                        Stage::Recording {
                            since,
                            binding_id,
                            silence_extension,
                            warned,
                            locked,
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
                            let locked_local = *locked;
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
                                crate::utils::set_tray_state(
                                    &app,
                                    crate::utils::TrayIconState::Idle,
                                );
                                crate::shortcut::handy_keys::reset_all_pressed_hotkeys(&app);
                                core.hard_cap_extension = Duration::ZERO;
                                set_stage(
                                    &mut core.stage,
                                    Stage::Idle,
                                    current_session,
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
                                locked: locked_local,
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
                                crate::utils::set_tray_state(
                                    &app,
                                    crate::utils::TrayIconState::Idle,
                                );
                                crate::shortcut::handy_keys::reset_all_pressed_hotkeys(&app);
                                core.hard_cap_extension = Duration::ZERO;
                                set_stage(
                                    &mut core.stage,
                                    Stage::Idle,
                                    current_session,
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
                                current_session,
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
                                &mut current_session,
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
                                current_session,
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
                            crate::utils::set_tray_state(&app, crate::utils::TrayIconState::Idle);
                        }
                        StageAction::None => {}
                    }

                    // Emit app-state event for the current stage. Non-action
                    // commands (Cancel, ProcessingFinished, SetProcessingWithBinding)
                    // have already updated the stage via process_command. The
                    // ProcessingTimeoutExpired arm ran its own side effects above
                    // but still benefits from this emit to broadcast AppState::Idle.
                    let new_app_state = core.stage.to_app_state(current_session);
                    emit_app_state(&app, &new_app_state);
                }));

                if let Err(e) = panic_result {
                    error!("Transcription coordinator iteration panicked: {:?}", e);
                    error!("Recovering: resetting state to Idle and continuing");

                    // Wrap ALL recovery work in catch_unwind so a secondary panic
                    // (e.g. in set_tray_state) doesn't kill the coordinator thread.
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
                        crate::utils::set_tray_state(&app, crate::utils::TrayIconState::Idle);
                    }));

                    active_use_clone.store(false, Ordering::SeqCst);
                    if let Ok(mut guard) = current_state_clone.write() {
                        *guard = AppState::Idle {
                            session_id: current_session,
                        };
                    }
                    emit_app_state(
                        &app,
                        &AppState::Idle {
                            session_id: current_session,
                        },
                    );
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
        })
        .expect("spawn coordinator");

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
            Err(_) => AppState::Idle { session_id: 0 }, // fallback if lock is poisoned
        }
    }

    /// Set the application state to UsbCycling and emit an app-state event.
    /// Called from the USB watchdog when cycling a USB port.
    pub fn set_usb_cycling(&self, app: &AppHandle, stage: String) {
        let session_id = crate::overlay::current_overlay_session();
        let new_state = AppState::UsbCycling { stage, session_id };
        if let Ok(mut guard) = self.current_state.write() {
            *guard = new_state.clone();
        }
        emit_app_state(app, &new_state);
    }

    /// Set the application state to Confirming and emit an app-state event.
    /// Called when the router preview is shown for user confirmation.
    /// The `binding_id` identifies the originating action (e.g. "transcribe_with_router").
    pub fn set_confirming(&self, app: &AppHandle, text: String, binding_id: Option<String>) {
        let session_id = crate::overlay::current_overlay_session();
        let new_state = AppState::Confirming {
            text,
            binding_id,
            session_id,
        };
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
        let session_id = crate::overlay::current_overlay_session();
        let new_state = AppState::Processing {
            binding_id,
            session_id,
        };
        if let Ok(mut guard) = self.current_state.write() {
            *guard = new_state.clone();
        }
        emit_app_state(app, &new_state);
    }

    /// Send a request to extend the current recording's deadlines by `seconds`.
    /// Has no effect if the coordinator is not currently Recording.
    pub fn extend_recording(&self, seconds: u64) {
        let _ = self.tx.send(Command::ExtendRecording { seconds });
    }

    /// Send a keyboard input event for a transcribe binding, tagged with the
    /// configured activation mode and hold threshold (upstream c62a5fcd).
    /// For signal-based toggles, use `send_external_input` — keyboard inputs
    /// pass through the 30ms debounce; external triggers bypass it.
    pub fn send_input(
        &self,
        binding_id: &str,
        hotkey_string: &str,
        is_pressed: bool,
        activation: ShortcutActivation,
        hold_threshold: Duration,
    ) {
        let send_start = Instant::now();
        let result = self.tx.send(Command::Input {
            binding_id: binding_id.to_string(),
            hotkey_string: hotkey_string.to_string(),
            is_pressed,
            activation,
            hold_threshold,
            external: false,
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

    /// Send an external trigger (SIGUSR1/2, CLI flags). Always a toggle press,
    /// always exempt from the keyboard debounce — each edge is a deliberate
    /// event from the user's own integration, and dropping it desyncs toggle
    /// parity (upstream c6fa60da).
    pub fn send_external_input(&self, binding_id: &str, source: &str) {
        if self
            .tx
            .send(Command::Input {
                binding_id: binding_id.to_string(),
                hotkey_string: source.to_string(),
                is_pressed: true,
                activation: ShortcutActivation::Toggle,
                hold_threshold: Duration::ZERO,
                external: true,
            })
            .is_err()
        {
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
    current_session: &mut u64,
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
        .is_some_and(|a| a.is_recording_effective())
    {
        // Allocate the new session id BEFORE transitioning so the emitted
        // AppState::Recording carries the id of the session that is starting.
        let new_session = crate::overlay::next_overlay_session();
        *current_session = new_session;
        // Preserve the hold state from the optimistic transition:
        // `since` is the real press time (a press remembered while busy is
        // measured from when the user pressed, upstream c62a5fcd) and
        // `locked` records a tap-locked drain or a toggle-mode start.
        let (since, locked) = match &*stage {
            Stage::Recording { since, locked, .. } => (*since, *locked),
            _ => (Instant::now(), false),
        };
        set_stage(
            stage,
            Stage::Recording {
                binding_id: binding_id.to_string(),
                since,
                silence_extension: Duration::ZERO,
                warned: false,
                locked,
            },
            new_session,
            active_use,
            current_state,
            app,
        );
    } else {
        warn!("[START-ROLLBACK] start dispatched for '{}' but is_recording()=false — rolling stage back to Idle (start hotkey may appear dead if this repeats)", binding_id);
        // process_command optimistically transitioned to Recording before
        // dispatching StartRecording. If the audio device failed to start,
        // roll the stage back to Idle so the coordinator isn't stuck claiming
        // to record with no active capture. Without this, a failed start leaves
        // active_use=true and AppState=Recording with no way to recover.
        if matches!(*stage, Stage::Recording { .. }) {
            set_stage(
                stage,
                Stage::Idle,
                *current_session,
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
    current_session: u64,
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
        current_session,
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
