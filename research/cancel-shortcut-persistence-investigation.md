# Research Report: Cancel Shortcut Persistence Investigation

## TL;DR

The cancel shortcut SHOULD persist correctly in normal operation — the save/load machinery is the same for all shortcuts. However, **several design-level issues** make it more fragile than other shortcuts:

1. **Hidden behind push-to-talk** — UI never renders the cancel input when PTT is on, so users can't even see what's set
2. **Race condition (no atomic read-modify-write)** — reads then writes settings without locking, making it vulnerable to being overwritten by concurrent settings changes
3. **No validation before save** — invalid shortcuts are silently saved, fail silently at registration time
4. **`if let` without `else` fallthrough** — if the cancel binding is missing from the HashMap (edge case), code falls through to the non-cancel path which tries to register it as a normal shortcut
5. **Debounce + exit timing** — 500ms debounce on disk writes; if exit happens before flush completes, the change is lost

---

## 1. Settings Storage Architecture

### 1.1 Struct Definitions

**File:** `src-tauri/src/settings/types.rs`

The core struct is `AppSettings` (lines 486-621), which stores all app settings. The `bindings` field is a `HashMap<String, ShortcutBinding>`:

```rust
// Line 487
pub struct AppSettings {
    pub bindings: HashMap<String, ShortcutBinding>,
    // ... many other fields
}
```

`ShortcutBinding` (lines 88-95) has 5 string fields:

```rust
pub struct ShortcutBinding {
    pub id: String,
    pub name: String,
    pub description: String,
    pub default_binding: String,
    pub current_binding: String,
}
```

All settings (including bindings) are serialized/deserialized via serde JSON using `#[serde(default)]` on the struct.

### 1.2 Default Value Construction

**File:** `src-tauri/src/settings/defaults.rs`

The cancel shortcut is defined at lines 90-99:

```rust
bindings.insert(
    "cancel".to_string(),
    ShortcutBinding {
        id: "cancel".to_string(),
        name: "Cancel".to_string(),
        description: "Cancels the current recording.".to_string(),
        default_binding: "escape".to_string(),
        current_binding: "escape".to_string(),
    },
);
```

Other shortcuts defined:
- `"transcribe"` (lines 19-28) — platform-specific: `option+space` (macOS), `ctrl+space` (Windows/Linux)
- `"transcribe_with_post_process"` (lines 38-48) — `option+shift+space` (macOS)
- `"transcribe_with_router"` (lines 58-67) — `option+ctrl+space` (macOS)
- `"transcribe_with_router"` DUPLICATE (lines 79-89) — **Same key inserted twice!** The second insert overwrites the first (different description text).

**Observation:** The duplicate `transcribe_with_router` insert is a bug. If you wanted different descriptions, you'd need different IDs.

### 1.3 Settings Cache and Write Path

**File:** `src-tauri/src/settings/store.rs`

The settings system uses a three-layer architecture:

```
In-Memory Cache (RwLock<AppSettings>)
       ↕
Debounced Writer (500ms timer)
       ↕
File-based Store (tauri-plugin-store → settings_store.json)
```

**`SettingsCache`** (lines 23-42):
- Single source of truth for reads
- Updated immediately on writes
- Protected by `RwLock`

**`SettingsWriter`** (lines 599-687):
- Debounces writes by 500ms
- Replaces pending writes on new changes (restarts timer)
- Flushes to disk on timer expiry

**Write path** (`write_settings`, lines 430-456):
1. Updates cache immediately
2. Replaces pending write in debounced writer
3. If no writer available, writes synchronously

### 1.4 Settings Load on Startup

**File:** `src-tauri/src/settings/store.rs`, `load_or_create_app_settings()` (lines 247-340)

On startup:
1. Opens the settings store (JSON file)
2. Tries to deserialize full `AppSettings` from the `"settings"` key
3. On success:
   - **Merges missing bindings from defaults** (lines 262-268):
     ```rust
     for (key, value) in default_settings.bindings {
         if !settings.bindings.contains_key(&key) {
             settings.bindings.insert(key, value);
             updated = true;
         }
     }
     ```
   - Runs migrations for new fields
   - Saves if anything was updated
4. On deserialization failure: **`salvage_settings()`** (lines 214-245)
   - Starts from defaults
   - Tries to insert each stored field individually
   - Drops fields that cause deserialization errors

**Key point:** The merge logic only adds **missing** bindings — it does NOT overwrite existing ones.

---

## 2. Cancel Shortcut — Special Handling

### 2.1 `change_binding` Command

**File:** `src-tauri/src/shortcut/mod.rs`, lines 122-241

The `change_binding` Tauri command has a **completely separate code path** for the cancel shortcut:

```rust
// Lines 165-176 — CANCEL PATH
if id == "cancel" {
    if let Some(mut b) = settings.bindings.get(&id).cloned() {
        b.current_binding = binding;
        settings.bindings.insert(id.clone(), b.clone());
        settings::write_settings_safe(&app, settings);
        return Ok(BindingResponse {
            success: true,
            binding: Some(b.clone()),
            error: None,
        });
    }
    // NOTE: No else clause! Falls through to non-cancel path below.
}

// Lines 178-240 — NON-CANCEL PATH
// 1. Unregister old binding
// 2. Validate new shortcut
// 3. Check for conflicts
// 4. Register new binding
// 5. Update settings
// 6. Save settings
```

**Critical differences:**

| Aspect | Cancel Shortcut | Other Shortcuts |
|--------|----------------|-----------------|
| Validation | **None** — invalid values saved silently | Validated before save |
| OS registration | **Skipped** — dynamically managed | Registered immediately |
| Unregister old | **Skipped** | Unregistered before new registration |
| Conflicting check | **Skipped** | Checked, warning emitted |
| `if let` fallthrough | **`if let` without `else`** — falls through to the non-cancel path if the binding is missing from the HashMap | Uses `binding_to_modify` which also falls back to defaults |

### 2.2 Dynamic Registration During Recording

The cancel shortcut is NOT registered during app startup. Instead, it's dynamically registered/unregistered when recording starts/stops:

**File:** `src-tauri/src/actions/transcribe.rs`

- **Line 174**: `shortcut::register_cancel_shortcut(app);` — when recording starts
- **Line 222**: `shortcut::unregister_cancel_shortcut(app);` — when recording stops

The registration reads the current value from settings at call time:

**File:** `src-tauri/src/shortcut/tauri_impl.rs`, lines 158-181 (Tauri impl):
```rust
pub fn register_cancel_shortcut(app: &AppHandle) {
    tauri::async_runtime::spawn(async move {
        if let Some(cancel_binding) = get_settings_safe(&app_clone)
            .bindings
            .get("cancel")
            .cloned()
        {
            if let Err(e) = register_shortcut(&app_clone, cancel_binding) {
                error!("Failed to register cancel shortcut: {}", e);
            }
        }
    });
}
```

### 2.3 Skipped During Init

Both shortcut implementations skip the cancel shortcut during initialization:

**File:** `src-tauri/src/shortcut/tauri_impl.rs`, line 23:
```rust
if id == "cancel" {
    continue; // Skip cancel shortcut, it will be registered dynamically
}
```

**File:** `src-tauri/src/shortcut/handy_keys.rs`, lines 422-424:
```rust
if id == "cancel" {
    continue;
}
```

This is deliberate — the cancel shortcut is only active during recording, not at all times.

---

## 3. Identified Root Causes for Non-Persistence

### 3.1 🔴 Race Condition: Non-Atomic Read-Modify-Write

**Severity: HIGH**

**File:** `src-tauri/src/shortcut/mod.rs`, lines 134-169

The `change_binding` function for the cancel shortcut follows this pattern:

```rust
let mut settings = settings::get_settings_safe(&app);   // READ
// ... modify settings.bindings["cancel"] ...
settings::write_settings_safe(&app, settings);           // WRITE
```

This is a classic TOCTOU (Time-of-Check Time-of-Use) race. If TWO settings commands execute concurrently (e.g., user changes cancel shortcut AND a background timer changes overlay position):

1. Thread A reads settings (cancel="escape", overlay="bottom")
2. Thread B reads settings (cancel="escape", overlay="bottom")
3. Thread A saves (cancel="ctrl+shift+x", overlay="bottom")
4. Thread B saves (cancel="escape", overlay="top") ← **Thread A's change is OVERWRITTEN!**

The commit `ea9f7fd5` introduced `modify_settings()` with a `tokio::sync::Mutex` to fix this, but that commit was never merged into the `new-models` branch (it exists only as a detached commit or on a different branch).

**Files that don't have modify_settings and are vulnerable:**
- `src-tauri/src/shortcut/mod.rs` — `change_binding()`, all individual `change_*_setting()` commands
- `src-tauri/src/commands/audio.rs`
- `src-tauri/src/commands/history.rs`
- `src-tauri/src/commands/models.rs`

### 3.2 🟡 Cancel-Specific Code Path: Missing `else` Clause

**Severity: MEDIUM**

**File:** `src-tauri/src/shortcut/mod.rs`, lines 165-176

```rust
if id == "cancel" {
    if let Some(mut b) = settings.bindings.get(&id).cloned() {
        // ... update and save ...
        return Ok(...);
    }
    // Falls through to non-cancel path if cancel binding is missing!
}
```

If the cancel binding is somehow missing from the settings HashMap (edge case: corrupted settings file, migration issue, race condition), the code falls through to the non-cancel path which tries to:
1. Unregister whatever `binding_to_modify` was computed (line 179)
2. Validate the new shortcut (line 185)
3. **Register the cancel shortcut as a normal shortcut** (line 219) — this would register "escape" as a global shortcut, which may or may not work
4. Save to settings (line 233)

Even if registration fails, the settings ARE saved (line 233 is AFTER the registration), so in theory this path would still persist the value. But the registration failure would show an error to the user.

### 3.3 🟡 No Validation Before Save (Cancel Only)

**Severity: MEDIUM**

**File:** `src-tauri/src/shortcut/mod.rs`, lines 165-176

The cancel path saves the new binding WITHOUT validating it against the current keyboard implementation. The non-cancel path validates first:

```rust
// Non-cancel path (line 185):
if let Err(e) = validate_shortcut_for_implementation(&binding, settings.keyboard_implementation) {
    return Err(e);  // Rejects invalid shortcuts before saving
}

// Cancel path (line 165-176):
// No validation at all — saves anything
```

If the user sets an invalid cancel shortcut (e.g., a string that doesn't parse as any shortcut), it will be saved to settings. On next app start + recording attempt, `register_cancel_shortcut` will fail silently (error logged), but the saved value remains in settings.

This doesn't directly cause non-persistence, but it means users get NO FEEDBACK about invalid cancel shortcuts.

### 3.4 🟡 UI Hidden When Push-to-Talk Is Enabled

**Severity: MEDIUM (UX)**

**File:** `src/components/settings/general/GeneralSettings.tsx`, lines 26-29

```tsx
{/* Cancel shortcut is hidden with push-to-talk (release key cancels) and on Linux */}
{!isLinux && !pushToTalk && (
    <ShortcutInput shortcutId="cancel" grouped={true} />
)}
```

When push-to-talk is enabled (which is the default on macOS: `push_to_talk: true`), the cancel shortcut input is **completely hidden from the UI**. Users can't see what it's set to, can't change it, and might not even know it exists.

**Why this matters for persistence:** If the cancel shortcut IS visible when push-to-talk is disabled, the user edits it, then re-enables push-to-talk — the cancel input is hidden again. The user might later think "it didn't persist" because they can't see or verify the value.

Additionally, this means the cancel shortcut can only be changed when push-to-talk is OFF, which is a confusing UX constraint.

### 3.5 🟢 Debounced Write Timing

**Severity: LOW (mitigated by flush on exit)**

**File:** `src-tauri/src/settings/store.rs`

The debounce interval is 500ms (`SETTINGS_DEBOUNCE_MS`). The potential race:

1. User changes cancel shortcut
2. Cache updated, debounced write scheduled
3. App exits before 500ms elapses
4. **Without `flush_settings`:** change is lost

The current code DOES call `flush_settings` on `RunEvent::ExitRequested`:

**File:** `src-tauri/src/lib.rs`, lines 1042-1045:
```rust
if let tauri::RunEvent::ExitRequested { .. } = &event {
    crate::settings::flush_settings(app);
}
```

The `flush_settings` function (store.rs lines 501-546) has a 2-second timeout and a fallback path that reads from the cache and writes synchronously. This should be robust.

**But:** There's no handler for `RunEvent::Exit`. In Tauri 2.x, `ExitRequested` should always fire before `Exit`, but if there's a code path that bypasses `ExitRequested` (e.g., the app is killed externally), the flush wouldn't happen.

### 3.6 🟢 Default Merge Logic Is Correct

**Severity: NOT A PROBLEM**

The merge logic in `load_or_create_app_settings` (store.rs lines 262-268) only adds bindings that are MISSING from the loaded settings:

```rust
for (key, value) in default_settings.bindings {
    if !settings.bindings.contains_key(&key) {
        settings.bindings.insert(key, value);
        updated = true;
    }
}
```

Since the cancel binding should already be in the saved settings (it was saved when the user changed it), it is NOT overwritten by the default. This is correct.

---

## 4. Reproduction Steps for the Most Likely Scenario

### Scenario 1: Race Condition (Concurrent Settings Change)

1. Open the Handy settings window
2. Change the cancel shortcut to something custom (e.g., `ctrl+shift+x`)
3. While that's saving, change another setting (e.g., overlay position)
4. The second save overwrites the first because both read the full `AppSettings` struct before writing

**Impact:** The cancel shortcut reverts to its previous value (or the value from the concurrent write).

### Scenario 2: Hidden UI + Debounce Timing

1. Push-to-talk is enabled (default)
2. Disable push-to-talk temporarily
3. Change the cancel shortcut
4. Re-enable push-to-talk
5. Close settings / quit app before 500ms debounce fires
6. `flush_settings` might fire or might not depending on timing

**Impact:** Change might be lost if `flush_settings` fails to persist the pending write.

### Scenario 3: First-Run with Corrupted Settings

1. App starts for the first time with a partly corrupted settings file
2. `salvage_settings` recovers most fields but drops the "bindings" hashmap
3. Merge logic adds cancel from defaults with "escape"
4. User's custom cancel shortcut is gone

---

## 5. Recommendations

### High Priority

1. **Add `modify_settings` with a Mutex** — Port the atomic read-modify-write pattern from the detached commit `ea9f7fd5` to prevent concurrent settings from clobbering each other. This affects all ~50 `change_*` commands, not just the cancel shortcut.

2. **Add an `else` clause to the cancel `if let`** — If the cancel binding is missing from the HashMap, handle it explicitly (e.g., create it from defaults) rather than falling through to the non-cancel code path.

### Medium Priority

3. **Show cancel shortcut input even with push-to-talk** — The "release key to cancel" behavior works without a cancel shortcut, but users should still be able to configure an explicit cancel shortcut if they want one. Consider showing the cancel input without the PTT constraint, or with a tooltip explaining when it applies.

4. **Add validation to the cancel shortcut path** — Use the same `validate_shortcut_for_implementation` check before saving, so users get immediate feedback on invalid shortcuts.

### Low Priority

5. **Add `RunEvent::Exit` handler** — In addition to `ExitRequested`, also flush settings on `RunEvent::Exit` as a safety net.

6. **Remove duplicate `transcribe_with_router` binding** — In `defaults.rs`, the router binding is inserted twice (lines 58-89). Fix by removing the duplicate.

---

## Files Examined

| File | Lines | Key Content |
|------|-------|-------------|
| `src-tauri/src/settings/types.rs` | 1-878 | `AppSettings` struct, `ShortcutBinding`, serde setup |
| `src-tauri/src/settings/defaults.rs` | 1-175 | Default bindings including cancel with "escape" |
| `src-tauri/src/settings/store.rs` | 1-731 | Cache, debounced writer, load/save/migration, flush |
| `src-tauri/src/settings/mod.rs` | 1-16 | Module re-exports |
| `src-tauri/src/shortcut/mod.rs` | 1-1527 | `change_binding` command with cancel-specific path |
| `src-tauri/src/shortcut/handler.rs` | 1-73 | Cancel shortcut event handler (only fires when recording) |
| `src-tauri/src/shortcut/handy_keys.rs` | 1-608 | HandyKeys impl, skip cancel on init |
| `src-tauri/src/shortcut/tauri_impl.rs` | 1-206 | Tauri impl, skip cancel on init, dynamic registration |
| `src-tauri/src/shortcut/conflicts.rs` | 1-532 | Shortcut conflict detection |
| `src-tauri/src/lib.rs` | 1-1049 | App setup, `flush_settings` on `ExitRequested` |
| `src-tauri/src/actions/transcribe.rs` | 150-249 | Cancel registration during recording start/stop |
| `src-tauri/src/utils.rs` | 40-139 | Cancel flow with timing diagnostics |
| `src/components/settings/general/GeneralSettings.tsx` | 1-45 | Cancel input hidden when PTT enabled |
| `src/components/settings/ShortcutInput.tsx` | 1-30 | Wrapper that selects Tauri vs HandyKeys input |
| `src/components/settings/GlobalShortcutInput.tsx` | 1-297 | Tauri shortcut input (calls `changeBinding`) |
| `src/components/settings/HandyKeysShortcutInput.tsx` | 1-289 | HandyKeys shortcut input (calls `changeBinding`) |
| `src/hooks/useSettings.ts` | 1-84 | Hook that delegates to `settingsStore` |
| `src/stores/settingsStore.ts` | 1-770 | Zustand store, `updateBinding` calls `commands.changeBinding` |
