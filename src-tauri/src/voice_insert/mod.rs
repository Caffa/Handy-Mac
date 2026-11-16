//! Voice insert: inject externally submitted text tokens into a running
//! transcription.
//!
//! Module layout:
//! - [`splice`] — pure anchor/splice math (no Tauri deps), unit-tested.
//!
// The pipeline wiring lands over the next commits (registry, actions, live
// display, server). Silence dead_code on the pure module until then.
#![allow(dead_code)]

pub mod splice;
