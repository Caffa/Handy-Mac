# Handy-Fork Learning Log

## USB Watchdog Power-Cycle Bug (2026-04-28)

### Problem

- When dictation is active and the USB microphone dies, Handy should power-cycle the USB hub via uhubctl and retry
- The transcription overlay visualizer would get stuck (frozen bars) when mic-level events stopped arriving
- Handy could crash because the power-cycle was async but the retry happened immediately (before the device re-enumerated)

### Root Causes

1. **`power_cycle()` was fire-and-forget (spawning a thread)**: The `cycling` flag was set to `true`, a thread was spawned to run uhubctl + 12s settle, but then `cycling` was **immediately set back to `false`** on the same line (line 178 original). This meant:
   - The caller (`start_microphone_stream` → `on_mic_open_failed`) returned `true` saying "cycle initiated" but the cycle hadn't happened yet
   - The retry happened immediately while the device was still offline
   - No cooldown protection against double-cycling

2. **`force_power_cycle()` had the same bug**: Cycling flag cleared immediately after `thread::spawn`, before the actual cycle completed

3. **Overlay visualizer had no level decay**: When mic-level events stopped (dead USB stream), the bars just froze at their last values with no fallback

4. **No frontend feedback during USB cycling**: User had no idea the device was being power-cycled

### Fixes

1. **Made `power_cycle()` → `power_cycle_blocking()`**: Runs uhubctl + settle synchronously on the calling thread. This ensures the mic-open retry actually finds the device re-enumerated. Called from `on_mic_open_failed()`.

2. **`force_power_cycle()` (UI-triggered)**: Uses `Arc<AtomicBool>` for the `cycling` flag so the spawned thread can properly clear it after completion. No raw pointers.

3. **Added `AppHandle` to `UsbWatchdog`**: Emits `usb-power-cycle-started`, `usb-power-cycle-finished`, and `usb-power-cycle-failed` Tauri events to the frontend.

4. **Overlay: level decay timer**: Added an 80ms interval that decays bar heights toward zero when no `mic-level` events arrive for 500ms (dead stream detection). Prevents frozen bars.

5. **Overlay: USB cycling state**: Added `"usb-cycling"` overlay state with gold pulsing text "USB cycling…" so the user knows what's happening.

6. **Settings UI**: Updated `UsbWatchdog.tsx` to listen for events instead of hardcoded timeouts.

7. **Added `is_cycling()` method**: Public API for checking if a cycle is in progress.

### Key Insight

When `std::thread::spawn` is used for async work, **never clear state flags immediately after the spawn call**. Either:

- Make the operation blocking (simple, correct)
- Use `Arc<AtomicX>` shared state that the spawned thread clears on completion

## CoreAudio Stream Teardown Crash (2026-05-19)

### Problem

- Handy crashed with `SIGABRT` / `nanov2_guard_corruption_detected` when pressing the hotkey to start transcription with always-on mic enabled
- The overlay briefly appeared before the entire app crashed (malloc heap corruption → abort)
- This occurred in both crash reports (13:11 and 16:01 on 2026-05-19)

### Root Cause

- The crash is a **heap memory corruption** in macOS's `nanov2` malloc allocator, triggered during CoreAudio stream teardown
- When the audio worker thread finishes, `stream.pause()` is called before `drop(stream)`, but `pause()` is **asynchronous** — it only _requests_ that CoreAudio stop calling the callback
- The `stop_flag` (AtomicBool) tells the callback to stop sending data, but there's a window where the IO thread is still executing the last callback invocation while `drop(stream)` tears down internal data structures
- In always-on mode, the stream runs continuously for much longer, increasing the probability that the callback is in-flight during teardown

### Fixes

1. **Added 100ms sleep after `stream.pause()` before `drop(stream)`** in `recorder.rs`: Gives the CoreAudio IO thread enough time (typical buffer period is 5–23ms) to fully return from the callback before the stream's internal buffers are deallocated
2. **Added global panic hook** in `lib.rs`: Captures panic info (message, location, thread name) and writes it to both the standard log file and the structured JSONL event log before the process terminates. This gives us much better crash diagnostics for future issues
3. **Added `AppCrashed` event type** to `logging.rs`: Structured event for panic capture

### Key Insight

On macOS, `cpal::Stream::pause()` is asynchronous — the CoreAudio callback may still be running for several milliseconds after `pause()` returns. Always add a sleep/yield after pausing an audio stream before dropping it, especially when the callback accesses shared data structures. The typical buffer period (5-23ms) means 100ms is a generous safety margin.

## Parakeet TDT Hallucination — Continuation Inference (2026-05-20)

### Problem

- Parakeet v2 (TDT model) sometimes generates extra words after the actual speech ends
- The output looks like the model took the user's words and then continued generating plausible-sounding text that wasn't spoken
- Example: user says "I went to the store" and gets "I went to the store and bought some milk"

### Root Cause

- The TDT (Token-and-Duration Transducer) decoder in `parakeet/mod.rs` is autoregressive — it updates decoder state after each emitted token, carrying forward context
- When the audio has trailing silence (or silence padding), the encoder still produces frames for that silence
- The decoder, having learned language model continuations from training data, generates plausible next tokens based on its accumulated state, even when there's no acoustic evidence
- The `decode_sequence` function uses pure greedy decoding (argmax) with NO confidence thresholding — even very uncertain predictions are emitted
- Trailing silence trimming (VAD) was only applied to Whisper models, not Parakeet, so Parakeet got all trailing silence frames
- Short-audio silence padding (3s minimum for Whisper) was also applied to Parakeet, extending the silence frames the hallucinating decoder could use

### Contributing Factors

1. `DEFAULT_LEADING_SILENCE_MS = 250` — prepended silence gives the model a "start" context
2. No trailing silence trimming for Parakeet (only Whisper gets VAD trim)
3. Short audio padded to 3s — these padding frames are all silence that the decoder hallucinates on
4. `MAX_TOKENS_PER_STEP = 10` — the decoder can emit up to 10 tokens per frame, amplifying continuations
5. No confidence thresholding in greedy decoder — no mechanism to suppress uncertain tokens

### Fixes (implemented 2026-05-20)

1. **VAD trim for all models** (transcription.rs): Changed `trim_trailing_silence` from Whisper-only to all models. Parakeet TDT's autoregressive decoder needs this even more than Whisper since it "free-runs" language model continuations into silence.

2. **Confidence-based blank suppression** (parakeet/mod.rs): Added softmax probability computation to `decode_sequence`. When the best token has probability < 0.5, force a blank instead — this suppresses low-confidence "hallucinated" continuations. After 5+ consecutive blanks (silence gap), threshold raises to 0.7, requiring even higher confidence to resume speech.

3. **Bigram repetition penalty** (parakeet/mod.rs): Track recent token bigrams in a sliding window. If the same bigram appears 3+ times, force a blank to break the repetition loop.

4. **Output length guard** (parakeet/mod.rs): Cap total emitted tokens at 10x the number of encoder frames. Normal speech has ~2-4 tokens/frame, so 10x is generous. Prevents runaway generation past audio end.

5. Fix 4 (skip silence padding for non-Whisper) was cancelled — VAD trim (Fix 1) already handles the trailing silence from padding, making this unnecessary.

### Key Insight

Autoregressive transducer models (like Parakeet TDT) with greedy decoding and no confidence thresholding will hallucinate continuations during silence because the decoder's language model naturally predicts "what comes next." The fix must combine VAD-based audio trimming (remove silence frames) with decoder-level confidence filtering (suppress uncertain tokens).

## macOS Paste-Focus-Stealing Bug (2026-05-22)

### Problem

- After 2+ rounds of transcription, the overlay shows appropriate status but nothing is pasted at the user's cursor
- "After a bit" it starts working again (once the user clicks back to their target app)
- The transcription text IS generated correctly — it just never reaches the target application

### Root Cause

- `tauri-nspanel`'s `Panel::show()` calls `orderFrontRegardless` on the NSPanel
- `orderFrontRegardless` **activates the Handy application**, bringing it to the foreground even though the panel has `can_become_key_window: false` and `noActivate: true`
- When `Cmd+V` is then sent (via enigo), the keystroke goes to the Handy app (which has no focused text field) instead of the user's original application
- The transcription text is written to the clipboard but the paste keystroke is consumed by Handy's event loop (or simply ignored since no text input is focused)
- On macOS 14+, `ActivateIgnoringOtherApps` is deprecated and has no effect (the system auto-activates the most recently used app), which is why "after a bit" it works — macOS's automatic focus management eventually restores the correct app

### Fixes (implemented 2026-05-22)

1. **New `focus.rs` module** (macOS-only): Provides `save_frontmost_app()` and `restore_frontmost_app()` using `NSWorkspace.sharedWorkspace().frontmostApplication` and `NSRunningApplication.activateWithOptions()`. Stores the bundleIdentifier + PID in `SavedFrontmostApp` managed state.

2. **`show_overlay_state()` in `overlay.rs`**: Calls `save_frontmost_app(app_handle)` **before** showing the overlay window, capturing which app should receive the paste.

3. **`paste()` in `clipboard.rs`**: Calls `restore_frontmost_app(&app_handle)` **before** acquiring the Enigo lock and sending keystrokes, re-activating the user's target application so Cmd+V goes to the right place.

4. **`SavedFrontmostApp` managed state** in `lib.rs`: Registered at startup so all modules can access it.

5. **`objc2-app-kit` + `objc2-foundation` dependencies** in `Cargo.toml`: Added with `NSWorkspace`, `NSRunningApplication`, `NSEnumerator` features for the safe API.

### Key Insight

On macOS, `NSPanel.orderFrontRegardless` activates the owning application regardless of panel configuration. Even with `canBecomeKeyWindow: false` and `noActivate: true`, the window server treats `orderFrontRegardless` as a request that activates the app. To prevent focus stealing, always save and restore the previously frontmost application before sending simulated keystrokes. The `NSRunningApplication.activateWithOptions(ActivateIgnoringOtherApps)` call restores the user's app.

## Paste Not Reaching Target App — Overlay Focus Stealing (2026-05-22)

### Problem

- After transcription completes, the overlay visualizer shows the correct status (recording → transcribing → hide), but nothing is pasted at the user's cursor
- The bug occurs after ~2 rounds of transcription, then "after a bit, it works again"
- The transcription text IS written to the clipboard successfully, but Cmd+V doesn't paste it into the user's target application

### Root Cause

- The recording overlay on macOS uses `tauri-nspanel`, which exposes a `show()` method that calls `[NSPanel orderFrontRegardless]`
- `orderFrontRegardless` explicitly brings the window to the front **regardless of the app's activation state** — it can **activate the Handy application**, stealing focus from the user's target application
- When the overlay is shown (during recording), Handy becomes the active app
- When the paste (Cmd+V) keystroke is sent via enigo, it goes to **the currently active application** — which is now Handy, not the user's text editor/terminal/browser
- Since Handy has no text input field focused, the paste goes nowhere and the text is "lost"
- "After a bit, it works again" because the user clicks back to their target application, restoring focus

### Why `orderFrontRegardless` is problematic

- `orderFrontRegardless` is the most aggressive of the NSPanel ordering methods
- `orderFront:` would show the panel without activating the app, but `orderFrontRegardless` forces it to the front even if the app isn't active
- The `no_activate(true)` flag in PanelBuilder sets `becomesKeyOnlyIfNeeded` on the NSPanel, but `orderFrontRegardless` overrides this behavior
- On macOS 14+, `ActivateIgnoringOtherApps` is deprecated and has no effect (the system decides), but the focus stealing from `orderFrontRegardless` still happens

### Fixes

1. **Added `focus.rs` module**: Tracks the frontmost application (bundle ID + PID) before the overlay is shown, and restores it before pasting. Uses `NSWorkspace.sharedWorkspace().frontmostApplication()` to save, and `NSRunningApplication.activateWithOptions()` to restore.

2. **`save_frontmost_app()` in `show_overlay_state()`**: Called before `overlay_window.show()` to capture the user's current application.

3. **`restore_frontmost_app()` in `paste()`**: Called before acquiring the Enigo lock / sending keystrokes, so Cmd+V goes to the restored (user's) application. Includes a 50ms sleep after activation to let the target app's run loop process the activation.

4. **Added `objc2-app-kit` and `objc2-foundation` dependencies** with `NSWorkspace`, `NSRunningApplication`, and `NSEnumerator` features for safe API access.

### Key Insight

On macOS, showing an NSPanel with `orderFrontRegardless` can activate the parent application, stealing focus from the user's target app. For any app that simulates keystrokes (like Cmd+V paste), you MUST restore the previous frontmost application before sending the keystroke, or the keystroke will go to the wrong app. The `NSWorkspace.frontmostApplication` / `NSRunningApplication.activateWithOptions` APIs provide a reliable way to save and restore application focus.

## Pre-Recording Buffer Slider Crash (2026-07-01)

### Problem

- Moving the pre-recording buffer slider in Handy settings causes the app to crash
- The slider calls `change_pre_recording_buffer_setting` which triggers a stop/recreate/start cycle of the audio recorder
- Crash type: `SIGABRT` (abort() called) — Rust panic in main thread

### Root Cause

The `recreate_recorder()` method in `src-tauri/src/managers/audio.rs` used `.expect("VAD path should be valid UTF-8")` on line 937, which would **panic** if the VAD model path contained non-UTF-8 characters. Since this was called while holding a `parking_lot::Mutex`, a panic here would abort the entire process.

### Crash Log Analysis

- **Exception**: `EXC_CRASH`, signal `SIGABRT` (abort trap 6)
- **ASI**: `abort() called`
- **Thread**: Main thread (triggered)
- **Location**: `src-tauri/src/managers/audio.rs` line 937 during `recreate_recorder()` call chain

### Fixes (implemented 2026-07-01)

1. **Replaced `.expect()` with `.ok_or_else()`** in `audio.rs`:

   ```rust
   // Before (line 937):
   vad_path.to_str().expect("VAD path should be valid UTF-8")

   // After:
   vad_path.to_str().ok_or_else(||
     anyhow::anyhow!("VAD path is not valid UTF-8: {:?}", vad_path)
   )?
   ```

2. **Moved `is_open` flag reset before recorder teardown** in `recreate_recorder()`:
   - Set `is_open.store(false, ...)` **before** taking the recorder lock
   - Prevents race where concurrent operations could use a recorder mid-teardown

3. **Added recovery path on recreation failure** in `shortcut/mod.rs`:
   - If `recreate_recorder()` fails for always-on/BT keep-alive modes, attempt to restart the microphone stream
   - Prevents leaving the app in a dead state with no mic stream

4. **Added trace logging** for debugging:
   - `info!("Closing old recorder before recreation")`
   - `info!("Applying pre-recording buffer change ({}ms): stopping stream, recreating recorder", ms)`

### Key Insight

When calling methods that acquire Mutexes during UI-triggered operations (like settings changes), **always use `.ok_or()` / `.map_err()` / `?` instead of `.expect()` / `.unwrap()`**. A panic under a Mutex will abort the entire process on most platforms because the Mutex is poisoned and cannot be safely recovered. The `?` operator propagates errors to the caller, allowing the frontend to display a toast notification instead of crashing the app.

**Files changed:**

- `src-tauri/src/managers/audio.rs` — Fixed `recreate_recorder()` panic point and is_open flag ordering
- `src-tauri/src/shortcut/mod.rs` — Added error recovery and logging to `change_pre_recording_buffer_setting`

## Git Caution (2026-05-23)

### Problem

- During a failed `sed` batch edit that broke the moonshine file, ran `git checkout -- src-tauri/src/ src-tauri/vendor/` to revert ALL Rust backend changes — accidentally wiping hours of work on the adaptive threshold decoder, suppressed token tracking, audio quality metrics, and settings changes
- Frontend changes survived because they were in `src/` (not `src-tauri/src/`)

### Rule

- `git checkout -- <path>` is **destructive** — permanently discards all uncommitted changes in the specified paths
- Never use `git checkout --` with broad paths like `src/` or `vendor/` unless you are certain you want to discard ALL changes
- Prefer `git checkout -- <specific-file>` for targeted reverts
- Alternatively, use `git stash push -m "description"` to save changes before experimenting
- **Always add small inline comments** at each modification stage to document intent and avoid confusion when revisiting code later
- Commit early and often — each logical checkpoint should get its own commit so you never lose more than one checkpoint's worth of work

## Router Filing Race Condition — Overlay Dismissal Bug (2026-06-15)

### Problem

- When Handy is routing and filing away a note, if the user starts a new transcription in the middle of this, the router's completion dismisses the visualizer while the second transcription is still active
- This has been reported multiple times by the user as a recurring bug

### Root Cause

The bug is a **race condition between the router thread and the frontend's overlay timeout**:

1. **Router action flow:**
   - Router spawns async block with `FinishGuard` (line 1011 in `actions.rs`)
   - Transcription happens, confirmation wait, then router thread spawned (line 1199)
   - Router thread is "fire-and-forget" — async block continues to end (line 1417)
   - **`FinishGuard` drops immediately when async block ends**
   - `FinishGuard::drop()` sends `ProcessingFinished` to coordinator
   - Coordinator transitions from `Processing` → `Idle`
   - User can now start new transcription

2. **Router thread execution:**
   - Runs subprocess (filing) independently
   - When done, emits `router-result` event to frontend (line 1254)
   - Frontend receives result and shows it for 5 seconds
   - After 5 seconds, frontend's `useEffect` timeout calls `setIsVisible(false)` (lines 392-401 in `RecordingOverlay.tsx`)
   - **This timeout hides the overlay without checking if a new recording is active**

3. **The race:**
   - Router thread finishes → `router-result` event → frontend shows result
   - 5-second timeout starts
   - User starts new transcription during those 5 seconds
   - Frontend state becomes "recording" for new transcription
   - 5-second timeout fires → frontend calls `setIsVisible(false)`
   - **Overlay disappears even though new recording is active**

### Why the Backend Check Doesn't Help

The backend's `is_active_use()` check (lines 1268-1276) correctly prevents hiding from the router thread when a new recording is active. However, this check happens at the moment the router thread finishes, which is **before** the frontend's 5-second timeout. The real bug is in the frontend's timeout handler.

### Fixes

1. **Frontend: Check overlay state before hiding after router result timeout** (RecordingOverlay.tsx lines 391-401):

   ```typescript
   // Handle router result timeout
   useEffect(() => {
     if (routerResult) {
       const timeout = setTimeout(() => {
         setRouterResult(null);
         // BUGFIX: Don't hide overlay if a new recording started during the 5-second display
         // This prevents race condition where router result timeout fires while user is
         // actively recording a second transcription
         setState((current) => {
           if (
             current === "recording" ||
             current === "transcribing" ||
             current === "processing" ||
             current === "confirming"
           ) {
             // New transcription is active — keep overlay visible
             return current;
           }
           // No active transcription — safe to hide
           setIsVisible(false);
           setTranscriptionPreview("");
           setCountdown(0);
           return current;
         });
       }, 5000);
       return () => clearTimeout(timeout);
     }
   }, [routerResult]);
   ```

2. **Add comment documenting the race condition** in `RecordingOverlay.tsx` near the router result timeout handler explaining why the state check is necessary.

3. **Backend: Add `is_active_use()` guard documentation** in `actions.rs` lines 1264-1276 explaining the guard and noting that the frontend timeout also needs this protection.

### Key Insight

When frontend state transitions depend on timeouts, **always check current state before executing the timeout's action**. Timers fire asynchronously and the component's state may have changed since the timer was set. The closure captures stale state by default — use a state updater function or ref to check current state at timeout-fire time. For overlays, this means checking if recording/transcription is still active before hiding.

## Router Filing Race Condition — Hide-Overlay Event Race (2026-06-17)

### Problem

- When Handy is routing and filing a note, if the user starts a second routing transcription in the middle of this, the visualizer disappears when the first routing finishes filing
- The first router's completion calls `hide_recording_overlay()` which emits a `hide-overlay` event
- If the user has already started a new recording, this event incorrectly hides the overlay mid-recording

### Root Cause

The bug is a **race condition between the hide-overlay event emission and the frontend's event handler**:

1. **Timeline:**
   - **t=0ms**: Router #1 finishes filing → backend checks `is_active_use()` → returns `false` (no #2 yet)
   - **t=0ms**: Backend calls `hide_recording_overlay()` → emits `hide-overlay` event immediately
   - **t=50ms**: User presses hotkey for routing #2
   - **t=50ms**: Backend starts recording #2 → emits `show-overlay` event with state "recording"
   - **t=100ms**: Frontend receives `hide-overlay` → **unconditionally hides overlay** (no state check!)
   - **t=100ms**: Frontend receives `show-overlay` → sets state to "recording" and `isVisible(true)`
   - **Result**: Race between hide and show events - if hide is processed after show, overlay disappears

2. **The `hide-overlay` event handler in `RecordingOverlay.tsx` (lines 549-561 original) only checked for `"usb-cycling"` state**, not for active recording/transcription states

3. **The backend's `is_active_use()` check is insufficient** because:
   - It checks state at the moment the router thread finishes
   - The user might start a new recording **after** the check but **before** the event is processed
   - Event emission is immediate, but frontend state changes happen asynchronously

### Why the Previous Fix Didn't Catch This

The 2026-06-15 fix addressed the **router-result timeout race** (frontend 5-second timeout firing during active recording). This fix addresses a different race: **the hide-overlay event emission race** where the event arrives at the frontend after a new recording has already started.

### Fixes

1. **Frontend: Add state check to `hide-overlay` event handler** (RecordingOverlay.tsx lines 548-588):

   ```typescript
   const unlistenHide = await listen("hide-overlay", () => {
     // Check current state before hiding
     setState((current) => {
       // Don't hide if a new recording/transcription is active
       if (
         current === "recording" ||
         current === "transcribing" ||
         current === "processing" ||
         current === "confirming"
       ) {
         // New transcription is active — ignore the hide event
         return current;
       }
       if (current !== "usb-cycling") {
         setIsVisible(false);
         setTranscriptionPreview("");
         setRouterResult(null);
         setIsEditing(false);
         setCountdown(0);
       }
       return current;
     });
   });
   ```

2. **Backend: Update comment documenting both race fixes** in `actions.rs` lines 1298-1317 noting:
   - Backend guard (is_active_use check) for hide before user starts new recording
   - Frontend guard (hide-overlay handler state check) for hide event arriving after new recording starts
   - Frontend guard (router-result timeout state check) for 5-second timeout firing during new recording

### Key Insight

Event-based UI updates must **defensively check current state at event-handler time**, not trust the state that existed when the event was emitted. The emission time and handling time are different — the user may have started a new action in between. For overlays, this means the `hide-overlay` event handler must check if recording/transcription is still active before hiding, just like the router-result timeout handler does.

## Visualizer Positioning Bug — Center Screen After Router (2026-07-01)

### Problem

- When Handy finishes routing (filing a note) and the user immediately starts a new transcription, the visualizer appears in the CENTER of the screen instead of at the configured position (top or bottom)
- On subsequent transcription rounds, the position is correct
- This is a recurring bug that happens specifically in the router → new transcription transition

### Root Cause

The bug is caused by **silent failure in monitor detection** combined with a **timing race on macOS**:

1. **Silent Failure in `position_overlay_fixed()` (overlay.rs:476)**:

   ```rust
   if let Some(monitor) = get_monitor_with_cursor(app_handle) {
       // ... positioning logic ...
   }
   // NO ELSE BRANCH! If monitor detection fails, window is shown unpositioned
   ```

   When `get_monitor_with_cursor()` returns `None`, the function exits without setting any position. The window is then shown at its previous/default position (center of screen).

2. **Multiple Failure Points in `get_monitor_with_cursor()` (overlay.rs:170-198)**:
   - `input::get_cursor_position()` may return `None` (cursor position unavailable)
   - `app_handle.available_monitors()` may fail
   - No monitor may contain the cursor
   - `primary_monitor()` fallback may also fail

3. **macOS Async Race (overlay.rs:511-531)**:
   On macOS, position is set via `run_on_main_thread()` which is asynchronous:

   ```rust
   let _ = overlay_window.run_on_main_thread(move || {
       let _ = window.set_position(...);
       let _ = window.set_size(...);
   });
   ```

   If `show()` is called immediately after, the window can be shown before the position update completes.

4. **Timing Window**:
   The bug occurs when starting a new transcription **immediately** after routing completes (within the hide animation window of ~200-300ms). This is when the display system may be unstable and monitor detection can fail transiently.

### Why Subsequent Rounds Work

- After the first failed positioning, the window ends up at some position (center or wherever)
- By the time the next transcription starts, the display system has stabilized
- Monitor detection succeeds on retry
- The position is correctly calculated and applied

### Fixes (implemented 2026-07-01)

1. **Add fallback position with error logging** in `overlay.rs`:

   ```rust
   if let Some(monitor) = get_monitor_with_cursor(app_handle) {
       // ... existing positioning logic ...
   } else {
       log::error!("position_overlay_fixed: get_monitor_with_cursor returned None! Using primary monitor fallback.");

       if let Some(monitor) = app_handle.primary_monitor().ok().flatten() {
           // Use the same positioning logic with primary monitor
           // ... calculate position and set ...
       } else {
           log::error!("position_overlay_fixed: CRITICAL - No monitor available for positioning!");
       }
   }
   ```

2. **Ensure position update completes before show on macOS**:
   Add a synchronous flush of the main thread's run loop to ensure the position update is processed before showing the window.

3. **Add debug logging** at key points to track when positioning fails.

### Key Insight

When positioning overlay windows, **always have a fallback** when primary detection methods fail. Monitor detection can fail transiently due to timing issues, display reconfiguration, or platform-specific quirks. The `primary_monitor()` fallback ensures the window is always positioned somewhere sensible, even if it's not the exact monitor with the cursor. Additionally, on macOS, `run_on_main_thread()` is asynchronous — position updates must be flushed before showing the window to avoid race conditions.

## Router Visualizer Shows Black Background (2026-07-06)

### Problem

Router mode should show blue background visualizer throughout (recording → transcribing → processing → confirming), but was showing the black background from regular transcription processing.

### Root Cause

The `isRouter` derivation in `RecordingOverlay.tsx` used `backendState.isVisible` as the condition to prefer backend state. When the backend is in `Processing` or `Confirming` state, `backendState.isRouter` is `false` because only `Recording { binding_id }` carries the binding information that determines router mode.

```typescript
// Before (Bug): Processing state has no binding_id, so isRouter = false
const isRouter = useMemo(() => {
    if (backendState.isVisible) {
      return backendState.isRouter; // false during Processing!
    }
    return legacyIsRouter;
}, ...);
```

### Fix

Only use backend's `isRouter` when backend is in `Recording` state (which carries binding_id). For other states, fall back to the legacy `isRouter` derived from the `show-overlay` event payload (`"transcribing:router"` → action = "router").

```typescript
// After: Only use backend isRouter during Recording state
const isRouter = useMemo(() => {
    if (backendState.isVisible && backendState.isRecording) {
      return backendState.isRouter;
    }
    return legacyIsRouter;
}, ...);
```

### Key Insight

During migration from legacy event-driven state to backend-driven state, **mode information must survive state transitions**. The backend's `AppState` only carries `binding_id` during `Recording`, so mode information (router vs. transcribe) must be preserved from the initial `show-overlay` event for subsequent states (Processing, Confirming). Use the legacy source as fallback when the backend source doesn't have the needed information.

## Router Transcription Click-to-Edit Timing Gap (2026-07-06)

### Problem

Clicking on the router transcription text box doesn't open edit mode, even though the preview is shown.

### Root Cause

The `effectivelyConfirming` flag in `useRouterPreview.ts` was derived from:
```typescript
const effectivelyConfirming = isConfirming ?? state === "confirming";
```

The `transcription-preview` event arrives before the `app-state: Confirming` event. During this gap, `isConfirming` (from backend) is still `false` and `state` (from backend) is still `"processing"`. So `effectivelyConfirming` is `false` and the click handler bails out.

### Fix

Also consider `transcriptionPreview` being non-empty as a confirming signal:
```typescript
const effectivelyConfirming =
    isConfirming ?? (state === "confirming" || !!transcriptionPreview);
```

This bridges the timing gap — when the transcription preview text arrives, the user can immediately click to edit, even before the backend state catches up.

### Key Insight

When bridging between event-driven and state-driven architectures, **consider the user-visible state as the source of truth, not just the backend state**. If the user sees confirming UI (transcription preview text), they should be able to interact with it, regardless of whether the backend state has transitioned yet.

## Clipboard Fallback on macOS Desktop (2026-07-06)

### Problem

When focused on the macOS desktop (Finder), transcription completes but no toast appears and files are selected instead of text being pasted.

### Root Cause

When Finder is the frontmost app, `Cmd+V` is interpreted as "paste files" (if a file reference is on the clipboard) or does nothing (if only text is on the clipboard). Neither is useful — the user wants text pasted into a text field, not files selected on the desktop.

The paste fallback code (`paste-error-clipboard-fallback`) only triggers when the paste operation itself fails, not when it succeeds but goes to the wrong context.

### Fix

Added `is_saved_app_desktop_like()` to `focus.rs` that checks if the saved frontmost app is Finder (`com.apple.finder`). When detected, the `paste()` function in `clipboard.rs` skips the paste entirely and falls back to clipboard-only mode (copy text to clipboard + emit toast notification).

### Key Insight

On macOS, Cmd+V has different behaviors depending on the target application. Finder interprets it as file paste, not text paste. When the user's target app is a file manager, always fall back to clipboard-only mode with a toast notification — pasting to the desktop is never the user's intent.

## Visualizer Bar Sensitivity — No Adaptive Gain (2026-07-11)

### Problem

- User reports microphone clearly hears them, but the volume bars only move a little bit when they should move much more
- They asked whether a detection mechanism exists to fix this

### Root Cause

- `AudioVisualiser` in `audio_toolkit/audio/visualizer.rs` used **fixed hardcoded scaling**: `DB_MIN = -55.0`, `DB_MAX = -8.0`, applied as `((db - DB_MIN) / (DB_MAX - DB_MIN)).clamp(0,1)` for every bucket
- For a quiet mic, speech lands at ~-58dB to -50dB. The fixed -55dB floor clamps this into the bottom ~10% of the visual range, so bars barely twitch despite clear audio
- A `noise_floor` per-bucket tracker already existed and was being updated (slow EMA, `NOISE_ALPHA = 0.001`), but it was **never used for output** — only logged. Dead data.
- No AGC, no normalization, no auto-calibration existed anywhere in the pipeline (backend or frontend). The frontend `useVisualizer.ts` only has a low-audio *warning* (threshold 0.05), not gain.

### Fix

- Replaced the fixed `DB_MIN`/`DB_MAX` absolute dB window with **adaptive normalization relative to the noise floor**, plus a new per-bucket **peak tracker** (`peak_db`) that snaps up fast (`PEAK_RISE_ALPHA = 0.7`) and decays slowly (`PEAK_DECAY_ALPHA = 0.001`).
- New normalization: `db_above_floor = db - noise_floor`; `window_width = max(peak_db - noise_floor, MIN_WINDOW_DB=15)`; `normalized = (db_above_floor / window_width).clamp(0,1)`. Then the existing `GAIN`/`CURVE_POWER` curve shaping still applies on top.
- This auto-calibrates to any mic: a quiet mic has a low noise floor, so even quiet speech sits well above the floor and fills the bar range. No user action or settings UI needed.
- Removed `DB_MIN` and `DB_MAX` constants (now unused). Added `MIN_WINDOW_DB`, `INITIAL_PEAK_OFFSET_DB`, `PEAK_RISE_ALPHA`, `PEAK_DECAY_ALPHA`. Added `peak_db` field to `AudioVisualiser`, initialized in `new()` and reset in `reset()`.
- Output contract unchanged: `feed()` still returns `Option<Vec<f32>>` with values 0..1, 16 buckets.

### Key Insight

When a tracked statistic (noise floor) already exists in the code but isn't wired to output, the cheapest correct fix is to wire it in rather than add a new mechanism. The noise floor was free data being discarded. Pairing it with a peak tracker turns the fixed absolute dB window into a per-mic adaptive window — fixing quiet-mic sensitivity without any settings UI work.

### Correction — noise floor in the output path caused cold-start failure

The first attempt wired `noise_floor` into the normalization formula: `db_above_floor = db - noise_floor`. This made the cold-start **worse**:

- `noise_floor` initialized to `-40dB`
- Quiet mic speech at `-55dB` → `db_above_floor = -55 - (-40) = -15` → **negative** → clamps to 0.0 → bars don't move at all at startup
- `noise_floor` adapts at `0.001/frame` (0.1%) → takes ~30 seconds to drop from -40 to -55
- Peak tracker also started too high (`-15dB`) and decayed too slowly (`0.001`) → couldn't come down to meet a `-50dB` signal
- User reported bars barely moving at the start of transcription plus false low-audio warnings

**Root cause:** putting an *adaptation* variable (noise floor) in the *output path* means cold-start values directly gate the output. Until the adaptation converges, the output is wrong. And at 0.1%/frame, convergence takes ~30 seconds.

**Fixed by removing `noise_floor` from the output entirely** and using:
1. A **fixed wide window** (`-70` to `-5`, 65dB range) — any mic's speech falls within the visible range from frame 1. A quiet mic at -55dB → normalized = 15/65 = 0.23 (visible, not zero).
2. A **peak-based gain boost** on the normalized value. Peak **snaps up instantly** (not EMA) when speech exceeds it, so the boost is computed on the very first speech frame. Peak decays slowly (`0.003`, half-life ~4s) so brief pauses don't collapse it.
3. Peak starts at **0.0** — so the first speech frame immediately sets it and bars fill. No cold-start delay.

**Key insight:** never put a slowly-adapting variable directly in the output path. Use a fixed window for the base (so cold-start is always visible) and apply adaptation as a *boost on top* (so it only improves things, never gates them). Instant snap-up for peaks avoids the EMA convergence delay entirely.

## Router Post-Filing Bugs — Stuck Coordinator + Instant Hide (2026-07-11)

### Problem

Two related bugs in the router flow after filing:

1. **No visualizer indication after filing**: After routing text is sent for filing, the overlay disappears almost immediately — the router result (✅/❌) is visible for milliseconds before the overlay hides.
2. **Transcription gets stuck after routing**: After routing completes, starting a new recording fails with "pipeline busy" in the logs. The stop recording button doesn't work because the coordinator stays in `Processing` state.

### Root Cause

Both bugs share the same root cause: `notify_processing_finished()` was called at the **end** of the router subprocess thread (after all result handling), keeping the coordinator in `Processing` state for the entire subprocess duration.

**Bug 1 (Instant Hide):**
- Router finishes → `is_active_use()` returns `true` (coordinator still Processing) → skip hide
- `notify_processing_finished()` fires → coordinator transitions to Idle → `app-state: Idle` emitted
- Frontend receives Idle → `isVisible = false` → overlay hides immediately
- User sees the result for milliseconds at most

**Bug 2 (Stuck Coordinator):**
- Router subprocess runs for 5-30 seconds (boss_router.py is synchronous)
- During this entire time, coordinator is in `Processing` → `active_use = true`
- User tries to start new recording → coordinator rejects with "pipeline busy"
- `FinishGuard` was deliberately dropped before the router subprocess (correct — it shouldn't fire while routing), but `notify_processing_finished()` was the only thing that could free the coordinator, and it was at the end of the thread

### Fixes

**Backend (`router.rs`):**

1. **Immediate `notify_processing_finished()`**: Move the call to fire right after the router subprocess completes (both success and error paths), before result handling. This frees the coordinator immediately, allowing new recordings.

2. **Delayed `hide_recording_overlay()`**: After freeing the coordinator, spawn a 5-second delayed hide in a new thread. `hide_recording_overlay()` has built-in session guards (`OVERLAY_SESSION` counter + `is_active_use()` check) that prevent hiding if a new recording starts during the delay.

**Frontend (`RecordingOverlay.tsx`):**

3. **Router result visibility override**: Change `isVisible` from `backendState.isVisible` to `backendState.isVisible || (isRouter && routerResult !== null)`. This keeps the overlay visible when a router result is being displayed, even after the backend transitions to Idle. The frontend's existing 10-second `ROUTER_RESULT_DISPLAY_MS` timeout clears `routerResult`, at which point `isVisible` re-evaluates to `false` and the overlay hides.

### Timing Diagram

```
Router subprocess finishes
  │
  ├─► emit router-result event (frontend shows result)
  ├─► send macOS notification
  ├─► notify_processing_finished() (coordinator → Idle, active_use = false)
  │     └─► User can now start new recording immediately!
  │
  └─► spawn delayed hide thread (5 seconds)
        └─► hide_recording_overlay() (with session guard)
              └─► If no new recording: overlay hides
              └─► If new recording started: session mismatch, skip hide

Frontend:
  t=0s:  routerResult arrives → isVisible = true (backend or routerResult)
  t=10s: ROUTER_RESULT_DISPLAY_MS timeout → routerResult = null → isVisible = false
```

### Key Insight

When a long-running background operation (like a subprocess) needs to both (a) free the coordinator for new operations and (b) keep the UI visible for result display, **decouple the two concerns**: immediately free the coordinator (`notify_processing_finished()`), then use a separate delayed mechanism for the UI hide. The frontend can independently control visibility based on its own state (`routerResult`), while the backend handles the coordinator lifecycle.

### Files Changed

- `src-tauri/src/actions/router.rs` — Restructured router subprocess thread: immediate `notify_processing_finished()` + delayed `hide_recording_overlay()`
- `src/overlay/RecordingOverlay.tsx` — `isVisible` override for router result display period
