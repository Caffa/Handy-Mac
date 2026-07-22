# Removed Custom Fork Features

This document lists the custom fork-only features that were removed while porting upstream Handy's unified `transcribe-cpp` model architecture into the `new-models` branch.

## Background

Upstream Handy replaced the dual `transcribe-rs` (ONNX) + `whisper-rs` (GGUF) pipeline with a single `transcribe-cpp` engine that handles all model types. That unified architecture has its own acceleration selection, model catalog, and transcription pipeline, so several fork-specific extensions became redundant or incompatible.

## Removed Features

### 1. Adaptive Parakeet Thresholds
- **What it did:** Dynamically adjusted voice-activity thresholds for Parakeet-based models based on observed audio.
- **Why removed:** The upstream VAD and transcription pipeline use fixed, model-agnostic settings. Maintaining fork-specific per-model threshold logic would conflict with the unified engine.
- **Files changed:**
  - `src-tauri/src/settings/types.rs` — removed `adaptive_parakeet_thresholds` field.
  - `src-tauri/src/settings/defaults.rs` — removed default value.
  - `src-tauri/src/settings/store.rs` — removed NaN sanitization.
  - `src-tauri/src/shortcut/mod.rs` — removed `change_adaptive_parakeet_thresholds_setting` command.
  - `src/components/settings/AdaptiveThresholds.tsx` — deleted.
  - `src/components/settings/advanced/AdvancedSettings.tsx` — removed toggle.
  - `src/components/settings/index.ts` — removed export.

### 2. Hybrid Mode
- **What it did:** Allowed configuring one "short" and one "long" audio model, routing recordings to different models based on duration.
- **Why removed:** Upstream uses a single selected model. The hybrid router was a fork-only feature with no equivalent in the upstream architecture.
- **Files changed:**
  - `src-tauri/src/settings/types.rs` — removed `hybrid_mode_enabled`, `hybrid_threshold_secs`, `hybrid_short_audio_model`, `hybrid_long_audio_model`.
  - `src-tauri/src/settings/defaults.rs` — removed defaults.
  - `src-tauri/src/shortcut/mod.rs` — removed hybrid setting commands.
  - `src-tauri/src/actions/router.rs` — removed hybrid fallback logic.
  - `src-tauri/src/actions/transcribe.rs` — removed hybrid fallback logic.
  - `src/components/settings/HybridMode.tsx` — deleted.
  - `src/components/settings/models/ModelsSettings.tsx` — removed hybrid role UI.
  - `src/components/model-selector/ModelSelector.tsx` — removed hybrid role display.
  - `src/components/model-selector/ModelDropdown.tsx` — removed hybrid badges.
  - `src/components/onboarding/ModelCard.tsx` — removed hybrid role props and buttons.
  - `src/overlay/hooks/useOverlayState.ts` — removed hybrid state.
  - `src/overlay/RecordingOverlay.tsx` — removed hybrid state consumption.

### 3. Verification Mode
- **What it did:** Enabled a secondary verification pass over transcriptions.
- **Why removed:** Upstream does not have a verification pass; the fork implementation was tied to the old pipeline.
- **Files changed:**
  - `src-tauri/src/settings/types.rs` — removed `verification_mode` field.
  - `src-tauri/src/settings/defaults.rs` — removed default value.
  - `src-tauri/src/shortcut/mod.rs` — removed `change_verification_mode_setting` command.
  - `src/components/settings/VerificationMode.tsx` — deleted.
  - `src/components/settings/advanced/AdvancedSettings.tsx` — removed component usage.
  - `src/components/settings/index.ts` — removed export.

### 4. Custom Whisper-Specific Acceleration Settings
- **What it did:** Settings named `whisper_accelerator` and `whisper_gpu_device` controlled only the whisper.cpp backend.
- **Why removed:** Upstream's unified engine uses `transcribe_accelerator` and `transcribe_gpu_device` for all GGUF/whisper-family models.
- **Files changed:**
  - `src-tauri/src/settings/types.rs` — replaced `WhisperAcceleratorSetting` with `TranscribeAcceleratorSetting`, renamed fields.
  - `src-tauri/src/settings/defaults.rs` — updated defaults.
  - `src-tauri/src/shortcut/mod.rs` — renamed commands to `change_transcribe_accelerator_setting` and `change_transcribe_gpu_device`.
  - `src-tauri/src/managers/transcription.rs` — uses `transcribe_accelerator` / `transcribe_gpu_device`.
  - `src/components/settings/AccelerationSelector.tsx` — uses new setting keys and type.
  - `src/stores/settingsStore.ts` — updated command mapping.

### 5. Segment-Based Live Captions
- **What it did:** The live-caption stream emitted `TranscriptionOutput` with per-segment metadata and applied per-segment post-processing.
- **Why removed:** Upstream's simplified `TranscriptionOutput` only contains `text` and `model_id`. The frontend now receives the fully post-processed text directly.
- **Files changed:**
  - `src-tauri/src/managers/audio.rs` — removed segment processing, emits `text` only.
  - `src-tauri/src/managers/transcription.rs` — `TranscriptionOutput` simplified to `{ text, model_id }`.

## Compatibility Notes

- Existing persisted settings from older fork builds will have obsolete keys silently ignored by the new backend.
- Users who previously relied on hybrid mode will need to select a single model in **Settings → Models**.
- Users who previously tuned `whisper_accelerator` will see the same UI under **Settings → Advanced → Acceleration**, now labeled for the unified engine.

## Verification

After the removal:
- `cargo check` and `cargo build` pass in `src-tauri`.
- `tsc --noEmit` passes for the frontend.
- ESLint reports no new errors in the files touched by this port.
