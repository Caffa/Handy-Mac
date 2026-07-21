//! Property-based tests for text processing functions.
//!
//! Uses proptest to verify invariants of text processing functions in
//! `audio_toolkit::text`, including `apply_custom_words`.
//!
//! **Removed symbols**: `suppress_repeated_words`, `detect_repeated_words`,
//! and `convert_us_to_british` were removed during upstream alignment.
//! When they are re-added, add proptest tests for:
//! - `suppress_repeated_words` level == 0 → returns text unchanged
//! - `suppress_repeated_words` never panics on any valid UTF-8 input
//! - `suppress_repeated_words` empty string → empty string
//! - `detect_repeated_words` never panics on any valid UTF-8 input
//! - `detect_repeated_words` empty → no results
//! - `convert_us_to_british` empty → empty
//! - `convert_us_to_british` never panics on any valid UTF-8 input
//! - `convert_us_to_british` is idempotent (US→British is stable after first pass)

use handy_app_lib::audio_toolkit::apply_custom_words;
use proptest::prelude::*;

// ─── Strategies ──────────────────────────────────────────────────────────

/// Generate random UTF-8 strings from a broad character range.
fn utf8_string() -> impl Strategy<Value = String> {
    prop_oneof![
        Just(String::new()),
        "[a-zA-Z ]{0,50}",
        "[a-zA-Z .,!?'\"\\-]{0,50}",
        "[a-zA-Zäöüéñ ]{0,30}",
        "[a-zA-Z]{1,20}",
    ]
}

/// Generate random custom word lists.
fn custom_words() -> impl Strategy<Value = Vec<String>> {
    prop::collection::vec("[a-zA-Z]{1,15}", 0..10)
}

/// Generate random thresholds.
fn correction_threshold() -> impl Strategy<Value = f64> {
    0.0f64..1.0
}

// ─── apply_custom_words ──────────────────────────────────────────────────

proptest! {
    /// Invariant: empty custom_words → returns text unchanged
    #[test]
    fn proptest_apply_custom_words_empty(text in utf8_string(), threshold in correction_threshold()) {
        let result = apply_custom_words(&text, &[], threshold);
        prop_assert_eq!(result, text,
            "Empty custom words should return text unchanged");
    }

    /// Invariant: never panics on any valid UTF-8 input
    #[test]
    fn proptest_apply_custom_words_no_panic(
        text in utf8_string(),
        words in custom_words(),
        threshold in correction_threshold()
    ) {
        let _ = apply_custom_words(&text, &words, threshold);
    }
}
