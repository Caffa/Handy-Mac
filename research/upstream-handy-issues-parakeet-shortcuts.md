# Research: Upstream Handy Issues — Parakeet Loading & Cancel Shortcuts

**Date:** 2026-07-25  
**Branch:** `new-models`  
**Upstream:** https://github.com/cjpais/Handy  
**Fork:** stable-handy (local)

---

## Executive Summary

The upstream Handy project (27k+ stars, active development) uses the same Parakeet loading path as our fork via `transcribe-rs` (`ParakeetModel::load()`). The cancel shortcut system in upstream is managed dynamically at runtime (registered when recording starts, unregistered when recording stops) via a `handy-computer/handy-keys` external crate. Both features are working in upstream with known caveats. This report catalogs the issues found and compares our fork's approach.

---

## 1. Parakeet Model Loading — Upstream vs Fork

### Upstream Loading Code (from `transcription.rs`)

```rust
// Upstream: simple synchronous load via transcribe-rs
EngineType::Parakeet => {
    let engine = ParakeetModel::load(&model_path, &Quantization::Int8)
        .map_err(|e| anyhow::anyhow!("Failed to load parakeet model {}: {}", model_id, e))?;
    LoadedEngine::Parakeet(engine)
}
```

### Our Fork Loading Code (from `managers/transcription.rs`)

```rust
// Identical pattern:
EngineType::Parakeet => {
    let engine = ParakeetModel::load(&model_path, &Quantization::Int8)
        .map_err(|e| {
            let error_msg = format!("Failed to load parakeet model {}: {}", model_id, e);
            emit_loading_failed(&error_msg);
            anyhow::anyhow!(error_msg)
        })?;
    LoadedEngine::Parakeet(engine)
}
```

**Key Finding:** Both upstream and fork use identical Parakeet loading logic. The only difference is our fork adds event emission on failure (`emit_loading_failed`).

### Upstream Parakeet Transcription

```rust
LoadedEngine::Parakeet(parakeet_engine) => {
    let params = ParakeetParams {
        timestamp_granularity: Some(TimestampGranularity::Segment),
        ..Default::default()
    };
    parakeet_engine
        .transcribe_with(&audio, &params)
        .map(|r| r.text)
        .map_err(|e| anyhow::anyhow!("Parakeet transcription failed: {}", e))
}
```

**Parakeet is a non-streaming engine.** Both upstream and fork correctly detect this and fall back to batch transcription. The `supports_streaming` flag is always false for Parakeet models.

### Parakeet Models Defined

| Model ID | Size | Languages | Streaming |
|----------|------|-----------|-----------|
| `parakeet-tdt-0.6b-v2` | ~451 MB | English only | No |
| `parakeet-tdt-0.6b-v3` | ~456 MB | 25 EU languages + RU/UK | No |

Both use directory-based downloads from `blob.handy.computer` (tar.gz archives).

---

## 2. Known Parakeet Issues in Upstream

### Issue #342: App Crash When Loading Parakeet V2

- **Symptom:** App crashes on load when selecting Parakeet V2 model (v3 works fine)
- **Root Cause:** Model switching between Parakeet versions is problematic
- **Workaround:** Unknown (issue may have been addressed by maintaining separate model IDs)
- **Relevance to Fork:** Our fork uses the same model ID separation (`parakeet-tdt-0.6b-v2` vs `parakeet-tdt-0.6b-v3`), so this issue should not recur unless switching between V2 and V3 at runtime

### Issue #574: Parakeet Fails to Load on Windows with Non-ASCII Path

- **Symptom:** Model loading fails on Windows when username contains non-ASCII characters
- **Root Cause:** ONNX runtime cannot handle non-ASCII paths in some configurations
- **Workaround:** Directory junction to an ASCII-only path
- **Relevance to Fork:** This is a Windows-specific ONNX runtime bug, not a code issue. Affects both upstream and fork equally. macOS (our target) is unaffected.

### Key Observations

1. **No recent Parakeet-specific commits** — the model loading code has been stable for months
2. **Parakeet is NOT a streaming model** — the `supports_streaming: false` flag is correctly set
3. **The Parakeet model loading is synchronous** — no async loading, no retry logic (unlike transcribe-cpp which has session reuse)

---

## 3. Cancel Shortcut System — Upstream vs Fork

### Upstream Architecture

The upstream uses `handy-computer/handy-keys` (external crate) for keyboard shortcuts:
- Global keyboard shortcuts are registered via `rdev`-based library
- Cancel shortcut is **dynamically registered/unregistered** — only active during recording
- Shortcut handling goes through `shortcut.rs` → `signal_handle.rs` → `TranscriptionCoordinator`

### Fork Architecture

Our fork has a **much more sophisticated shortcut system**:

1. **Dual Implementation:** Tauri global-shortcut plugin OR handy-keys library (configurable)
2. **Dynamic Cancel Registration:** Cancel shortcut is registered when recording starts, unregistered when recording stops (same pattern as upstream)
3. **Settings Persistence:** Cancel binding is stored in `settings.bindings["cancel"]` with `current_binding` field
4. **Implementation Switching:** Runtime switch between Tauri and handy-keys with rollback on failure

### How Cancel Shortcut Works in Our Fork

```rust
// shortcut/mod.rs — dispatch to correct implementation
pub fn register_cancel_shortcut(app: &AppHandle) {
    let settings = get_settings(app);
    match settings.keyboard_implementation {
        KeyboardImplementation::Tauri => tauri_impl::register_cancel_shortcut(app),
        KeyboardImplementation::HandyKeys => handy_keys::register_cancel_shortcut(app),
    }
}
```

The cancel shortcut is:
- **Registered** in `actions/transcribe.rs` when recording starts: `shortcut::register_cancel_shortcut(app)`
- **Unregistered** in `actions/transcribe.rs` when recording stops: `shortcut::unregister_cancel_shortcut(app)`
- **Handled** in `shortcut/handler.rs`: Only fires when `audio_manager.is_recording() && is_pressed`

### Cancel Binding Persistence (Fork)

```rust
// shortcut/mod.rs — change_binding for cancel
if id == "cancel" {
    // Update settings and return — cancel is managed dynamically
    let b = apply_cancel_binding_update(&mut settings, &id, binding.clone());
    settings::write_settings_safe(&app, settings);
    return Ok(BindingResponse { success: true, binding: Some(b), ... });
}
```

**Important:** Cancel binding is saved to settings but **not registered at app startup** — it's only active during recording sessions. This means:
- Changing the cancel shortcut in settings saves correctly
- The shortcut is re-registered with the new binding on the next recording session
- There's no "cancel shortcut not saving" bug in our code — the binding is persisted properly

---

## 4. Known Shortcut Issues in Upstream

### Issue #63: Command/Windows Key in Hotkey Causes Bad State

- **Symptom:** Including Command (macOS) or Windows key in a shortcut causes the app to enter a bad state
- **Status:** Open issue
- **Relevance:** Our fork has conflict detection (`conflicts.rs`) that warns about problematic shortcuts

### Issue #96: Changing Shortcut from Ctrl+Space Doesn't Work (14 comments)

- **Symptom:** Users cannot change the default Ctrl+Space shortcut to a custom one
- **Workaround:** Quit app → edit `settings.json` directly → restart
- **Root Cause (upstream):** The old shortcut doesn't un-register before the new one is registered, causing conflicts
- **PR #856:** Merged fix for this issue
- **Relevance to Fork:** Our fork avoids this by:
  1. Using `suspend_binding` → `change_binding` → `resume_binding` flow
  2. Properly unregistering old bindings before registering new ones
  3. Serializing binding updates with a lock to prevent TOCTOU races

### Issue #569: Settings Reset After Upgrade

- **Symptom:** User settings are lost after updating Handy
- **Relevance:** This is a data migration issue, not a shortcut/Parakeet issue

---

## 5. Comparison: Fork vs Upstream

### Parakeet Loading

| Aspect | Upstream | Fork |
|--------|----------|------|
| Loading mechanism | `ParakeetModel::load()` | `ParakeetModel::load()` (identical) |
| Quantization | `Int8` | `Int8` (identical) |
| Error handling | Basic `map_err` | Event emission + error propagation |
| Streaming support | Not supported (correct) | Not supported (correct) |
| Model paths | Directory-based | Directory-based (identical URLs) |

**Verdict:** Parakeet loading is identical between upstream and fork. No fork-specific bugs introduced.

### Cancel Shortcut

| Aspect | Upstream | Fork |
|--------|----------|------|
| Registration | Dynamic (during recording) | Dynamic (during recording) |
| Backend | `handy-keys` only | `handy-keys` OR Tauri (configurable) |
| Settings persistence | Via `settings.json` | Via `settings.json` with TOCTOU protection |
| Conflict detection | Basic | `conflicts.rs` with platform-specific checks |
| Implementation switching | Not available | Runtime switch with rollback |
| Race condition prevention | None | Mutex-serialized binding updates |

**Verdict:** Fork's cancel shortcut system is strictly more robust than upstream's. The fork adds implementation switching, conflict detection, and race condition prevention.

---

## 6. Potential Issues to Watch

### Parakeet-Specific

1. **Memory usage:** Parakeet models are loaded as ONNX sessions — each load allocates full model memory. No session reuse between transcriptions (unlike transcribe-cpp which holds sessions).
2. **No retry logic:** If Parakeet fails to load, there's no automatic retry. The user must retry manually.
3. **Path issues on Windows:** ONNX runtime may fail with non-ASCII paths (upstream issue #574) — not relevant on macOS.

### Shortcut-Specific

1. **Cancel binding not registered at startup:** This is by design — cancel only fires during recording. If the user expects it to be "always on," this is a misunderstanding.
2. **Implementation fallback:** If handy-keys fails, we silently fall back to Tauri implementation and persist this choice. The user may not notice the switch.
3. **Push-to-talk vs toggle:** Cancel shortcut only works in toggle mode (not push-to-talk). In PTT mode, releasing the main shortcut stops recording, making cancel redundant.

---

## 7. Recommendations

1. **No code changes needed** for Parakeet loading — it's working identically to upstream
2. **Cancel shortcut is correctly implemented** — the "not saving" perception likely comes from:
   - The shortcut not being registered when not recording (by design)
   - Users confusing the cancel shortcut with the main transcription shortcut
3. **Consider adding a diagnostic log** when cancel binding is loaded from settings, to help debug user reports
4. **Monitor upstream Issue #342** — if Parakeet V2 crashes on load, check if our model ID separation prevents it

---

## Files Referenced

### Fork (our code)
- `src-tauri/src/managers/model.rs` — Model definitions, Parakeet entries (lines 698-773)
- `src-tauri/src/managers/transcription.rs` — Parakeet loading (lines 716-724), transcription (lines 1447-1455)
- `src-tauri/src/shortcut/mod.rs` — Cancel shortcut registration/unregistration (lines 66-81), binding change (lines 194-207)
- `src-tauri/src/shortcut/handler.rs` — Cancel shortcut handling (lines 56-65)
- `src-tauri/src/actions/transcribe.rs` — Cancel shortcut lifecycle (lines 173-174, 221-222)

### Upstream (cjpais/Handy)
- `src-tauri/src/transcription.rs` — Model loading with Parakeet branch
- `src-tauri/src/signal_handle.rs` — Signal handling (delegates to TranscriptionCoordinator)
- `handy-computer/handy-keys/src/lib.rs` — External keyboard shortcut crate

### GitHub Issues
- #342 — Parakeet V2 crash on load
- #574 — Non-ASCII path issue (Windows)
- #63 — Command/Windows key in hotkey
- #96 — Shortcut changing doesn't work (14 comments, PR #856 merged)
- #569 — Settings reset on upgrade
