use log::debug;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

/// Thread-safe cancellation flag for streaming transcription with a
/// monotonically increasing sequence number.
///
/// The sequence counter makes it possible to correlate log lines across
/// threads and detect lost updates (e.g. a `set(true)` that races ahead of a
/// `set(false)`). Writes use `Release` and reads use `Acquire` so that the
/// flag/sequence pair is synchronized consistently across cores.
#[derive(Clone, Debug)]
pub struct StreamingCancelFlag {
    flag: Arc<AtomicBool>,
    seq: Arc<AtomicU64>,
}

impl StreamingCancelFlag {
    /// Create a new flag initialized to `false` with sequence number 0.
    pub fn new() -> Self {
        Self {
            flag: Arc::new(AtomicBool::new(false)),
            seq: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Set the flag to `value`, bumping the sequence number and logging
    /// the new state with caller-supplied context.
    pub fn set(&self, value: bool, context: &str) {
        let next_seq = self.seq.fetch_add(1, Ordering::AcqRel).wrapping_add(1);
        self.flag.store(value, Ordering::Release);
        debug!(
            "[CANCEL-FLAG seq={} now={} context={}]",
            next_seq, value, context
        );
    }

    /// Load the current flag value using acquire ordering.
    pub fn load_flag(&self) -> bool {
        self.flag.load(Ordering::Acquire)
    }

    /// Load the current sequence number using acquire ordering.
    pub fn load_seq(&self) -> u64 {
        self.seq.load(Ordering::Acquire)
    }

    /// Get a clone of the entire `StreamingCancelFlag`. Because the type is
    /// just two `Arc`s, this is cheap and gives downstream code the full
    /// typed API without exposing the internal atomics.
    pub fn inner(&self) -> StreamingCancelFlag {
        self.clone()
    }
}

impl Default for StreamingCancelFlag {
    fn default() -> Self {
        Self::new()
    }
}

impl From<Arc<AtomicBool>> for StreamingCancelFlag {
    /// Wrap an existing `Arc<AtomicBool>` so the same atomic is shared with
    /// legacy code (e.g. `TranscriptionManager::streaming_cancel_flag`).
    /// The sequence counter is separate and only used for diagnostics.
    fn from(flag: Arc<AtomicBool>) -> Self {
        Self {
            flag,
            seq: Arc::new(AtomicU64::new(0)),
        }
    }
}
