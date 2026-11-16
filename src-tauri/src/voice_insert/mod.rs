//! Voice insert: inject externally submitted text tokens into a running
//! transcription.
//!
//! Module layout:
//! - [`splice`] — pure anchor/splice math (no Tauri deps), unit-tested.
//! - [`registry`] — per-recording token buffer owned outside the coordinator.
//! - [`server`] — localhost HTTP server (Phase 1).

pub mod registry;
pub mod server;
pub mod splice;

pub use registry::VoiceInsertRegistry;
pub use splice::{splice_display, splice_tokens};
