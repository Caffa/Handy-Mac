# Handy-Mac Wiring Audit Report

**Date:** 2026-07-06  
**Scope:** Audio recorder, transcription manager, and overlay event wiring  
**Pattern Searched:** Settings read at creation time that can be changed later without recreating/reconfiguring the object

---

## 1. Confirmed Wiring Issues

### Issue 1: `noise_suppression_enabled` Toggle Doesn't Recreate Recorder

| Field | Details |
|-------|---------|
| **Setting** | `noise_suppression_enabled` |
| **Read at** | `src-tauri/src/managers/audio.rs:173` in `create_audio_recorder()` |
| **Setter at** | `src-tauri/src/shortcut/mod.rs:1481-1489` in `change_noise_suppression_enabled_setting()` |
| **What's Missing** | No `recreate_recorder()` call after toggling |
| **Impact** | Toggling noise suppression has no effect until the app restarts or the recorder is recreated for another reason |
| **Suggested Fix** | Add the same pattern used for `change_pre_recording_buffer_setting()` (lines 791-838) - call `recreate_recorder()` if stream is open |

### Issue 2: `noise_suppression_level` Change Doesn't Recreate Recorder

| Field | Details |
|-------|---------|
| **Setting** | `noise_suppression_level` (Low/Medium/High) |
| **Read at** | `src-tauri/src/managers/audio.rs:174` in `create_audio_recorder()` |
| **Setter at** | `src-tauri/src/shortcut/mod.rs:1493-1507` in `change_noise_suppression_level_setting()` |
| **What's Missing** | No `recreate_recorder()` call after changing level |
| **Impact** | Changing noise suppression intensity has no effect until app restart |
| **Suggested Fix** | Add `recreate_recorder()` call if stream is open (same pattern as pre_recording_buffer) |

### Issue 3: `vad_sensitivity` Change Doesn't Recreate Recorder

| Field | Details |
|-------|---------|
| **Setting** | `vad_sensitivity` (VeryQuick/Quick/Balanced/Relaxed/VeryRelaxed) |
| **Read at** | `src-tauri/src/managers/audio.rs:162-163` via `threshold()` and `hangover_frames()` methods |
| **Setter at** | `src-tauri/src/shortcut/mod.rs:1399-1415` in `change_vad_sensitivity_setting()` |
| **What's Missing** | No `recreate_recorder()` call after changing sensitivity |
| **Impact** | Changing VAD sensitivity has no effect on the running recorder; new threshold only applies after recorder recreation |
| **Suggested Fix** | Add `recreate_recorder()` call if stream is open |

---

## 2. Potential Issues (Require Further Investigation)

### Potential Issue 1: Microphone Device Selection Path Complexity

| Field | Details |
|-------|---------|
| **Setting** | `selected_microphone`, `clamshell_microphone` |
| **Setter** | `src-tauri/src/commands/audio.rs:240-257` in `set_selected_microphone()` |
| **Observation** | Calls `rm.update_selected_device()` which DOES stop/restart stream and recreates recorder (audio.rs:1312-1326) |
| **Status** | ✅ **Actually wired correctly** - The `update_selected_device()` method properly recreates the recorder |

### Potential Issue 2: Transcription Manager Accelerator Settings

| Field | Details |
|-------|---------|
| **Settings** | `whisper_accelerator`, `ort_accelerator`, `whisper_gpu_device` |
| **Setter** | `src-tauri/src/shortcut/mod.rs:1283-1314` |
| **Observation** | Uses `apply_and_reload_accelerator()` which calls `unload_model()` - this is correct behavior |
| **Status** | ✅ **Clean** - Model is unloaded and will reload with new settings on next transcription |

### Potential Issue 3: Transcription Settings That Might Need Model Reload

The following transcription settings are read fresh on each `transcribe()` call, so they don't need model reload:
- `translate_to_english` ✅ Read fresh each transcription
- `selected_language` ✅ Read fresh each transcription
- `word_correction_mode`, `custom_words`, etc. ✅ Read fresh each transcription
- `hybrid_mode_enabled`, `hybrid_threshold_secs` ✅ Used to select model per-transcription

However, these settings affect the Whisper `initial_prompt` which is passed at transcription time, not model load time:
- `word_correction_mode` + `custom_words`/`advanced_custom_words`/`word_replacements` ✅ **Clean** - passed as `initial_prompt` in `WhisperInferenceParams` (transcription.rs:900-932)

**Status**: ✅ All transcription settings are correctly read fresh on each transcription

---

## 3. Settings That Are Correctly Wired

### Audio Recorder Settings (Correct)

| Setting | Read At | Setter Pattern | Status |
|---------|---------|----------------|--------|
| `live_captions_enabled` | audio.rs:171 | shortcut/mod.rs:1419-1461 - **Calls `recreate_recorder()`** | ✅ Fixed |
| `pre_recording_buffer_ms` | audio.rs:172 | shortcut/mod.rs:791-838 - **Calls `recreate_recorder()`** | ✅ Correct |
| `selected_microphone` | audio.rs:763-764 via `get_effective_microphone_device()` | commands/audio.rs:240-257 → `update_selected_device()` | ✅ Correct |
| `clamshell_microphone` | audio.rs:758-761 via `get_effective_microphone_device()` | commands/audio.rs:327-336 | ✅ Correct |

### Transcription Settings (Correct - Read Fresh Each Time)

| Setting | Where Read | Pattern | Status |
|---------|------------|---------|--------|
| `translate_to_english` | transcription.rs:899 | Read in `transcribe()` | ✅ Clean |
| `selected_language` | transcription.rs:755-778 | Read in `transcribe()` | ✅ Clean |
| `word_correction_mode` + custom words | transcription.rs:900-932 | Passed as `initial_prompt` | ✅ Clean |
| `hybrid_mode_enabled` + models | transcription.rs:658-710 | Model selection per-transcription | ✅ Clean |
| `vad_sensitivity` (for trimming) | transcription.rs:722 | Read fresh in `transcribe()` | ✅ Clean |

### Accelerator Settings (Correct - Model Unload Pattern)

| Setting | Setter | Pattern | Status |
|---------|--------|---------|--------|
| `whisper_accelerator` | shortcut/mod.rs:1285-1293 | Calls `apply_and_reload_accelerator()` → `unload_model()` | ✅ Correct |
| `ort_accelerator` | shortcut/mod.rs:1297-1305 | Calls `apply_and_reload_accelerator()` → `unload_model()` | ✅ Correct |
| `whisper_gpu_device` | shortcut/mod.rs:1309-1314 | Calls `apply_and_reload_accelerator()` → `unload_model()` | ✅ Correct |

### Other Settings (Not Affecting Recorder/Transcription)

These settings don't require object recreation:
- `push_to_talk` ✅ Only affects shortcut behavior
- `audio_feedback` ✅ Read fresh each time feedback is played
- `audio_feedback_volume` ✅ Read fresh each time
- `sound_theme` ✅ Read fresh each time
- `paste_method`, `paste_delay_ms` ✅ Read fresh at paste time
- `auto_submit`, `auto_submit_key` ✅ Read fresh at submit time
- `mute_while_recording` ✅ Read fresh in `apply_mute()` (audio.rs:828)
- `lazy_stream_close` ✅ Read fresh in `stop_recording()` (audio.rs:1469)
- `overlay_position`, `overlay_scale` ✅ UI-only settings
- `debug_mode`, `log_level` ✅ Logging configuration
- `post_process_enabled`, related settings ✅ Post-processing pipeline

---

## 4. Frontend Event Wiring Analysis

### Overlay Event Listeners (Clean)

| File | Event | Listener | Cleanup | Status |
|------|-------|----------|---------|--------|
| `useLiveCaptions.ts` | `partial-transcription` | Lines 183-259 | Lines 264-266 | ✅ Clean |
| `useOverlayState.ts` | `show-overlay` | Lines 249-277 | Lines 322-325 | ✅ Clean |
| `useOverlayState.ts` | `hide-overlay` | Lines 279-319 | Lines 322-325 | ✅ Clean |
| `useAppState.ts` | `app-state` | Lines 151-154 | Lines 160-162 | ✅ Clean |

### Event Listener Pattern Check

All event listeners in the overlay hooks follow the correct pattern:
1. Register listener in `useEffect`
2. Store unlisten function
3. Return cleanup function that calls unlisten

**Status**: ✅ All event listeners properly cleaned up

### Frontend State Sync

| Setting | Backend→Frontend Sync | Status |
|---------|----------------------|--------|
| `liveCaptionsEnabled` | Fetched via `getAppSettings()` in `useOverlayState.ts:185-205` | ✅ Correct |
| `overlayScale` | Fetched via `getAppSettings()` in `useOverlayState.ts:208-220` | ✅ Correct |
| `hybrid_mode_enabled`, `hybrid_threshold_secs` | Fetched via `getAppSettings()` in `useOverlayState.ts:185-205` | ✅ Correct |

---

## 5. Settings Persistence Analysis

### Settings.rs Analysis

The `settings.rs` file implements robust persistence:

1. **Debounced Writes**: Uses `SettingsWriter` with 500ms debounce (line 1532)
2. **Safe Wrappers**: `get_settings_safe()`, `write_settings_safe()`, etc. with panic catching
3. **Migration Logic**: Handles field additions and migrations (lines 1265-1311)
4. **NaN Sanitization**: `sanitize_floats()` prevents serialization failures (lines 1131-1148)

### Default Values Consistency

All settings have consistent defaults between:
- `get_default_settings()` (line 933)
- `#[serde(default = "...")]` attributes
- `#[serde(default)]` for Option fields

**Status**: ✅ Settings persistence is robust and consistent

---

## Summary

### Critical Issues to Fix (3 items)

1. **`change_noise_suppression_enabled_setting`** - Missing `recreate_recorder()` call
2. **`change_noise_suppression_level_setting`** - Missing `recreate_recorder()` call
3. **`change_vad_sensitivity_setting`** - Missing `recreate_recorder()` call

### Pattern for Fix

Copy the implementation from `change_pre_recording_buffer_setting()` (lines 791-838):

```rust
// After write_settings_safe(), add:
if let Some(rm) = app.try_state::<Arc<AudioRecordingManager>>() {
    if rm.is_stream_open() {
        info!("Applying setting change: stopping stream, recreating recorder");
        rm.stop_microphone_stream();
        if let Err(e) = rm.recreate_recorder() {
            error!("Failed to recreate recorder: {}", e);
            // Handle error, attempt restart if needed
        }
        if rm.is_always_on() || rm.is_bt_keep_alive() {
            if let Err(e) = rm.start_microphone_stream() {
                error!("Failed to restart stream: {}", e);
            }
        }
    }
}
```

### Frontend/Backend Wiring

✅ **All clean** - Event listeners properly managed, state sync working correctly

### Settings Persistence

✅ **All clean** - Robust debounced writes with proper error handling

---

## Files Requiring Changes

1. `src-tauri/src/shortcut/mod.rs` - Add `recreate_recorder()` calls to:
   - `change_noise_suppression_enabled_setting()` (after line 1487)
   - `change_noise_suppression_level_setting()` (after line 1505)
   - `change_vad_sensitivity_setting()` (after line 1413)
