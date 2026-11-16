//! Per-recording buffer of externally inserted voice tokens.
//!
//! The registry is owned outside the transcription coordinator: the actions
//! hook `begin`/`finalize`/`take_final` into the recording lifecycle, while
//! the live-captions emitters and the HTTP server only ever touch the
//! current active session. It is self-cleaning — `begin` replaces any
//! previous (stale) session, so cancel/timeout/USB-cycle leftovers never
//! leak into the next recording.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use parking_lot::Mutex;

use crate::logging::SessionId;

use super::splice::{AnchorTotals, InjectedToken};

/// Error returned when tokens arrive while no unfinalized recording is
/// active (idle, already finalized, or stale).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotRecording;

/// One active (or just-finalized) recording's token buffer.
struct Active {
    session_id: SessionId,
    started_at: Instant,
    tokens: Vec<InjectedToken>,
    /// `Some` once `finalize` froze the anchors; acts as the finalized flag.
    totals: Option<AnchorTotals>,
}

/// Shared registry managed as Tauri state (`Arc<VoiceInsertRegistry>`).
pub struct VoiceInsertRegistry {
    inner: Mutex<Option<Active>>,
    /// Monotonic max of live-display chars seen this recording. The batch
    /// fallback path re-transcribes from scratch every 2.5s, so its text
    /// lengths are non-monotonic (VAD drops silence); taking the max keeps
    /// char anchors stable.
    display_chars: AtomicUsize,
}

impl VoiceInsertRegistry {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(None),
            display_chars: AtomicUsize::new(0),
        }
    }

    /// Start tracking a new recording. Resets the display meter and replaces
    /// any previous Active (self-cleaning on cancel/timeout/USB-cycle).
    pub fn begin(&self, session_id: SessionId) {
        *self.inner.lock() = Some(Active {
            session_id,
            started_at: Instant::now(),
            tokens: Vec::new(),
            totals: None,
        });
        self.display_chars.store(0, Ordering::Relaxed);
    }

    /// Record an inserted token against the active, unfinalized recording.
    /// Captures both anchors: the current display char meter and the elapsed
    /// milliseconds since the recording started.
    pub fn record_token(&self, text: &str) -> Result<(), NotRecording> {
        let mut guard = self.inner.lock();
        match guard.as_mut() {
            Some(active) if active.totals.is_none() => {
                active.tokens.push(InjectedToken {
                    text: text.to_string(),
                    chars_anchor: self.display_chars.load(Ordering::Relaxed),
                    ms_anchor: active.started_at.elapsed().as_millis() as u64,
                });
                Ok(())
            }
            _ => Err(NotRecording),
        }
    }

    /// Freeze the anchor totals for this recording. After finalize, further
    /// tokens are rejected and `tokens_for_display` returns empty, but the
    /// token list stays available for `take_final`.
    pub fn finalize(&self, session_id: &SessionId) {
        let mut guard = self.inner.lock();
        if let Some(active) = guard.as_mut() {
            if &active.session_id == session_id {
                active.totals = Some(AnchorTotals {
                    total_chars: self.display_chars.load(Ordering::Relaxed),
                    total_ms: active.started_at.elapsed().as_millis() as u64,
                });
            }
        }
    }

    /// Drain the finalized token buffer for this session. Returns `None`
    /// when there is nothing (no session, session mismatch, or not yet
    /// finalized). Draining removes the session, so this is single-shot.
    pub fn take_final(&self, session_id: &SessionId) -> Option<(Vec<InjectedToken>, AnchorTotals)> {
        let mut guard = self.inner.lock();
        let active = guard.as_ref()?;
        if &active.session_id != session_id {
            return None;
        }
        let totals = active.totals?;
        let active = guard.take()?;
        Some((active.tokens, totals))
    }

    /// Tokens for the live display overlay — only while a recording is
    /// active and not yet finalized.
    pub fn tokens_for_display(&self) -> Vec<InjectedToken> {
        let guard = self.inner.lock();
        match guard.as_ref() {
            Some(active) if active.totals.is_none() => active.tokens.clone(),
            _ => Vec::new(),
        }
    }

    /// Advance the display char meter. Monotonic max: batch-fallback lengths
    /// are non-monotonic, so a shorter emission never shrinks the meter.
    pub fn note_display_chars(&self, count: usize) {
        self.display_chars.fetch_max(count, Ordering::Relaxed);
    }
}

impl Default for VoiceInsertRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn begin_replaces_stale_session() {
        let reg = VoiceInsertRegistry::new();
        reg.begin("s1".to_string());
        let _ = reg.record_token("old");
        reg.begin("s2".to_string());
        // The stale token is gone; the new session starts empty.
        assert!(reg.tokens_for_display().is_empty());
        reg.finalize(&"s2".to_string());
        let (tokens, _) = reg.take_final(&"s2".to_string()).unwrap();
        assert!(tokens.is_empty());
    }

    #[test]
    fn begin_resets_display_meter() {
        let reg = VoiceInsertRegistry::new();
        reg.note_display_chars(500);
        reg.begin("s1".to_string());
        reg.finalize(&"s1".to_string());
        let (_, totals) = reg.take_final(&"s1".to_string()).unwrap();
        assert_eq!(totals.total_chars, 0);
    }

    #[test]
    fn finalize_freezes_totals_and_rejects_later_tokens() {
        let reg = VoiceInsertRegistry::new();
        reg.begin("s1".to_string());
        reg.note_display_chars(42);
        assert!(reg.record_token("mid").is_ok());
        reg.finalize(&"s1".to_string());
        assert!(reg.record_token("late").is_err());
        assert!(reg.tokens_for_display().is_empty());
        let (tokens, totals) = reg.take_final(&"s1".to_string()).unwrap();
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].text, "mid");
        assert_eq!(tokens[0].chars_anchor, 42);
        assert_eq!(totals.total_chars, 42);
    }

    #[test]
    fn take_drains_and_clears() {
        let reg = VoiceInsertRegistry::new();
        reg.begin("s1".to_string());
        let _ = reg.record_token("a");
        reg.finalize(&"s1".to_string());
        assert!(reg.take_final(&"s1".to_string()).is_some());
        // Second take: nothing left.
        assert!(reg.take_final(&"s1".to_string()).is_none());
    }

    #[test]
    fn record_rejected_without_active_session() {
        let reg = VoiceInsertRegistry::new();
        assert_eq!(reg.record_token("nope"), Err(NotRecording));
    }

    #[test]
    fn take_final_rejects_session_mismatch() {
        let reg = VoiceInsertRegistry::new();
        reg.begin("s1".to_string());
        let _ = reg.record_token("a");
        reg.finalize(&"s1".to_string());
        assert!(reg.take_final(&"other".to_string()).is_none());
        // The s1 buffer is untouched by the mismatched take.
        assert!(reg.take_final(&"s1".to_string()).is_some());
    }

    #[test]
    fn display_meter_is_monotonic_max() {
        let reg = VoiceInsertRegistry::new();
        reg.begin("s1".to_string());
        reg.note_display_chars(100);
        reg.note_display_chars(40); // batch fallback shrink — ignored
        reg.note_display_chars(80); // still below max — ignored
        reg.finalize(&"s1".to_string());
        let (_, totals) = reg.take_final(&"s1".to_string()).unwrap();
        assert_eq!(totals.total_chars, 100);
    }
}
