# Handy-Mac Stop-Recording Freeze Bug - Deep Investigation Report

**Date:** 2026-07-09  
**Investigator:** AI Analysis  
**Scope:** Backend stop-recording flow investigation

## Executive Summary

The user's symptom is **NOT** "visualizer closes wrongly" — it's "press stop → visualizer FREEZES, recording does NOT stop." This is a critical distinction. The visualizer staying visible while recording fails to stop indicates the stop command is not reaching or completing in the audio recording pipeline.

After deep analysis of the current codebase, I've identified several areas where the stop-recording flow could hang, block, or fail silently.

---

## 1. Current Stop-Recording Flow (From Button/Hotkey to Recording Stop)

### Path A: Global Shortcut Stop (Hotkey Release)

```
src-tauri/src/shortcut/handler.rs:37-44
    ↓ (if is_transcribe_binding)
src-tauri/src/transcription_coordinator.rs:518-536 (send_input)
    ↓ (coordinator thread processes Command::Input)
src-tauri/src/transcription_coordinator.rs:273-348 (coordinator Input handler)
    ↓ (if !is_pressed && matches Stage::Recording)
src-tauri/src/transcription_coordinator.rs:304-312 (stop() function)
    ↓
src-tauri/src/actions.rs:598-1019 (TranscribeAction::stop)
    ↓
src-tauri/src/managers/audio.rs:1365-1510 (AudioRecordingManager::stop_recording)
```

### Path B: Frontend Stop Button Click

The frontend stop button likely invokes a Tauri command. Based on the commands available:

```
Frontend: Calls invoke("cancel_operation") or similar
    ↓
src-tauri/src/commands/mod.rs:15-18 (cancel_operation command)
    ↓
src-tauri/src/utils.rs:21-86 (cancel_current_operation)
    ↓
src-tauri/src/managers/audio.rs:1531-1561 (AudioRecordingManager::cancel_recording)
```

### Critical Path Analysis

**Key observation:** There are TWO different stop paths:
1. **Normal stop** (actions.rs:598) - Goes through coordinator, changes stage to Processing, spawns async task
2. **Cancel** (utils.rs:21) - Directly calls `cancel_recording()`, skips coordinator stage change

---

## 2. Verification of Prior Suspected Causes

### Prior Item 1: Frontend hide-overlay handler had NO guard for active transcription
**STATUS: FIXED** ✅

**Evidence:** 
- `src-tauri/src/overlay.rs:851-900` (`hide_recording_overlay`)
- Lines 889-898: Has dual guards - session ID check AND `is_active_use()` check in the closure
- Lines 869-887: Captures session at call time and compares inside closure
- Frontend also has guards in RecordingOverlay.tsx (per learning-log.md entries 2026-06-15, 2026-06-17)

### Prior Item 2: Router result 5s timeout hid overlay regardless of active state
**STATUS: FIXED** ✅

**Evidence:**
- `src-tauri/src/actions.rs:1621-1633` (Router success case)
- Lines 1621-1623: Checks `is_active_use()` before hiding
- Lines 1682-1694: Same check in failure case
- Learning-log.md (2026-06-15): Frontend now has state check in timeout handler

### Prior Item 3: Backend error paths lacked is_active_use() guard before hide_recording_overlay
**STATUS: PARTIALLY FIXED** ⚠️

**Evidence:**

**Still problematic paths found:**

1. **actions.rs:1280** (TranscribeWithRouterAction::stop - empty samples)
   - Line 1286: `return;` without hiding overlay or changing tray icon when samples empty
   - Missing cleanup: No `hide_recording_overlay` or `change_tray_icon(Idle)` 
   - **Session is marked failed but UI stays in router transcribing state**

2. **actions.rs:1275** - If `stop_recording` returns `None`, there's no handling
   - The match at line 1276 only handles `Some(samples)` - `None` case falls through
   - This means if `stop_recording` returns None (e.g., wrong binding_id or not recording), nothing happens

3. **actions.rs:1003-1012** (TranscribeAction::stop - no samples)
   - Lines 1003-1012: Does call `hide_recording_overlay` and `change_tray_icon(Idle)`
   - **This is correct** ✅

### Prior Item 4: Streaming transcription race
**STATUS: FIXED** ✅

**Evidence:**
- `src-tauri/src/actions.rs:608-613` (TranscribeAction::stop)
- Lines 608-610: Uses `Arc<AtomicBool>` for cancellation, avoiding TM lock
- Comments at lines 603-606 explain the fix
- `src-tauri/src/managers/audio.rs:228-242` (streaming callback)
- Lines 229-231, 240-242: Checks cancel flag before and after transcription without holding TM lock

### Prior Item 5: cancel_current_operation() missing cancel_streaming() call
**STATUS: FIXED** ✅

**Evidence:**
- `src-tauri/src/utils.rs:32-41` (cancel_current_operation)
- Lines 32-41: Explicitly cancels streaming via `Arc<AtomicBool>` at the START of the function
- Comment at lines 27-30 explains why this is first

### Prior Item 6: Lock contention - is_streaming_cancelled() required Mutex lock
**STATUS: FIXED** ✅

**Evidence:**
- `src-tauri/src/managers/audio.rs:207-218` (streaming callback setup)
- Line 210: Gets `streaming_cancel_flag()` which returns `Arc<AtomicBool>`
- Lines 223-231: Uses atomic `load(Ordering::Acquire)` - no mutex needed
- `src-tauri/src/managers/transcription.rs` must have `streaming_cancel_flag()` returning `Arc<AtomicBool>`

### Prior Item 7: State not resetting between recordings
**STATUS: FIXED** ✅

**Evidence:**
- `src-tauri/src/actions.rs:480-486` (TranscribeAction::start)
- Lines 480-486: Clears streaming cancel flag at start of new recording
- `src-tauri/src/actions.rs:1106-1112` (TranscribeWithRouterAction::start)
- Lines 1106-1112: Same clearing in router start

---

## 3. NEW Hypotheses - Potential Causes of Freeze

### Hypothesis A: The "Async Task Never Spawns" Bug

**Location:** `src-tauri/src/actions.rs:658-1019` (TranscribeAction::stop)

**The Issue:** 
The entire stop logic is wrapped in `tauri::async_runtime::spawn(async move { ... })` at line 658. If this spawn fails (e.g., runtime shutting down, thread pool exhausted), the code silently does nothing:

```rust
// Lines 646-658
change_tray_icon(app, TrayIconState::Transcribing);
show_transcribing_overlay(app);

// ... setup ...

tauri::async_runtime::spawn(async move {  // <-- If this fails...
    let _guard = FinishGuard(ah.clone());
    // ... rest of stop logic never runs ...
});

// Line 1017 - function returns immediately, no confirmation spawn succeeded
debug!("TranscribeAction::stop completed");
```

**Why this causes freeze:**
- `show_transcribing_overlay` was called at line 647
- `change_tray_icon(Transcribing)` was called at line 646
- If the async task never runs:
  - `stop_recording` is never called
  - Audio keeps recording
  - Overlay stays in "transcribing" state
  - User sees frozen visualizer

**Severity: HIGH** - No error handling if spawn fails

### Hypothesis B: The "Wrong Binding ID" Silent Failure

**Location:** `src-tauri/src/managers/audio.rs:1368-1509`

**The Issue:**
`stop_recording` returns `None` if the binding_id doesn't match:

```rust
// Lines 1368-1372
RecordingState::Recording {
    binding_id: ref active,
    start_time,
} if active == binding_id => { ... }
_ => None,  // <-- Silent return if binding_id doesn't match
```

In `actions.rs:666`:
```rust
let samples = rm.stop_recording(&binding_id);
if let Some(samples) = samples {  // <-- Only handles Some case
    // ... transcription logic ...
} else {
    debug!("No samples retrieved from recording stop");  // <-- Just logs
    // Lines 1003-1012: DOES handle UI cleanup
}
```

Actually this is handled in TranscribeAction (lines 1003-1012). But check TranscribeWithRouterAction:

```rust
// actions.rs:1275-1286
let samples = rm.stop_recording(&binding_id);
if let Some(samples) = samples {
    // ...
    if samples.is_empty() {
        debug!("Recording produced no audio samples; skipping");
        // ... session tracking ...
        return;  // <-- NO UI CLEANUP! Overlay stays, tray stays
    }
} else {
    // No else block! Falls through to end of function
    // But there's no samples, so no transcription happens
    // Yet UI was set to transcribing at lines 1261-1262
}
```

**In TranscribeWithRouterAction::stop (lines 1206-1888):**
- Line 1262: `show_transcribing_overlay_with_mode(app, OverlayMode::Router);`
- Line 1261: `change_tray_icon(app, TrayIconState::Transcribing);`
- If `stop_recording` returns `None` or empty samples, the function may not properly clean up
- Line 1276-1286: Only handles `Some(samples)`, and if empty, just returns
- **No hide_recording_overlay called in the None case or empty samples case!**

**Severity: HIGH** - Missing cleanup in router path

### Hypothesis C: The "Smart Stop Hang" Bug

**Location:** `src-tauri/src/managers/audio.rs:1388-1404`

**The Issue:**
When `extra_recording_buffer_ms > 0`, `smart_stop` is called:

```rust
// Lines 1388-1404
let samples = if settings.extra_recording_buffer_ms > 0 {
    debug!("Smart-stop: starting volume-aware buffer...");
    if let Some(rec) = self.recorder.lock().as_ref() {
        match rec.smart_stop(settings.extra_recording_buffer_ms) {
            Ok(buf) => buf,
            Err(e) => {
                error!("smart_stop() failed: {e}");
                Vec::new()
            }
        }
    } else {
        error!("Recorder not available for smart_stop");
        Vec::new()
    }
}
```

**Potential deadlock:**
- Line 1393: `self.recorder.lock()` - acquires mutex
- `smart_stop` may block for up to `extra_recording_buffer_ms` milliseconds
- If `extra_recording_buffer_ms` is set to a very large value (e.g., 30000 for 30 seconds), this blocks the async task
- The user sees frozen visualizer during this time
- **This is NOT a bug per se** (it's the intended behavior of smart_stop), but could explain "freeze" if the setting is too high

**Severity: MEDIUM** - Depends on user settings

### Hypothesis D: The "Coordinator Stage Mismatch" Bug

**Location:** `src-tauri/src/transcription_coordinator.rs:273-348`

**The Issue:**
The coordinator only transitions to stop if the stage matches:

```rust
// Lines 301-313 (push-to-talk mode)
} else if !is_pressed
    && matches!(&stage, Stage::Recording(id) if id == &binding_id)
{
    // ... stop ...
}

// Lines 330-343 (toggle mode)
Stage::Recording(id) if id == &binding_id => {
    // ... stop ...
}
```

**If the stage is NOT `Stage::Recording(binding_id)`:**
- The stop command is silently ignored
- `action.stop()` is never called
- Audio continues recording
- Visualizer stays frozen

**How can stage mismatch happen?**
1. Panic recovery reset stage to Idle, but AudioRecordingManager is still recording
2. Previous transcription's `FinishGuard` fired and reset stage to Idle
3. `notify_processing_finished` was called erroneously

**Evidence of recovery attempt:**
- Lines 420-449: Panic recovery in coordinator DOES try to reset audio recorder
- Lines 431-435: "Reset audio recorder if it's stuck in Recording state"
- But if panic recovery itself fails or isn't triggered, stage mismatch persists

**Severity: MEDIUM** - Requires specific timing or panic conditions

### Hypothesis E: The "HandyKeys Event Lost" Bug

**Location:** `src-tauri/src/shortcut/handy_keys.rs:110-185`

**The Issue:**
When using HandyKeys implementation (not Tauri), shortcuts are handled in a dedicated thread:

```rust
// Lines 128-138 (manager_thread)
while let Some(event) = manager.try_recv() {
    if let Some((binding_id, hotkey_string)) = hotkey_to_binding.get(&event.id) {
        let is_pressed = event.state == HotkeyState::Pressed;
        handle_shortcut_event(&app, binding_id, hotkey_string, is_pressed);
    }
}
```

**Race condition:**
- Lines 451-476: `register_cancel_shortcut` is synchronous
- But lines 157-179: Shortcut registration is via channel to manager thread
- If the manager thread is busy (e.g., processing another event), the stop event could be delayed
- The `try_recv` at line 129 is non-blocking, but events queue up

**More importantly:**
- Lines 462-476: `register_cancel_shortcut` does check if HandyKeysState exists
- If HandyKeysState was not initialized (fallback to Tauri), but user somehow triggers HandyKeys path, event is lost

**Severity: LOW** - Requires specific configuration (HandyKeys mode)

### Hypothesis F: The "TranscriptionManager Lock Timeout" Bug

**Location:** `src-tauri/src/actions.rs:695-708`

**The Issue:**
There's a 10-second timeout on the TM lock:

```rust
// Lines 695-708
let transcription_result = match tm.try_lock_for(Duration::from_secs(10)) {
    Some(guard) => {
        guard.clear_streaming_cancel();
        guard.transcribe(samples)
    }
    None => {
        warn!("Timed out waiting for TranscriptionManager lock after 10s");
        Err(AppError::TranscriptionBusy)
    }
};
```

**But the bug is BEFORE this:**
At lines 619-642, the code does:
```rust
let Some(rm) = app.try_state::<Arc<AudioRecordingManager>>() else {
    warn!("...");
    utils::hide_recording_overlay(app);
    change_tray_icon(app, TrayIconState::Idle);
    return;  // <-- Early return with cleanup
};
```

This part is correct. But what if the early returns at lines 621-623, 628-630, 634-637 happen AFTER `show_transcribing_overlay` was called at line 647?

Looking more carefully:
```rust
// Line 647: Called BEFORE the try_state checks
show_transcribing_overlay(app);

// Lines 619-642: try_state checks happen AFTER overlay shown
```

**Wait, let me re-read the order...**

```rust
// Lines 598-647: TranscribeAction::stop
fn stop(&self, app: &AppHandle, binding_id: &str, _shortcut_str: &str) {
    shortcut::unregister_cancel_shortcut(app);  // Line 600
    
    // Lines 608-613: Cancel streaming (OK)
    
    let stop_time = Instant::now();  // Line 615
    debug!(...);  // Line 616
    
    let ah = app.clone();  // Line 618
    let Some(rm) = app.try_state::<Arc<AudioRecordingManager>>() else {  // Lines 619-624
        // Error handling with cleanup
    };
    // ... more try_state checks ...
    
    change_tray_icon(app, TrayIconState::Transcribing);  // Line 646
    show_transcribing_overlay(app);  // Line 647
    
    // Lines 649-653: Unmute and play sound
    
    tauri::async_runtime::spawn(async move {  // Line 658
        // ... actual stop_recording call happens here at line 666 ...
    });
}
```

**The order is correct** - try_state checks happen BEFORE overlay is shown.

But wait - there's still an issue. Look at lines 619-624:
```rust
let Some(rm) = app.try_state::<Arc<AudioRecordingManager>>() else {
    warn!("AudioRecordingManager not available, cannot stop recording");
    utils::hide_recording_overlay(app);  // <-- But overlay wasn't shown yet!
    change_tray_icon(app, TrayIconState::Idle);
    return;
};
```

This is actually fine - hiding an already-hidden overlay is idempotent (or should be).

**Severity: LOW** - Not a direct cause, but confusing code

### Hypothesis G: The "FinishGuard Double Notify" Bug

**Location:** `src-tauri/src/actions.rs:80-90`

**The Issue:**
`FinishGuard` is supposed to notify when transcription completes:

```rust
// Lines 80-90
struct FinishGuard(AppHandle);
impl Drop for FinishGuard {
    fn drop(&mut self) {
        if let Some(c) = self.0.try_state::<TranscriptionCoordinator>() {
            c.notify_processing_finished();  // <-- Sends ProcessingFinished command
        }
    }
}
```

**Double notification risk:**
- Line 659: `_guard` created at start of async block
- If the async block completes normally, guard drops and notifies
- But what if there's a panic? 
- Lines 420-449 in transcription_coordinator.rs show panic recovery
- If panic happens AFTER stage is set to Processing but BEFORE transcription finishes, the guard drops (in catch_unwind) and notifies
- Then on recovery, stage is reset to Idle
- But what if the async task is restarted somehow?

Actually, looking at the code flow, the real issue is:
- `FinishGuard` ONLY notifies `notify_processing_finished()` 
- It does NOT handle cleanup of overlay or tray icon if the async task panics
- The panic recovery in coordinator (lines 426-439) handles this, but only for the coordinator thread's panic catching
- If the async task panics, it's caught by Tauri's async runtime, not by the coordinator's catch_unwind

**Severity: MEDIUM** - Depends on panic behavior in async runtime

---

## 4. Top Suspects Ranked

### 🥇 SUSPECT #1: TranscribeWithRouterAction Missing Cleanup (HIGH PRIORITY)

**Location:** `src-tauri/src/actions.rs:1206-1888`

**Evidence:**
- Lines 1261-1262: Sets tray to Transcribing and shows overlay
- Lines 1275-1286: If `stop_recording` returns `None` OR empty samples, cleanup is missing
- Line 1286: `return;` with no `hide_recording_overlay` or `change_tray_icon(Idle)`
- **The router path leaves the UI stuck in "transcribing" state if recording fails**

**Reproduction scenario:**
1. User starts router transcription
2. For some reason, recording stops with no samples (mic unplugged, permission denied mid-recording, etc.)
3. `stop_recording` returns `None` or empty samples
4. Function returns without hiding overlay
5. Visualizer frozen on "transcribing" forever

**Fix needed:**
Add proper cleanup in the `else` branch (None case) and after the empty samples check.

### 🥈 SUSPECT #2: Async Task Spawn Failure (HIGH PRIORITY)

**Location:** `src-tauri/src/actions.rs:658`

**Evidence:**
- Line 658: `tauri::async_runtime::spawn(async move { ... })`
- Return value is ignored (assigned to `_guard` which is just a drop guard)
- If spawn fails (runtime shutting down, pool exhausted), no error handling
- Line 646-647: UI already changed to transcribing state BEFORE spawn
- If spawn fails, no code runs to stop recording or reset UI

**Reproduction scenario:**
1. System under heavy load or Tauri runtime in bad state
2. User presses stop
3. UI changes to transcribing
4. spawn() fails silently
5. Recording continues, UI frozen

**Fix needed:**
Check spawn result and handle failure:
```rust
match tauri::async_runtime::spawn(async move { ... }) {
    Ok(handle) => { /* guard created, all good */ },
    Err(e) => {
        error!("Failed to spawn transcription task: {}", e);
        hide_recording_overlay(&ah);
        change_tray_icon(&ah, TrayIconState::Idle);
        // Also need to cancel recording
    }
}
```

### 🥉 SUSPECT #3: Smart Stop Blocking (MEDIUM PRIORITY)

**Location:** `src-tauri/src/managers/audio.rs:1388-1404`

**Evidence:**
- `smart_stop` blocks for up to `extra_recording_buffer_ms`
- If user sets this to a very high value (30 seconds), the UI appears frozen
- No cancellation mechanism during smart_stop
- The streaming cancel flag doesn't affect the recording stop

**Reproduction scenario:**
1. User sets "Extra recording buffer" to 30 seconds
2. User presses stop
3. smart_stop waits up to 30 seconds for silence
4. UI appears frozen during this time
5. Eventually completes, but user thinks it's broken

**Fix needed:**
- Add cancellation check to smart_stop
- Or add warning in UI about high buffer values

### 🏅 SUSPECT #4: Coordinator Stage Desync (MEDIUM PRIORITY)

**Location:** `src-tauri/src/transcription_coordinator.rs:273-348`

**Evidence:**
- Lines 301-313: Stop only executes if stage is Recording with matching binding_id
- If stage is Idle (due to panic recovery or race), stop is silently ignored
- AudioRecordingManager continues recording independently

**Reproduction scenario:**
1. Previous transcription panicked and was recovered
2. Coordinator reset to Idle
3. AudioRecordingManager still thinks it's recording (race)
4. User presses stop
5. Coordinator ignores command (stage is Idle, not Recording)
6. Recording continues forever

**Fix needed:**
In `transcription_coordinator.rs:589-611` (stop function), add a check:
```rust
fn stop(...) {
    // Current: just calls action.stop()
    // Should also verify recording actually stops
    action.stop(app, binding_id, hotkey_string);
    
    // Add: Check if recording is still active after a delay
    // If so, force cancel
}
```

---

## 5. Recommendations

### Immediate Actions (Fix Suspects #1 and #2)

1. **Fix TranscribeWithRouterAction cleanup** (actions.rs:1275-1286)
   - Add `hide_recording_overlay(&ah)` and `change_tray_icon(&ah, TrayIconState::Idle)` before `return` on line 1286
   - Also add cleanup in the `else` case at line 1276 (when `stop_recording` returns `None`)

2. **Add spawn error handling** (actions.rs:658)
   - Check the result of `tauri::async_runtime::spawn`
   - If spawn fails, immediately clean up UI and cancel recording

3. **Add defensive stop check** (transcription_coordinator.rs:589-611)
   - After calling `action.stop()`, verify recording actually stopped
   - If still recording after a short delay, force cancel

### Investigation Actions

1. **Add detailed logging** around the stop path:
   - Log when `stop_recording` is called
   - Log return value (Some/None, samples count)
   - Log when async task starts and ends
   - Log coordinator stage transitions

2. **Add metrics** for:
   - Time from stop hotkey to recording actually stopping
   - Count of "stop ignored due to stage mismatch"
   - Count of "spawn failed"

3. **Create recovery command**:
   - Add a "force reset" command that can be called from frontend
   - Resets coordinator state, cancels recording, hides overlay
   - Can be triggered by user if they detect frozen state

---

## Appendix: Code References

### Critical Lines in Current Code

**actions.rs:**
- Line 598: `TranscribeAction::stop` starts
- Line 647: `show_transcribing_overlay` called
- Line 658: Async task spawn (potential failure point)
- Line 666: `stop_recording` called inside async task
- Line 1003: Empty samples handling
- Line 1206: `TranscribeWithRouterAction::stop` starts
- Line 1262: `show_transcribing_overlay_with_mode` called (router)
- Line 1275: `stop_recording` for router
- Line 1286: `return` without cleanup in router path

**audio.rs:**
- Line 1365: `stop_recording` starts
- Line 1368: Matching on RecordingState
- Line 1508: Returns `None` if not recording or wrong binding_id
- Line 1531: `cancel_recording` function

**transcription_coordinator.rs:**
- Line 301: Push-to-talk stop condition
- Line 330: Toggle mode stop condition
- Line 420: Panic recovery block
- Line 589: `stop` function (coordinator)

**utils.rs:**
- Line 21: `cancel_current_operation`
- Line 48-61: Recording cancellation

---

**End of Report**

*Generated: 2026-07-09*
*Codebase: Handy-Fork/Handy-Mac, branch main*
