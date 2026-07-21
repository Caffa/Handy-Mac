//! Property-based tests for `normalize_shortcut` idempotency.
//!
//! Verifies that `normalize(normalize(x)) == normalize(x)` for arbitrary
//! shortcut strings, including valid modifier+key combinations and
//! random garbage strings.

use handy_app_lib::shortcut::conflicts::normalize_shortcut;
use proptest::prelude::*;

// ─── Strategies ──────────────────────────────────────────────────────────

/// Generate realistic shortcut strings: modifier+key combinations.
fn realistic_shortcut() -> impl Strategy<Value = String> {
    let modifiers = vec![
        "ctrl", "control", "shift", "alt", "option", "cmd", "command", "meta", "super", "win",
        "windows", "fn",
    ];
    let keys = vec![
        "a",
        "b",
        "c",
        "z",
        "0",
        "1",
        "9",
        "f1",
        "f5",
        "f12",
        "space",
        "enter",
        "tab",
        "escape",
        "arrow_up",
        "arrow_down",
        "arrow_left",
        "arrow_right",
        "delete",
        "backspace",
        "insert",
        "home",
        "end",
        "comma",
        "period",
        "slash",
        "semicolon",
    ];

    (0..3_usize).prop_flat_map(move |n_mods| {
        let mod_strat = modifiers.clone();
        let key_strat = keys.clone();
        (
            prop::collection::vec(prop::sample::select(mod_strat), n_mods),
            prop::sample::select(key_strat),
        )
            .prop_map(move |(mods, key)| {
                let mut parts: Vec<&str> = mods;
                parts.push(&key);
                parts.join("+")
            })
    })
}

/// Generate completely random strings (including garbage, unicode, etc.).
fn arbitrary_string() -> impl Strategy<Value = String> {
    prop_oneof![
        Just(String::new()),
        "[a-zA-Z0-9+ ]{0,30}",
        "[a-zA-Z+!@#$%^&*()]{0,20}",
        "[cC][mM][dD]\\+[a-zA-Z]{1,5}",
    ]
}

proptest! {
    /// Invariant: normalize is idempotent — normalize(normalize(x)) == normalize(x)
    /// for realistic shortcut strings.
    #[test]
    fn proptest_normalize_shortcut_idempotent_realistic(shortcut in realistic_shortcut()) {
        let first = normalize_shortcut(&shortcut);
        let second = normalize_shortcut(&first);
        prop_assert_eq!(second, first,
            "Idempotency violated: input={:?}", shortcut);
    }

    /// Invariant: normalize is idempotent for arbitrary strings (including garbage).
    #[test]
    fn proptest_normalize_shortcut_idempotent_arbitrary(shortcut in arbitrary_string()) {
        let first = normalize_shortcut(&shortcut);
        let second = normalize_shortcut(&first);
        prop_assert_eq!(second, first,
            "Idempotency violated for arbitrary input: {:?}", shortcut);
    }

    /// Invariant: normalize never panics on any input.
    #[test]
    fn proptest_normalize_shortcut_no_panic(shortcut in arbitrary_string()) {
        let _ = normalize_shortcut(&shortcut);
    }

    /// Invariant: normalize output is always lowercase (except for the key part
    /// which may have underscores). All modifier names should be lowercase.
    #[test]
    fn proptest_normalize_shortcut_modifiers_lowercase(shortcut in realistic_shortcut()) {
        let normalized = normalize_shortcut(&shortcut);
        let parts: Vec<&str> = normalized.split('+').collect();
        if parts.len() > 1 {
            let modifier_set = [
                "ctrl", "control", "shift", "alt", "option", "meta",
                "cmd", "command", "super", "win", "windows", "fn",
            ];
            for part in &parts[..parts.len() - 1] {
                prop_assert!(modifier_set.contains(part),
                    "Modifier '{}' not in known set (normalized from '{}')",
                    part, shortcut);
            }
        }
    }
}
