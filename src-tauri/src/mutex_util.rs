//! Shared mutex utilities for poison-safe locking.
//!
//! # The Problem
//!
//! When a thread panics while holding a `std::sync::Mutex`, the mutex becomes
//! "poisoned". Any subsequent `.lock().unwrap()` on that mutex will panic,
//! causing cascading failures across the entire app.
//!
//! Common panic sources in this app:
//! - Transcription engine panics (unsafe FFI code, model loading failures)
//! - Audio device errors during stream setup
//! - Any `.unwrap()` or `.expect()` on `Err` / `None` while holding a lock
//!
//! # The Solution
//!
//! `lock_mutex()` recovers from poisoned state instead of panicking. It logs
//! the poisoning event (for debugging) and returns the guard with access to
//! the potentially-stale-but-recoverable data inside.
//!
//! For `bool` flags that are only ever read or written atomically (no Condvar
//! pairing), prefer `AtomicBool` over `Mutex<bool>` — it has no poisoning
//! risk and no lock contention.

use log::warn;
use std::sync::{Mutex, MutexGuard};

/// Lock a mutex, recovering from poison instead of panicking.
///
/// This should be used **everywhere** instead of `.lock().unwrap()`.
/// It logs a warning when poison is detected (useful for debugging the root
/// cause) and then recovers the guard so the app can continue running.
///
/// # Example
///
/// ```ignore
/// use crate::mutex_util::lock_mutex;
///
/// // BEFORE (will panic on poison):
/// let guard = some_mutex.lock().unwrap();
///
/// // AFTER (recovers from poison):
/// let guard = lock_mutex(&some_mutex, "some_mutex");
/// ```
pub fn lock_mutex<'a, T>(mutex: &'a Mutex<T>, name: &str) -> MutexGuard<'a, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            warn!(
                "Mutex '{}' was poisoned (a thread panicked while holding it). \
                 Recovering — data may be inconsistent.",
                name
            );
            poisoned.into_inner()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn test_lock_mutex_recovers_from_poison() {
        let mutex = Arc::new(Mutex::new(42_i32));
        let mutex_clone = Arc::clone(&mutex);

        // Poison the mutex by panicking while holding it
        let result = thread::spawn(move || {
            let _guard = mutex_clone.lock().unwrap();
            panic!("intentional test panic");
        })
        .join();
        assert!(result.is_err(), "thread should have panicked");

        // .lock().unwrap() would panic here, but lock_mutex recovers
        let guard = lock_mutex(&mutex, "test_mutex");
        assert_eq!(*guard, 42, "recovered data should still be accessible");
    }

    #[test]
    fn test_lock_mutex_works_normally() {
        let mutex = Mutex::new(10_i32);
        let guard = lock_mutex(&mutex, "normal_mutex");
        assert_eq!(*guard, 10);
    }
}