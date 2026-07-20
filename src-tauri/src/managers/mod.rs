pub mod audio;
pub mod gguf_meta;
pub mod history;
pub mod model;
pub mod model_capabilities;
pub mod transcription;

// Modules from later PRs — temporarily disabled so PR 1 compiles independently.
// Remove the cfg(any()) guard when each module's PR is merged.
#[cfg(any())] pub mod retry_worker;          // PR: retry worker — depends on TranscriptionOutput
#[cfg(any())] pub mod transcription_retry;   // PR: transcription retry — depends on uuid crate
