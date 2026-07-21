//! File-based IPC for CLI query flags (`--is-active-use`, `--is-recording`).
//!
//! The running Handy instance writes its state to a temp file whenever the
//! application state changes.  A second instance launched with `--is-active-use`
//! or `--is-recording` (which runs in headless mode so the single-instance
//! plugin doesn't kill it) reads this file and exits with the appropriate code.
//!
//! Exit codes expected by `build-reinstall.sh`:
//! - 0: app is active use / recording
//! - 1: app is idle
//! - 2: app is not running (state file absent or unreadable)

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

/// Cached path so every caller gets the same value without re-computing.
static STATE_FILE_PATH: OnceLock<PathBuf> = OnceLock::new();

/// Guard to avoid writing the state file during initialisation before the
/// coordinator has been created.  Flipped to `true` once the coordinator
/// starts and begins emitting state.
static STATE_FILE_READY: AtomicBool = AtomicBool::new(false);

/// Mark the state file mechanism as ready (called once the coordinator is
/// initialised and will start writing state).
pub fn mark_state_file_ready() {
    STATE_FILE_READY.store(true, Ordering::Release);
}

/// Whether the state file mechanism has been marked ready.
fn is_ready() -> bool {
    STATE_FILE_READY.load(Ordering::Acquire)
}

/// Return the path to the query-state JSON file.
///
/// The file is placed in the OS temp directory so it is accessible from both
/// the running instance and any headless query instances.  On macOS this is
/// typically `/private/var/folders/…/T/`.
pub fn query_state_file_path() -> PathBuf {
    STATE_FILE_PATH
        .get_or_init(|| {
            let mut p = std::env::temp_dir();
            p.push("handy_query_state.json");
            p
        })
        .clone()
}

/// The serialisable state written to the file.
#[derive(Serialize, Deserialize, Debug)]
pub struct QueryState {
    pub is_active_use: bool,
    pub is_recording: bool,
}

/// Write the current application state to the query-state file.
///
/// Called from `TranscriptionCoordinator` after every state transition.
/// Silently ignores write errors (best-effort; a missing/stale file just means
/// the query instance will see "not running" which is safe).
pub fn write_query_state(is_active_use: bool, is_recording: bool) {
    if !is_ready() {
        return;
    }
    let state = QueryState {
        is_active_use,
        is_recording,
    };
    let path = query_state_file_path();
    if let Ok(json) = serde_json::to_string(&state) {
        let _ = std::fs::write(&path, json);
    }
}

/// Remove the query-state file on application exit.
///
/// When the file is absent the query instance interprets it as "not running"
/// (exit code 2), which is the correct semantic.
pub fn remove_query_state_file() {
    let path = query_state_file_path();
    let _ = std::fs::remove_file(&path);
}

/// Read the query-state file.  Returns `None` if the file doesn't exist or
/// can't be parsed (treated as "not running" by the caller).
pub fn read_query_state() -> Option<QueryState> {
    let path = query_state_file_path();
    let contents = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&contents).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_file_roundtrip() {
        let tmp = std::env::temp_dir().join("handy_query_state_test.json");
        STATE_FILE_PATH.get_or_init(|| tmp.clone());

        write_query_state(true, false);
        let s = read_query_state().unwrap();
        assert!(s.is_active_use);
        assert!(!s.is_recording);

        write_query_state(false, true);
        let s = read_query_state().unwrap();
        assert!(!s.is_active_use);
        assert!(s.is_recording);

        remove_query_state_file();
        assert!(read_query_state().is_none());

        // Clean up OnceLock — can't reset it, so just remove the file.
        let _ = std::fs::remove_file(&tmp);
    }
}
