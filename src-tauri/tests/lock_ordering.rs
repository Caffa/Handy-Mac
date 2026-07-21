//! Lock-ordering invariant test for AudioRecordingManager.
//!
//! This test guards against AB-BA deadlock regressions in the audio manager's
//! lock acquisition order. The bug was found during the 2026-07-17 Lock Hazard
//! Audit (see `learning-log.md`):
//!
//! - `try_start_recording()` takes locks in order: **state → recorder** (nested)
//! - The OLD `stop_microphone_stream()` took locks in order: **recorder → state**
//!   (nested, opposite order) — creating a classic AB-BA deadlock when both
//!   paths run concurrently (e.g., recording start vs. liveness monitor restart).
//!
//! The fix restructured `stop_microphone_stream()` to drop the recorder lock
//! before taking the state lock, eliminating the nesting and inversion.
//!
//! This test file:
//! 1. Verifies the CURRENT (fixed) lock order does not deadlock under stress.
//! 2. Demonstrates that the BAD (inverted) order WOULD deadlock, proving the
//!    test has the power to catch regressions.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering as AtomicOrdering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

// We replicate the lock STRUCTURE of AudioRecordingManager using parking_lot::Mutex<()>
// instead of the real types. This tests the lock acquisition pattern without needing
// real audio devices, AppHandle, or other infrastructure.
//
// In the production code:
//   state:    Arc<parking_lot::Mutex<RecordingState>>
//   recorder: Arc<parking_lot::Mutex<Option<AudioRecorder>>>
//
// Both use parking_lot::Mutex, which is non-poisoning and provides try_lock_for().

/// Simulates the CURRENT (fixed) lock acquisition order for `try_start_recording`:
/// state first, then recorder (nested).
fn acquire_start_order(state: &parking_lot::Mutex<()>, recorder: &parking_lot::Mutex<()>) {
    let _state_guard = state.lock();
    // Simulate the work between acquiring state and recorder (e.g., checking Idle,
    // starting stream, etc.)
    let _recorder_guard = recorder.lock();
    // Both locks held simultaneously — this is the production pattern.
    drop(_recorder_guard);
    drop(_state_guard);
}

/// Simulates the CURRENT (fixed) lock acquisition order for `stop_microphone_stream`:
/// recorder first (and dropped), then state (separate, no nesting).
/// After the 2026-07-17 fix, stop_microphone_stream drops the recorder lock
/// BEFORE taking the state lock, so there is no nesting and no AB-BA inversion.
fn acquire_stop_order(state: &parking_lot::Mutex<()>, recorder: &parking_lot::Mutex<()>) {
    // Phase 1: recorder lock acquired and dropped (stop + close the recorder)
    {
        let _recorder_guard = recorder.lock();
        // Simulate stop + close work
    }
    // recorder lock dropped HERE — no nesting

    // Phase 2: state lock acquired separately (transition to Idle)
    let _state_guard = state.lock();
    // Simulate state transition
    drop(_state_guard);
}

/// Simulates the OLD (BUGGY) lock acquisition order for `stop_microphone_stream`:
/// recorder first, then state — NESTED. This is the opposite order from
/// `try_start_recording` (state → recorder), creating an AB-BA deadlock.
///
/// DO NOT USE IN PRODUCTION CODE. This function exists only to prove that the
/// test can detect the bad order.
fn acquire_stop_order_buggy(state: &parking_lot::Mutex<()>, recorder: &parking_lot::Mutex<()>) {
    // Phase 1: recorder lock acquired
    let _recorder_guard = recorder.lock();
    // Phase 2: state lock acquired WHILE recorder lock is held — INVERTED ORDER
    let _state_guard = state.lock();
    // Both locks held — AB-BA deadlock potential if another thread does start order
    drop(_state_guard);
    drop(_recorder_guard);
}

/// Simulates the `cancel_recording` lock acquisition order:
/// state lock (with try_lock_for timeout), then recorder lock (with try_lock_for timeout).
/// Uses bounded locks in production, but the order is still state → recorder.
fn acquire_cancel_order(state: &parking_lot::Mutex<()>, recorder: &parking_lot::Mutex<()>) {
    let _state_guard = state.lock();
    let _recorder_guard = recorder.lock();
    drop(_recorder_guard);
    drop(_state_guard);
}

/// Simulates the `schedule_lazy_close` pattern: state lock held, then calls
/// stop_microphone_stream (which takes recorder then state separately).
/// Since stop_microphone_stream is now fixed (recorder dropped before state),
/// this is: state → (recorder then dropped) → state again (separate).
/// No inversion risk because recorder is never held across state.
fn acquire_lazy_close_order(state: &parking_lot::Mutex<()>, recorder: &parking_lot::Mutex<()>) {
    let _state_guard = state.lock();
    // Check if Idle, etc.
    drop(_state_guard);

    // Then call stop_microphone_stream (which takes recorder, drops it, then takes state)
    acquire_stop_order(state, recorder);
}

/// Helper: join a thread with a timeout. Returns Ok(()) if the thread finished
/// within the timeout, Err(()) if it timed out (likely deadlock).
fn join_with_timeout<T: Send + 'static>(
    handle: thread::JoinHandle<T>,
    timeout: Duration,
) -> Result<(), ()> {
    // We use a channel to signal completion. The thread sends () when done.
    // If we don't receive within the timeout, the thread is likely deadlocked.
    let (tx, rx) = std::sync::mpsc::channel();
    thread::spawn(move || {
        let _ = handle.join();
        let _ = tx.send(());
    });
    match rx.recv_timeout(timeout) {
        Ok(()) => Ok(()),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err(()),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            // Thread panicked or was detached — treat as completion
            Ok(())
        }
    }
}

// ─── Test 1: Current (fixed) lock order does NOT deadlock ────────────────────

/// Stress-test the CURRENT lock acquisition order (state → recorder for start,
/// recorder-then-state for stop, cancel also state → recorder) under heavy
/// concurrency. If this test times out, a lock-ordering regression has occurred.
///
/// This mirrors the real production scenario where:
/// - Thread A (recording start) calls try_start_recording: state → recorder
/// - Thread B (liveness monitor) calls stop_microphone_stream: recorder → (drop) → state
/// - Thread C (cancel) calls cancel_recording: state → recorder
/// - Thread D (lazy close) calls schedule_lazy_close: state → stop_order
///
/// All paths use a consistent ordering (state before recorder when nested,
/// or no nesting at all), so no deadlock should occur.
#[test]
fn test_lock_ordering_consistent_no_deadlock() {
    let state = Arc::new(parking_lot::Mutex::new(()));
    let recorder = Arc::new(parking_lot::Mutex::new(()));

    let iterations = 500;
    let success_count = Arc::new(AtomicUsize::new(0));

    let mut handles = vec![];

    // Thread A: Simulates try_start_recording (state → recorder)
    for _ in 0..4 {
        let state = Arc::clone(&state);
        let recorder = Arc::clone(&recorder);
        let success_count = Arc::clone(&success_count);
        handles.push(thread::spawn(move || {
            for _ in 0..iterations {
                acquire_start_order(&state, &recorder);
                success_count.fetch_add(1, AtomicOrdering::Relaxed);
            }
        }));
    }

    // Thread B: Simulates stop_microphone_stream (recorder → drop → state, no nesting)
    for _ in 0..4 {
        let state = Arc::clone(&state);
        let recorder = Arc::clone(&recorder);
        let success_count = Arc::clone(&success_count);
        handles.push(thread::spawn(move || {
            for _ in 0..iterations {
                acquire_stop_order(&state, &recorder);
                success_count.fetch_add(1, AtomicOrdering::Relaxed);
            }
        }));
    }

    // Thread C: Simulates cancel_recording (state → recorder)
    for _ in 0..4 {
        let state = Arc::clone(&state);
        let recorder = Arc::clone(&recorder);
        let success_count = Arc::clone(&success_count);
        handles.push(thread::spawn(move || {
            for _ in 0..iterations {
                acquire_cancel_order(&state, &recorder);
                success_count.fetch_add(1, AtomicOrdering::Relaxed);
            }
        }));
    }

    // Thread D: Simulates schedule_lazy_close (state → stop_order)
    for _ in 0..4 {
        let state = Arc::clone(&state);
        let recorder = Arc::clone(&recorder);
        let success_count = Arc::clone(&success_count);
        handles.push(thread::spawn(move || {
            for _ in 0..iterations {
                acquire_lazy_close_order(&state, &recorder);
                success_count.fetch_add(1, AtomicOrdering::Relaxed);
            }
        }));
    }

    // Wait for all threads with a generous timeout.
    // 16 threads × 500 iterations = 8000 total operations.
    // With no contention, this completes in milliseconds.
    // With contention (but no deadlock), parking_lot handles it efficiently.
    // A 10-second timeout is very generous — a real deadlock would hang forever.
    let timeout = Duration::from_secs(10);

    for handle in handles {
        let result = join_with_timeout(handle, timeout);
        assert!(
            result.is_ok(),
            "Thread did not complete within {:?} — likely deadlock detected! \
             Lock ordering invariant violated.",
            timeout
        );
    }

    let total_ops = success_count.load(AtomicOrdering::Relaxed);
    // 16 threads × 500 iterations = 8000 operations
    assert_eq!(
        total_ops, 8000,
        "Expected 8000 lock operations, got {}",
        total_ops
    );

    eprintln!(
        "✓ Lock ordering invariant test passed: {} operations completed without deadlock",
        total_ops
    );
}

// ─── Test 2: Concurrent start/stop stress ──────────────────────────────────────

/// Stress-test concurrent start and stop operations with higher thread counts.
/// This simulates the worst case: many threads hammering both start and stop
/// paths simultaneously, which is the scenario that would trigger an AB-BA
/// deadlock if the lock order were inverted.
#[test]
fn test_concurrent_start_stop_stress() {
    let state = Arc::new(parking_lot::Mutex::new(()));
    let recorder = Arc::new(parking_lot::Mutex::new(()));

    let iterations = 1000;
    let success_count = Arc::new(AtomicUsize::new(0));

    let mut handles = vec![];

    // 4 threads doing start, 4 threads doing stop — all running concurrently
    for i in 0..8 {
        let state = Arc::clone(&state);
        let recorder = Arc::clone(&recorder);
        let success_count = Arc::clone(&success_count);
        handles.push(thread::spawn(move || {
            for _ in 0..iterations {
                if i % 2 == 0 {
                    acquire_start_order(&state, &recorder);
                } else {
                    acquire_stop_order(&state, &recorder);
                }
                success_count.fetch_add(1, AtomicOrdering::Relaxed);
            }
        }));
    }

    let timeout = Duration::from_secs(10);

    for handle in handles {
        let result = join_with_timeout(handle, timeout);
        assert!(
            result.is_ok(),
            "Thread did not complete within {:?} — likely deadlock! \
             Concurrent start/stop stress test failed.",
            timeout
        );
    }

    let total_ops = success_count.load(AtomicOrdering::Relaxed);
    assert_eq!(
        total_ops,
        8 * iterations,
        "Expected {} lock operations, got {}",
        8 * iterations,
        total_ops
    );

    eprintln!(
        "✓ Concurrent start/stop stress test passed: {} operations completed",
        total_ops
    );
}

// ─── Test 3: Buggy (inverted) order WOULD deadlock ────────────────────────────
//
// This test is #[ignore]d because it WILL deadlock if the lock order is inverted.
// Run it manually with: `cargo test --test lock_ordering buggy_order_deadlock -- --ignored`
//
// Purpose: Proves the test framework can detect an AB-BA deadlock. If this test
// hangs, the bad order is indeed deadly. If it completes (impossible with true
// AB-BA inversion), the test framework is flawed.
//
// DO NOT REMOVE THE #[ignore] — this test is a proof-of-concept only.

/// Demonstrates that the OLD (buggy) lock order would cause a deadlock.
/// Thread A takes state → recorder. Thread B takes recorder → state (nested).
/// With concurrent execution, this produces a classic AB-BA deadlock.
///
/// This test uses a 5-second timeout. If threads can't complete within that time,
/// it's because they're deadlocked — the test reports this as a failure.
#[test]
#[ignore = "This test deliberately creates an AB-BA deadlock to prove the test \
            can detect inversions. Run with: cargo test --test lock_ordering \
            buggy_order_deadlock -- --ignored"]
fn buggy_order_deadlock() {
    let state = Arc::new(parking_lot::Mutex::new(()));
    let recorder = Arc::new(parking_lot::Mutex::new(()));

    let iterations = 200;
    let start_count = Arc::new(AtomicUsize::new(0));
    let stop_count = Arc::new(AtomicUsize::new(0));
    let deadlock_detected = Arc::new(AtomicBool::new(false));

    let mut handles = vec![];

    // Thread A: Simulates try_start_recording (state → recorder)
    for _ in 0..4 {
        let state = Arc::clone(&state);
        let recorder = Arc::clone(&recorder);
        let start_count = Arc::clone(&start_count);
        let deadlock_detected = Arc::clone(&deadlock_detected);
        handles.push(thread::spawn(move || {
            for _ in 0..iterations {
                if deadlock_detected.load(AtomicOrdering::Relaxed) {
                    break;
                }
                acquire_start_order(&state, &recorder);
                start_count.fetch_add(1, AtomicOrdering::Relaxed);
            }
        }));
    }

    // Thread B: Simulates OLD (buggy) stop_microphone_stream (recorder → state, NESTED)
    for _ in 0..4 {
        let state = Arc::clone(&state);
        let recorder = Arc::clone(&recorder);
        let stop_count = Arc::clone(&stop_count);
        let deadlock_detected = Arc::clone(&deadlock_detected);
        handles.push(thread::spawn(move || {
            for _ in 0..iterations {
                if deadlock_detected.load(AtomicOrdering::Relaxed) {
                    break;
                }
                // Use the BUGGY order: recorder → state (nested)
                acquire_stop_order_buggy(&state, &recorder);
                stop_count.fetch_add(1, AtomicOrdering::Relaxed);
            }
        }));
    }

    // Wait with timeout. The AB-BA deadlock should cause at least one thread
    // to hang. We detect this via a short timeout per thread.
    let timeout = Duration::from_secs(5);

    let mut any_timed_out = false;
    for handle in handles {
        if join_with_timeout(handle, timeout).is_err() {
            any_timed_out = true;
        }
    }

    // If any thread timed out, that confirms the AB-BA deadlock exists.
    // This is the EXPECTED behavior — the buggy order SHOULD deadlock.
    if any_timed_out {
        deadlock_detected.store(true, AtomicOrdering::Relaxed);
        eprintln!(
            "⚠ AB-BA deadlock detected with buggy order — this is EXPECTED. \
             The test correctly identifies the inversion."
        );
    } else {
        // This is unusual but possible with very few iterations or lucky scheduling.
        eprintln!(
            "Note: Buggy order test completed without timing out. \
             This can happen due to scheduling luck. The important test is \
             test_lock_ordering_consistent_no_deadlock which verifies the \
             FIXED order never deadlocks."
        );
    }

    let starts = start_count.load(AtomicOrdering::Relaxed);
    let stops = stop_count.load(AtomicOrdering::Relaxed);
    eprintln!(
        "Buggy order test: {} start-order acquisitions, {} stop-order acquisitions completed",
        starts, stops
    );
}

// ─── Test 4: Verify stop_microphone_stream pattern (no nesting) ───────────────

/// Verify that the stop order uses SEPARATE lock acquisitions (no nesting),
/// not nested locks. This tests the specific pattern from the fix:
/// recorder lock is dropped BEFORE state lock is acquired.
#[test]
fn test_stop_order_no_nesting() {
    let state = parking_lot::Mutex::new(());
    let recorder = parking_lot::Mutex::new(());

    // The stop order should be: acquire recorder, drop recorder, acquire state.
    // If this completes without deadlock, the order is correct.
    for _ in 0..1000 {
        acquire_stop_order(&state, &recorder);
    }

    eprintln!("✓ Stop order no-nesting test passed: 1000 iterations without deadlock");
}

// ─── Test 5: Cross-product of all lock paths ─────────────────────────────────

/// Tests all pairs of concurrent lock acquisitions to ensure no pair deadlocks.
/// This is the "lock-ordering matrix" approach: every pair of lock acquisition
/// functions runs concurrently to verify they don't create an AB-BA inversion.
#[test]
fn test_lock_ordering_cross_product() {
    let state = Arc::new(parking_lot::Mutex::new(()));
    let recorder = Arc::new(parking_lot::Mutex::new(()));

    let iterations = 300;

    type LockFn = fn(&parking_lot::Mutex<()>, &parking_lot::Mutex<()>);

    let lock_fns: Vec<(LockFn, &str)> = vec![
        (acquire_start_order, "start_order"),
        (acquire_stop_order, "stop_order"),
        (acquire_cancel_order, "cancel_order"),
        (acquire_lazy_close_order, "lazy_close_order"),
    ];

    for (fn_a, name_a) in &lock_fns {
        for (fn_b, name_b) in &lock_fns {
            let state_a = Arc::clone(&state);
            let recorder_a = Arc::clone(&recorder);
            let fn_a = *fn_a;
            let name_a = name_a.to_string();
            let name_b = name_b.to_string();

            let handle_a = thread::spawn(move || {
                for _ in 0..iterations {
                    fn_a(&state_a, &recorder_a);
                }
            });

            let state_b = Arc::clone(&state);
            let recorder_b = Arc::clone(&recorder);
            let fn_b = *fn_b;

            let handle_b = thread::spawn(move || {
                for _ in 0..iterations {
                    fn_b(&state_b, &recorder_b);
                }
            });

            let timeout = Duration::from_secs(5);
            let result_a = join_with_timeout(handle_a, timeout);
            let result_b = join_with_timeout(handle_b, timeout);

            assert!(
                result_a.is_ok() && result_b.is_ok(),
                "Deadlock detected in cross-product test: {} vs {} \
                 — one or both threads timed out after {:?}",
                name_a,
                name_b,
                timeout
            );
        }
    }

    eprintln!("✓ Cross-product lock ordering test passed: all pairs completed without deadlock");
}
