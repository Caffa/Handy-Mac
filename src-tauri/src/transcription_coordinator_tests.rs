// State machine tests for `TranscriptionCoordinator`.
//
// These tests exercise `CoordinatorCore`, the pure state-machine logic
// extracted from `TranscriptionCoordinator`. By testing the core directly,
// we avoid needing a real `AppHandle` — the core only touches plain Rust
// types (`Stage`, `AtomicBool`, `RwLock<AppState>`).

use super::*;

/// Helper to create a `CoordinatorCore` with a short processing timeout
/// and shared state flags mirroring the production coordinator.
struct TestHarness {
    core: CoordinatorCore,
    active_use: Arc<AtomicBool>,
    current_state: Arc<RwLock<AppState>>,
    cancel_flag: Arc<AtomicBool>,
}

impl TestHarness {
    fn new() -> Self {
        Self::with_timeout(Duration::from_millis(500))
    }

    fn with_timeout(processing_timeout: Duration) -> Self {
        Self {
            core: CoordinatorCore::new_for_test(processing_timeout),
            active_use: Arc::new(AtomicBool::new(false)),
            current_state: Arc::new(RwLock::new(AppState::Idle)),
            cancel_flag: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Feed a command directly through the core's process_command method.
    /// Clears debounce before processing so sequential commands aren't
    /// throttled by the 30ms debounce window.
    fn process(&mut self, cmd: Command) -> StageAction {
        self.core.clear_debounce();
        self.core
            .process_command(cmd, &self.active_use, &self.current_state)
    }

    /// Send a cancel signal via the AtomicBool (mimics CancelSignal).
    fn send_cancel_signal(&mut self) {
        self.cancel_flag.store(true, Ordering::SeqCst);
    }

    /// Check and consume the cancel flag, resetting to Idle if active.
    /// Returns true if a cancel was consumed.
    fn check_cancel(&mut self) -> bool {
        if self.cancel_flag.swap(false, Ordering::SeqCst) {
            self.core.check_cancel();
            self.core.sync_state(&self.active_use, &self.current_state);
            true
        } else {
            false
        }
    }

    /// Get the current shared AppState.
    fn state(&self) -> AppState {
        self.current_state.read().unwrap().clone()
    }

    /// Check if active_use is set.
    fn is_active(&self) -> bool {
        self.active_use.load(Ordering::SeqCst)
    }
}

// ── Test 1: Happy path: Idle → Recording → Processing → Idle ──

#[test]
fn test_happy_path_idle_recording_processing_idle() {
    let mut h = TestHarness::new();

    // Start: Idle
    assert!(matches!(h.state(), AppState::Idle));
    assert!(!h.is_active());

    // Press key → start recording (simulate toggle mode)
    let action = h.process(Command::Input {
        binding_id: "transcribe".into(),
        hotkey_string: "Cmd+Shift+S".into(),
        is_pressed: true,
        push_to_talk: false,
    });
    // In toggle mode from Idle, core transitions to Recording and returns StartRecording
    assert!(matches!(action, StageAction::StartRecording { .. }));
    assert!(matches!(h.state(), AppState::Recording { .. }));
    assert!(h.is_active());

    // Press same key again → stop recording, transition to Processing
    let action = h.process(Command::Input {
        binding_id: "transcribe".into(),
        hotkey_string: "Cmd+Shift+S".into(),
        is_pressed: true,
        push_to_talk: false,
    });
    assert!(matches!(action, StageAction::StopRecording { .. }));
    assert!(matches!(h.state(), AppState::Processing { .. }));
    assert!(h.is_active());

    // ProcessingFinished → back to Idle
    h.process(Command::ProcessingFinished);
    assert!(matches!(h.state(), AppState::Idle));
    assert!(!h.is_active());
}

// ── Test 2: Cancel during Recording ──

#[test]
fn test_cancel_during_recording() {
    let mut h = TestHarness::new();

    // Enter Recording
    h.process(Command::Input {
        binding_id: "transcribe".into(),
        hotkey_string: "Cmd+Shift+S".into(),
        is_pressed: true,
        push_to_talk: false,
    });
    assert!(matches!(h.state(), AppState::Recording { .. }));

    // Cancel
    h.process(Command::Cancel {
        recording_was_active: true,
    });
    assert!(matches!(h.state(), AppState::Idle));
    assert!(!h.is_active());
}

// ── Test 3: Cancel during Processing ──

#[test]
fn test_cancel_during_processing() {
    let mut h = TestHarness::new();

    // Enter Processing via: Idle → Recording → Processing
    h.process(Command::Input {
        binding_id: "transcribe".into(),
        hotkey_string: "Cmd+Shift+S".into(),
        is_pressed: true,
        push_to_talk: false,
    });
    h.process(Command::Input {
        binding_id: "transcribe".into(),
        hotkey_string: "Cmd+Shift+S".into(),
        is_pressed: true,
        push_to_talk: false,
    });
    assert!(matches!(h.state(), AppState::Processing { .. }));

    // Cancel during Processing
    h.process(Command::Cancel {
        recording_was_active: false,
    });
    assert!(matches!(h.state(), AppState::Idle));
    assert!(!h.is_active());
}

// ── Test 4: FinishGuard race — ProcessingFinished after Cancel ──
// The coordinator should remain Idle even if a late ProcessingFinished
// arrives after cancel has already reset the state to Idle.
// This reproduces the race documented in transcribe.rs:FinishGuard:
//   1. User starts recording → Recording
//   2. User stops → Processing, async task starts
//   3. User cancels → Idle
//   4. FinishGuard fires (async task completes) → ProcessingFinished
//   Step 4 must NOT resurrect Processing state.

#[test]
fn test_finish_guard_race_no_spurious_idle_after_cancel() {
    let mut h = TestHarness::new();

    // Idle → Recording
    h.process(Command::Input {
        binding_id: "transcribe".into(),
        hotkey_string: "Cmd+Shift+S".into(),
        is_pressed: true,
        push_to_talk: false,
    });

    // Recording → Processing
    h.process(Command::Input {
        binding_id: "transcribe".into(),
        hotkey_string: "Cmd+Shift+S".into(),
        is_pressed: true,
        push_to_talk: false,
    });
    assert!(matches!(h.state(), AppState::Processing { .. }));

    // Cancel — resets to Idle
    h.process(Command::Cancel {
        recording_was_active: false,
    });
    assert!(matches!(h.state(), AppState::Idle));
    assert!(!h.is_active());

    // Late ProcessingFinished (from FinishGuard) — should be a no-op
    // because we're already Idle, not Processing.
    h.process(Command::ProcessingFinished);
    assert!(matches!(h.state(), AppState::Idle));
    assert!(!h.is_active());
}

// ── Test 5: ProcessingTimeout auto-reset ──
// Enter Processing, then simulate timeout by sending ProcessingTimeout.

#[test]
fn test_processing_timeout_auto_resets_to_idle() {
    let mut h = TestHarness::with_timeout(Duration::from_millis(100));

    // Enter Processing via: Idle → Recording → Processing
    h.process(Command::Input {
        binding_id: "transcribe".into(),
        hotkey_string: "Cmd+Shift+S".into(),
        is_pressed: true,
        push_to_talk: false,
    });
    h.process(Command::Input {
        binding_id: "transcribe".into(),
        hotkey_string: "Cmd+Shift+S".into(),
        is_pressed: true,
        push_to_talk: false,
    });
    assert!(matches!(h.state(), AppState::Processing { .. }));

    // Send ProcessingTimeout command — should reset to Idle
    h.process(Command::ProcessingTimeout);
    assert!(matches!(h.state(), AppState::Idle));
    assert!(!h.is_active());
}

// ── Test 6: SetProcessingWithBinding (router mode) ──

#[test]
fn test_set_processing_with_binding() {
    let mut h = TestHarness::new();

    // Start: enter Processing via normal flow
    h.process(Command::Input {
        binding_id: "transcribe".into(),
        hotkey_string: "Cmd+Shift+S".into(),
        is_pressed: true,
        push_to_talk: false,
    });
    h.process(Command::Input {
        binding_id: "transcribe".into(),
        hotkey_string: "Cmd+Shift+S".into(),
        is_pressed: true,
        push_to_talk: false,
    });
    assert!(matches!(h.state(), AppState::Processing { .. }));

    // SetProcessingWithBinding resets timer and updates binding_id
    h.process(Command::SetProcessingWithBinding {
        binding_id: Some("transcribe_with_router".into()),
    });
    match h.state() {
        AppState::Processing { binding_id } => {
            assert_eq!(binding_id.as_deref(), Some("transcribe_with_router"));
        }
        other => panic!("Expected Processing state, got {:?}", other),
    }
    assert!(h.is_active());
}

// ── Test 7: Rapid interleaving — no panic, no deadlock, final state Idle ──

#[test]
fn test_rapid_interleaving_no_panic_or_deadlock() {
    let mut h = TestHarness::new();

    // Rapid sequence: Input → Cancel → Input → ProcessingFinished → Cancel
    h.process(Command::Input {
        binding_id: "transcribe".into(),
        hotkey_string: "Cmd+Shift+S".into(),
        is_pressed: true,
        push_to_talk: false,
    }); // → Recording

    h.process(Command::Cancel {
        recording_was_active: true,
    }); // → Idle

    h.process(Command::Input {
        binding_id: "transcribe".into(),
        hotkey_string: "Cmd+Shift+S".into(),
        is_pressed: true,
        push_to_talk: false,
    }); // → Recording again

    h.process(Command::Input {
        binding_id: "transcribe".into(),
        hotkey_string: "Cmd+Shift+S".into(),
        is_pressed: true,
        push_to_talk: false,
    }); // → Processing

    h.process(Command::ProcessingFinished); // → Idle

    // Final cancel on already-Idle — should be a no-op
    h.process(Command::Cancel {
        recording_was_active: false,
    });

    // No panic, no deadlock, and final state should be Idle
    assert!(matches!(h.state(), AppState::Idle));
    assert!(!h.is_active());
}

// ── Test 8: Panic recovery ──
// TODO: panic recovery test — needs test hook. The coordinator thread wraps
// each iteration in catch_unwind and resets to Idle. Testing this requires
// injecting a panic into the thread loop, which would need a test-only
// command variant or a hook. Marked for future implementation.

// ── Test 9: CancelSignal functionality ──

#[test]
fn test_cancel_signal_send_and_consume() {
    let signal = CancelSignal::new();
    assert!(!signal.consume_cancel());

    signal.send_cancel();
    assert!(signal.consume_cancel());
    // Second consume should return false (already consumed)
    assert!(!signal.consume_cancel());
}

// ── Test 10: Cancel via CancelSignal resets state ──

#[test]
fn test_cancel_signal_resets_active_state() {
    let mut h = TestHarness::new();

    // Enter Recording
    h.process(Command::Input {
        binding_id: "transcribe".into(),
        hotkey_string: "Cmd+Shift+S".into(),
        is_pressed: true,
        push_to_talk: false,
    });
    assert!(h.is_active());

    // Send cancel via the AtomicBool signal (mimics CancelSignal)
    h.send_cancel_signal();
    assert!(h.check_cancel());
    assert!(!h.is_active());
    assert!(matches!(h.state(), AppState::Idle));
}

// ── Test 11: Push-to-talk mode ──

#[test]
fn test_push_to_talk_press_starts_recording() {
    let mut h = TestHarness::new();

    // Push-to-talk press starts recording from Idle
    let action = h.process(Command::Input {
        binding_id: "transcribe".into(),
        hotkey_string: "F6".into(),
        is_pressed: true,
        push_to_talk: true,
    });
    assert!(matches!(action, StageAction::StartRecording { .. }));
    assert!(matches!(h.state(), AppState::Recording { .. }));

    // Push-to-talk release stops recording → Processing
    let action = h.process(Command::Input {
        binding_id: "transcribe".into(),
        hotkey_string: "F6".into(),
        is_pressed: false,
        push_to_talk: true,
    });
    assert!(matches!(action, StageAction::StopRecording { .. }));
    assert!(matches!(h.state(), AppState::Processing { .. }));
}

// ── Test 12: Debounce — rapid press events are ignored ──

#[test]
fn test_debounce_ignores_rapid_presses() {
    let mut h = TestHarness::new();

    // First press → starts recording
    let action = h.process(Command::Input {
        binding_id: "transcribe".into(),
        hotkey_string: "Cmd+Shift+S".into(),
        is_pressed: true,
        push_to_talk: false,
    });
    assert!(matches!(action, StageAction::StartRecording { .. }));

    // Immediate second press (without clearing debounce) → debounced
    // NOTE: Unlike other tests, this one intentionally does NOT clear
    // debounce between presses to verify the debounce logic works.
    let action = h.core.process_command(
        Command::Input {
            binding_id: "transcribe".into(),
            hotkey_string: "Cmd+Shift+S".into(),
            is_pressed: true,
            push_to_talk: false,
        },
        &h.active_use,
        &h.current_state,
    );
    assert!(matches!(action, StageAction::None));
}

// ── Test 13: Cancel on already-Idle is a no-op ──

#[test]
fn test_cancel_on_idle_is_noop() {
    let mut h = TestHarness::new();

    // Cancel when already Idle — recording_was_active=false, stage is Idle
    h.process(Command::Cancel {
        recording_was_active: false,
    });
    assert!(matches!(h.state(), AppState::Idle));
    assert!(!h.is_active());
}

// ── Test 14: ProcessingFinished on Idle is a no-op ──

#[test]
fn test_processing_finished_on_idle_is_noop() {
    let mut h = TestHarness::new();

    h.process(Command::ProcessingFinished);
    assert!(matches!(h.state(), AppState::Idle));
    assert!(!h.is_active());
}

// ── Test 15: ProcessingFinished on Recording is a no-op ──
// ProcessingFinished should only transition from Processing to Idle,
// not from Recording.

#[test]
fn test_processing_finished_on_recording_is_noop() {
    let mut h = TestHarness::new();

    h.process(Command::Input {
        binding_id: "transcribe".into(),
        hotkey_string: "Cmd+Shift+S".into(),
        is_pressed: true,
        push_to_talk: false,
    });
    assert!(matches!(h.state(), AppState::Recording { .. }));

    // ProcessingFinished should NOT transition from Recording to Idle
    h.process(Command::ProcessingFinished);
    assert!(matches!(h.state(), AppState::Recording { .. }));
    assert!(h.is_active());
}

// ── Test 16: Stage::is_active correctness ──

#[test]
fn test_stage_is_active() {
    use std::time::Instant;

    let idle = Stage::Idle;
    assert!(!idle.is_active());

    let recording = Stage::Recording("transcribe".into());
    assert!(recording.is_active());

    let processing = Stage::Processing {
        since: Instant::now(),
        binding_id: Some("transcribe".into()),
    };
    assert!(processing.is_active());
}

// ── Test 17: Stage::to_app_state correctness ──

#[test]
fn test_stage_to_app_state() {
    use std::time::Instant;

    let idle = Stage::Idle;
    assert!(matches!(idle.to_app_state(), AppState::Idle));

    let recording = Stage::Recording("transcribe".into());
    assert!(matches!(recording.to_app_state(), AppState::Recording { .. }));

    let processing = Stage::Processing {
        since: Instant::now(),
        binding_id: Some("transcribe".into()),
    };
    assert!(matches!(processing.to_app_state(), AppState::Processing { .. }));
}

// ── Test 18: is_transcribe_binding helper ──

#[test]
fn test_is_transcribe_binding() {
    assert!(is_transcribe_binding("transcribe"));
    assert!(is_transcribe_binding("transcribe_with_post_process"));
    assert!(is_transcribe_binding("transcribe_with_router"));
    assert!(!is_transcribe_binding("other"));
    assert!(!is_transcribe_binding(""));
}

// ── Test 19: CancelSignal Default trait ──

#[test]
fn test_cancel_signal_default() {
    let signal = CancelSignal::default();
    assert!(!signal.consume_cancel());
}

// ── Test 20: SetProcessingWithBinding from Idle ──
// SetProcessingWithBinding can be called from any state; it always
// transitions to Processing (the router uses this after confirmation).

#[test]
fn test_set_processing_with_binding_from_idle() {
    let mut h = TestHarness::new();

    h.process(Command::SetProcessingWithBinding {
        binding_id: Some("transcribe_with_router".into()),
    });
    assert!(matches!(h.state(), AppState::Processing { .. }));
    assert!(h.is_active());
}

// ── Test 21: CoordinatorCore recv_timeout returns correct durations ──

#[test]
fn test_recv_timeout_idle_returns_none() {
    let h = TestHarness::new();
    // In Idle state, recv_timeout should return None (no timeout needed)
    let timeout = h.core.recv_timeout();
    assert!(timeout.is_none());
}

#[test]
fn test_recv_timeout_processing_returns_some() {
    let mut h = TestHarness::with_timeout(Duration::from_secs(30));

    // Enter Processing via: Idle → Recording → Processing
    h.process(Command::Input {
        binding_id: "transcribe".into(),
        hotkey_string: "Cmd+Shift+S".into(),
        is_pressed: true,
        push_to_talk: false,
    });
    h.process(Command::Input {
        binding_id: "transcribe".into(),
        hotkey_string: "Cmd+Shift+S".into(),
        is_pressed: true,
        push_to_talk: false,
    });

    // Should return Some duration (close to 30s)
    let timeout = h.core.recv_timeout();
    assert!(timeout.is_some());
    // Should be less than the full timeout since some time has elapsed
    assert!(timeout.unwrap() <= Duration::from_secs(30));
}

// ── Test 22: CancelSignal::flag() returns independent handle ──

#[test]
fn test_cancel_signal_flag_shares_state() {
    let signal = CancelSignal::new();
    let flag = signal.flag();

    // Setting via the signal should be visible via the flag
    signal.send_cancel();
    assert!(flag.load(Ordering::SeqCst));

    // Consuming via the signal clears it for both
    assert!(signal.consume_cancel());
    assert!(!flag.load(Ordering::SeqCst));
}

// ── Test 23: AppState PartialEq for assertions ──

#[test]
fn test_app_state_equality() {
    assert_eq!(AppState::Idle, AppState::Idle);
    assert_ne!(AppState::Idle, AppState::Recording { binding_id: "x".into() });
    assert_eq!(
        AppState::Recording { binding_id: "transcribe".into() },
        AppState::Recording { binding_id: "transcribe".into() }
    );
}

// ── Test 24: Processing timeout — actual elapsed timeout ──
// Verify that is_processing_expired() returns true after the timeout
// duration has passed.

#[test]
fn test_processing_expired_after_timeout() {
    let mut h = TestHarness::with_timeout(Duration::from_millis(50));

    // Enter Processing
    h.process(Command::Input {
        binding_id: "transcribe".into(),
        hotkey_string: "Cmd+Shift+S".into(),
        is_pressed: true,
        push_to_talk: false,
    });
    h.process(Command::Input {
        binding_id: "transcribe".into(),
        hotkey_string: "Cmd+Shift+S".into(),
        is_pressed: true,
        push_to_talk: false,
    });
    assert!(!h.core.is_processing_expired());

    // Wait for the timeout to elapse
    std::thread::sleep(Duration::from_millis(80));
    assert!(h.core.is_processing_expired());
}

// ── Test 25: Processing timeout resets on SetProcessingWithBinding ──

#[test]
fn test_set_processing_with_binding_resets_timer() {
    let mut h = TestHarness::with_timeout(Duration::from_millis(100));

    // Enter Processing
    h.process(Command::Input {
        binding_id: "transcribe".into(),
        hotkey_string: "Cmd+Shift+S".into(),
        is_pressed: true,
        push_to_talk: false,
    });
    h.process(Command::Input {
        binding_id: "transcribe".into(),
        hotkey_string: "Cmd+Shift+S".into(),
        is_pressed: true,
        push_to_talk: false,
    });

    // Wait most of the timeout
    std::thread::sleep(Duration::from_millis(70));

    // SetProcessingWithBinding resets the timer
    h.process(Command::SetProcessingWithBinding {
        binding_id: Some("transcribe_with_router".into()),
    });
    assert!(!h.core.is_processing_expired());

    // After the full timeout from the reset, it should be expired
    std::thread::sleep(Duration::from_millis(120));
    assert!(h.core.is_processing_expired());
}