//! Property-based tests for text processing functions.
//!
//! Uses proptest to verify invariants of text processing functions in
//! `audio_toolkit::text`, including `suppress_repeated_words`,
//! `detect_repeated_words`, `convert_us_to_british`, and `apply_custom_words`.
//!
//! TODO: `suppress_repeated_words`, `detect_repeated_words`, and
//! `convert_us_to_british` were removed during upstream alignment.
//! Tests referencing them are ignored until re-added. Only `apply_custom_words`
//! tests are active.

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
// TODO: suppress_repeated_words was removed during upstream alignment.
// These tests are ignored until the function is re-added.

proptest! {
    /// Invariant: level == 0 → returns text unchanged
    #[test]
    #[ignore = "suppress_repeated_words not yet ported from main"]
    fn proptest_suppress_level_zero(text in utf8_string()) {
        let _ = text; // Placeholder: suppress_repeated_words not available
    }

    /// Invariant: never panics on any valid UTF-8 input
    #[test]
    #[ignore = "suppress_repeated_words not yet ported from main"]
    fn proptest_suppress_no_panic(text in utf8_string(), level in suppression_level()) {
        let _ = (text, level); // Placeholder
    }

    /// Invariant: empty string → empty string (for any level)
    #[test]
    #[ignore = "suppress_repeated_words not yet ported from main"]
    fn proptest_suppress_empty(level in suppression_level()) {
        let _ = level; // Placeholder
    }
}

// ─── detect_repeated_words ───────────────────────────────────────────────
// TODO: detect_repeated_words was removed during upstream alignment.

proptest! {
    /// Invariant: never panics on any valid UTF-8 input
    #[test]
    #[ignore = "detect_repeated_words not yet ported from main"]
    fn proptest_detect_repeated_no_panic(text in utf8_string()) {
        let _ = text; // Placeholder
    }
}

#[test]
#[ignore = "detect_repeated_words not yet ported from main"]
fn detect_repeated_empty_yields_nothing() {
    // Placeholder: detect_repeated_words not available
}

// ─── convert_us_to_british ───────────────────────────────────────────────
// TODO: convert_us_to_british was removed during upstream alignment.
// Only convert_us_to_british_with_dict exists now.

#[test]
#[ignore = "convert_us_to_british not yet ported from main"]
fn convert_us_to_british_empty_yields_empty() {
    // Placeholder: convert_us_to_british not available
}

proptest! {
    /// Invariant: never panics on any valid UTF-8 input
    #[test]
    #[ignore = "convert_us_to_british not yet ported from main"]
    fn proptest_convert_us_to_british_no_panic(text in utf8_string()) {
        let _ = text; // Placeholder
    }

    /// Invariant: converting twice should be idempotent (US→British is stable after first pass)
    #[test]
    #[ignore = "convert_us_to_british not yet ported from main"]
    fn proptest_convert_us_to_british_idempotent(text in utf8_string()) {
        let _ = text; // Placeholder
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