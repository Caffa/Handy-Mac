// State machine tests for `TranscriptionCoordinator`.
//
// These tests exercise `CoordinatorCore`, the pure state-machine logic
// extracted from `TranscriptionCoordinator`. By testing the core directly,
// we avoid needing a real `AppHandle` — the core only touches plain Rust
// types (`Stage`, `AtomicBool`, `RwLock<AppState>`).
//
// TODO: `CoordinatorCore` was removed during upstream alignment. These tests
// are ignored until the core state machine is re-extracted or re-added.

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "CoordinatorCore not yet ported from main"]
    fn placeholder_transcription_coordinator_tests() {
        // All tests in this file depend on CoordinatorCore which doesn't exist yet
    }
}