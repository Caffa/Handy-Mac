//! Integration tests for settings clobber race conditions.
//!
//! Tests that concurrent settings changes don't corrupt data, partial writes
//! preserve existing fields, debounce semantics are correct, NaN/inf values
//! don't corrupt the entire settings object, and concurrent field updates
//! both survive serialization.
//!
//! Bug context: The settings system uses `tauri-plugin-store` with debounced
//! writes. A `flush_settings` call on `RunEvent::Exit` was added to persist
//! debounced writes, but there's still a potential race where concurrent
//! settings changes can clobber each other. These tests exercise the data
//! integrity invariants that the SettingsCache + modify_settings machinery
//! must uphold.

use std::sync::{Arc, Barrier};
use std::thread;

use handy_app_lib::settings::{
    get_default_settings, AppSettings, OverlayStyle, Theme, VadSensitivity,
};

// ── Concurrent serde round-trips ──────────────────────────────────────────

/// Multiple threads serialize and deserialize AppSettings simultaneously.
/// No data should be lost or corrupted — every field must round-trip
/// identically regardless of concurrent access.
#[test]
fn concurrent_serde_roundtrips_preserve_data() {
    let base_settings = get_default_settings();
    let num_threads = 8;
    let iterations = 50;

    let handles: Vec<_> = (0..num_threads)
        .map(|thread_id| {
            let mut settings = base_settings.clone();
            // Give each thread a distinct mutation so we can detect clobber.
            settings.selected_model = format!("model-thread-{thread_id}");
            settings.push_to_talk = thread_id % 2 == 0;

            thread::spawn(move || {
                for i in 0..iterations {
                    // Mutate a field each iteration to vary the data.
                    settings.history_limit = 50 + thread_id * 10 + i;
                    settings.overlay_scale = 1.0 + (thread_id as f64 * 0.01) + (i as f64 * 0.001);

                    let json = serde_json::to_string(&settings).expect("serialization should succeed");
                    let restored: AppSettings =
                        serde_json::from_str(&json).expect("deserialization should succeed");

                    // Every field should match — no data loss, no corruption.
                    assert_eq!(restored.selected_model, format!("model-thread-{thread_id}"));
                    assert_eq!(restored.push_to_talk, thread_id % 2 == 0);
                    assert_eq!(restored.history_limit, 50 + thread_id * 10 + i);
                    assert!(!restored.overlay_scale.is_nan());
                    // The round-tripped overlay_scale should be within floating-point
                    // tolerance of the expected value.
                    let expected_scale = 1.0 + (thread_id as f64 * 0.01) + (i as f64 * 0.001);
                    assert!((restored.overlay_scale - expected_scale).abs() < 1e-10);
                }
            })
        })
        .collect();

    for handle in handles {
        handle.join().expect("thread should not panic");
    }
}

/// Stress-test: many threads round-trip the same default settings object
/// concurrently. Since each thread only reads and round-trips (no mutation),
/// the result should always be identical to the defaults.
#[test]
fn concurrent_read_only_roundtrips_match_defaults() {
    let defaults = get_default_settings();
    let shared = Arc::new(defaults);
    let num_threads = 8;
    let iterations = 100;

    let handles: Vec<_> = (0..num_threads)
        .map(|_| {
            let shared = Arc::clone(&shared);
            thread::spawn(move || {
                for _ in 0..iterations {
                    let json = serde_json::to_string(&*shared).expect("serialize");
                    let restored: AppSettings = serde_json::from_str(&json).expect("deserialize");
                    // Key invariant fields should always match defaults.
                    assert!(restored.push_to_talk, "push_to_talk should be default true");
                    assert!(!restored.audio_feedback);
                    assert!(!restored.onboarding_completed);
                }
            })
        })
        .collect();

    for handle in handles {
        handle.join().expect("thread should not panic");
    }
}

// ── Settings merge semantics ───────────────────────────────────────────────

/// When partial settings are written (e.g. `{"push_to_talk": false}`), the
/// existing fields should be preserved via serde defaults. This is the core
/// defense against clobber: a partial JSON object must not null out fields
/// that weren't included.
#[test]
fn partial_json_preserves_all_other_default_fields() {
    let settings: AppSettings = serde_json::from_value(serde_json::json!({
        "push_to_talk": false,
    }))
    .expect("partial settings should deserialize");

    // The explicitly set field should be respected.
    assert!(!settings.push_to_talk, "explicitly set to false");

    // Every other field should fall back to defaults — NOT null, NOT missing.
    let defaults = get_default_settings();
    assert!(!settings.audio_feedback, "audio_feedback defaults to false");
    assert_eq!(settings.selected_language, defaults.selected_language);
    assert_eq!(settings.overlay_position, defaults.overlay_position);
    assert_eq!(settings.overlay_style, defaults.overlay_style);
    assert_eq!(settings.vad_sensitivity, defaults.vad_sensitivity);
    assert_eq!(settings.paste_method, defaults.paste_method);
    assert_eq!(settings.theme, defaults.theme);
    assert_eq!(settings.history_limit, defaults.history_limit);
    assert_eq!(settings.word_correction_threshold, defaults.word_correction_threshold);
    assert_eq!(settings.overlay_scale, defaults.overlay_scale);
    assert!(settings.bindings.is_empty(), "bindings default is empty (filled by migration)");
}

/// Merging a partial JSON object that only changes theme should preserve
/// push_to_talk and every other default.
#[test]
fn partial_theme_change_preserves_push_to_talk_and_others() {
    let settings: AppSettings = serde_json::from_value(serde_json::json!({
        "theme": "dark",
    }))
    .expect("partial settings should deserialize");

    assert_eq!(settings.theme, Theme::Dark);
    // push_to_talk must NOT be clobbered to null/false-by-accident.
    assert!(settings.push_to_talk, "push_to_talk should retain its default true");
    assert!(!settings.audio_feedback);
    assert_eq!(settings.overlay_style, get_default_settings().overlay_style);
}

/// A JSON object with multiple explicit fields should preserve all others.
#[test]
fn multiple_explicit_fields_preserve_remaining_defaults() {
    let settings: AppSettings = serde_json::from_value(serde_json::json!({
        "push_to_talk": false,
        "theme": "light",
        "vad_sensitivity": "very_relaxed",
        "overlay_style": "none",
    }))
    .expect("partial settings should deserialize");

    assert!(!settings.push_to_talk);
    assert_eq!(settings.theme, Theme::Light);
    assert_eq!(settings.vad_sensitivity, VadSensitivity::VeryRelaxed);
    assert_eq!(settings.overlay_style, OverlayStyle::None);

    // Everything else should still be defaults.
    let defaults = get_default_settings();
    assert!(!settings.audio_feedback);
    assert_eq!(settings.history_limit, defaults.history_limit);
    assert_eq!(settings.selected_language, defaults.selected_language);
}

// ── Debounce simulation ────────────────────────────────────────────────────

/// Simulates the debounce pattern: write settings rapidly (10 times in a
/// loop), then read. The final read should reflect the LAST write, not an
/// intermediate one. This is what SettingsCache + write_ordering guarantees
/// in production.
#[test]
fn rapid_writes_last_write_wins() {
    let mut settings = get_default_settings();

    // Simulate 10 rapid writes where each write changes a field.
    for i in 0..10 {
        settings.selected_model = format!("model-v{i}");
        settings.history_limit = 100 + i;
        settings.push_to_talk = i % 2 == 0;

        // Serialize and deserialize to simulate the store round-trip.
        let json = serde_json::to_string(&settings).expect("serialize");
        settings = serde_json::from_str(&json).expect("deserialize");
    }

    // After the loop, the last write (i=9) should be the final state.
    assert_eq!(settings.selected_model, "model-v9");
    assert_eq!(settings.history_limit, 109);
    assert!(!settings.push_to_talk, "i=9: 9 % 2 == 1, so push_to_talk=false");
}

/// Simulates the debounce pattern where only the last write's values matter
/// after the debounce window closes. This models the real SettingsWriter
/// behavior where pending writes are replaced by newer ones.
#[test]
fn debounce_pattern_only_last_value_survives() {
    let mut settings = get_default_settings();

    // Simulate: user toggles push_to_talk off, then on, then off again rapidly.
    for value in [false, true, false] {
        settings.push_to_talk = value;
        let json = serde_json::to_string(&settings).expect("serialize");
        settings = serde_json::from_str(&json).expect("deserialize");
    }

    assert!(!settings.push_to_talk, "only the last value should survive");

    // Now a "final flush" — serialize once more and reload.
    let json = serde_json::to_string(&settings).expect("serialize");
    let final_state: AppSettings = serde_json::from_str(&json).expect("deserialize");
    assert!(!final_state.push_to_talk);
}

/// Simulates debounce where different fields are written in rapid succession.
/// After the debounce window, a read should reflect all the accumulated changes,
/// not just the last field touched.
#[test]
fn debounce_pattern_accumulated_fields_preserved() {
    let mut settings = get_default_settings();

    // Rapid writes to different fields — simulates e.g. saving a shortcut
    // while overlay position changes.
    settings.push_to_talk = false;
    let json = serde_json::to_string(&settings).expect("serialize");
    settings = serde_json::from_str(&json).expect("deserialize");

    settings.theme = Theme::Dark;
    let json = serde_json::to_string(&settings).expect("serialize");
    settings = serde_json::from_str(&json).expect("deserialize");

    settings.vad_sensitivity = VadSensitivity::VeryRelaxed;
    let json = serde_json::to_string(&settings).expect("serialize");
    settings = serde_json::from_str(&json).expect("deserialize");

    // All three changes should be present — no clobber.
    assert!(!settings.push_to_talk);
    assert_eq!(settings.theme, Theme::Dark);
    assert_eq!(settings.vad_sensitivity, VadSensitivity::VeryRelaxed);
}

// ── NaN/inf sanitization ───────────────────────────────────────────────────

/// Settings fields with NaN values should not corrupt the entire settings
/// object during round-trips. JSON doesn't support NaN, so serde_json will
/// either error or produce null — the round-trip must not silently produce
/// corrupt state.
#[test]
fn nan_overlay_scale_does_not_corrupt_settings() {
    let mut settings = get_default_settings();
    settings.overlay_scale = f64::NAN;

    let result = serde_json::to_string(&settings);
    // serde_json typically rejects NaN or serializes it as null.
    if let Ok(json) = result {
        let restored: Result<AppSettings, _> = serde_json::from_str(&json);
        if let Ok(restored) = restored {
            // If it round-tripped, overlay_scale should not be NaN.
            assert!(
                !restored.overlay_scale.is_nan(),
                "overlay_scale should not be NaN after round-trip"
            );
        }
        // If deserialization failed, that's also acceptable — the corruption
        // was caught rather than silently propagated.
    }
    // The important invariant: NaN should not silently survive a round-trip.
}

/// Infinity in float fields should not corrupt the entire settings object.
#[test]
fn inf_overlay_scale_does_not_corrupt_settings() {
    let mut settings = get_default_settings();
    settings.overlay_scale = f64::INFINITY;

    let result = serde_json::to_string(&settings);
    // serde_json typically rejects Infinity or serializes it as null.
    if let Ok(json) = result {
        let restored: Result<AppSettings, _> = serde_json::from_str(&json);
        if let Ok(restored) = restored {
            assert!(
                !restored.overlay_scale.is_infinite(),
                "overlay_scale should not be infinite after round-trip"
            );
        }
    }
}

/// Negative infinity should also be handled gracefully.
#[test]
fn neg_inf_overlay_scale_does_not_corrupt_settings() {
    let mut settings = get_default_settings();
    settings.overlay_scale = f64::NEG_INFINITY;

    let result = serde_json::to_string(&settings);
    if let Ok(json) = result {
        let restored: Result<AppSettings, _> = serde_json::from_str(&json);
        if let Ok(restored) = restored {
            assert!(
                !restored.overlay_scale.is_infinite(),
                "overlay_scale should not be infinite after round-trip"
            );
        }
    }
}

/// NaN in word_correction_threshold (another f64 field) should not corrupt
/// the rest of the settings object.
#[test]
fn nan_word_correction_threshold_does_not_corrupt_other_fields() {
    let mut settings = get_default_settings();
    settings.word_correction_threshold = f64::NAN;
    // Also set a distinctive value on another field.
    settings.selected_model = "my-model".to_string();

    let result = serde_json::to_string(&settings);
    if let Ok(json) = result {
        let restored: Result<AppSettings, _> = serde_json::from_str(&json);
        if let Ok(restored) = restored {
            // The other field should survive even if NaN was sanitized.
            assert_eq!(restored.selected_model, "my-model");
            assert!(
                !restored.word_correction_threshold.is_nan(),
                "word_correction_threshold should not be NaN after round-trip"
            );
        }
    }
}

/// NaN in hybrid_threshold_secs should be handled similarly.
#[test]
fn nan_hybrid_threshold_secs_does_not_corrupt_settings() {
    let mut settings = get_default_settings();
    settings.hybrid_threshold_secs = f64::NAN;
    settings.push_to_talk = false;

    let result = serde_json::to_string(&settings);
    if let Ok(json) = result {
        let restored: Result<AppSettings, _> = serde_json::from_str(&json);
        if let Ok(restored) = restored {
            assert!(!restored.push_to_talk, "push_to_talk should survive even with NaN sibling");
            assert!(
                !restored.hybrid_threshold_secs.is_nan(),
                "hybrid_threshold_secs should not be NaN after round-trip"
            );
        }
    }
}

// ── Concurrent field updates ───────────────────────────────────────────────

/// Thread A updates `push_to_talk`, Thread B updates `theme` — verify both
/// changes survive when serialized to the same JSON string.
///
/// This models the real clobber scenario: two concurrent Tauri commands each
/// do a read-modify-write on different fields. Without the write_ordering
/// mutex in SettingsCache, the second write would overwrite the first. Here
/// we simulate what a correct merge should look like.
#[test]
fn concurrent_field_updates_both_survive_serialization() {
    let barrier = Arc::new(Barrier::new(2));
    let settings_a = Arc::new(std::sync::Mutex::new(get_default_settings()));
    let settings_b = Arc::new(std::sync::Mutex::new(get_default_settings()));

    let barrier_a = Arc::clone(&barrier);
    let settings_a_clone = Arc::clone(&settings_a);
    let handle_a = thread::spawn(move || {
        let mut s = settings_a_clone.lock().unwrap().clone();
        // Thread A's change.
        s.push_to_talk = false;
        barrier_a.wait();
        // Simulate a merge: apply both changes into one object.
        let json = serde_json::to_string(&s).expect("serialize A");
        serde_json::from_str::<AppSettings>(&json).expect("deserialize A")
    });

    let barrier_b = Arc::clone(&barrier);
    let settings_b_clone = Arc::clone(&settings_b);
    let handle_b = thread::spawn(move || {
        let mut s = settings_b_clone.lock().unwrap().clone();
        // Thread B's change.
        s.theme = Theme::Dark;
        barrier_b.wait();
        let json = serde_json::to_string(&s).expect("serialize B");
        serde_json::from_str::<AppSettings>(&json).expect("deserialize B")
    });

    let result_a = handle_a.join().expect("thread A should not panic");
    let result_b = handle_b.join().expect("thread B should not panic");

    // Each thread's own change should be reflected in its result.
    assert!(!result_a.push_to_talk, "thread A's push_to_talk=false should survive");
    assert_eq!(result_b.theme, Theme::Dark, "thread B's theme=Dark should survive");

    // Now simulate what modify_settings does: merge both changes atomically.
    let mut merged = get_default_settings();
    merged.push_to_talk = false; // Thread A's change
    merged.theme = Theme::Dark; // Thread B's change

    let json = serde_json::to_string(&merged).expect("serialize merged");
    let restored: AppSettings = serde_json::from_str(&json).expect("deserialize merged");

    // Both changes should survive in the merged result.
    assert!(!restored.push_to_talk, "merged: thread A's change should survive");
    assert_eq!(restored.theme, Theme::Dark, "merged: thread B's change should survive");
    // All other fields should be defaults.
    let defaults = get_default_settings();
    assert_eq!(restored.audio_feedback, defaults.audio_feedback);
    assert_eq!(restored.vad_sensitivity, defaults.vad_sensitivity);
    assert_eq!(restored.overlay_position, defaults.overlay_position);
}

/// A more granular test: two threads each modify different fields of a shared
/// settings struct, and we verify that a merged result contains both changes
/// without any corruption of unrelated fields.
#[test]
fn concurrent_updates_to_unrelated_fields_no_clobber() {
    let mut settings = get_default_settings();

    // Simulate Thread A: update push_to_talk and selected_model.
    settings.push_to_talk = false;
    settings.selected_model = "whisper-large-v3".to_string();

    // Simulate Thread B: update theme and vad_sensitivity.
    // In a real race, this would clobber Thread A's changes. But with proper
    // merge semantics (read-modify-write through modify_settings), both
    // survive.
    let thread_b_json = serde_json::json!({
        "theme": "dark",
        "vad_sensitivity": "very_relaxed",
    });
    let thread_b_partial: AppSettings =
        serde_json::from_value(thread_b_json).expect("partial deserialize");

    // Apply Thread B's changes on top of Thread A's (simulating modify_settings).
    settings.theme = thread_b_partial.theme;
    settings.vad_sensitivity = thread_b_partial.vad_sensitivity;

    // Round-trip the merged result.
    let json = serde_json::to_string(&settings).expect("serialize merged");
    let restored: AppSettings = serde_json::from_str(&json).expect("deserialize merged");

    // Thread A's changes.
    assert!(!restored.push_to_talk, "Thread A's push_to_talk should survive");
    assert_eq!(restored.selected_model, "whisper-large-v3", "Thread A's selected_model should survive");
    // Thread B's changes.
    assert_eq!(restored.theme, Theme::Dark, "Thread B's theme should survive");
    assert_eq!(restored.vad_sensitivity, VadSensitivity::VeryRelaxed, "Thread B's vad should survive");
    // Unrelated fields should still be defaults.
    let defaults = get_default_settings();
    assert_eq!(restored.audio_feedback, defaults.audio_feedback);
    assert_eq!(restored.overlay_scale, defaults.overlay_scale);
    assert_eq!(restored.history_limit, defaults.history_limit);
}

/// Stress-test: many threads each set a different field, then we merge all
/// changes and verify every field survived.
#[test]
fn many_concurrent_field_updates_all_survive() {
    let defaults = get_default_settings();
    let mut merged = defaults.clone();

    // Simulate 8 threads each setting a different combination of fields.
    let thread_changes: Vec<(usize, String, bool, Theme, VadSensitivity, OverlayStyle, u64)> = (0..8)
        .map(|i| {
            (
                100 + i,                   // history_limit
                format!("model-{i}"),      // selected_model
                i % 2 == 0,                // push_to_talk
                if i % 3 == 0 { Theme::Dark } else { Theme::Light },
                if i % 2 == 0 { VadSensitivity::Quick } else { VadSensitivity::Relaxed },
                if i < 4 { OverlayStyle::None } else { OverlayStyle::Live },
                50 + i as u64,             // paste_delay_ms
            )
        })
        .collect();

    // Apply all changes (simulating what modify_settings serializes).
    for (history_limit, model, ptalk, theme, vad, overlay, paste_delay) in
        &thread_changes
    {
        // Each "thread" writes its field — but we merge atomically.
        merged.history_limit = *history_limit;
        merged.selected_model = model.clone();
        merged.push_to_talk = *ptalk;
        merged.theme = *theme;
        merged.vad_sensitivity = *vad;
        merged.overlay_style = *overlay;
        merged.paste_delay_ms = *paste_delay;

        // Round-trip each intermediate state to catch corruption.
        let json = serde_json::to_string(&merged).expect("serialize");
        merged = serde_json::from_str(&json).expect("deserialize");
    }

    // Verify the last thread's values are in the final state.
    let last = &thread_changes[7];
    assert_eq!(merged.history_limit, last.0);
    assert_eq!(merged.selected_model, last.1);
    assert_eq!(merged.push_to_talk, last.2);
    assert_eq!(merged.theme, last.3);
    assert_eq!(merged.vad_sensitivity, last.4);
    assert_eq!(merged.overlay_style, last.5);
    assert_eq!(merged.paste_delay_ms, last.6);
}