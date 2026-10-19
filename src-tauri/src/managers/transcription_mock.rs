// CI-only mock TranscriptionManager - avoids whisper/Vulkan/transcribe-cpp deps.
// This file is copied over transcription.rs by the "Use mock TranscriptionManager
// (CI only)" steps in .github/workflows/ci.yml and test.yml.
//
// It mirrors the public surface of the real engine-supervisor client in
// transcription.rs (plan §6: "keep the mock compiling"). Stub bodies are enough
// — CI tests never exercise transcription — so no EngineClient trait seam is
// needed: the real manager is a thin client over the cheap-clone
// `EngineSupervisor` handle, which the mock holds verbatim. Keep in sync with
// transcription.rs's pub surface; `cargo check` with the CI swap applied catches
// drift.
//
// The surface is intentionally mirror-complete, so some stubs exist only to
// match the real file's API and are never called under CI. Silence the
// resulting dead-code lints for this file only.
#![allow(dead_code)]

use crate::engine_supervisor::EngineSupervisor;
use crate::errors::AppResult;
use crate::managers::model::ModelManager;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use specta::Type;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;
use tauri::AppHandle;

/// Mirror of the real slot-wait timeout (value irrelevant in CI).
pub(crate) const TRANSCRIBING_SLOT_WAIT_TIMEOUT: Duration = Duration::from_secs(30);

/// Result of a transcription call, including metadata about which model was used.
#[derive(Clone, Debug, Serialize)]
pub struct TranscriptionOutput {
    /// The transcribed text.
    pub text: String,
    /// The model ID that produced this transcription (e.g. "turbo", "parakeet-tdt-0.6b-v2").
    pub model_id: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ModelStateEvent {
    pub event_type: String,
    pub model_id: Option<String>,
    pub model_name: Option<String>,
    pub error: Option<String>,
}

/// Live transcription snapshot emitted to the overlay during a streaming run.
#[derive(Clone, Debug, Serialize, Deserialize, Type, tauri_specta::Event)]
pub struct StreamTextEvent {
    pub committed: String,
    pub tentative: String,
}

/// Phase of the streaming overlay card, emitted to drive its UI state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "lowercase")]
pub enum StreamPhase {
    Listening,
    Working,
}

/// Semantic kind of "working" phase, used to localize the spinner label.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "lowercase")]
pub enum StreamWorkKind {
    Transcribing,
    Polishing,
}

/// Emitted to switch the streaming overlay to a working spinner.
#[derive(Clone, Debug, Serialize, Deserialize, Type, tauri_specta::Event)]
pub struct StreamPhaseEvent {
    pub phase: StreamPhase,
    /// Present only when `phase` is `Working`.
    pub kind: Option<StreamWorkKind>,
}

/// Routes real-time audio frames to the active streaming worker. Always-closed
/// no-op in the mock: `feed` drops frames and `is_open` reports false, so the
/// recorder treats every stream as inactive.
#[derive(Default)]
pub struct StreamRouter;

impl StreamRouter {
    /// No-op in the mock.
    pub fn feed(&self, _frame: &[f32]) {}

    /// No stream is ever pending or active in the mock.
    pub fn is_open(&self) -> bool {
        false
    }
}

/// RAII guard that is a no-op in the mock — mirrors the real `LoadingGuard`.
pub struct LoadingGuard;

#[derive(Clone)]
pub struct TranscriptionManager {
    /// Cheap-clone supervisor handle, mirroring the real manager's `engine`
    /// field so `engine_supervisor()` hands out clones instead of spawning a
    /// supervisor per call.
    engine: EngineSupervisor,
    #[allow(dead_code)]
    app_handle: AppHandle,
}

impl TranscriptionManager {
    pub fn new(app_handle: &AppHandle, _model_manager: Arc<ModelManager>) -> Result<Self> {
        Ok(Self {
            engine: EngineSupervisor::new(false),
            app_handle: app_handle.clone(),
        })
    }

    pub fn is_model_loaded(&self) -> bool {
        false
    }

    pub fn try_is_model_loaded(&self) -> bool {
        false
    }

    pub fn is_model_loading(&self) -> bool {
        false
    }

    pub fn set_pending_model(&self, _model_id: &str) {}

    pub fn take_pending_model(&self) -> Option<String> {
        None
    }

    pub fn reload_model_on_next_use(&self) {}

    pub fn retry_transcribe_gpu(&self, _reason: &str) {}

    pub fn try_start_loading(&self) -> Option<LoadingGuard> {
        Some(LoadingGuard)
    }

    pub fn unload_model(&self) -> Result<()> {
        Ok(())
    }

    pub fn request_unload(&self) {}

    pub fn cancel_streaming(&self) {}

    pub fn clear_streaming_cancel(&self) {}

    pub fn is_streaming_cancelled(&self) -> bool {
        false
    }

    pub fn streaming_cancel_flag(&self) -> Arc<AtomicBool> {
        Arc::new(AtomicBool::new(false))
    }

    pub fn check_and_force_release_transcribing_slot(&self) {}

    pub fn maybe_unload_immediately(&self, _context: &str) {}

    pub fn load_model(&self, _model_id: &str) -> Result<()> {
        Ok(())
    }

    pub fn load_model_with_device(&self, _model_id: &str, _device_index: Option<usize>) -> Result<()> {
        Ok(())
    }

    pub fn initiate_model_load(&self) {}

    pub fn initiate_model_load_for(&self, _model_id: &str) {}

    pub fn get_current_model(&self) -> Option<String> {
        None
    }

    pub fn current_backend(&self) -> Option<String> {
        None
    }

    pub fn is_streaming(&self) -> bool {
        false
    }

    pub fn stream_router(&self) -> Arc<StreamRouter> {
        Arc::new(StreamRouter)
    }

    pub fn cancel_transcription(&self) {}

    /// Cheap clone of the mock's supervisor handle (never spawns a worker —
    /// `EngineSupervisor::new(false)` only starts its idle owner thread).
    pub fn engine_supervisor(&self) -> EngineSupervisor {
        self.engine.clone()
    }

    pub fn start_stream(&self) {}

    pub fn finalize_stream(&self) -> Result<Option<String>> {
        Ok(None)
    }

    pub fn cancel_stream(&self) {}

    pub fn emit_stream_working(&self, _kind: StreamWorkKind) {}

    pub fn transcribe_with_hook<F>(
        &self,
        _audio: Vec<f32>,
        _clear_cancel_after_slot: F,
    ) -> AppResult<TranscriptionOutput>
    where
        F: FnOnce(),
    {
        Ok(self.stub_output())
    }

    pub fn transcribe(&self, _audio: Vec<f32>) -> AppResult<TranscriptionOutput> {
        Ok(self.stub_output())
    }

    pub fn transcribe_for_benchmark(&self, _audio: Vec<f32>) -> Result<String> {
        Ok(String::new())
    }

    fn stub_output(&self) -> TranscriptionOutput {
        TranscriptionOutput {
            text: String::new(),
            model_id: String::new(),
        }
    }
}

/// No-op in CI mock.
pub fn apply_accelerator_settings(_app: &tauri::AppHandle) {}

/// No-op in CI mock (detached-handle variant).
pub fn report_compute_devices_on(_engine: &EngineSupervisor) {}

/// Returns empty lists in CI mock (detached-handle variant).
pub fn get_available_accelerators_on(_engine: &EngineSupervisor) -> AvailableAccelerators {
    AvailableAccelerators {
        transcribe: vec![],
        ort: vec![],
        gpu_devices: vec![],
    }
}

/// Returns empty lists in CI mock.
pub fn get_available_accelerators(tm: &TranscriptionManager) -> AvailableAccelerators {
    get_available_accelerators_on(&tm.engine)
}

// The CI swap removes the real transcription.rs — the only non-test caller of
// the model-language helpers (P2/P6 language-coverage wiring) and of the
// Windows-emulation probe in utils.rs. Reference them here so the swapped-in
// tree stays clippy-clean under `-D warnings`; model.rs's private
// `base_language` stays live transitively through these two.
#[allow(dead_code)]
fn keep_swapped_orphaned_helpers_reachable() {
    let _ = crate::managers::model::canonical_language_code;
    let _ = crate::managers::model::effective_language;
    let _ = crate::utils::is_windows_x64_emulated_on_arm64;
}

#[derive(Serialize, Clone, Debug, Type)]
pub struct GpuDeviceOption {
    pub id: String,
    pub name: String,
    pub total_vram_mb: usize,
}

#[derive(Serialize, Clone, Debug, Type)]
pub struct AvailableAccelerators {
    pub transcribe: Vec<String>,
    pub ort: Vec<String>,
    pub gpu_devices: Vec<GpuDeviceOption>,
}
