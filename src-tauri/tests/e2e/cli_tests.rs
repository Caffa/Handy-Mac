//! Integration tests for CLI argument parsing.
//!
//! Tests `CliArgs` parsing via `clap::Parser::try_from()`, exercising valid
//! inputs, flag conflicts, and edge cases. No Tauri runtime needed.

use clap::Parser;
use handy_app_lib::cli::CliArgs;

// ── Basic parsing ───────────────────────────────────────────────────────────

#[test]
fn cli_parses_empty_args() {
    // clap requires at least the binary name; Default works for the struct.
    let default = CliArgs::default();
    assert!(!default.start_hidden);
    assert!(!default.no_tray);
    assert!(!default.toggle_transcription);
    assert!(!default.toggle_post_process);
    assert!(!default.cancel);
    assert!(!default.is_active_use);
    assert!(!default.is_recording);
    assert!(!default.debug);
    assert!(default.transcribe_file.is_none());
    assert!(default.model.is_none());
    assert!(default.device_index.is_none());
    assert!(!default.list_devices);
    assert!(!default.list_models);
    assert!(default.repeat.is_none());
    assert!(!default.json);
}

#[test]
fn cli_parses_start_hidden() {
    let args = CliArgs::try_parse_from(["handy", "--start-hidden"]).unwrap();
    assert!(args.start_hidden);
}

#[test]
fn cli_parses_no_tray() {
    let args = CliArgs::try_parse_from(["handy", "--no-tray"]).unwrap();
    assert!(args.no_tray);
}

#[test]
fn cli_parses_debug() {
    let args = CliArgs::try_parse_from(["handy", "--debug"]).unwrap();
    assert!(args.debug);
}

#[test]
fn cli_parses_multiple_flags() {
    let args = CliArgs::try_parse_from(["handy", "--start-hidden", "--debug", "--no-tray"])
        .unwrap();
    assert!(args.start_hidden);
    assert!(args.debug);
    assert!(args.no_tray);
}

// ── Transcribe file ──────────────────────────────────────────────────────────

#[test]
fn cli_parses_transcribe_file() {
    let args = CliArgs::try_parse_from(["handy", "--transcribe-file", "/path/to/audio.wav"]).unwrap();
    assert_eq!(
        args.transcribe_file.unwrap().to_str().unwrap(),
        "/path/to/audio.wav"
    );
}

#[test]
fn cli_parses_transcribe_file_short_flag() {
    let args = CliArgs::try_parse_from(["handy", "-f", "audio.wav"]).unwrap();
    assert_eq!(args.transcribe_file.unwrap().to_str().unwrap(), "audio.wav");
}

#[test]
fn cli_parses_transcribe_file_with_model() {
    let args = CliArgs::try_parse_from([
        "handy",
        "--transcribe-file",
        "audio.wav",
        "--model",
        "large-v3",
    ])
    .unwrap();
    assert_eq!(args.transcribe_file.unwrap().to_str().unwrap(), "audio.wav");
    assert_eq!(args.model.unwrap(), "large-v3");
}

#[test]
fn cli_parses_transcribe_file_with_device_index() {
    let args = CliArgs::try_parse_from([
        "handy",
        "--transcribe-file",
        "audio.wav",
        "--device-index",
        "2",
    ])
    .unwrap();
    assert_eq!(args.device_index.unwrap(), 2);
}

#[test]
fn cli_parses_json_flag() {
    let args = CliArgs::try_parse_from(["handy", "--json"]).unwrap();
    assert!(args.json);
}

#[test]
fn cli_parses_transcribe_file_with_json_and_repeat() {
    let args = CliArgs::try_parse_from([
        "handy",
        "--transcribe-file",
        "audio.wav",
        "--json",
        "--repeat",
        "5",
    ])
    .unwrap();
    assert!(args.json);
    assert_eq!(args.repeat.unwrap(), 5);
}

// ── Query flags ──────────────────────────────────────────────────────────────

#[test]
fn cli_parses_is_active_use() {
    let args = CliArgs::try_parse_from(["handy", "--is-active-use"]).unwrap();
    assert!(args.is_active_use);
}

#[test]
fn cli_parses_is_recording() {
    let args = CliArgs::try_parse_from(["handy", "--is-recording"]).unwrap();
    assert!(args.is_recording);
}

// ── List flags ───────────────────────────────────────────────────────────────

#[test]
fn cli_parses_list_models() {
    let args = CliArgs::try_parse_from(["handy", "--list-models"]).unwrap();
    assert!(args.list_models);
}

#[test]
fn cli_parses_list_devices() {
    let args = CliArgs::try_parse_from(["handy", "--list-devices"]).unwrap();
    assert!(args.list_devices);
}

// ── Conflict detection ──────────────────────────────────────────────────────

#[test]
fn cli_rejects_is_active_use_with_start_hidden() {
    let result = CliArgs::try_parse_from(["handy", "--is-active-use", "--start-hidden"]);
    assert!(result.is_err(), "conflicting flags should be rejected");
}

#[test]
fn cli_rejects_is_active_use_with_no_tray() {
    let result = CliArgs::try_parse_from(["handy", "--is-active-use", "--no-tray"]);
    assert!(result.is_err(), "conflicting flags should be rejected");
}

#[test]
fn cli_rejects_is_active_use_with_toggle_transcription() {
    let result = CliArgs::try_parse_from(["handy", "--is-active-use", "--toggle-transcription"]);
    assert!(result.is_err(), "conflicting flags should be rejected");
}

#[test]
fn cli_rejects_is_active_use_with_toggle_post_process() {
    let result = CliArgs::try_parse_from(["handy", "--is-active-use", "--toggle-post-process"]);
    assert!(result.is_err(), "conflicting flags should be rejected");
}

#[test]
fn cli_rejects_is_active_use_with_cancel() {
    let result = CliArgs::try_parse_from(["handy", "--is-active-use", "--cancel"]);
    assert!(result.is_err(), "conflicting flags should be rejected");
}

#[test]
fn cli_rejects_is_active_use_with_transcribe_file() {
    let result = CliArgs::try_parse_from([
        "handy",
        "--is-active-use",
        "--transcribe-file",
        "audio.wav",
    ]);
    assert!(result.is_err(), "conflicting flags should be rejected");
}

#[test]
fn cli_rejects_is_recording_with_start_hidden() {
    let result = CliArgs::try_parse_from(["handy", "--is-recording", "--start-hidden"]);
    assert!(result.is_err(), "conflicting flags should be rejected");
}

#[test]
fn cli_rejects_is_recording_with_transcribe_file() {
    let result = CliArgs::try_parse_from([
        "handy",
        "--is-recording",
        "--transcribe-file",
        "audio.wav",
    ]);
    assert!(result.is_err(), "conflicting flags should be rejected");
}

// ── Valid combinations ───────────────────────────────────────────────────────

#[test]
fn cli_allows_toggle_transcription_with_debug() {
    let args = CliArgs::try_parse_from(["handy", "--toggle-transcription", "--debug"]).unwrap();
    assert!(args.toggle_transcription);
    assert!(args.debug);
}

#[test]
fn cli_allows_transcribe_file_with_json_and_repeat() {
    let args = CliArgs::try_parse_from([
        "handy",
        "--transcribe-file",
        "test.wav",
        "--json",
        "--repeat",
        "3",
    ])
    .unwrap();
    assert_eq!(args.transcribe_file.unwrap().to_str().unwrap(), "test.wav");
    assert!(args.json);
    assert_eq!(args.repeat.unwrap(), 3);
}

#[test]
fn cli_allows_toggle_post_process_with_cancel() {
    // These don't conflict — they're processed sequentially by the single-instance handler.
    let args = CliArgs::try_parse_from(["handy", "--toggle-post-process", "--cancel"]).unwrap();
    assert!(args.toggle_post_process);
    assert!(args.cancel);
}