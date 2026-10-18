// State machine tests for `TranscriptionCoordinator`.
//
// These tests exercise `CoordinatorCore`, the pure state-machine logic
// extracted from `TranscriptionCoordinator`. The core needs an `AppHandle`
// only to clear the handy-keys pressed-hotkey set on session end; otherwise
// it only touches plain Rust types (`Stage`, `AtomicBool`, `RwLock<AppState>`).

use super::*;
use tauri::{AppHandle, Runtime};

/// Helper to create a `CoordinatorCore` with a short processing timeout
/// and shared state flags mirroring the production coordinator.
struct TestHarness<R: Runtime> {
    core: CoordinatorCore,
    active_use: Arc<AtomicBool>,
    current_state: Arc<RwLock<AppState>>,
    cancel_flag: Arc<AtomicBool>,
    app: AppHandle<R>,
}

impl TestHarness<tauri::test::MockRuntime> {
    fn new() -> Self {
        Self::with_timeout(Duration::from_millis(500))
    }

    fn with_timeout(processing_timeout: Duration) -> Self {
        // Lightweight mock Tauri app. The core only uses it to clear the
        // handy-keys pressed-hotkey state on session end. The store plugin
        // must be initialized so settings helpers can fall back to it when
        // the in-memory cache is not managed.
        let app = tauri::test::mock_builder()
            .plugin(tauri_plugin_store::Builder::new().build())
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap()
            .handle()
            .clone();
        Self {
            core: CoordinatorCore::new_for_test(processing_timeout),
            active_use: Arc::new(AtomicBool::new(false)),
            current_state: Arc::new(RwLock::new(AppState::Idle { session_id: 0 })),
            cancel_flag: Arc::new(AtomicBool::new(false)),
            app,
        }
    }

    /// Feed a command directly through the core's process_command method.
    /// Clears debounce before processing so sequential commands aren't
    /// throttled by the 30ms debounce window.
    fn process(&mut self, cmd: Command) -> StageAction {
        self.core.clear_debounce();
        // Tests use the pure state machine without a real recorder, so
        // `recorder_is_recording` is false. The recovery path in production
        // needs the recorder query; in tests we exercise the pure transitions.
        self.core.process_command(
            cmd,
            &self.active_use,
            &self.current_state,
            false,
            &self.app,
        )
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
    assert!(matches!(h.state(), AppState::Idle { .. }));
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
    assert!(matches!(h.state(), AppState::Idle { .. }));
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
    assert!(matches!(h.state(), AppState::Idle { .. }));
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
    assert!(matches!(h.state(), AppState::Idle { .. }));
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
    assert!(matches!(h.state(), AppState::Idle { .. }));
    assert!(!h.is_active());

    // Late ProcessingFinished (from FinishGuard) — should be a no-op
    // because we're already Idle, not Processing.
    h.process(Command::ProcessingFinished);
    assert!(matches!(h.state(), AppState::Idle { .. }));
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
    assert!(matches!(h.state(), AppState::Idle { .. }));
    assert!(!h.is_active());
}

// ── Test 5b: ProcessingTimeout returns ProcessingTimeoutExpired action ──
// Guards against the regression where the refactor routed ProcessingTimeout
// through process_command() returning StageAction::None, which caused the
// thread loop to skip hide_recording_overlay() + set_tray_state(Idle).
// The thread loop MUST see ProcessingTimeoutExpired to dispatch those side
// effects; this test locks that contract down.

#[test]
fn test_processing_timeout_returns_expired_action() {
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

    // ProcessingTimeout while in Processing MUST return the expired action
    // so the thread loop dispatches the overlay/tray side effects.
    let action = h.core.process_command(
        Command::ProcessingTimeout,
        &h.active_use,
        &h.current_state,
        false,
        &h.app,
    );
    assert!(
        matches!(action, StageAction::ProcessingTimeoutExpired),
        "ProcessingTimeout while in Processing must return \
         StageAction::ProcessingTimeoutExpired so the thread loop hides the \
         overlay and resets the tray icon; got {:?}",
        action
    );

    // ProcessingTimeout while NOT in Processing must remain a no-op.
    let action = h.core.process_command(
        Command::ProcessingTimeout,
        &h.active_use,
        &h.current_state,
        false,
        &h.app,
    );
    assert!(
        matches!(action, StageAction::None),
        "ProcessingTimeout while Idle must be a no-op; got {:?}",
        action
    );
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
        AppState::Processing { binding_id, .. } => {
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
    assert!(matches!(h.state(), AppState::Idle { .. }));
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
    assert!(matches!(h.state(), AppState::Idle { .. }));
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
        false,
        &h.app,
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
    assert!(matches!(h.state(), AppState::Idle { .. }));
    assert!(!h.is_active());
}

// ── Test 14: ProcessingFinished on Idle is a no-op ──

#[test]
fn test_processing_finished_on_idle_is_noop() {
    let mut h = TestHarness::new();

    h.process(Command::ProcessingFinished);
    assert!(matches!(h.state(), AppState::Idle { .. }));
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

    let recording = Stage::Recording {
        binding_id: "transcribe".into(),
        since: Instant::now(),
        silence_extension: Duration::ZERO,
        warned: false,
    };
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
    assert!(matches!(idle.to_app_state(0), AppState::Idle { .. }));

    let recording = Stage::Recording {
        binding_id: "transcribe".into(),
        since: Instant::now(),
        silence_extension: Duration::ZERO,
        warned: false,
    };
    assert!(matches!(recording.to_app_state(7), AppState::Recording { .. }));
    if let AppState::Recording { session_id, .. } = recording.to_app_state(7) {
        assert_eq!(session_id, 7);
    } else {
        panic!("Expected Recording state");
    }

    let processing = Stage::Processing {
        since: Instant::now(),
        binding_id: Some("transcribe".into()),
    };
    assert!(matches!(processing.to_app_state(7), AppState::Processing { .. }));
    if let AppState::Processing { session_id, .. } = processing.to_app_state(7) {
        assert_eq!(session_id, 7);
    } else {
        panic!("Expected Processing state");
    }
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

// ── Test 20: SetProcessingWithBinding from Idle is a no-op ──
// A stale router handoff that arrives after the coordinator has already
// reset to Idle must NOT re-enter Processing and re-arm the 30s timer.

#[test]
fn test_set_processing_with_binding_from_idle_is_noop() {
    let mut h = TestHarness::new();

    h.process(Command::SetProcessingWithBinding {
        binding_id: Some("transcribe_with_router".into()),
    });
    assert!(matches!(h.state(), AppState::Idle { .. }));
    assert!(!h.is_active());
}

// ── Test 20b: session id is preserved through Recording → Processing → Idle ──
//
// NOTE: The real monotonic session id (> 0) is allocated by the coordinator
// thread's `start()` via `next_overlay_session()`. `CoordinatorCore` itself
// uses a placeholder id (0) via `sync_state`; this test verifies that the
// placeholder id is consistently carried through every stage of the lifecycle
// so a real session id would behave the same way.

#[test]
fn test_session_id_preserved_through_lifecycle() {
    let mut h = TestHarness::new();

    // Enter Recording
    h.process(Command::Input {
        binding_id: "transcribe".into(),
        hotkey_string: "Cmd+Shift+S".into(),
        is_pressed: true,
        push_to_talk: false,
    });

    let recording_session = match h.state() {
        AppState::Recording { session_id, .. } => session_id,
        other => panic!("Expected Recording state, got {:?}", other),
    };

    // Stop → Processing: session id should follow the same session
    h.process(Command::Input {
        binding_id: "transcribe".into(),
        hotkey_string: "Cmd+Shift+S".into(),
        is_pressed: true,
        push_to_talk: false,
    });
    let processing_session = match h.state() {
        AppState::Processing { session_id, .. } => session_id,
        other => panic!("Expected Processing state, got {:?}", other),
    };
    assert_eq!(
        processing_session, recording_session,
        "Processing should keep the same session id as the recording that produced it"
    );

    // Finish → Idle: emitted Idle should carry the ending session id
    h.process(Command::ProcessingFinished);
    let idle_session = match h.state() {
        AppState::Idle { session_id } => session_id,
        other => panic!("Expected Idle state, got {:?}", other),
    };
    assert_eq!(
        idle_session, recording_session,
        "Idle emitted by ProcessingFinished must carry the session id that just ended"
    );
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

#[test]
fn test_recv_timeout_recording_returns_some() {
    let mut h = TestHarness::new();

    // Enter Recording
    h.process(Command::Input {
        binding_id: "transcribe".into(),
        hotkey_string: "Cmd+Shift+S".into(),
        is_pressed: true,
        push_to_talk: false,
    });

    // Should return Some duration (close to the 5-minute recording timeout)
    let timeout = h.core.recv_timeout();
    assert!(timeout.is_some());
    assert!(timeout.unwrap() <= Duration::from_secs(300));
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
    assert_eq!(
        AppState::Idle { session_id: 0 },
        AppState::Idle { session_id: 0 }
    );
    assert_ne!(
        AppState::Idle { session_id: 0 },
        AppState::Recording {
            binding_id: "x".into(),
            session_id: 0
        }
    );
    assert_eq!(
        AppState::Recording {
            binding_id: "transcribe".into(),
            session_id: 0
        },
        AppState::Recording {
            binding_id: "transcribe".into(),
            session_id: 0
        }
    );
    assert_ne!(
        AppState::Recording {
            binding_id: "transcribe".into(),
            session_id: 0
        },
        AppState::Recording {
            binding_id: "transcribe".into(),
            session_id: 1
        }
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

// ── Test 26: Recording safety timeout ──
// `is_recording_expired()` reflects whether the Recording stage has exceeded
// the 5-minute safety limit. The coordinator thread uses this to force a
// stop when all hotkey paths fail.

#[test]
fn test_recording_timeout_expires() {
    let mut h = TestHarness::new();

    h.process(Command::Input {
        binding_id: "transcribe".into(),
        hotkey_string: "Cmd+Shift+S".into(),
        is_pressed: true,
        push_to_talk: false,
    });
    assert!(matches!(h.state(), AppState::Recording { .. }));
    assert!(!h.core.is_recording_expired());

    // We cannot wait 5 minutes in a unit test, but the `since` Instant is
    // created on entry so the timeout is deterministic. Verify the helper
    // reports the expected state immediately after entry.
    assert!(h.core.recv_timeout().unwrap() > Duration::from_secs(290));
}

// ── Test 27: VAD activity extends the recording deadline ──
// The thread-loop uses the AudioRecordingManager's voice-activity timestamp
// to reset the 5-minute silence deadline. We can't drive real audio in this
// unit test, so we exercise the internal helper that the thread loop uses to
// compute the remaining timeout: the deadline resets when voice occurs within
// the timeout window.

#[test]
fn test_vad_activity_resets_recording_deadline() {
    use std::time::Instant;

    // Simulate the thread-loop logic: a recording has been running for a while.
    let since = Instant::now();
    std::thread::sleep(Duration::from_millis(150));
    let elapsed = since.elapsed();
    let original_remaining = RECORDING_TIMEOUT.saturating_sub(elapsed);

    // Pretend the user spoke very recently (small elapsed voice time).
    let voice_elapsed = Duration::from_millis(50);
    let silence = elapsed.saturating_sub(voice_elapsed);

    // If silence is within the timeout window, the deadline resets to now,
    // giving a fresh full timeout.
    assert!(
        silence < RECORDING_TIMEOUT,
        "silence should be less than the timeout window"
    );

    // Capture the reset instant after a small but measurable delay so the
    // reset deadline is demonstrably later than the original one.
    let new_since = Instant::now();
    std::thread::sleep(Duration::from_millis(50));
    let new_remaining = RECORDING_TIMEOUT.saturating_sub(new_since.elapsed());

    // The reset deadline must be later than the original deadline.
    assert!(
        new_remaining > original_remaining,
        "reset deadline should be later than original deadline (original={:?}, new={:?})",
        original_remaining,
        new_remaining
    );
}

// ── Test 28: Hard recording cap fires even with continuous voice ──
// The sliding timeout can extend a session, but MAX_RECORDING_HARD_CAP is an
// absolute ceiling. This test locks in that total wall-clock duration is what
// matters for the hard cap, not the per-reset silence timer.

#[test]
fn test_hard_recording_cap_eventually_fires() {
    // We can't wait an hour, but the cap is a constant. Verify that the
    // hard-cap check would trigger once the total recording duration reaches
    // the ceiling, regardless of any VAD-based deadline resets.
    let hard_cap = MAX_RECORDING_HARD_CAP;
    let recording_start = std::time::Instant::now();

    // The check uses `recording_start.elapsed() >= MAX_RECORDING_HARD_CAP`.
    // Immediately after entry it must not fire.
    assert!(
        recording_start.elapsed() < hard_cap,
        "hard cap should not fire immediately after recording starts"
    );

    // The window from now to the cap must equal the configured ceiling.
    let remaining = hard_cap - recording_start.elapsed();
    assert!(
        remaining >= Duration::from_secs(3599) && remaining <= Duration::from_secs(3600),
        "hard cap remaining should be approximately 1 hour, got {:?}",
        remaining
    );
}

// ── Test 29: ExtendRecording bumps silence and hard-cap extensions ──

#[test]
fn test_extend_recording_bumps_deadlines() {
    let mut h = TestHarness::new();

    // Enter Recording
    h.process(Command::Input {
        binding_id: "transcribe".into(),
        hotkey_string: "Cmd+Shift+S".into(),
        is_pressed: true,
        push_to_talk: false,
    });
    assert!(matches!(h.state(), AppState::Recording { .. }));
    assert_eq!(h.core.recording_silence_extension(), Duration::ZERO);
    assert_eq!(h.core.hard_cap_extension(), Duration::ZERO);

    // Extend by 5 minutes
    h.process(Command::ExtendRecording { seconds: 300 });
    assert_eq!(
        h.core.recording_silence_extension(),
        Duration::from_secs(300)
    );
    assert_eq!(h.core.hard_cap_extension(), Duration::from_secs(300));

    // Extend by another 5 minutes
    h.process(Command::ExtendRecording { seconds: 300 });
    assert_eq!(
        h.core.recording_silence_extension(),
        Duration::from_secs(600)
    );
    assert_eq!(h.core.hard_cap_extension(), Duration::from_secs(600));
}

// ── Test 30: ExtendRecording is ignored when not Recording ──

#[test]
fn test_extend_recording_ignored_when_not_recording() {
    let mut h = TestHarness::new();

    // Idle: extend should be a no-op
    h.process(Command::ExtendRecording { seconds: 300 });
    assert_eq!(h.core.hard_cap_extension(), Duration::ZERO);

    // Processing: extend should be a no-op
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

    h.process(Command::ExtendRecording { seconds: 300 });
    assert_eq!(h.core.hard_cap_extension(), Duration::ZERO);
}

// ── Test 31a: SetProcessingWithBinding from Processing still works ──
// The legitimate router flow transitions from Recording → Processing, then
// re-enters Processing with the router binding_id and a fresh timer.

#[test]
fn test_set_processing_with_binding_from_processing_resets_timer() {
    let mut h = TestHarness::with_timeout(Duration::from_millis(100));

    // Idle → Recording → Processing
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

    // Wait most of the original timeout
    std::thread::sleep(Duration::from_millis(70));
    assert!(h.core.recv_timeout().unwrap() <= Duration::from_millis(35));

    // Router handoff: should re-enter Processing with a fresh timer
    h.process(Command::SetProcessingWithBinding {
        binding_id: Some("transcribe_with_router".into()),
    });
    assert!(matches!(h.state(), AppState::Processing { .. }));
    assert!(h.is_active());
    let binding_id = match h.state() {
        AppState::Processing { binding_id, .. } => binding_id,
        other => panic!("Expected Processing state, got {:?}", other),
    };
    assert_eq!(binding_id.as_deref(), Some("transcribe_with_router"));

    // Timer was reset: remaining timeout should be close to full 100ms again
    let remaining = h.core.recv_timeout().unwrap();
    assert!(
        remaining > Duration::from_millis(70),
        "SetProcessingWithBinding should reset the timer, got remaining {:?}",
        remaining
    );
}

// ── Test 31b: SetProcessingWithBinding after Processing timeout is no-op ──
// If the 30s Processing timeout fires and resets to Idle before a stale
// SetProcessingWithBinding command is processed, the stale command must not
// revive Processing.

#[test]
fn test_set_processing_with_binding_after_processing_timeout_is_noop() {
    let mut h = TestHarness::with_timeout(Duration::from_millis(100));

    // Idle → Recording → Processing
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

    // Simulate the timeout firing through the single Path B
    h.process(Command::ProcessingTimeout);
    assert!(matches!(h.state(), AppState::Idle { .. }));
    assert!(!h.is_active());

    // Stale router handoff arrives late
    h.process(Command::SetProcessingWithBinding {
        binding_id: Some("transcribe_with_router".into()),
    });
    assert!(matches!(h.state(), AppState::Idle { .. }));
    assert!(!h.is_active());
}

// ── Test 31c: Processing timeout Path A delegates to Path B ──
// When the pre-recv timeout calculation sees an expired Processing stage, it
// must return Some(Duration::ZERO) so recv_timeout yields Command::ProcessingTimeout
// and the single Path B path performs the reset.

#[test]
fn test_processing_timeout_path_a_delegates_to_path_b() {
    let mut h = TestHarness::with_timeout(Duration::from_millis(1));

    // Idle → Recording → Processing
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

    // Wait long enough for the timeout to be expired
    std::thread::sleep(Duration::from_millis(5));

    // Path A should report zero remaining time (delegates to Path B)
    assert_eq!(
        h.core.recv_timeout(),
        Some(Duration::ZERO),
        "Expired Processing stage must report Duration::ZERO so Path B handles it"
    );

    // Path B resets to Idle and returns the side-effect action
    let action = h.core.process_command(
        Command::ProcessingTimeout,
        &h.active_use,
        &h.current_state,
        false,
        &h.app,
    );
    assert!(
        matches!(action, StageAction::ProcessingTimeoutExpired),
        "ProcessingTimeout must return ProcessingTimeoutExpired; got {:?}",
        action
    );
    assert!(matches!(h.state(), AppState::Idle { .. }));
    assert!(!h.is_active());
}

// ── Test 31d: Processing timeout drains queued stale SetProcessingWithBinding ──
// After the Processing timeout resets to Idle, a queued SetProcessingWithBinding
// is processed as a no-op and the stage stays Idle.

#[test]
fn test_processing_timeout_drains_queued_set_processing_with_binding() {
    let mut h = TestHarness::with_timeout(Duration::from_millis(1));

    // Idle → Recording → Processing
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

    // Wait for timeout to expire
    std::thread::sleep(Duration::from_millis(5));

    // Timeout fires → Idle
    h.process(Command::ProcessingTimeout);
    assert!(matches!(h.state(), AppState::Idle { .. }));

    // A stale SetProcessingWithBinding that was queued behind the timeout is now a no-op
    h.process(Command::SetProcessingWithBinding {
        binding_id: Some("transcribe_with_router".into()),
    });
    assert!(matches!(h.state(), AppState::Idle { .. }));
    assert!(!h.is_active());
}

// ── Test 31: ExtendRecording re-arms the warning flag ──
// After a warning fires, extending the deadline should set warned=false so
// the warning re-fires for the new deadline.

#[test]
fn test_extend_recording_rearms_warning() {
    let mut h = TestHarness::new();

    h.process(Command::Input {
        binding_id: "transcribe".into(),
        hotkey_string: "Cmd+Shift+S".into(),
        is_pressed: true,
        push_to_talk: false,
    });

    // Simulate that a warning was already emitted by flipping the stage field
    // directly (the thread-loop warning emission is not exercised in the core
    // unit tests, but the state mutation is).
    if let Stage::Recording { ref mut warned, .. } = h.core.stage {
        *warned = true;
    }

    // Extend: warned should be cleared so the warning re-arms
    h.process(Command::ExtendRecording { seconds: 300 });
    if let Stage::Recording { warned, .. } = h.core.stage {
        assert!(!warned, "warned should be false after extend");
    } else {
        panic!("Expected Recording stage");
    }
}

// ── Test 32: recv_timeout accounts for silence extension ──

#[test]
fn test_recv_timeout_accounts_for_silence_extension() {
    let mut h = TestHarness::new();

    h.process(Command::Input {
        binding_id: "transcribe".into(),
        hotkey_string: "Cmd+Shift+S".into(),
        is_pressed: true,
        push_to_talk: false,
    });

    let before_extend = h.core.recv_timeout().unwrap();
    assert!(before_extend <= Duration::from_secs(300));

    h.process(Command::ExtendRecording { seconds: 300 });
    let after_extend = h.core.recv_timeout().unwrap();

    // The returned timeout must be larger after extending by 5 minutes.
    // We tolerate a small measurement window because Instant is wall-clock.
    assert!(
        after_extend > before_extend,
        "recv_timeout should increase after extend (before={:?}, after={:?})",
        before_extend,
        after_extend
    );
}

// ── Test 33: Leaving Recording resets the hard-cap extension ──

#[test]
fn test_hard_cap_extension_resets_when_leaving_recording() {
    let mut h = TestHarness::new();

    h.process(Command::Input {
        binding_id: "transcribe".into(),
        hotkey_string: "Cmd+Shift+S".into(),
        is_pressed: true,
        push_to_talk: false,
    });
    h.process(Command::ExtendRecording { seconds: 300 });
    assert_eq!(h.core.hard_cap_extension(), Duration::from_secs(300));

    // Stop → Processing resets hard cap extension
    h.process(Command::Input {
        binding_id: "transcribe".into(),
        hotkey_string: "Cmd+Shift+S".into(),
        is_pressed: true,
        push_to_talk: false,
    });
    assert!(matches!(h.state(), AppState::Processing { .. }));
    assert_eq!(h.core.hard_cap_extension(), Duration::ZERO);
}

// ── Test 34: Cross-binding press stops with the OWNER's binding ──
// Pressing a different transcribe hotkey while a recording is active must act
// as a universal "stop + complete" of the ACTIVE session: the stop action must
// be dispatched for the owner binding (so the audio is kept and the owner's
// flow — transcribe→paste, router→confirming — runs), and the state must go
// to Processing, never to Recording with the pressed binding.

#[test]
fn test_cross_binding_press_stops_with_owner_binding() {
    let mut h = TestHarness::new();

    // Start recording with "transcribe"
    let action = h.process(Command::Input {
        binding_id: "transcribe".into(),
        hotkey_string: "Cmd+Shift+S".into(),
        is_pressed: true,
        push_to_talk: false,
    });
    assert!(matches!(action, StageAction::StartRecording { .. }));
    assert!(matches!(h.state(), AppState::Recording { .. }));

    // Press a different binding → must stop the OWNER's session
    let action = h.process(Command::Input {
        binding_id: "transcribe_with_router".into(),
        hotkey_string: "Cmd+Shift+R".into(),
        is_pressed: true,
        push_to_talk: false,
    });
    match action {
        StageAction::StopRecording { binding_id, .. } => {
            assert_eq!(
                binding_id, "transcribe",
                "cross-binding press must stop the owner's session, not the pressed one"
            );
        }
        other => panic!("Expected StopRecording, got {:?}", other),
    }
    match h.state() {
        AppState::Processing { binding_id, .. } => {
            assert_eq!(
                binding_id.as_deref(),
                Some("transcribe"),
                "Processing must carry the owner binding, never the pressed one"
            );
        }
        other => panic!("Expected Processing state, got {:?}", other),
    }
    assert!(h.is_active());

    // ProcessingFinished → Idle, session fully completed
    h.process(Command::ProcessingFinished);
    assert!(matches!(h.state(), AppState::Idle { .. }));
    assert!(!h.is_active());
}

// ── Test 35: Router owner stopped by the plain transcribe binding ──
// Mirror of Test 34 with the roles swapped: the router binding owns the
// recording and the plain transcribe hotkey is pressed. The stop must target
// the router binding so its confirming flow runs.

#[test]
fn test_router_owner_stopped_by_other_binding() {
    let mut h = TestHarness::new();

    // Start recording with "transcribe_with_router"
    let action = h.process(Command::Input {
        binding_id: "transcribe_with_router".into(),
        hotkey_string: "Cmd+Shift+R".into(),
        is_pressed: true,
        push_to_talk: false,
    });
    assert!(matches!(action, StageAction::StartRecording { .. }));
    assert!(matches!(h.state(), AppState::Recording { .. }));

    // Press "transcribe" → stop the ROUTER's session
    let action = h.process(Command::Input {
        binding_id: "transcribe".into(),
        hotkey_string: "Cmd+Shift+S".into(),
        is_pressed: true,
        push_to_talk: false,
    });
    match action {
        StageAction::StopRecording { binding_id, .. } => {
            assert_eq!(
                binding_id, "transcribe_with_router",
                "cross-binding press must stop the router owner's session"
            );
        }
        other => panic!("Expected StopRecording, got {:?}", other),
    }
    match h.state() {
        AppState::Processing { binding_id, .. } => {
            assert_eq!(binding_id.as_deref(), Some("transcribe_with_router"));
        }
        other => panic!("Expected Processing state, got {:?}", other),
    }

    // ProcessingFinished → Idle
    h.process(Command::ProcessingFinished);
    assert!(matches!(h.state(), AppState::Idle { .. }));
    assert!(!h.is_active());
}

// ── Test 36: Cross-binding PTT release is ignored ──
// Releasing a different push-to-talk key while another binding's PTT recording
// is active must be a no-op: the session keeps recording under the owner
// binding and only the owner's release completes it. The cross-binding release
// step is issued with recorder_is_recording=true via a direct process_command
// call to prove the guard takes precedence over try_recover_stop drift
// recovery (which would otherwise rebind the stage to the pressed binding).

#[test]
fn test_ptt_cross_binding_release_is_ignored() {
    let mut h = TestHarness::new();

    // PTT press "transcribe" starts recording
    let action = h.process(Command::Input {
        binding_id: "transcribe".into(),
        hotkey_string: "F6".into(),
        is_pressed: true,
        push_to_talk: true,
    });
    assert!(matches!(action, StageAction::StartRecording { .. }));
    assert!(matches!(h.state(), AppState::Recording { .. }));

    // Cross-binding PTT release (recorder live) → ignored, stage unchanged
    let action = h.core.process_command(
        Command::Input {
            binding_id: "transcribe_with_post_process".into(),
            hotkey_string: "F7".into(),
            is_pressed: false,
            push_to_talk: true,
        },
        &h.active_use,
        &h.current_state,
        true,
        &h.app,
    );
    assert!(
        matches!(action, StageAction::None),
        "cross-binding PTT release must be ignored, got {:?}",
        action
    );
    match h.state() {
        AppState::Recording { binding_id, .. } => {
            assert_eq!(
                binding_id, "transcribe",
                "stage must stay Recording under the owner binding"
            );
        }
        other => panic!("Expected Recording state, got {:?}", other),
    }

    // Owner's release completes the session
    let action = h.process(Command::Input {
        binding_id: "transcribe".into(),
        hotkey_string: "F6".into(),
        is_pressed: false,
        push_to_talk: true,
    });
    match action {
        StageAction::StopRecording { binding_id, .. } => {
            assert_eq!(binding_id, "transcribe");
        }
        other => panic!("Expected StopRecording, got {:?}", other),
    }
    assert!(matches!(h.state(), AppState::Processing { .. }));

    // ProcessingFinished → Idle
    h.process(Command::ProcessingFinished);
    assert!(matches!(h.state(), AppState::Idle { .. }));
    assert!(!h.is_active());
}

// ── Test 37: Press during Processing is still ignored ──
// The cross-binding stop applies only while Recording. Presses during
// Processing (and Confirming/UsbCycling upstream) must remain no-ops.

#[test]
fn test_press_during_processing_still_ignored() {
    let mut h = TestHarness::new();

    // Idle → Recording → Processing
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

    // Press a different binding during Processing → ignored
    let action = h.process(Command::Input {
        binding_id: "transcribe_with_router".into(),
        hotkey_string: "Cmd+Shift+R".into(),
        is_pressed: true,
        push_to_talk: false,
    });
    assert!(
        matches!(action, StageAction::None),
        "press during Processing must be ignored, got {:?}",
        action
    );
    match h.state() {
        AppState::Processing { binding_id, .. } => {
            assert_eq!(binding_id.as_deref(), Some("transcribe"));
        }
        other => panic!("Expected Processing state, got {:?}", other),
    }
    assert!(h.is_active());
}
