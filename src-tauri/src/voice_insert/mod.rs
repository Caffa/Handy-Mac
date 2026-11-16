//! Voice insert: inject externally submitted text tokens into a running
//! transcription.
//!
//! Module layout:
//! - [`splice`] — pure anchor/splice math (no Tauri deps), unit-tested.
//! - [`registry`] — per-recording token buffer owned outside the coordinator.
//!
// The remaining wiring lands over the next commits (actions, live display,
// server). Silence dead_code on the pure modules until then.
#![allow(dead_code)]

pub mod registry;
pub mod splice;

pub use registry::VoiceInsertRegistry;
pub use splice::{splice_display, splice_tokens};
