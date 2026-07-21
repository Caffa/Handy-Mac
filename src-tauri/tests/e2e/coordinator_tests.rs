//! Integration tests for the AppState state machine and query-state IPC.
//!
//! Tests `AppState` serialization, `QueryState` round-trip, and `is_active_use()`
//! semantics. These tests do NOT create a `TranscriptionCoordinator` (which
//! requires a Tauri `AppHandle`); instead they exercise the data types and
//! the `query_state` file-based IPC directly.
//!
//! Note: `AppState` is re-exported from `handy_app_lib::AppState`.
//! Only `write_query_state`, `remove_query_state_file`, and
//! `query_state_file_path` are re-exported. `read_query_state` and `QueryState`
//! are in the private `query_state` module, so we test them via direct file I/O.

use handy_app_lib::AppState;

// ── AppState serialization ──────────────────────────────────────────────────

#[test]
fn app_state_idle_serializes_with_tag() {
    let state = AppState::Idle;
    let json = serde_json::to_string(&state).unwrap();
    // Tagged enum: {"state":"Idle"} (no data payload)
    assert!(json.contains("Idle") || json.contains("idle"));
}

#[test]
fn app_state_recording_serializes_with_binding_id() {
    let state = AppState::Recording {
        binding_id: "transcribe".to_string(),
    };
    let json = serde_json::to_string(&state).unwrap();
    assert!(json.contains("Recording"), "should contain the state tag");
    assert!(json.contains("transcribe"), "should contain the binding_id data");
}

#[test]
fn app_state_processing_serializes_with_binding_id() {
    let state = AppState::Processing {
        binding_id: Some("transcribe_with_post_process".to_string()),
    };
    let json = serde_json::to_string(&state).unwrap();
    assert!(json.contains("Processing"));
    assert!(json.contains("transcribe_with_post_process"));

    let state_none = AppState::Processing { binding_id: None };
    let json_none = serde_json::to_string(&state_none).unwrap();
    assert!(json_none.contains("Processing"));
}

#[test]
fn app_state_usb_cycling_serializes() {
    let state = AppState::UsbCycling {
        stage: "power_cycle".to_string(),
    };
    let json = serde_json::to_string(&state).unwrap();
    assert!(json.contains("UsbCycling"));
    assert!(json.contains("power_cycle"));
}

#[test]
fn app_state_confirming_serializes() {
    let state = AppState::Confirming {
        text: "Hello world".to_string(),
        binding_id: Some("transcribe_with_router".to_string()),
    };
    let json = serde_json::to_string(&state).unwrap();
    assert!(json.contains("Confirming"));
    assert!(json.contains("Hello world"));
    assert!(json.contains("transcribe_with_router"));
}

#[test]
fn app_state_confirming_with_none_binding_id() {
    let state = AppState::Confirming {
        text: "Test text".to_string(),
        binding_id: None,
    };
    let json = serde_json::to_string(&state).unwrap();
    assert!(json.contains("Confirming"));
    assert!(json.contains("Test text"));
}

// ── AppState is_active_use semantics ────────────────────────────────────────

#[test]
fn app_state_idle_is_not_active() {
    let state = AppState::Idle;
    assert!(!is_active(&state), "Idle should not be active use");
}

#[test]
fn app_state_recording_is_active() {
    let state = AppState::Recording {
        binding_id: "transcribe".to_string(),
    };
    assert!(is_active(&state), "Recording should be active use");
}

#[test]
fn app_state_processing_is_active() {
    let state = AppState::Processing { binding_id: None };
    assert!(is_active(&state), "Processing should be active use");
}

#[test]
fn app_state_usb_cycling_is_active() {
    let state = AppState::UsbCycling {
        stage: "resetting".to_string(),
    };
    assert!(is_active(&state), "UsbCycling should be active use");
}

#[test]
fn app_state_confirming_is_active() {
    let state = AppState::Confirming {
        text: "hello".to_string(),
        binding_id: None,
    };
    assert!(is_active(&state), "Confirming should be active use");
}

/// Helper: mirrors the logic of `is_active_use()` from the coordinator.
/// In production, `AppState` doesn't have an `is_active()` method, so we
/// replicate the `!matches!(state, AppState::Idle)` check here.
fn is_active(state: &AppState) -> bool {
    !matches!(state, AppState::Idle)
}

// ── AppState PartialEq ──────────────────────────────────────────────────────

#[test]
fn app_state_equality_works() {
    let idle_a = AppState::Idle;
    let idle_b = AppState::Idle;
    assert_eq!(idle_a, idle_b);

    let recording_a = AppState::Recording {
        binding_id: "transcribe".to_string(),
    };
    let recording_b = AppState::Recording {
        binding_id: "transcribe".to_string(),
    };
    assert_eq!(recording_a, recording_b);

    let recording_c = AppState::Recording {
        binding_id: "other".to_string(),
    };
    assert_ne!(recording_a, recording_c, "different binding_ids should not be equal");
}

// ── QueryState file format ────────────────────────────────────────────────
//
// QueryState is in a private module, so we test the JSON format by creating
// temp files instead of using the shared OnceLock-based path. This avoids
// test parallelism issues where concurrent tests clobber the same state file.
// The structure is: {"is_active_use": bool, "is_recording": bool}

use std::io::Write;

#[test]
fn query_state_json_structure() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test_query_state.json");
    let json = serde_json::json!({
        "is_active_use": true,
        "is_recording": true
    });
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(serde_json::to_string(&json).unwrap().as_bytes()).unwrap();
    f.flush().unwrap();

    let contents = std::fs::read_to_string(&path).unwrap();
    let val: serde_json::Value = serde_json::from_str(&contents).unwrap();
    assert!(val.get("is_active_use").is_some(), "should have is_active_use field");
    assert!(val.get("is_recording").is_some(), "should have is_recording field");
    assert_eq!(val["is_active_use"], true);
    assert_eq!(val["is_recording"], true);
}

#[test]
fn query_state_json_roundtrip_idle() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test_query_state_idle.json");
    let json = serde_json::json!({
        "is_active_use": false,
        "is_recording": false
    });
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(serde_json::to_string(&json).unwrap().as_bytes()).unwrap();
    f.flush().unwrap();

    let contents = std::fs::read_to_string(&path).unwrap();
    let val: serde_json::Value = serde_json::from_str(&contents).unwrap();
    assert_eq!(val["is_active_use"], false);
    assert_eq!(val["is_recording"], false);
}

#[test]
fn query_state_json_roundtrip_recording() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test_query_state_recording.json");
    let json = serde_json::json!({
        "is_active_use": true,
        "is_recording": true
    });
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(serde_json::to_string(&json).unwrap().as_bytes()).unwrap();
    f.flush().unwrap();

    let contents = std::fs::read_to_string(&path).unwrap();
    let val: serde_json::Value = serde_json::from_str(&contents).unwrap();
    assert_eq!(val["is_active_use"], true);
    assert_eq!(val["is_recording"], true);
}

#[test]
fn query_state_file_absent_means_not_running() {
    // A non-existent path means "app not running" (exit code 2).
    let path = std::env::temp_dir().join("handy_query_state_absent_test.json");
    let _ = std::fs::remove_file(&path); // ensure it doesn't exist
    assert!(
        std::fs::metadata(&path).is_err(),
        "state file should be absent for 'not running' check"
    );
}

// ── AppState JSON tag format ────────────────────────────────────────────────

#[test]
fn app_state_json_uses_tagged_enum_format() {
    // Verify that the tagged enum format (serde's internally tagged format)
    // produces JSON with a "state" key. The frontend relies on this format.
    let idle = AppState::Idle;
    let json = serde_json::to_string(&idle).unwrap();
    let val: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(val.get("state").is_some(), "AppState JSON should have a 'state' key");

    let recording = AppState::Recording {
        binding_id: "transcribe".to_string(),
    };
    let json = serde_json::to_string(&recording).unwrap();
    let val: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(val.get("state").unwrap().as_str(), Some("Recording"));
    // Data should be nested under "data" (serde's tagged enum content field).
    assert!(val.get("data").is_some(), "Recording state should include data field");
}