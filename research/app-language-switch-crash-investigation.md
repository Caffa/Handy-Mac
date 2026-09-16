# Investigation: Interface language switch causes crash / infinite spinning wheel

**Date:** 2026-08-26
**Branch:** `feature/streaming-live-captions`
**App:** Handy (Tauri 2.10.2, `react-i18next`)

---

## TL;DR — Most likely root cause

The **backend command `change_app_language_setting` runs synchronously on Tauri's main/IPC thread** and, while holding the global `TRAY_LOCK`, **acquires two other locks that are independently held by background threads** (`TranscriptionManager` engine mutex and `ModelManager` `available_models` mutex). Any background thread that happens to hold one of those locks at the moment the user picks a language **blocks the main thread indefinitely** — the window stops painting, the spinner spins forever, and the app appears hung/crashed.

This is a **lock-contention / lock-ordering hazard on the main thread**, not an infinite React render loop and not a JS crash. The front-end wiring is mostly fine; the danger is concentrated in the synchronous backend tray refresh.

---

## The failure path (step by step)

### Frontend — two actions fired, order matters

`src/components/settings/AppLanguageSelector.tsx:26-29`:

```ts
const handleLanguageChange = (langCode: string) => {
  i18n.changeLanguage(langCode); // 1. fire-and-forget, not awaited
  updateSetting("app_language", langCode); // 2. calls backend synchronously
};
```

`updateSetting("app_language", ...)` → `src/stores/settingsStore.ts:176`:

```ts
app_language: (value) => commands.changeAppLanguageSetting(value as string),
```

This is the **synchronous (non-`async`) Tauri invoke** to the Rust command. The optimistic Zustand `set()` at `settingsStore.ts:401-403` is fine.

### Backend — the synchronous main-thread work

`src-tauri/src/shortcut/mod.rs:1340-1351`:

```rust
#[tauri::command]
pub fn change_app_language_setting(app: AppHandle, language: String) -> Result<(), String> {
    let mut settings = settings::get_settings_safe(&app);   // RwLock read (safe)
    settings.app_language = language.clone();
    settings::write_settings_safe(&app, settings);          // cache update + debounced write (non-blocking)
    tray::update_tray_menu(&app, &tray::TrayIconState::Idle, Some(&language));  // <-- THE PROBLEM
    Ok(())
}
```

This command is **synchronous** (no `async fn`), so Tauri executes the whole body — including `update_tray_menu` — on the **main thread**, blocking the event loop and the webview render.

### `update_tray_menu` acquires three locks

`src-tauri/src/tray.rs:323-331`:

```rust
pub fn update_tray_menu(app: &AppHandle, state: &TrayIconState, locale: Option<&str>) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = TRAY_LOCK.lock();                 // LOCK 1
        update_tray_menu_internal(app, state, locale);
    }));
}
```

Inside `update_tray_menu_internal` (`tray.rs:120-319`) it additionally grabs, still on the main thread:

- **LOCK 2 — `TranscriptionManager` mutex** at `tray.rs:180-183`:

  ```rust
  let model_loaded = app
      .try_state::<Arc<Mutex<TranscriptionManager>>>()
      .map(|tm| tm.lock().is_model_loaded())   // engine mutex
      .unwrap_or(false);
  ```

  `is_model_loaded()` at `src-tauri/src/managers/transcription.rs:477-481` calls `self.lock_engine()` — the same `std::sync::Mutex<Option<LoadedEngine>>` (`transcription.rs:317`) that the **streaming worker holds for the entire duration of a live/streaming transcription** (leased out at `transcription.rs:1164`, returned at `:1373`).

- **LOCK 3 — `ModelManager.available_models` mutex** at `tray.rs:194-198` via `model_manager.get_available_models()` (`model.rs:1156-1159`):
  ```rust
  let models = self.available_models.lock().unwrap();
  ```
  This mutex is held for long stretches during model operations (rescan `model.rs:1232`, download/extract at `:1374`, `:1938`, `:2126`, `:2163`, etc.).

### Lock-ordering hazard (inversion) — the hang

- **Background recording/streaming thread** holds the **`TranscriptionManager` engine mutex** for the whole stream (streaming leases the engine out). If the user changes language mid-stream, the main thread blocks on LOCK 2 forever → **spinning wheel, no more renders**.

- **A model download/extract thread** holds **`available_models`** for potentially seconds. Same result on LOCK 3.

- **Worse — true deadlock potential:** a model-download/extract background thread can call `write_settings(...)` (`model.rs:1447,1467`) and emit `model-state-changed` (`model.rs:1256`, `model.rs:1817`). The Rust listener for `model-state-changed` at `src-tauri/src/lib.rs:424-428` calls `tray::update_tray_menu` → tries to take `TRAY_LOCK`. If a download thread already holds `available_models` and the main thread holds `TRAY_LOCK` while waiting on `available_models`, the two threads can deadlock: main waits for `available_models` (held by download thread), download thread waits for `TRAY_LOCK` (held by main). **Neither ever releases → permanent freeze.**

- `change_tray_icon` (the other `TRAY_LOCK` user, `tray.rs:71-77`) is called from recording/streaming lifecycle code paths (`actions/transcribe.rs`, `actions/router.rs`, `transcription_coordinator.rs`, `managers/audio.rs`), often from threads that have already touched the transcription/engine locks. Every one of those contends on the same `TRAY_LOCK` the language command needs.

---

## Why it's a hang and not a JS error

- `syncLanguageFromSettings()` (`src/i18n/index.ts:89-108`) only calls `changeLanguage` when the stored language actually differs from `i18n.language` (`:94`), so there is **no self-reinforcing loop** between i18n and settings.
- `updateSetting` has proper optimistic-update + rollback (`settingsStore.ts:390-435`) and does **not** call `refreshSettings()` after `app_language` (the `app_language` updater at `:176` never calls `refreshSettings`, unlike e.g. `setPostProcessProvider`). So no settings-refresh → changeLanguage ping-pong.
- No `useEffect([i18n.language])` anywhere calls `updateSetting`; the only one is `App.tsx:59-61` which only calls `initializeRTL` (DOM attribute writes — harmless).
- `AppLanguageSelector` is `React.memo`'d but its props are stable primitives; a language change re-renders it once with a new `currentLanguage`, no loop.
- The overlay's `syncLanguageFromSettings()` on show (`src/overlay/hooks/useOverlayState.ts:320-323`) is guarded by `supported !== i18n.language`, so also non-looping.

All of the above confirm the JS layer is not the source of a crash/loop — leaving the **synchronous backend tray refresh blocking the main thread** as the operative cause of the infinite spinner.

---

## Secondary contributing factor (not the crash, but worth noting)

The `model-state-changed` → `refreshSettings()` listener in `settingsStore.ts:777-783` fires on **every** model state change. During a language switch this is not directly implicated, but combined with the tray rebuilds on `model-state-changed` (`lib.rs:426-428`) it increases main-thread work and the window of lock contention. Not the root cause.

---

## Exact file/line references

| File                                              | Lines       | What                                                                    |
| ------------------------------------------------- | ----------- | ----------------------------------------------------------------------- |
| `src/components/settings/AppLanguageSelector.tsx` | 26–29       | Fires `i18n.changeLanguage` + `updateSetting("app_language")`           |
| `src/stores/settingsStore.ts`                     | 176         | `app_language` → `commands.changeAppLanguageSetting`                    |
| `src/stores/settingsStore.ts`                     | 390–435     | `updateSetting` (optimistic; no `refreshSettings` for app_language)     |
| `src-tauri/src/shortcut/mod.rs`                   | 1340–1351   | **Synchronous command** calling `tray::update_tray_menu`                |
| `src-tauri/src/tray.rs`                           | 323–331     | `update_tray_menu` takes `TRAY_LOCK`                                    |
| `src-tauri/src/tray.rs`                           | 180–183     | Locks `TranscriptionManager` (engine mutex)                             |
| `src-tauri/src/tray.rs`                           | 194–198     | Locks `ModelManager.available_models`                                   |
| `src-tauri/src/managers/transcription.rs`         | 477–481     | `is_model_loaded` → `lock_engine()`                                     |
| `src-tauri/src/managers/transcription.rs`         | 1164 / 1373 | Engine leased by streaming worker (long hold)                           |
| `src-tauri/src/managers/model.rs`                 | 1156–1159   | `get_available_models` holds `available_models`                         |
| `src-tauri/src/managers/model.rs`                 | 1447 / 1467 | Model thread calls `write_settings`                                     |
| `src-tauri/src/lib.rs`                            | 424–428     | `model-state-changed` → `update_tray_menu` on main thread               |
| `src-tauri/src/lib.rs`                            | 390–407     | model-select tray handler threads its `update_tray_menu` (good pattern) |

---

## Recommended fixes (in priority order)

1. **Don't rebuild the tray on the main thread inside a synchronous command.** Make `change_app_language_setting` an `async fn` and/or spawn the tray refresh on a background thread (the tray `model_select:` handler already demonstrates the safe pattern at `lib.rs:397-407`). Same for the `model-state-changed` tray rebuild at `lib.rs:426-428`.

2. **Break the `TRAY_LOCK` + `available_models` + engine-mutex nesting.** In `update_tray_menu_internal`, acquire each lock in a narrow scope and drop it before the next (e.g., copy the `model_loaded` bool first, then release, then call `get_available_models`, then release), so the main thread never holds two backend locks simultaneously. Prefer cloning the model list once and passing it in.

3. **Make `change_app_language_setting` not block the render.** Since it's a fire-and-forget from the UI, run the whole settings-write + tray-refresh off the main thread so the webview keeps painting even under contention.

4. **Optional hardening:** serialize `change_tray_icon` / `update_tray_menu` on the background coordinator thread instead of the main thread, and avoid calling `get_available_models` under `TRAY_LOCK`.

---

## Sources / cross-reference

- Backend command + tray path: `shortcut/mod.rs`, `tray.rs`, `lib.rs` (read directly)
- Frontend flow: `AppLanguageSelector.tsx`, `settingsStore.ts`, `i18n/index.ts`, `App.tsx`, `useOverlayState.ts` (read directly)
- Confirmed `tauri = 2.10.2` in `src-tauri/Cargo.toml`
- History of the tray localization feature: `62cbe712` ("feat: add tray menu localization with live refresh")
