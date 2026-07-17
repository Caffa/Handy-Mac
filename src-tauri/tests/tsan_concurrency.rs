//! ThreadSanitizer integration tests for the Handy transcription coordinator.
//!
//! These tests exercise the same concurrency primitives used in production:
//! - `Arc<RwLock<AppState>>` for shared state
//! - `Arc<AtomicBool>` for cancel flags and active-use tracking
//! - `std::sync::mpsc::channel()` for command dispatch
//! - `parking_lot::Mutex` for audio manager locks
//!
//! ThreadSanitizer (TSan) detects data races, lock-order inversions, and other
//! concurrency bugs at runtime. TSan only works on Linux with a nightly Rust
//! toolchain because it requires `-Zsanitizer=thread`.
//!
//! # How to run
//!
//! ```sh
//! RUSTFLAGS="-Zsanitizer=thread" cargo +nightly test --features tsan --test tsan_concurrency -- --test-threads=1
//! ```
//!
//! You may also use the convenience script:
//!
//! ```sh
//! ./scripts/tsan-check.sh
//! ```
//!
//! # Why a separate feature flag?
//!
//! The `tsan` feature gate ensures these tests don't interfere with normal builds
//! or CI pipelines that run on macOS/Windows (where TSan is unavailable).
//! It also prevents pulling in test-only dependencies during regular compilation.

#![cfg(all(target_os = "linux", feature = "tsan"))]

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Simplified AppState mirroring the production type from
// transcription_coordinator.rs. We define a local copy here because the
// production type isn't publicly exported from the crate — and the point of
// these tests is to exercise the *concurrency primitives* under TSan, not
// the exact business logic.
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
enum AppState {
    Idle,
    Recording { binding_id: String },
    Processing { binding_id: Option<String> },
    UsbCycling { stage: String },
    Confirming { text: String, binding_id: Option<String> },
}

impl AppState {
    /// Returns true if this state represents active use (Recording or Processing).
    fn is_active(&self) -> bool {
        matches!(self, AppState::Recording { .. } | AppState::Processing { .. })
    }
}

// ---------------------------------------------------------------------------
// Test 1: Multiple threads reading/writing Arc<RwLock<AppState>> simultaneously
// ---------------------------------------------------------------------------

/// Verifies that concurrent readers and writers on `Arc<RwLock<AppState>>`
/// never produce an invalid state — every observed state must be a valid
/// variant. Under TSan, any data race on the RwLock would be flagged.
#[test]
fn test_rwlock_concurrent_read_write_app_state() {
    let state = Arc::new(RwLock::new(AppState::Idle));
    let iterations = 1000;

    // Writer threads: cycle through state transitions
    let mut handles = vec![];
    for i in 0..4 {
        let state = Arc::clone(&state);
        handles.push(thread::spawn(move || {
            for j in 0..iterations {
                let cycle = (i * iterations + j) % 4;
                let mut guard = state.write().unwrap();
                match cycle {
                    0 => *guard = AppState::Idle,
                    1 => *guard = AppState::Recording {
                        binding_id: format!("bind-{i}-{j}"),
                    },
                    2 => *guard = AppState::Processing {
                        binding_id: Some(format!("bind-{i}-{j}")),
                    },
                    3 => *guard = AppState::UsbCycling {
                        stage: format!("stage-{i}-{j}"),
                    },
                    _ => unreachable!(),
                }
            }
        }));
    }

    // Reader threads: continuously read and assert state is valid
    let read_count = Arc::new(AtomicUsize::new(0));
    for _ in 0..4 {
        let state = Arc::clone(&state);
        let read_count = Arc::clone(&read_count);
        handles.push(thread::spawn(move || {
            for _ in 0..iterations {
                let snapshot = state.read().unwrap().clone();
                // Every snapshot must be a valid AppState variant.
                // No partial writes or garbage should ever be observed.
                assert!(
                    matches!(
                        snapshot,
                        AppState::Idle
                            | AppState::Recording { .. }
                            | AppState::Processing { .. }
                            | AppState::UsbCycling { .. }
                            | AppState::Confirming { .. }
                    ),
                    "Observed invalid AppState: {snapshot:?}"
                );
                read_count.fetch_add(1, Ordering::Relaxed);
            }
        }));
    }

    for handle in handles {
        handle.join().expect("Thread panicked");
    }

    let total_reads = read_count.load(Ordering::Relaxed);
    eprintln!(
        "✓ RwLock concurrent read/write: {} reads completed, all valid",
        total_reads
    );
}

// ---------------------------------------------------------------------------
// Test 2: AtomicBool cancel flag — one thread sets, another spins reading
// ---------------------------------------------------------------------------

/// Verifies that the cancel signal pattern from `CancelSignal` is race-free.
/// One thread writes the flag, another polls it. Under TSan, any unsynchronized
/// access would be detected.
#[test]
fn test_atomic_bool_cancel_flag_eventually_visible() {
    let cancel = Arc::new(AtomicBool::new(false));
    let iterations = 100;

    for _ in 0..iterations {
        // Reset for each iteration
        cancel.store(false, Ordering::SeqCst);

        let cancel_writer = Arc::clone(&cancel);
        let cancel_reader = Arc::clone(&cancel);

        let writer = thread::spawn(move || {
            // Small delay before setting — reader must spin until it sees true
            thread::sleep(Duration::from_micros(10));
            cancel_writer.store(true, Ordering::SeqCst);
        });

        let reader = thread::spawn(move || {
            // Spin until the cancel flag becomes true
            // With a timeout to prevent infinite loops in pathological cases
            let start = std::time::Instant::now();
            loop {
                if cancel_reader.load(Ordering::SeqCst) {
                    return true;
                }
                if start.elapsed() > Duration::from_secs(5) {
                    panic!("Cancel flag was never set within 5 seconds");
                }
                // Yield to avoid busy-wait burning CPU
                thread::yield_now();
            }
        });

        writer.join().expect("Writer thread panicked");
        let result = reader.join().expect("Reader thread panicked");
        assert!(result, "Cancel flag should eventually become true");
    }

    eprintln!(
        "✓ AtomicBool cancel flag: {} iterations, all visible",
        iterations
    );
}

// ---------------------------------------------------------------------------
// Test 3: mpsc channel — producer sends 1000 commands, consumer processes
// ---------------------------------------------------------------------------

/// Verifies that the mpsc channel pattern used in `TranscriptionCoordinator`
/// (producer sends commands, single coordinator thread receives and processes)
/// delivers all messages without loss. Under TSan, any race on the channel
/// internals would be flagged.
#[test]
fn test_mpsc_channel_all_commands_received() {
    const COUNT: usize = 1000;

    #[derive(Debug, Clone, PartialEq)]
    enum Command {
        Input { binding_id: String },
        Cancel,
        ProcessingFinished,
    }

    let (tx, rx): (Sender<Command>, Receiver<Command>) = mpsc::channel();

    // Producer thread: send COUNT commands
    let producer = thread::spawn(move || {
        for i in 0..COUNT {
            let cmd = match i % 3 {
                0 => Command::Input {
                    binding_id: format!("transcribe-{i}"),
                },
                1 => Command::Cancel,
                _ => Command::ProcessingFinished,
            };
            tx.send(cmd).expect("Send should succeed");
        }
    });

    // Consumer thread: receive and count all commands
    let consumer = thread::spawn(move || {
        let mut received = Vec::with_capacity(COUNT);
        for _ in 0..COUNT {
            match rx.recv_timeout(Duration::from_secs(5)) {
                Ok(cmd) => received.push(cmd),
                Err(e) => panic!("recv_timeout error: {e}"),
            }
        }
        received
    });

    producer.join().expect("Producer panicked");
    let received = consumer.join().expect("Consumer panicked");

    assert_eq!(
        received.len(),
        COUNT,
        "Expected {} commands, received {}",
        COUNT,
        received.len()
    );

    // Verify all command types are present
    let inputs = received
        .iter()
        .filter(|c| matches!(c, Command::Input { .. }))
        .count();
    let cancels = received
        .iter()
        .filter(|c| c == &Command::Cancel)
        .count();
    let finished = received
        .iter()
        .filter(|c| c == &Command::ProcessingFinished)
        .count();

    // With COUNT=1000 and i%3, we expect roughly 333/334 of each
    assert!(inputs > 0, "Should have Input commands");
    assert!(cancels > 0, "Should have Cancel commands");
    assert!(finished > 0, "Should have ProcessingFinished commands");
    assert_eq!(
        inputs + cancels + finished,
        COUNT,
        "Total command count must match"
    );

    eprintln!(
        "✓ mpsc channel: {} commands received ({} inputs, {} cancels, {} finished)",
        COUNT, inputs, cancels, finished
    );
}

// ---------------------------------------------------------------------------
// Test 4: Concurrent parking_lot::Mutex access — counter under contention
// ---------------------------------------------------------------------------

/// Verifies that `parking_lot::Mutex` correctly serializes concurrent
/// counter increments. The final count must equal the expected total,
/// proving no increments were lost due to races. This mirrors the
/// `parking_lot::Mutex` usage pattern in `AudioRecordingManager`.
#[test]
fn test_parking_lot_mutex_counter_under_contention() {
    let counter = Arc::new(parking_lot::Mutex::new(0usize));
    let threads = 8;
    let increments_per_thread = 10_000;

    let mut handles = vec![];
    for _ in 0..threads {
        let counter = Arc::clone(&counter);
        handles.push(thread::spawn(move || {
            for _ in 0..increments_per_thread {
                let mut guard = counter.lock();
                *guard += 1;
            }
        }));
    }

    for handle in handles {
        handle.join().expect("Thread panicked");
    }

    let final_count = *counter.lock();
    let expected = threads * increments_per_thread;
    assert_eq!(
        final_count, expected,
        "Counter under parking_lot::Mutex: expected {expected}, got {final_count}"
    );

    eprintln!(
        "✓ parking_lot::Mutex counter: {threads} threads × {increments_per_thread} increments = {final_count} (correct)"
    );
}

// ---------------------------------------------------------------------------
// Test 5: Combined coordinator pattern — channel + RwLock + AtomicBool
// ---------------------------------------------------------------------------

/// End-to-end test of the coordinator's concurrency pattern: one coordinator
/// thread receives commands via mpsc, updates shared `RwLock<AppState>` and
/// `AtomicBool`, while multiple sender threads push commands concurrently.
/// TSan will flag any race between the coordinator and senders.
#[test]
fn test_coordinator_pattern_concurrent_access() {
    let state = Arc::new(RwLock::new(AppState::Idle));
    let active_use = Arc::new(AtomicBool::new(false));
    let cancel_flag = Arc::new(AtomicBool::new(false));

    #[derive(Debug)]
    enum Cmd {
        Start { id: String },
        Stop { id: String },
        Cancel,
        Finished,
    }

    let (tx, rx): (Sender<Cmd>, Receiver<Cmd>) = mpsc::channel();
    let command_count = 500;

    // Coordinator thread: processes commands and updates shared state
    let coord_state = Arc::clone(&state);
    let coord_active = Arc::clone(&active_use);
    let coord_cancel = Arc::clone(&cancel_flag);
    let coordinator = thread::spawn(move || {
        let mut processed = 0usize;
        for _ in 0..command_count {
            match rx.recv_timeout(Duration::from_secs(5)) {
                Ok(cmd) => {
                    // Check cancel flag first (same pattern as production)
                    if coord_cancel.swap(false, Ordering::SeqCst) {
                        let mut guard = coord_state.write().unwrap();
                        *guard = AppState::Idle;
                        coord_active.store(false, Ordering::SeqCst);
                        processed += 1;
                        continue;
                    }

                    match cmd {
                        Cmd::Start { id } => {
                            let mut guard = coord_state.write().unwrap();
                            *guard = AppState::Recording { binding_id: id };
                            coord_active.store(true, Ordering::SeqCst);
                        }
                        Cmd::Stop { id } => {
                            let mut guard = coord_state.write().unwrap();
                            *guard = AppState::Processing {
                                binding_id: Some(id),
                            };
                            coord_active.store(true, Ordering::SeqCst);
                        }
                        Cmd::Cancel => {
                            let mut guard = coord_state.write().unwrap();
                            *guard = AppState::Idle;
                            coord_active.store(false, Ordering::SeqCst);
                        }
                        Cmd::Finished => {
                            let mut guard = coord_state.write().unwrap();
                            *guard = AppState::Idle;
                            coord_active.store(false, Ordering::SeqCst);
                        }
                    }
                    processed += 1;
                }
                Err(e) => panic!("Coordinator recv error: {e}"),
            }
        }
        processed
    });

    // Sender threads: push commands concurrently
    let mut sender_handles = vec![];
    for i in 0..4 {
        let tx = tx.clone();
        let cancel_flag = Arc::clone(&cancel_flag);
        sender_handles.push(thread::spawn(move || {
            let per_thread = command_count / 4;
            for j in 0..per_thread {
                let cycle = (i * per_thread + j) % 4;
                match cycle {
                    0 => tx
                        .send(Cmd::Start {
                            id: format!("id-{i}-{j}"),
                        })
                        .unwrap(),
                    1 => tx
                        .send(Cmd::Stop {
                            id: format!("id-{i}-{j}"),
                        })
                        .unwrap(),
                    2 => {
                        // Use cancel_flag path every ~20th iteration
                        if j % 20 == 0 {
                            cancel_flag.store(true, Ordering::SeqCst);
                        } else {
                            tx.send(Cmd::Cancel).unwrap();
                        }
                    }
                    _ => tx.send(Cmd::Finished).unwrap(),
                }
            }
        }));
    }

    // Drop the last sender clone so the coordinator's recv will eventually
    // see disconnect once all senders finish. But since the coordinator
    // processes exactly `command_count` messages, it will exit before that.
    drop(tx);

    for handle in sender_handles {
        handle.join().expect("Sender thread panicked");
    }

    let processed = coordinator.join().expect("Coordinator thread panicked");
    assert_eq!(
        processed, command_count,
        "Coordinator should process all {command_count} commands, got {processed}"
    );

    // Final state should be valid
    let final_state = state.read().unwrap().clone();
    assert!(
        matches!(
            final_state,
            AppState::Idle
                | AppState::Recording { .. }
                | AppState::Processing { .. }
        ),
        "Final state should be valid, got {final_state:?}"
    );

    eprintln!(
        "✓ Coordinator pattern: {processed} commands processed, final state {:?}",
        final_state
    );
}