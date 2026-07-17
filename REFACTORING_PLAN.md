<!-- Status: Active — Phases 1-3 done (state consolidation, visualizer fix). Phase 4 (remove legacy state) is in progress — the overlay state machine unification (commit 2255ae8) has made useAppState the sole visibility authority. -->

# Refactoring Plan: State Consolidation & Visualizer Frozen Bug Fix

**Date:** 2026-07-06  
**Status:** Planning  
**Scope:** Backend state machine unification, frontend state migration, cancel race fix  

---

## Table of Contents

1. [Problem Statement](#1-problem-statement)
2. [Current Architecture](#2-current-architecture)
3. [Target Architecture](#3-target-architecture)
4. [Phase 1: Backend State Consolidation](#4-phase-1-backend-state-consolidation)
5. [Phase 2: Frontend State Migration](#5-phase-2-frontend-state-migration)
6. [Phase 3: Fix Visualizer Frozen Bug](#6-phase-3-fix-visualizer-frozen-bug)
7. [Phase 4: Remove Legacy State](#7-phase-4-remove-legacy-state)
8. [Migration Strategy](#8-migration-strategy)
9. [Rollback Plan](#9-rollback-plan)
10. [Testing Checklist](#10-testing-checklist)

---

## 1. Problem Statement

Handy has 3 independent state machines that can desync, causing visual bugs:

### Symptom: Visualizer Frozen on Cancel

1. User presses cancel hotkey while GPU transcription holds the `TranscriptionManager` mutex
2. `cancel_current_operation()` in `utils.rs` calls `audio_manager.cancel_recording()` and `force_hide_recording_overlay()`
3. The overlay hides, but the `TranscriptionCoordinator` stage may still be `Processing`
4. If the coordinator later receives `ProcessingFinished`, it resets to `Idle` — but the frontend never sees this
5. On the next recording, the frontend can show stale state

### Race Conditions

| # | Race | Impact |
|---|------|--------|
| 1 | Cancel hotkey pressed while GPU transcription holds TM mutex | UI freezes; cancel feels unresponsive |
| 2 | Frontend thinks "recording" but backend is already "processing" | Wrong overlay state shown |
| 3 | USB cycling timeout fires but backend already finished | Overlay stuck in usb-cycling |
| 4 | `hide-overlay` event arrives after new `show-overlay` event | Overlay closes during active recording |
| 5 | `FinishGuard` drops after cancel resets coordinator to Idle | Coordinator ignores it (current fix), but frontend still gets stale `show-overlay` timing |

---

## 2. Current Architecture

### 2.1 Frontend State Machine (TypeScript)

```
Location: src/overlay/hooks/useOverlayState.ts

OverlayState = "recording" | "transcribing" | "processing" | "usb-cycling" | "confirming"
OverlayAction = "transcribe" | "post_process" | "router"

Event-driven updates:
  show-overlay(payload) → parse "state:action" → setState + setAction
  hide-overlay({force})  → if !force && active state → keep visible; else hide

State transitions triggered by events:
  mic-level          → visualizer bars
  partial-transcription → streaming text
  transcription-preview → router confirm countdown
  routing-state      → processing state
  router-result      → show result
  usb-power-cycle-*  → USB cycling overlay
  recording-error    → hide overlay
```

### 2.2 Backend State Machine #1: TranscriptionCoordinator (Rust)

```
Location: src-tauri/src/transcription_coordinator.rs

Stage::Idle
Stage::Recording(binding_id)
Stage::Processing { since: Instant }

Single-threaded command loop via mpsc::channel:
  Command::Input { binding_id, is_pressed, push_to_talk }
  Command::Cancel { recording_was_active }
  Command::ProcessingFinished
  Command::ProcessingTimeout

State transitions:
  Idle → Recording: start() called (hotkey press)
  Recording → Processing: stop() called (hotkey release)
  Processing → Idle: ProcessingFinished or timeout or cancel
  Any → Idle: Cancel (with recording_was_active flag)
```

### 2.3 Backend State Machine #2: AudioRecordingManager (Rust)

```
Location: src-tauri/src/managers/audio.rs

RecordingState::Idle
RecordingState::Recording { binding_id, start_time }

Additional flags:
  is_recording: Arc<AtomicBool>  — checked by coordinator start()
  is_open: Arc<AtomicBool>       — stream open/closed
  did_mute: Arc<AtomicBool>      — mute while recording
  bt_keep_alive: Arc<AtomicBool> — BT headset keep-alive

State transitions:
  Idle → Recording: try_start_recording()
  Recording → Idle: stop_recording() or cancel_recording()
```

### 2.4 Event Flow (Current)

```
                    ┌─────────────────────┐
                    │  TranscriptionCoordinator  │
                    │  (Stage: Idle/Recording/Processing) │
                    └──────────┬──────────┘
                               │
                    ┌──────────▼──────────┐
                    │  AudioRecordingManager  │
                    │  (RecordingState + AtomicBools) │
                    └──────────┬──────────┘
                               │
         ┌─────────────────────┼─────────────────────┐
         │                     │                     │
    ┌────▼─────┐        ┌──────▼──────┐      ┌──────▼──────┐
    │ actions.rs│       │  utils.rs    │      │ overlay.rs  │
    │ start/stop│       │ cancel_current│      │ show/hide   │
    └────┬─────┘       │ _operation() │      │ overlay      │
         │              └──────┬──────┘      └──────┬──────┘
         │                     │                     │
         │    ┌────────────────┼─────────────────────┤
         │    │                │                     │
    ┌────▼────▼──────┐  ┌─────▼──────┐  ┌──────────▼──────────┐
    │ show-overlay    │  │ force-hide │  │ hide-overlay         │
    │ "recording:..."│  │ overlay    │  │ { force: true }      │
    │ "transcribing:"│  │            │  │                      │
    │ "processing:" │  │            │  │                      │
    └────────────────┘  └────────────┘  └─────────────────────┘
              │                              │
              ▼                              ▼
    ┌─────────────────────────────────────────────┐
    │          Frontend (useOverlayState)          │
    │  listen("show-overlay") → setState + setAction │
    │  listen("hide-overlay") → conditional hide    │
    └─────────────────────────────────────────────┘
```

### 2.5 Desync Points

```
PROBLEM: Three independent state sources with no reconciliation

  Frontend (OverlayState)        Backend (Stage)         AudioRecordingManager
  ─────────────────────         ──────────────          ─────────────────────
  "recording"                    Recording(X)            is_recording=true
  "transcribing"                 Processing               is_recording=false
  "processing"                  Processing               is_recording=false
  "usb-cycling"                 Recording or Idle        is_recording=true
  "confirming"                  Idle                     is_recording=false
  (stale state after cancel)     Idle                     is_recording=false
```

**Key issue:** The frontend has no way to query backend state. It can only infer it from events, which can arrive out of order or be lost.

---

## 3. Target Architecture

### 3.1 Single Source of Truth: Backend Owns State

```
┌──────────────────────────────────────────────────────────┐
│              AppState (Rust, single truth)                │
│                                                          │
│  pub enum AppState {                                     │
│      Idle,                                               │
│      Recording { binding_id: String, since: Instant },   │
│      Processing { since: Instant },                      │
│      UsbCycling { stage: String },                       │
│      Confirming { text: String },                        │
│  }                                                       │
│                                                          │
│  Owned by: TranscriptionCoordinator                      │
│  Queried via: get_state() → AppState                     │
│  Emitted via: "app-state" event to frontend              │
└──────────────────────────────────────────────────────────┘
         │                              │
         │ get_state()                  │ emit("app-state")
         ▼                              ▼
  ┌─────────────┐              ┌─────────────────┐
  │ Backend      │              │ Frontend        │
  │ (coordinator │              │ (useAppState)   │
  │  decisions)  │              │ (read-only)     │
  └─────────────┘              └─────────────────┘
```

### 3.2 Frontend Reflects Backend State

```tsx
// src/overlay/hooks/useAppState.ts
const [appState, setAppState] = useState<AppState>({ state: "idle" });

useEffect(() => {
  const unlisten = listen<AppState>("app-state", (event) => {
    setAppState(event.payload);
  });
  return () => { unlisten.then(f => f()); };
}, []);
```

### 3.3 Cancel via Channel (Non-blocking)

```
┌──────────────────────────────────────────────┐
│  CancelSignal (tokio::sync::broadcast)        │
│                                              │
│  cancel_current_operation()                  │
│    → cancel_sender.send(Cancel)  (non-block) │
│    → coordinator receives Cancel              │
│    → transitions to Idle                     │
│    → emits app-state: Idle                    │
│    → frontend hides overlay                   │
└──────────────────────────────────────────────┘
```

---

## 4. Phase 1: Backend State Consolidation

**Goal:** Create a unified `AppState` enum, add `get_state()` to the coordinator, emit `app-state` events. No frontend changes.

**Duration:** ~1-2 days  
**Risk:** Low — additive changes, backward compatible

### 4.1 File Changes

#### `src-tauri/src/transcription_coordinator.rs`

```rust
// ADD: Unified application state enum
#[derive(Clone, Debug, serde::Serialize)]
#[serde(tag = "state", content = "data")]
pub enum AppState {
    Idle,
    Recording { binding_id: String },
    Processing,
    UsbCycling { stage: String },
    Confirming { text: String },
}

// MODIFY: Replace Stage with AppState internally
// The coordinator's internal Stage enum remains, but AppState
// is the public-facing type that includes ALL states.

// ADD: State change notification
impl TranscriptionCoordinator {
    /// Get the current application state (thread-safe)
    pub fn get_state(&self) -> AppState {
        // Read from shared state, not from channel thread
        // Use Arc<RwLock<AppState>> for reads
    }
}

// ADD: Emit app-state on every state transition
fn emit_app_state(app: &AppHandle, state: &AppState) {
    if let Some(overlay) = app.get_webview_window("recording_overlay") {
        let _ = overlay.emit("app-state", state);
    }
    // Also emit to main window for settings UI
    let _ = app.emit("app-state", state);
}
```

**Detailed changes:**

1. Add `AppState` enum with `serde::Serialize` derive
2. Add `Arc<RwLock<AppState>>` field to `TranscriptionCoordinator` to track current state
3. Add `get_state()` method returning `AppState`
4. In the coordinator thread loop, after every `set_stage()` call, also update the shared `AppState` and emit `app-state` event
5. For states not managed by the coordinator (usb-cycling, confirming), add methods to set them:
   - `set_usb_cycling(stage: String)` → updates shared state + emits
   - `set_confirming(text: String)` → updates shared state + emits
   - `set_idle()` → resets to Idle + emits

#### `src-tauri/src/overlay.rs`

```rust
// ADD: Emit app-state alongside show-overlay for backward compatibility
pub(crate) fn show_overlay_state(app_handle: &AppHandle, state: &str, mode: &OverlayMode) {
    // ... existing show-overlay emit ...

    // NEW: Also emit app-state for the new frontend hook
    let app_state = match state {
        "recording" => AppState::Recording { 
            binding_id: String::new() // will be updated by coordinator
        },
        "transcribing" | "processing" => AppState::Processing,
        "usb-cycling" => AppState::UsbCycling { stage: String::new() },
        _ => AppState::Idle,
    };
    emit_app_state(app_handle, &app_state);
}
```

#### `src-tauri/src/utils.rs`

```rust
// MODIFY: cancel_current_operation() to also emit app-state
pub fn cancel_current_operation(app: &AppHandle) {
    // ... existing cancel logic ...

    // NEW: Emit app-state: Idle after cancel
    emit_app_state(app, &AppState::Idle);
}
```

#### `src-tauri/src/lib.rs`

```rust
// ADD: Register AppState as managed state
// (needed if we want to expose get_state as a Tauri command)
```

### 4.2 State Mapping: Old → New

| Old Event | Old Payload | New `app-state` Payload |
|-----------|-------------|------------------------|
| `show-overlay` | `"recording:transcribe"` | `{ state: "Recording", data: { binding_id: "transcribe" } }` |
| `show-overlay` | `"transcribing:transcribe"` | `{ state: "Processing" }` |
| `show-overlay` | `"processing:router"` | `{ state: "Processing" }` |
| `show-overlay` | `"usb-cycling:transcribe"` | `{ state: "UsbCycling", data: { stage: "" } }` |
| `hide-overlay` | `{ force: true }` | `{ state: "Idle" }` |
| `hide-overlay` | `{ force: false }` | `{ state: "Idle" }` (conditional) |
| coordinator cancel | (implicit) | `{ state: "Idle" }` |
| `transcription-preview` | (string) | `{ state: "Confirming", data: { text: "..." } }` |

### 4.3 Testing (Phase 1)

- [ ] Verify `app-state` events fire on every state transition
- [ ] Verify backward compatibility: existing `show-overlay`/`hide-overlay` still work
- [ ] Verify coordinator still manages `Stage` correctly
- [ ] Verify `get_state()` returns correct state from any thread
- [ ] Manual test: record → transcribe → paste, all states transition correctly
- [ ] Manual test: cancel during recording resets to Idle
- [ ] Manual test: cancel during processing resets to Idle

---

## 5. Phase 2: Frontend State Migration

**Goal:** Create `useAppState` hook, migrate components to use it alongside existing state. Dual-listening period.

**Duration:** ~1-2 days  
**Risk:** Medium — frontend changes, but dual-listening provides fallback

### 5.1 File Changes

#### `src/overlay/hooks/useAppState.ts` (NEW)

```typescript
/**
 * useAppState — Reactive frontend state derived from the backend's
 * single source of truth. Listens to "app-state" events emitted by
 * the Rust TranscriptionCoordinator.
 *
 * During migration, this hook runs alongside useOverlayState.
 * Once migration is complete, useOverlayState will be removed and
 * this hook becomes the sole state authority.
 */

import { listen } from "@tauri-apps/api/event";
import { useEffect, useState, useRef, useCallback } from "react";

// Mirror the Rust AppState enum
export type AppState =
  | { state: "Idle" }
  | { state: "Recording"; data: { binding_id: string } }
  | { state: "Processing" }
  | { state: "UsbCycling"; data: { stage: string } }
  | { state: "Confirming"; data: { text: string } };

// Convenience type guard helpers
export function isIdle(s: AppState): s is { state: "Idle" } {
  return s.state === "Idle";
}
export function isRecording(s: AppState): s is { state: "Recording"; data: { binding_id: string } } {
  return s.state === "Recording";
}
export function isProcessing(s: AppState): s is { state: "Processing" } {
  return s.state === "Processing";
}
export function isUsbCycling(s: AppState): s is { state: "UsbCycling"; data: { stage: string } } {
  return s.state === "UsbCycling";
}
export function isConfirming(s: AppState): s is { state: "Confirming"; data: { text: string } } {
  return s.state === "Confirming";
}

// Map AppState to legacy OverlayState for gradual migration
export function appStateToOverlayState(appState: AppState): OverlayState {
  switch (appState.state) {
    case "Recording":
      return "recording";
    case "Processing":
      return "processing"; // will be split into transcribing/processing later
    case "UsbCycling":
      return "usb-cycling";
    case "Confirming":
      return "confirming";
    case "Idle":
    default:
      return "recording"; // default; shouldn't happen when visible
  }
}

export function useAppState() {
  const [appState, setAppState] = useState<AppState>({ state: "Idle" });
  const appStateRef = useRef<AppState>(appState);
  
  useEffect(() => {
    appStateRef.current = appState;
  }, [appState]);

  useEffect(() => {
    const setup = async () => {
      const unlisten = await listen<AppState>("app-state", (event) => {
        setAppState(event.payload);
      });
      return unlisten;
    };
    
    let unlistenFn: (() => void) | null = null;
    setup().then((unlisten) => {
      unlistenFn = unlisten;
    });
    
    return () => {
      unlistenFn?.();
    };
  }, []);

  // Derive legacy-compatible values
  const overlayState = appStateToOverlayState(appState);
  const isVisible = appState.state !== "Idle";
  const isRouter = appState.state === "Recording" 
    ? appState.data.binding_id === "transcribe_with_router"
    : false;

  return {
    appState,
    overlayState, // legacy compat
    isVisible,
    isRouter,
    isIdle: appState.state === "Idle",
    isRecording: appState.state === "Recording",
    isProcessing: appState.state === "Processing",
    isUsbCycling: appState.state === "UsbCycling",
    isConfirming: appState.state === "Confirming",
    appStateRef, // for callback access
  };
}
```

#### `src/overlay/RecordingOverlay.tsx`

```tsx
// ADD: Import useAppState alongside useOverlayState
import { useAppState } from "./hooks/useAppState";

const RecordingOverlay: React.FC = () => {
  // Existing state (kept for migration)
  const overlayState = useOverlayState();
  
  // New backend-driven state (gradually takes over)
  const backendState = useAppState();
  
  // During migration: use backendState when available, fall back to overlayState
  // This ensures no regressions during the transition period
  const state = backendState.appState.state !== "Idle" 
    ? backendState.overlayState 
    : overlayState.state;
    
  // ... rest of component unchanged ...
};
```

### 5.2 Migration Strategy for Each Sub-hook

| Hook | Current State Source | Migration Target | Phase |
|------|---------------------|-----------------|-------|
| `useOverlayState` | `show-overlay`/`hide-overlay` events | `useAppState` | Phase 2 (gradual) |
| `useVisualizer` | `state` from `useOverlayState` | `useAppState.isRecording` | Phase 2 |
| `useLiveCaptions` | `state` from `useOverlayState` | `useAppState.isRecording` | Phase 2 |
| `useRouterPreview` | `state` + `transcription-preview` event | `useAppState.isConfirming` | Phase 2 |
| `useUSBRecovery` | `state` + `usb-power-cycle-*` events | `useAppState.isUsbCycling` | Phase 2 |

### 5.3 Dual-Listening Period

During Phase 2, both old and new event sources are active:

```
Frontend receives:
  1. show-overlay → useOverlayState (legacy, kept as fallback)
  2. app-state   → useAppState (new, gradually becomes primary)

Migration order:
  a. Visual state (recording/transcribing/processing indicators)
  b. USB cycling state
  c. Router confirmation state
  d. Cancel/visibility state (last, most critical)
```

### 5.4 Testing (Phase 2)

- [ ] Verify `useAppState` correctly reflects backend state
- [ ] Verify fallback to `useOverlayState` works when `app-state` events are missing
- [ ] Test recording → transcribing → paste cycle
- [ ] Test router mode: recording → confirming → processing → result
- [ ] Test cancel during recording
- [ ] Test cancel during processing (the frozen bug case)
- [ ] Test USB cycling flow
- [ ] Verify no visual regressions in overlay behavior

---

## 6. Phase 3: Fix Visualizer Frozen Bug

**Goal:** Make cancellation truly async so the UI never blocks on the TM mutex. Use channels instead of blocking calls.

**Duration:** ~2-3 days  
**Risk:** High — core cancel path changes

### 6.1 Root Cause Analysis

The visualizer frozen bug occurs because:

1. **Cancel path blocks on TM mutex**: `cancel_current_operation()` in `utils.rs` does NOT acquire the TM mutex (this was already fixed). However, the `force_hide_recording_overlay()` call hides the overlay window immediately while the streaming callback may still be running on a background thread holding GPU resources.

2. **Frontend state desync**: After `force_hide_recording_overlay()` fires `hide-overlay { force: true }`, the frontend sets `isVisible = false` but the backend `Stage` might still be `Processing`. If `FinishGuard` later fires, it sends `ProcessingFinished` to the coordinator which transitions to `Idle` — but no `app-state` event reaches the frontend because no overlay event is emitted for this transition.

3. **Visualizer bars freeze**: The `mic-level` event listener in `useVisualizer` continues running but the visualizer bars don't update because `isVisible` is `false` and `state` is no longer `"recording"`. The decay timer in `useVisualizer` is supposed to fade bars to zero, but the `LEVEL_TIMEOUT_MS` timeout can leave bars stuck at non-zero values if the `mic-level` events keep arriving.

### 6.2 Fix: Async Cancel via Channel

#### `src-tauri/src/transcription_coordinator.rs`

```rust
use tokio::sync::broadcast;

/// Cancel signal channel. Senders are non-blocking.
/// The coordinator thread listens for Cancel and transitions
/// to Idle regardless of what the transcription pipeline is doing.
pub struct CancelSignal {
    sender: broadcast::Sender<()>,
}

impl CancelSignal {
    pub fn new() -> Self {
        let (sender, _) = broadcast::channel(16);
        Self { sender }
    }

    /// Non-blocking cancel signal. Returns immediately.
    pub fn send_cancel(&self) {
        let _ = self.sender.send(());
    }

    /// Create a receiver for the cancel signal.
    pub fn subscribe(&self) -> broadcast::Receiver<()> {
        self.sender.subscribe()
    }
}
```

**Modify coordinator loop to listen for cancel:**

```rust
// In the coordinator thread, add a cancel receiver
let mut cancel_rx = cancel_signal.subscribe();

loop {
    // Use recv_timeout to allow checking both channels
    let cmd = select! {
        cmd = rx.recv() => cmd,
        _ = cancel_rx.recv() => {
            // Cancel received — transition to Idle immediately
            set_stage(&mut stage, Stage::Idle, &active_use_clone);
            emit_app_state(&app, &AppState::Idle);
            info!("Coordinator: cancel via broadcast, reset to Idle");
            continue;
        }
    };
    // ... existing command handling ...
}
```

#### `src-tauri/src/utils.rs`

```rust
pub fn cancel_current_operation(app: &AppHandle) {
    info!("Initiating operation cancellation...");

    // 1. Cancel streaming transcription (non-blocking, AtomicBool)
    if let Some(cancel_flag) = app.try_state::<Arc<AtomicBool>>() {
        cancel_flag.swap(true, Ordering::AcqRel);
    }

    // 2. Unregister cancel shortcut (non-blocking)
    shortcut::unregister_cancel_shortcut(app);

    // 3. Cancel any ongoing recording
    let Some(audio_manager) = app.try_state::<Arc<AudioRecordingManager>>() else {
        warn!("AudioRecordingManager not available for cancellation");
        return;
    };
    
    let recording_was_active = audio_manager.is_recording();
    if recording_was_active {
        audio_manager.cancel_recording();
    }

    // 4. Send cancel signal through broadcast channel (non-blocking)
    if let Some(cancel_signal) = app.try_state::<CancelSignal>() {
        cancel_signal.send_cancel();
        info!("Cancel signal sent via broadcast channel");
    }

    // 5. Notify coordinator (existing path, kept for backward compat)
    if let Some(coordinator) = app.try_state::<TranscriptionCoordinator>() {
        coordinator.notify_cancel(recording_was_active);
    }

    // 6. Update tray and force-hide overlay
    change_tray_icon(app, TrayIconState::Idle);
    force_hide_recording_overlay(app);

    // 7. Emit unified app-state: Idle
    emit_app_state(app, &AppState::Idle);

    info!("Operation cancellation completed - returned to idle state");
}
```

### 6.3 Fix: Ensure Visualizer Clears on Cancel

#### `src/overlay/hooks/useVisualizer.ts`

```typescript
// ADD: Clear visualizer bars when state transitions away from recording
useEffect(() => {
  if (state !== "recording" || !isVisible) {
    // Immediately zero out all bars when not recording
    setLevels(Array(9).fill(0));
  }
}, [state, isVisible]);
```

#### `src/overlay/hooks/useAppState.ts`

```typescript
// ADD: When transitioning to Idle, ensure all visual state resets
useEffect(() => {
  if (appState.state === "Idle") {
    // Reset all derived visual state
    // This is the single point where we know the app is truly idle
  }
}, [appState.state]);
```

### 6.4 Fix: Prevent Stale State on Cancel

The core issue is that `force_hide_recording_overlay` fires `hide-overlay { force: true }` but the frontend doesn't get a corresponding `app-state: Idle` event. With Phase 1's `emit_app_state(app, &AppState::Idle)` in `cancel_current_operation()`, the frontend will now receive both events:

1. `hide-overlay { force: true }` → immediately hides overlay
2. `app-state { state: "Idle" }` → resets all state to known-good idle

### 6.5 Testing (Phase 3)

- [ ] **Critical test**: Record → press cancel hotkey → verify overlay disappears and bars fade to zero
- [ ] Record → press cancel hotkey → immediately start new recording → verify new recording works
- [ ] Record → release hotkey (transcription starts) → press cancel hotkey → verify overlay hides
- [ ] Start recording → cancel → start recording again within 500ms → verify no freeze
- [ ] Cancel during GPU transcription (the exact frozen bug scenario)
- [ ] Cancel during USB cycling
- [ ] Cancel during router confirmation countdown
- [ ] Verify cancel signal doesn't block (measure `cancel_current_operation` execution time < 50ms)
- [ ] Stress test: rapid cancel-restart cycles (10 times rapidly)

---

## 7. Phase 4: Remove Legacy State

**Goal:** Remove `show-overlay`/`hide-overlay` payload parsing, remove frontend `OverlayState` enum, remove backend `Stage` enum.

**Duration:** ~1-2 days  
**Risk:** Medium — removing working code, but thoroughly tested in Phase 2-3

**Progress (2026-07-09):** The overlay state machine unification (commit `2255ae8`) has completed the core of Phase 4:
- `useAppState` is now the sole visibility authority — no more dual-listening
- `useOverlayState` renamed to `useOverlaySharedState` — visibility logic stripped, only shared mutable state remains
- `show-overlay`/`hide-overlay` frontend listeners removed (backend still emits for backward compat)
- Visibility is a pure function of `AppState` (Idle = hidden, anything else = visible)
- Remaining cleanup: remove `OverlayState` type entirely, remove `appStateToOverlayState` mapping, remove backend `show-overlay`/`hide-overlay` emissions

### 7.1 File Changes

#### `src/overlay/hooks/useOverlayState.ts`

- Remove `OverlayState` and `OverlayAction` types
- Remove `parseOverlayPayload()`
- Remove `show-overlay` and `hide-overlay` event listeners
- Keep only the settings-derived state (hybrid mode, overlay scale, etc.)
- Rename to `useOverlaySettings()` or merge into `useAppState()`

#### `src/overlay/hooks/useAppState.ts`

- Remove legacy compat (`overlayState`, `appStateToOverlayState`)
- Remove `isRouter` derivation (move to dedicated hook or pass via event data)
- Make this the primary state source

#### `src/overlay/RecordingOverlay.tsx`

- Replace `useOverlayState()` with `useAppState()`
- Remove all `state === "recording"` / `state === "transcribing"` checks, replace with `backendState.isRecording` etc.
- Remove `setState` usage (backend now owns state)

#### `src-tauri/src/transcription_coordinator.rs`

- Remove `Stage` enum
- Replace with `AppState` everywhere
- Remove `set_stage()` helper (direct `AppState` updates instead)

#### `src-tauri/src/overlay.rs`

- Remove `show-overlay` event emission (or deprecate with a console warning)
- Keep `app-state` as the only state event

#### `src-tauri/src/utils.rs`

- Remove `force_hide_recording_overlay()` call from cancel path
- Replace with `emit_app_state(app, &AppState::Idle)` 
- The frontend will handle hiding the overlay based on `app-state: Idle`

### 7.2 Event Migration Map

| Old Event | New Event | Migration |
|-----------|-----------|-----------|
| `show-overlay` with `"recording:transcribe"` | `app-state` with `{ state: "Recording", data: { binding_id: "transcribe" } }` | Phase 2 |
| `show-overlay` with `"transcribing:transcribe"` | `app-state` with `{ state: "Processing" }` | Phase 2 |
| `show-overlay` with `"processing:router"` | `app-state` with `{ state: "Processing" }` + mode info | Phase 2 |
| `show-overlay` with `"usb-cycling:transcribe"` | `app-state` with `{ state: "UsbCycling", data: { stage: "" } }` | Phase 2 |
| `hide-overlay` with `{ force: true }` | `app-state` with `{ state: "Idle" }` | Phase 3 |
| `hide-overlay` with `{ force: false }` | `app-state` with `{ state: "Idle" }` (conditional) | Phase 3 |
| `transcription-preview` | `app-state` with `{ state: "Confirming", data: { text: "..." } }` | Phase 4 |
| `routing-state` with `"processing"` | `app-state` with `{ state: "Processing" }` | Phase 4 |
| `mic-level` | Keep as-is (data event, not state) | No change |

### 7.3 Testing (Phase 4)

- [ ] Remove `show-overlay` event from backend, verify `app-state` drives all UI
- [ ] Remove `hide-overlay` event from backend, verify `app-state: Idle` hides overlay
- [ ] Remove `OverlayState` enum from frontend
- [ ] Full regression: recording, transcribing, processing, USB cycling, router
- [ ] Full regression: cancel at every stage
- [ ] Performance: no visual lag between state transitions
- [ ] Verify no TypeScript errors after removing legacy types

---

## 8. Migration Strategy

### 8.1 Phased Rollout

Each phase is independently deployable. If a phase causes regressions, it can be reverted without affecting the previous phase.

```
Phase 1 (Backend)     ──→  Deploy  ──→  Test 1 week
Phase 2 (Frontend)    ──→  Deploy  ──→  Test 1 week
Phase 3 (Cancel Fix)  ──→  Deploy  ──→  Test 1 week  
Phase 4 (Cleanup)     ──→  Deploy  ──→  Test 1 week
```

### 8.2 Feature Flags

```typescript
// src/lib/constants/featureFlags.ts
export const FEATURE_FLAGS = {
  // Enable app-state event handling (Phase 2+)
  USE_APP_STATE: true,
  
  // Use app-state as primary state source instead of show-overlay (Phase 2+)
  APP_STATE_PRIMARY: false, // flip to true in Phase 3
  
  // Remove legacy show-overlay/hide-overlay listeners (Phase 4)
  REMOVE_LEGACY_EVENTS: false,
};
```

```rust
// src-tauri/src/settings.rs
pub struct AppSettings {
    // ... existing settings ...
    
    /// Feature flag: emit app-state events alongside show-overlay
    /// Default: true (Phase 1+). Can be disabled for rollback.
    pub emit_app_state_events: bool,
}
```

### 8.3 Monitoring

Add logging at every state transition to track desync:

```rust
// In TranscriptionCoordinator loop
info!(
    "State transition: {:?} → {:?} (source: {})",
    old_state, new_state, source
);
```

```typescript
// In useAppState
useEffect(() => {
  console.log("[AppState]", appState);
}, [appState]);
```

### 8.4 Git Strategy

```
main
  └── refactor/state-consolidation-phase1  ← Phase 1 branch
        └── refactor/state-consolidation-phase2  ← Phase 2 branch
              └── refactor/fix-visualizer-frozen  ← Phase 3 branch
                    └── refactor/remove-legacy-state  ← Phase 4 branch
```

Each branch merges to `main` only after passing all tests for that phase.

---

## 9. Rollback Plan

### Phase 1 Rollback

**If `app-state` events cause issues:**  
- Remove `emit_app_state()` calls from `overlay.rs` and `utils.rs`
- Remove `AppState` enum (or mark `#[allow(dead_code)]`)
- No frontend changes were made, so no rollback needed there

**Risk:** Very low. The `app-state` event is additive — existing `show-overlay`/`hide-overlay` still work.

### Phase 2 Rollback

**If `useAppState` causes desync:**  
- Set `APP_STATE_PRIMARY = false` in feature flags
- Frontend falls back to `useOverlayState` for all state decisions
- `useAppState` continues listening but is not used for rendering

**Risk:** Medium. If the `app-state` events arrive out of order, the feature flag allows instant fallback.

### Phase 3 Rollback

**If cancel channel causes missed cancels:**  
- Revert to direct `cancel_current_operation()` path
- Remove `CancelSignal` from app state
- Keep Phase 1 and 2 changes (they're independent)

**Risk:** High. The cancel path is critical. Thorough testing required.

### Phase 4 Rollback

**If removing legacy events breaks overlay:**  
- Re-add `show-overlay` and `hide-overlay` event emissions
- Re-add `useOverlayState` event listeners
- Feature flag `REMOVE_LEGACY_EVENTS = false` restores old behavior

**Risk:** Medium. Removing working code is always risky, but the feature flag provides a safety net.

---

## 10. Testing Checklist

### Phase 1: Backend State Consolidation

- [ ] `AppState` enum serializes correctly via serde
- [ ] `app-state` event emitted on every coordinator state transition
- [ ] `app-state` event emitted on USB cycling transitions
- [ ] `app-state` event emitted on router confirmation transitions
- [ ] `app-state: Idle` emitted on cancel
- [ ] `get_state()` returns correct state from any thread
- [ ] Backward compatibility: `show-overlay` and `hide-overlay` still work
- [ ] `cargo test` passes
- [ ] `cargo clippy` passes
- [ ] Manual test: full recording cycle (record → transcribe → paste)
- [ ] Manual test: cancel during recording
- [ ] Manual test: cancel during processing

### Phase 2: Frontend State Migration

- [ ] `useAppState` hook receives and parses `app-state` events
- [ ] `appStateToOverlayState()` mapping is correct for all states
- [ ] Dual-listening: both `useOverlayState` and `useAppState` work simultaneously
- [ ] Feature flag `USE_APP_STATE` toggles new behavior
- [ ] Visual state (recording/transcribing/processing indicators) correct
- [ ] USB cycling overlay shows and hides correctly
- [ ] Router confirmation countdown works with new state
- [ ] Cancel hides overlay and resets state
- [ ] No visual regressions compared to pre-migration behavior
- [ ] TypeScript compiles without errors
- [ ] ESLint passes

### Phase 3: Visualizer Frozen Bug Fix

- [ ] **Primary bug scenario**: Record → release → cancel during GPU transcription → overlay hides immediately, bars fade to zero
- [ ] Record → cancel → record again within 500ms → no freeze
- [ ] Record → cancel → record again after 2s → no freeze
- [ ] Cancel during USB cycling → overlay hides, state resets
- [ ] Cancel during router confirmation → overlay hides, state resets
- [ ] `cancel_current_operation()` completes in < 50ms (non-blocking)
- [ ] Cancel signal broadcast is received by coordinator within 100ms
- [ ] Visualizer bars clear to zero on cancel (no frozen bars)
- [ ] Stress test: 10 rapid cancel-restart cycles → no freeze, no crash
- [ ] No regression in normal recording/transcription flow
- [ ] Performance: no increased latency in state transitions

### Phase 4: Remove Legacy State

- [ ] `OverlayState` enum removed from frontend
- [ ] `OverlayAction` type removed from frontend
- [ ] `parseOverlayPayload()` removed from frontend
- [ ] `show-overlay` event listener removed from `useOverlayState`
- [ ] `hide-overlay` event listener removed from `useOverlayState`
- [ ] `Stage` enum removed from `transcription_coordinator.rs`
- [ ] `set_stage()` helper removed
- [ ] `app-state` is sole state event
- [ ] Full recording cycle works end-to-end
- [ ] All overlay states render correctly
- [ ] Cancel at every stage works correctly
- [ ] Router mode: full flow works
- [ ] USB cycling: full flow works
- [ ] TypeScript compiles without errors
- [ ] `cargo test` passes
- [ ] `cargo clippy` passes
- [ ] ESLint passes

### Cross-Phase Integration Tests

- [ ] Record 10-second clip → transcribe → paste → correct text
- [ ] Record → cancel → record again → transcribe → paste → correct text
- [ ] Router mode: record → confirm → auto-send → result display
- [ ] Router mode: record → edit → send → result display
- [ ] USB cycling: disconnect mic → reconnect → resume recording
- [ ] Push-to-talk: hold hotkey → release → transcribe → paste
- [ ] Toggle mode: press hotkey → record → press again → transcribe → paste
- [ ] Multiple rapid hotkey presses (debounce test)
- [ ] Settings change during recording (should not crash)
- [ ] App restart while recording (should recover cleanly)
- [ ] Model switch during idle (should work next recording)
- [ ] Live captions during recording → cancel → verify captions stop

---

## Appendix A: Key Files Reference

| File | Current Role | Phase 1 Changes | Phase 2 Changes | Phase 3 Changes | Phase 4 Changes |
|------|-------------|-----------------|-----------------|-----------------|-----------------|
| `src-tauri/src/transcription_coordinator.rs` | Stage state machine | Add `AppState`, `get_state()`, emit | No change | Add `CancelSignal` listener | Remove `Stage` enum |
| `src-tauri/src/managers/audio.rs` | Audio recording state | No change | No change | No change | Minor: use `AppState` instead of `RecordingState` |
| `src-tauri/src/utils.rs` | Cancel logic | Add `emit_app_state` call | No change | Add cancel channel, async cancel | Remove `force_hide_recording_overlay` call |
| `src-tauri/src/overlay.rs` | Overlay show/hide | Add `app-state` emit alongside `show-overlay` | No change | No change | Remove `show-overlay` emit, keep `app-state` only |
| `src-tauri/src/lib.rs` | App setup | Register `AppState` managed state | No change | Register `CancelSignal` | No change |
| `src/overlay/hooks/useOverlayState.ts` | Frontend state machine | No change | Add `useAppState` import, dual-listen | Add cancel handling | Remove legacy event listeners |
| `src/overlay/hooks/useAppState.ts` | (NEW) | Create file | Implement hook | Add cancel state reset | Primary state source |
| `src/overlay/hooks/useVisualizer.ts` | Audio bars | No change | Use `useAppState.isRecording` | Add clear-on-cancel | Remove `OverlayState` dependency |
| `src/overlay/hooks/useUSBRecovery.ts` | USB cycling | No change | Use `useAppState.isUsbCycling` | No change | Remove `OverlayState` dependency |
| `src/overlay/hooks/useRouterPreview.ts` | Router confirm | No change | Use `useAppState.isConfirming` | No change | Remove `OverlayState` dependency |
| `src/overlay/hooks/useLiveCaptions.ts` | Live captions | No change | Use `useAppState.isRecording` | No change | Remove `OverlayState` dependency |
| `src/overlay/RecordingOverlay.tsx` | Main component | No change | Import `useAppState` | Use backend state for cancel | Remove `useOverlayState` dependency |

## Appendix B: State Transition Diagram

```
                          ┌─────────────────────────────────────────┐
                          │          Backend AppState               │
                          └─────────────┬───────────────────────────┘
                                        │
                    ┌───────────────────┼───────────────────────┐
                    │                   │                       │
              ┌─────▼─────┐      ┌──────▼──────┐        ┌───────▼──────┐
              │   Idle     │◄─────│  Recording  │        │  UsbCycling  │
              │            │      │  {binding}  │        │  {stage}     │
              └─────┬──────┘      └──────┬──────┘        └───────┬──────┘
                    │                   │                        │
                    │   hotkey press    │ hotkey release         │ recovered
                    │                   ▼                        │
                    │           ┌───────────────┐               │
                    │           │  Processing    │               │
                    │           │               │               │
                    │           └───────┬───────┘               │
                    │                   │                       │
                    │    transcription  │    cancel             │ cancel
                    │    complete       │                       │
                    │                   ▼                       ▼
                    ◄───────────────────┘◄──────────────────────┘
                    │
                    │  transcription-preview event
                    │
              ┌─────▼─────┐
              │ Confirming │
              │  {text}    │
              └─────┬──────┘
                    │
                    │  auto-send or manual confirm
                    │
                    ▼
              ┌──────────┐
              │ Processing│
              └─────┬──────┘
                    │
                    │  complete / timeout
                    │
                    ▼
              ┌──────────┐
              │   Idle    │
              └──────────┘
```

## Appendix C: Event Flow After Refactoring

```
                    ┌─────────────────────────────┐
                    │   TranscriptionCoordinator   │
                    │   (owns AppState, single truth) │
                    └──────────────┬───────────────┘
                                   │
                    ┌──────────────▼───────────────┐
                    │   State change → emit app-state│
                    │   (single event, all info)     │
                    └──────────────┬───────────────┘
                                   │
              ┌────────────────────┼────────────────────┐
              │                    │                    │
        ┌─────▼─────┐       ┌──────▼──────┐     ┌──────▼──────┐
        │ Overlay    │       │ Settings    │     │ CLI         │
        │ Window     │       │ Window      │     │ --is-active │
        │ (useApp    │       │ (show      │     │  -use flag  │
        │  State)    │       │  recording  │     └─────────────┘
        └────────────┘       │  indicator) │
                             └─────────────┘
```