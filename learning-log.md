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
