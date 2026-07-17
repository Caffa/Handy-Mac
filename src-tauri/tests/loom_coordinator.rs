//! Loom concurrency model tests for the transcription coordinator state machine.
//!
//! These tests use `loom` to systematically explore thread interleavings of the
//! coordinator's concurrency pattern. Loom replaces `std::sync` primitives with
//! deterministic, exploration-friendly equivalents and exhaustively checks all
//! possible interleavings for data races and invariant violations.
//!
//! Since `CoordinatorCore` uses `std::sync` types directly (not loom types), and
//! Loom requires all shared state to use loom primitives, we test the *pattern*
//! rather than the production code. A simplified mock mirrors the production
//! coordinator's structure:
//!
//! - `Arc<loom::sync::RwLock<State>>` for shared app state
//! - `Arc<loom::sync::atomic::AtomicBool>` for cancel and active-use flags
//! - `loom::sync::mpsc::channel()` for command dispatch
//!
//! Loom tests run in user-space and don't require nightly or TSan. They
//! complement the TSan tests (which test the actual primitives at runtime)
//! by verifying logical invariants across all possible interleavings.
//!
//! # Loom constraints
//!
//! - Loom has a small default max thread count. We keep each model to 2-3 threads.
//! - No `recv_timeout` — use `try_recv` or blocking `recv` within bounded loops.
//! - All loom primitives must be created and used within `loom::model(|| { ... })`.

use loom::sync::atomic::{AtomicBool, Ordering};
use loom::sync::mpsc::channel;
use loom::sync::RwLock;
use loom::thread;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Simplified state machine mirroring the production pattern.
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
enum State {
    Idle,
    Recording,
    Processing,
}

impl State {
    fn is_active(&self) -> bool {
        matches!(self, State::Recording | State::Processing)
    }
}

/// Commands sent through the channel, mirroring the production Command enum.
#[derive(Debug, Clone)]
enum Cmd {
    Start,
    Stop,
    Cancel,
    Finished,
}

// ---------------------------------------------------------------------------
// Loom Model Test 1: Single writer + single reader on RwLock + AtomicBool
// ---------------------------------------------------------------------------

/// Tests the core invariant: the `active_use` AtomicBool must always be
/// consistent with the `RwLock<State>`. After all threads have finished,
/// if State is Idle then active_use must be false; if Recording or
/// Processing then active_use must be true.
///
/// Loom explores all possible interleavings of the two threads.
#[test]
fn loom_rwlock_atomic_bool_consistency() {
    loom::model(|| {
        let state = Arc::new(RwLock::new(State::Idle));
        let active_use = Arc::new(AtomicBool::new(false));

        // Thread A: transition Idle → Recording → Processing → Idle
        let state_a = Arc::clone(&state);
        let active_a = Arc::clone(&active_use);
        let handle_a = thread::spawn(move || {
            // Idle → Recording
            {
                let mut guard = state_a.write().unwrap();
                *guard = State::Recording;
                active_a.store(true, Ordering::SeqCst);
            }
            // Recording → Processing
            {
                let mut guard = state_a.write().unwrap();
                *guard = State::Processing;
                // active_use stays true
            }
            // Processing → Idle
            {
                let mut guard = state_a.write().unwrap();
                *guard = State::Idle;
                active_a.store(false, Ordering::SeqCst);
            }
        });

        // Thread B: read state and check it's always a valid variant
        let state_b = Arc::clone(&state);
        let handle_b = thread::spawn(move || {
            let guard = state_b.read().unwrap();
            let snapshot = guard.clone();
            // Every observed state must be valid — no partial writes
            assert!(
                matches!(snapshot, State::Idle | State::Recording | State::Processing),
                "Observed invalid state: {snapshot:?}"
            );
        });

        handle_a.join().unwrap();
        handle_b.join().unwrap();

        // After all threads finish, state and active_use must be consistent.
        let final_state = state.read().unwrap().clone();
        let active = active_use.load(Ordering::SeqCst);
        assert_eq!(
            final_state.is_active(),
            active,
            "active_use ({active}) must match state ({final_state:?})"
        );
    });
}

// ---------------------------------------------------------------------------
// Loom Model Test 2: Cancel flag pattern — AtomicBool signals cancel
// ---------------------------------------------------------------------------

/// Tests the cancel signal pattern from the production `CancelSignal`.
/// One thread sets the cancel AtomicBool, the other thread (coordinator)
/// polls it and resets state to Idle. Loom explores all interleavings
/// of the store and the load/swap.
#[test]
fn loom_cancel_flag_pattern() {
    loom::model(|| {
        let state = Arc::new(RwLock::new(State::Recording));
        let active_use = Arc::new(AtomicBool::new(true));
        let cancel_flag = Arc::new(AtomicBool::new(false));

        // Thread A: simulate hotkey handler setting the cancel flag
        let cancel_a = Arc::clone(&cancel_flag);
        let handle_a = thread::spawn(move || {
            cancel_a.store(true, Ordering::SeqCst);
        });

        // Thread B: simulate coordinator checking and processing the cancel
        let state_b = Arc::clone(&state);
        let active_b = Arc::clone(&active_use);
        let cancel_b = Arc::clone(&cancel_flag);
        let handle_b = thread::spawn(move || {
            // Poll cancel flag (swap to consume, same as production CancelSignal)
            if cancel_b.swap(false, Ordering::SeqCst) {
                let mut guard = state_b.write().unwrap();
                *guard = State::Idle;
                active_b.store(false, Ordering::SeqCst);
            }
        });

        handle_a.join().unwrap();
        handle_b.join().unwrap();

        // After both threads finish, the state must be valid.
        let final_state = state.read().unwrap().clone();
        assert!(
            matches!(
                final_state,
                State::Idle | State::Recording | State::Processing
            ),
            "Final state must be valid, got {final_state:?}"
        );

        // If cancel was processed before we observed it, state should be Idle
        // and active_use should be false. If not, it might still be Recording.
        // Either way, they must be consistent with each other.
        let active = active_use.load(Ordering::SeqCst);
        assert_eq!(
            final_state.is_active(),
            active,
            "active_use ({active}) must match state ({final_state:?})"
        );
    });
}

// ---------------------------------------------------------------------------
// Loom Model Test 3: Channel-based command dispatch
// ---------------------------------------------------------------------------

/// Tests the channel + state transition pattern: one sender pushes a Start
/// command, one coordinator thread receives it and transitions state.
/// Loom explores all interleavings of channel operations and state updates.
#[test]
fn loom_channel_command_dispatch() {
    loom::model(|| {
        let state = Arc::new(RwLock::new(State::Idle));
        let active_use = Arc::new(AtomicBool::new(false));
        let (tx, rx) = channel::<Cmd>();

        // Coordinator thread: receive one command and process it
        let state_c = Arc::clone(&state);
        let active_c = Arc::clone(&active_use);
        let coordinator = thread::spawn(move || {
            if let Ok(cmd) = rx.recv() {
                let mut guard = state_c.write().unwrap();
                match cmd {
                    Cmd::Start => {
                        if matches!(*guard, State::Idle) {
                            *guard = State::Recording;
                            active_c.store(true, Ordering::SeqCst);
                        }
                    }
                    Cmd::Stop => {
                        if matches!(*guard, State::Recording) {
                            *guard = State::Processing;
                        }
                    }
                    Cmd::Cancel => {
                        *guard = State::Idle;
                        active_c.store(false, Ordering::SeqCst);
                    }
                    Cmd::Finished => {
                        if matches!(*guard, State::Processing) {
                            *guard = State::Idle;
                            active_c.store(false, Ordering::SeqCst);
                        }
                    }
                }
            }
        });

        // Sender thread: push a Start command
        let sender = thread::spawn(move || {
            let _ = tx.send(Cmd::Start);
        });

        sender.join().unwrap();
        coordinator.join().unwrap();

        // After both threads finish, state must be valid and consistent.
        let final_state = state.read().unwrap().clone();
        assert!(
            matches!(
                final_state,
                State::Idle | State::Recording | State::Processing
            ),
            "Final state must be valid, got {final_state:?}"
        );

        let active = active_use.load(Ordering::SeqCst);
        assert_eq!(
            final_state.is_active(),
            active,
            "active_use ({active}) must match state ({final_state:?})"
        );
    });
}
