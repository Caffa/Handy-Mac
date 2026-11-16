//! Pure anchor/splice math for voice-insert tokens.
//!
//! No Tauri deps: everything here operates on plain strings and the
//! [`InjectedToken`] / [`AnchorTotals`] value types, so the behavior is fully
//! unit-testable without an app handle.
//!
//! Two splice targets exist:
//! - [`splice_tokens`] — final transcription text, positioned by a fractional
//!   anchor (char fraction, falling back to time fraction) computed against
//!   the recorded audio's totals.
//! - [`splice_display`] — live-caption display text, positioned by the
//!   display-time char anchor. Because every live captions emission
//!   re-derives its text from the same underlying base string, re-splicing
//!   from that base each tick defeats the 2.5s batch rewrite that would
//!   otherwise drop already-inserted tokens.

/// A single externally inserted token with both anchor coordinate systems.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InjectedToken {
    /// Raw token text as submitted (trimmed at insert time).
    pub text: String,
    /// Anchor as a character offset into the display/live text at insert time.
    pub chars_anchor: usize,
    /// Anchor as milliseconds since the recording started.
    pub ms_anchor: u64,
}

/// Totals of a recording session, captured when the recording finalizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnchorTotals {
    /// Total characters of live/display text seen during the recording.
    pub total_chars: usize,
    /// Total duration of the recording in milliseconds.
    pub total_ms: u64,
}

/// Snap a raw char index to a word boundary: scan forward for whitespace
/// within `max_scan` chars and insert before it, else insert at the raw
/// index. Scanning by chars (not bytes) keeps this CJK-safe, since CJK text
/// has no whitespace and simply falls through to the raw index.
fn snap_to_word_boundary(base: &str, raw_index: usize, max_scan: usize) -> usize {
    let total_chars = base.chars().count();
    if raw_index >= total_chars {
        return total_chars;
    }
    let scan_end = (raw_index + max_scan).min(total_chars);
    for idx in raw_index..scan_end {
        if base.chars().nth(idx).is_some_and(|c| c.is_whitespace()) {
            return idx;
        }
    }
    raw_index
}

/// Copy `base` chars up to `char_index` into `out`, tracking how many base
/// chars have already been copied. Returns the updated copy count.
fn copy_base_chars_up_to(
    base_chars: &[char],
    out: &mut String,
    copied: &mut usize,
    char_index: usize,
) {
    while *copied < char_index && *copied < base_chars.len() {
        out.push(base_chars[*copied]);
        *copied += 1;
    }
}

/// Append a trimmed token with single-space padding: a space before the token
/// when the output so far is non-empty and does not already end with
/// whitespace, and a space after when the next base char is not itself
/// whitespace (mid-word insertion).
fn insert_token_at(out: &mut String, base_chars: &[char], copied: usize, text: &str) {
    if !out.is_empty() && !out.ends_with(char::is_whitespace) {
        out.push(' ');
    }
    out.push_str(text.trim());
    if copied < base_chars.len() && !base_chars[copied].is_whitespace() {
        out.push(' ');
    }
}

/// Splice finalized tokens into the final transcription text.
///
/// Per token (applied in click order; earlier insertions shift later indices
/// because the copy cursor keeps moving through the original base):
/// 1. fraction = chars_anchor/total_chars when total_chars > 0,
///    else ms_anchor/total_ms (round-half-up);
/// 2. target char index = round(fraction × base char count);
/// 3. word-boundary snap: scan forward for whitespace within 5 chars, else
///    insert at the target index;
/// 4. insert the trimmed token with single-space padding.
///
/// An empty token list returns the base unchanged (idempotent).
pub fn splice_tokens(base: &str, tokens: &[InjectedToken], totals: &AnchorTotals) -> String {
    if tokens.is_empty() {
        return base.to_string();
    }

    let base_chars: Vec<char> = base.chars().collect();
    let base_len = base_chars.len();

    let mut out = String::with_capacity(base.len());
    let mut copied = 0usize; // base chars already copied into `out`

    for token in tokens {
        let fraction = if totals.total_chars > 0 {
            token.chars_anchor as f64 / totals.total_chars as f64
        } else if totals.total_ms > 0 {
            token.ms_anchor as f64 / totals.total_ms as f64
        } else {
            // No anchor basis at all (empty recording): append at the end.
            1.0
        };
        let raw_index = ((fraction * base_len as f64).round() as usize).min(base_len);
        let index = snap_to_word_boundary(base, raw_index, 5).min(base_len);

        copy_base_chars_up_to(&base_chars, &mut out, &mut copied, index);
        insert_token_at(&mut out, &base_chars, copied, &token.text);
    }

    // Copy the remainder of the base.
    copy_base_chars_up_to(&base_chars, &mut out, &mut copied, base_len);
    out
}

/// Splice tokens into the live display text at their display-time anchors.
///
/// Each token is inserted at `min(chars_anchor, display length)`, so every
/// emission re-derives the same text from the same base — tokens neither
/// accumulate nor get lost to the 2.5s batch rewrite.
pub fn splice_display(live_text: &str, tokens: &[InjectedToken]) -> String {
    if tokens.is_empty() {
        return live_text.to_string();
    }

    let live_chars: Vec<char> = live_text.chars().collect();
    let live_len = live_chars.len();

    let mut out = String::with_capacity(live_text.len());
    let mut copied = 0usize;

    for token in tokens {
        let index = token.chars_anchor.min(live_len);
        copy_base_chars_up_to(&live_chars, &mut out, &mut copied, index);
        insert_token_at(&mut out, &live_chars, copied, &token.text);
    }

    copy_base_chars_up_to(&live_chars, &mut out, &mut copied, live_len);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(text: &str, chars_anchor: usize, ms_anchor: u64) -> InjectedToken {
        InjectedToken {
            text: text.to_string(),
            chars_anchor,
            ms_anchor,
        }
    }

    #[test]
    fn mid_string_token_inserts_at_fraction() {
        // "hello world" (11 chars); anchor 5/11 → index 5, which is the
        // separating whitespace → insert right before "world".
        let out = splice_tokens(
            "hello world",
            &[token("TOKEN", 5, 0)],
            &AnchorTotals {
                total_chars: 11,
                total_ms: 0,
            },
        );
        assert_eq!(out, "hello TOKEN world");
    }

    #[test]
    fn boundary_snap_scans_forward_for_whitespace() {
        // Anchor 5/13 in "one two three" lands inside "two" (4..7); the
        // forward scan within 5 chars finds the space at 7 → insert there.
        let out = splice_tokens(
            "one two three",
            &[token("X", 5, 0)],
            &AnchorTotals {
                total_chars: 13,
                total_ms: 0,
            },
        );
        assert_eq!(out, "one two X three");
    }

    #[test]
    fn multi_token_order_and_running_offset() {
        // Tokens anchored at 3/11 and 7/11 of "aaa bbb ccc": both targets are
        // word boundaries; the second insert lands after the first even
        // though the copy cursor already moved past the original anchor area.
        let out = splice_tokens(
            "aaa bbb ccc",
            &[token("T1", 3, 0), token("T2", 7, 0)],
            &AnchorTotals {
                total_chars: 11,
                total_ms: 0,
            },
        );
        assert_eq!(out, "aaa T1 bbb T2 ccc");
    }

    #[test]
    fn anchors_at_zero_and_full_percent() {
        let out = splice_tokens(
            "middle text",
            &[token("START", 0, 0), token("END", 11, 0)],
            &AnchorTotals {
                total_chars: 11,
                total_ms: 0,
            },
        );
        assert_eq!(out, "START middle text END");
    }

    #[test]
    fn empty_base_returns_tokens() {
        let out = splice_tokens(
            "",
            &[token("ONLY", 3, 500)],
            &AnchorTotals {
                total_chars: 0,
                total_ms: 1000,
            },
        );
        assert_eq!(out, "ONLY");
    }

    #[test]
    fn zero_total_chars_falls_back_to_time_anchor() {
        // total_chars == 0 → fraction from ms: 250/1000 = 0.25 → char 2 of
        // "abcdef" (6 chars). No whitespace within 5 chars → insert at 2.
        let out = splice_tokens(
            "abcdef",
            &[token("T", 0, 250)],
            &AnchorTotals {
                total_chars: 0,
                total_ms: 1000,
            },
        );
        assert_eq!(out, "ab T cdef");
    }

    #[test]
    fn cjk_string_inserts_without_word_boundary() {
        // 10 CJK chars, no whitespace: the boundary scan falls through and
        // the raw index is used. Single-space padding still applies.
        let base = "你好世界这是测试文本";
        let out = splice_tokens(
            base,
            &[token("注", 6, 0)],
            &AnchorTotals {
                total_chars: 10,
                total_ms: 0,
            },
        );
        let expected = "你好世界这是 注 测试文本";
        assert_eq!(out, expected);
    }

    #[test]
    fn empty_tokens_is_idempotent() {
        assert_eq!(
            splice_tokens(
                "unchanged",
                &[],
                &AnchorTotals {
                    total_chars: 9,
                    total_ms: 9,
                }
            ),
            "unchanged"
        );
    }

    #[test]
    fn display_splice_inserts_at_char_anchor() {
        let out = splice_display("live caption text", &[token("ALIAS", 4, 0)]);
        assert_eq!(out, "live ALIAS caption text");
    }

    #[test]
    fn display_splice_clamps_overflowing_anchor() {
        let out = splice_display("short", &[token("X", 100, 0)]);
        assert_eq!(out, "short X");
    }

    #[test]
    fn display_splice_is_repeatable_from_same_base() {
        // Every emission re-derives from the same base: same inputs, same
        // output — tokens neither drift nor accumulate.
        let tokens = [token("A", 3, 0), token("B", 9, 0)];
        let first = splice_display("the quick brown fox", &tokens);
        let second = splice_display("the quick brown fox", &tokens);
        assert_eq!(first, second);
        assert_eq!(first, "the A quick B brown fox");
    }

    #[test]
    fn token_text_is_trimmed_on_insert() {
        let out = splice_tokens(
            "word",
            &[token("  padded  ", 2, 0)],
            &AnchorTotals {
                total_chars: 4,
                total_ms: 0,
            },
        );
        assert_eq!(out, "wo padded rd");
    }
}
