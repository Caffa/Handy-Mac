# E2E Testing Guide — Handy-Mac

> **Status:** Active  
> **Last updated:** 2026-07-21

This document describes how to run the end-to-end test suite for Handy-Mac. The suite has **three layers**, each testing at a different level of the stack.

## Known Issues

### App Startup Test (SIGABRT on launch from terminal)

The app startup test (`test-app-startup.sh`) currently **fails** when launching from the terminal with `--start-hidden --no-tray --debug`. The app crashes with `SIGABRT` during `_postDidFinishNotification` — this is a **double-panic** pattern where:

1. Something panics during the Tauri `.setup()` callback (app initialization)
2. The panic hook (`logging::install_panic_hook()`) tries to emit an `AppCrashed` event
3. Emitting the event also panics (double panic → `abort()`)

This is a **real bug** that the E2E test discovered. The other two startup sub-tests pass:
- ✅ `--is-active-use` returns idle (exit code 1) when the app is not running
- ✅ `--is-recording` returns not-recording (exit code 1) when the app is not running

**Workaround:** The `--transcribe-file`, `--is-active-use`, and `--is-recording` CLI flags all work correctly in headless mode. The crash only occurs when the full GUI app launches from a non-display context (terminal, CI).

### Query State File Stale State

After the app exits, the query state file (`$TMPDIR/handy_query_state.json`) may persist. This causes `--is-active-use` to return exit code 1 (idle) instead of 2 (not running). The shell E2E tests handle this by cleaning up the state file in their teardown, but if a test is interrupted, the stale file may remain.

---

## Quick Start

```bash
# From the Handy-Mac/ directory:

# 1. Rust integration tests (no binary needed, fastest)
cd src-tauri && cargo test --test e2e

# 2. Shell script E2E tests (needs release binary)
bash scripts/e2e-tests/run-all.sh

# 3. WebdriverIO UI tests (needs release binary + WDIO setup)
bun run test:e2e
```

---

## Test Layers

### Layer 1: Rust Integration Tests

**Location:** `src-tauri/tests/e2e/`  
**Runner:** `cargo test --test e2e`  
**Prerequisites:** None (no app binary or model needed)  
**Test count:** 86 tests  
**Execution time:** ~0.07s

These test the core Rust data types and logic without launching the full Tauri app. They cover:

| File | Tests | Coverage |
|------|-------|----------|
| `settings_tests.rs` | 28 | `AppSettings` serde round-trips, defaults, `SecretMap` redaction, NaN handling, persistence simulation, enum defaults |
| `cli_tests.rs` | 26 | `CliArgs` parsing, flag conflicts, `--transcribe-file` args, `--json`/`--repeat` flags, query flags |
| `transcription_tests.rs` | 17 | Hybrid mode thresholds, accelerator settings, model unload timeouts, VAD config, `OverlayStyle` variants, real-time factor, query state format |
| `coordinator_tests.rs` | 15 | `AppState` tagged enum serialization, `is_active_use` semantics, `PartialEq`, query state JSON structure |

**Run individual test modules:**
```bash
cargo test --test e2e -- settings_tests
cargo test --test e2e -- cli_tests
cargo test --test e2e -- transcription_tests
cargo test --test e2e -- coordinator_tests
```

**Run a single test:**
```bash
cargo test --test e2e -- settings_serialize_deserialize_roundtrip
```

### Layer 2: Shell Script E2E Tests

**Location:** `scripts/e2e-tests/`  
**Runner:** `bash scripts/e2e-tests/run-all.sh`  
**Prerequisites:** Release binary built (`cargo build --release` from `src-tauri/`), `jq` for settings tests, `ffmpeg` or `sox` for audio tests  
**Execution time:** ~30-60s (depends on model availability)

These test the **compiled binary** directly — does the app start? Do CLI flags work? Can we transcribe a file?

| Script | What it tests |
|--------|---------------|
| `test-app-startup.sh` | App starts without crashing, `--is-active-use` returns exit code 1 (idle), startup time benchmark |
| `test-settings-persistence.sh` | Settings file at `~/Library/Application Support/com.pais.handy/settings_store.json` survives app restart |
| `test-transcribe-file.sh` | Headless transcription via `--transcribe-file --json`, validates output fields (`text`, `model`, `transcribe_ms`, `best_ms`, `rtf`) |
| `test-consecutive-runs.sh` | 5 consecutive transcriptions in sequence, per-run timing, degradation check |
| `test-cli-flags.sh` | `--is-active-use`/`--is-recording` when not running (exit 2), `--list-models`, `--list-devices`, conflicting flags rejected |
| `run-all.sh` | Master runner with `--release`/`--debug` flag, `--skip-*` filters, summary table |

**Run individual scripts:**
```bash
bash scripts/e2e-tests/test-app-startup.sh
bash scripts/e2e-tests/test-settings-persistence.sh
bash scripts/e2e-tests/test-transcribe-file.sh
bash scripts/e2e-tests/test-consecutive-runs.sh
bash scripts/e2e-tests/test-cli-flags.sh
```

**Or via npm:**
```bash
bun run test:e2e:shell:startup
bun run test:e2e:shell:settings
bun run test:e2e:shell:transcribe
bun run test:e2e:shell:consecutive
bun run test:e2e:shell:cli
bun run test:e2e:shell        # runs all shell tests
```

**Run-all.sh options:**
```bash
bash scripts/e2e-tests/run-all.sh --release      # Use release binary (default)
bash scripts/e2e-tests/run-all.sh --debug          # Use debug binary
bash scripts/e2e-tests/run-all.sh --skip-transcription  # Skip transcription tests
bash scripts/e2e-tests/run-all.sh --consecutive-runs 3   # Only 3 consecutive runs
```

**Important notes:**
- Transcription tests (`test-transcribe-file.sh`, `test-consecutive-runs.sh`) **skip automatically** if no model is downloaded
- Settings persistence tests skip if `jq` is not installed
- All scripts clean up processes and temp files in trap handlers
- Exit codes: 0=pass, 1=fail, 2=skip

**Building the release binary first:**
```bash
cd src-tauri && cargo build --release
# Binary at: src-tauri/target/release/handy
```

### Layer 3: WebdriverIO UI Tests

**Location:** `e2e/`  
**Runner:** `bun run test:e2e` or `wdio run e2e/wdio.conf.ts`  
**Prerequisites:** Release binary built, `bun install` completed  
**Dependencies:** `@wdio/cli`, `@wdio/mocha-framework`, `@wdio/spec-reporter`, `@wdio/tauri-service`, `webdriverio`

These test the **running app with its UI** via WebDriver, invoking Tauri commands from the webview context.

| Spec | What it tests |
|------|---------------|
| `specs/app-lifecycle.spec.ts` | App starts, has main window, responds to `get_app_settings`, defaults present |
| `specs/settings.spec.ts` | Theme/overlay/VAD persistence across reloads, multi-setting saves, restore in `after()` hook |

**Run individual specs:**
```bash
bun run test:e2e:lifecycle    # Just app-lifecycle.spec.ts
bun run test:e2e:settings     # Just settings.spec.ts
```

**Helper modules:**
- `helpers/tauri-commands.ts` — Wraps `browser.execute()` for invoking Tauri commands cleanly
- `helpers/audio-helpers.ts` — Generates 440Hz sine WAV fixtures via ffmpeg

**How Tauri commands are invoked in tests:**
```typescript
// Via the helper module:
import { getAppSettings, changeSetting } from '../helpers/tauri-commands.js';
const settings = await getAppSettings();

// Directly in tests:
const settings = await browser.execute(() => {
  return (window as any).__TAURI_INTERNALS__.invoke('get_app_settings');
});
```

---

## Prerequisites Summary

| Layer | Needs binary? | Needs model? | Needs extra tools? | Time |
|-------|--------------|-------------|-------------------|------|
| Rust integration | No | No | Rust toolchain | 0.07s |
| Shell E2E | Yes (release) | Only for transcription | `jq`, `ffmpeg`/`sox` | 30-60s |
| WebdriverIO | Yes (release) | No | WDIO deps installed | 10-30s |

---

## Troubleshooting

### Rust tests fail with "linker" errors
```bash
cd src-tauri && cargo clean && cargo test --test e2e
```

### Shell tests can't find the binary
The `find_binary()` function in `common.sh` checks:
1. `src-tauri/target/release/handy`
2. `src-tauri/target/release/Handy-Mac`

Build first:
```bash
cd src-tauri && cargo build --release
```

### Transcription tests skip with "no models available"
Download a model first (via the app UI or CLI):
```bash
# Via the app: Settings → Model → Download
# Or check available models:
./src-tauri/target/release/handy --list-models
```

### Settings persistence tests skip with "jq not found"
```bash
brew install jq
```

### WebdriverIO tests fail to connect
Ensure the release binary is built and `bun install` has been run:
```bash
cd src-tauri && cargo build --release
cd .. && bun install
```

### WebdriverIO `browser.execute` returns undefined
The `__TAURI_INTERNALS__` object is only available when the app is running in Tauri's webview. Make sure the `@wdio/tauri-service` is properly configured to launch the app binary.

---

## Adding New Tests

### Rust integration test
1. Add a new test file in `src-tauri/tests/e2e/`
2. Add it to `src-tauri/tests/e2e/main.rs` as `mod your_module;`
3. Follow existing patterns (use `tempfile` for isolated test dirs, `serde_json` for JSON assertions)

### Shell script test
1. Create `scripts/e2e-tests/test-your-feature.sh`
2. Source `common.sh` at the top: `source "$(dirname "$0")/common.sh"`
3. Use helper functions: `find_binary`, `wait_for_app`, `kill_app`, `assert_exit_code`, `log_test`, etc.
4. Add to `run-all.sh` in the test list
5. Make executable: `chmod +x scripts/e2e-tests/test-your-feature.sh`

### WebdriverIO test
1. Create `e2e/specs/your-feature.spec.ts`
2. Use helpers from `e2e/helpers/tauri-commands.ts`
3. Always restore original state in `after()` hooks
4. Run: `bun run test:e2e`

---

## File Locations Quick Reference

```
Handy-Mac/
├── src-tauri/tests/e2e/          # Layer 1: Rust integration tests
│   ├── main.rs                   # Module declarations
│   ├── settings_tests.rs         # 28 tests
│   ├── cli_tests.rs              # 26 tests
│   ├── transcription_tests.rs    # 17 tests
│   └── coordinator_tests.rs      # 15 tests
├── scripts/e2e-tests/           # Layer 2: Shell E2E tests
│   ├── common.sh                 # Shared utilities
│   ├── run-all.sh                # Master runner
│   ├── test-app-startup.sh       # App lifecycle
│   ├── test-settings-persistence.sh
│   ├── test-transcribe-file.sh
│   ├── test-consecutive-runs.sh
│   └── test-cli-flags.sh
├── e2e/                          # Layer 3: WebdriverIO UI tests
│   ├── wdio.conf.ts              # WDIO configuration
│   ├── tsconfig.json
│   ├── fixtures/.gitkeep
│   ├── helpers/
│   │   ├── tauri-commands.ts
│   │   └── audio-helpers.ts
│   └── specs/
│       ├── app-lifecycle.spec.ts
│       └── settings.spec.ts
└── package.json                  # npm scripts for all layers
```