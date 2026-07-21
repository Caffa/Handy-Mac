
## 2026-07-21: Comprehensive Test Coverage Added

**Problem:** Many critical modules had zero test coverage despite being frequent bug sources (overlay, usb_watchdog, focus, audio manager). Frontend had no test infrastructure at all.

**What was added:**

1. **Rust unit tests (29 new):** `usb_watchdog.rs` — failure threshold logic (Bug #8: requires 2 consecutive failures), grace period, duration thresholds, disabled watchdog, config updates, serialization.

2. **Rust unit tests (6 new):** `focus.rs` — `SavedFrontmostApp` constructor, store/retrieve/clear/overwrite, Finder detection (Bug #14 regression).

3. **Rust integration tests (16 new):** `settings_race_tests.rs` — concurrent serde round-trips, partial JSON merge semantics, debounce simulation, NaN/inf sanitization, concurrent field updates.

4. **Frontend test infrastructure:** Vitest + jsdom setup with Tauri API mocks. 76 tests across `settingsStore.test.ts`, `modelStore.test.ts`, and `useAppState.test.ts`.

5. **Proptest cleanup:** Removed empty `#[ignore]` placeholder tests from `proptest_audio.rs` and `proptest_text.rs`. Documented what needs re-adding when removed symbols are ported back.

6. **Architecture fix:** Added `PartialEq` derive to `RecordingState` and `MicrophoneMode` enums in `audio.rs` to enable test assertions.

**Test counts after changes:**
- Rust lib tests: 393 total (386 pass, 6 pre-existing failures unrelated to this work)
- Rust E2E integration tests: 102 total (all pass)
- Frontend tests: 76 total (all pass)
- Proptests: 6 total (all pass)

**Lesson:** When adding tests for platform-specific code (macOS ObjC, USB hardware), test the pure logic and data structures that don't require the platform dependency. The `SharedState` pattern in `transcription_coordinator.rs` tests is a good example — it mirrors the production state machine without needing `AppHandle`.

## 2026-07-21: Model Switching Stuck in "Switching..." Bug

**Problem:** Switching transcription models (e.g., Parakeet Unified EN 0.6B) gets stuck showing "switching..." with a circling animation forever.

**Root Causes (3 bugs):**
1. **ModelSelector.tsx**: The `model-state-changed` event listener didn't handle the `selection_changed` event type. When `model_unload_timeout` is set to "Immediately", the Rust backend emits `selection_changed` instead of `loading_started`/`loading_completed`. This left `pendingModelId` stuck as non-null, keeping the selector in "loading" state forever.
2. **ModelsSettings.tsx**: When `selectModel()` fails (e.g., "Model load already in progress"), no error was displayed to the user. The switching spinner disappeared but the user had no idea why.
3. **Missing translation key**: No user-facing error message for failed model switches.

**Fix:**
- Added `case "selection_changed"` handler in ModelSelector.tsx that sets `modelStatus` to `"ready"`, clears `modelError`, and clears `pendingModelId`
- Added `toast.error()` call in `handleModelSelect` when `selectModel()` returns `false`
- Added `switchFailed` translation key in `en/translation.json`

**Lesson:** When frontend event listeners have switch statements for backend events, they must handle ALL event variants — not just the common ones. The `selection_changed` event was a valid backend path that the frontend never accounted for.

## 2026-07-21: E2E Testing Framework & Startup Crash Discovery

**Problem:** Building an E2E testing framework for Handy-Mac, we discovered the app crashes (SIGABRT) when launched from the terminal with `--start-hidden --no-tray --debug`.

**Root Cause:** Double-panic pattern during Tauri app initialization:
1. A panic occurs during the `.setup()` callback (app initialization)
2. The panic hook (`logging::install_panic_hook()`) tries to emit an `AppCrashed` Tauri event
3. The event emission also panics (double-panic → `abort()`)

**Fix:** Not yet implemented. The `safe_settings_operation()` function in `settings/store.rs` already wraps settings operations in panic-catching to prevent this pattern for WebKit's URL scheme handler, but the general panic hook doesn't guard against re-entrant panics during `emit()`.

**Lesson:** Always wrap panic hook logic that interacts with Tauri's event system in a `std::panic::catch_unwind()` to prevent double-panics. The panic hook should be infallible — if anything it calls can panic, the hook must catch that too.

**Test Results:**
- ✅ Rust integration tests: 86/86 pass (0.07s)
- ⚠️ Shell E2E startup test: App crashes on terminal launch (real bug found!)
- ✅ `--is-active-use` and `--is-recording` CLI flags: work correctly in headless mode
- ✅ Shell E2E CLI flags test: all flag parsing works correctly

**E2E Framework Architecture:**
- Layer 1: Rust integration tests (`src-tauri/tests/e2e/`) — no binary needed
- Layer 2: Shell script E2E tests (`scripts/e2e-tests/`) — tests compiled binary directly
- Layer 3: WebdriverIO UI tests (`e2e/`) — tests running app via WebDriver
