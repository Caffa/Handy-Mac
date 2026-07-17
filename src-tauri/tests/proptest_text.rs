//! Property-based tests for text processing functions.
//!
//! Uses proptest to verify invariants of text processing functions in
//! `audio_toolkit::text`, including `suppress_repeated_words`,
//! `detect_repeated_words`, `convert_us_to_british`, and `apply_custom_words`.

use handy_app_lib::audio_toolkit::{
    apply_custom_words, convert_us_to_british, detect_repeated_words,
    suppress_repeated_words,
};
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

/// Generate random suppression levels (0-3).
fn suppression_level() -> impl Strategy<Value = u8> {
    0u8..=3
}

/// Generate random custom word lists.
fn custom_words() -> impl Strategy<Value = Vec<String>> {
    prop::collection::vec("[a-zA-Z]{1,15}", 0..10)
}

/// Generate random thresholds.
fn correction_threshold() -> impl Strategy<Value = f64> {
    0.0f64..1.0
}

// ─── suppress_repeated_words ────────────────────────────────────────────

proptest! {
    /// Invariant: level == 0 → returns text unchanged
    #[test]
    fn proptest_suppress_level_zero(text in utf8_string()) {
        let result = suppress_repeated_words(&text, 0);
        prop_assert_eq!(result, text,
            "Level 0 should return text unchanged");
    }

    /// Invariant: never panics on any valid UTF-8 input
    #[test]
    fn proptest_suppress_no_panic(text in utf8_string(), level in suppression_level()) {
        let _ = suppress_repeated_words(&text, level);
    }

    /// Invariant: empty string → empty string (for any level)
    #[test]
    fn proptest_suppress_empty(level in suppression_level()) {
        let result = suppress_repeated_words("", level);
        prop_assert_eq!(result, "",
            "Empty string should remain empty");
    }
}

// ─── detect_repeated_words ───────────────────────────────────────────────

proptest! {
    /// Invariant: never panics on any valid UTF-8 input
    #[test]
    fn proptest_detect_repeated_no_panic(text in utf8_string()) {
        let _ = detect_repeated_words(&text);
    }
}

#[test]
fn detect_repeated_empty_yields_nothing() {
    let result = detect_repeated_words("");
    assert!(result.is_empty(), "Empty string should yield no repetitions");
}

// ─── convert_us_to_british ───────────────────────────────────────────────

#[test]
fn convert_us_to_british_empty_yields_empty() {
    let result = convert_us_to_british("");
    assert_eq!(result, "", "Empty string should remain empty");
}

proptest! {
    /// Invariant: never panics on any valid UTF-8 input
    #[test]
    fn proptest_convert_us_to_british_no_panic(text in utf8_string()) {
        let _ = convert_us_to_british(&text);
    }

    /// Invariant: converting twice should be idempotent (US→British is stable after first pass)
    #[test]
    fn proptest_convert_us_to_british_idempotent(text in utf8_string()) {
        let first = convert_us_to_british(&text);
        let second = convert_us_to_british(&first);
        prop_assert_eq!(second, first,
            "US→British conversion should be idempotent");
    }
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