//! End-to-end integration tests for Handy core functionality.
//!
//! These tests exercise the app's Rust code paths without a GUI or full Tauri
//! runtime. Each submodule focuses on a distinct subsystem:
//!
//! - `settings_tests` — settings creation, serialization, persistence, salvage
//! - `cli_tests` — CLI argument parsing and validation
//! - `transcription_tests` — transcription pipeline types and data flow
//! - `coordinator_tests` — AppState state machine and query-state IPC

mod cli_tests;
mod coordinator_tests;
mod settings_race_tests;
mod settings_tests;
mod transcription_tests;