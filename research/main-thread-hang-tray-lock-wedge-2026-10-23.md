# 2026-10-23 Wedge: main thread parked → `set_tray_state` blocks → coordinator wedged

**Status: diagnosis SOLVED (cascade proven live), primary blocker partially identified**
Live incident investigated against the running process (`/Applications/Handy.app`, pid 29205,
launched Wed 22:18, v0.12.2, macOS 26.6.2 / arm64), worker pid 96118.

This case is **not** the old `is_transcribing`-slot wedge and **not** the streaming-callback
lock wedge. It is a *main-thread hang* that cascades into the coordinator thread.

---

## 1. Live evidence collected

| Probe | Result |
|---|---|
| `sample 29205` (14:44:22 and 14:47, 1 ms interval) | Main thread **100 % in `_pthread_cond_wait`** (`__psynch_cvwait`), identical stack both samples → permanent park |
| `sample 29205` thread census | 50 → 52 threads; `__psynch_mutexwait` count = **0** (no pthread mutex deadlock; `parking_lot` parks surface as `__psynch_cvwait`) |
| `sample 96118` (worker) | **Idle**: main thread on `dispatch_semaphore_wait`, `transcribe-worker-in` blocked in `read`, no engine work in flight → engine/supervisor is *not* the blocker |
| `handy --is-recording` | `not-recording` (exit 1) → `AudioRecordingManager::is_recording()` (atomic `recording_active`) = false |
| `handy --is-active-use` | `active-use` (exit 0) → `coordinator.is_active_use()` = true |
| `handy --cancel` (live test) | Reaches `[CANCEL-TIMING] step 6 (emit_app_state Idle) took 118µs`, then **hangs at step 7** (`set_tray_state`) — no step 7/8/COMPLETE line, forever |
| `CGWindowListCopyWindowInfo` | Overlay window still **on screen** (layer 3, 800×280 @ 880,46) → OS-level hide never ran |
| `handy.log` @ 06:44:23 | `handy-keys ... state=Pressed` + `[CMD-CHAN] send_input enqueued for 'transcribe' (send took 1.541µs)` and **never** the matching `[CMD-CHAN] processed command Input ...` → coordinator thread is not draining its channel |
| Event log | Last events: `TranscriptionCompleted` → `RecordingStarted` → `ShortcutTriggered`, all for the new session s-…122.969Z. **No `PasteSucceeded` for the *previous* session s-…110.127Z** (32 311 `PasteSucceeded` events exist historically) |

### Why the earlier evidence was thin
`tauri_plugin_log` is configured `max_file_size(500_000)` + `RotationStrategy::KeepOne`
(`lib.rs`, `LogBuilder`), and `FILE_LOG_LEVEL` defaults to **Debug**, so
`[visualizer] bucket_0` in `audio_toolkit/audio/visualizer.rs` writes ~30 lines/s.
The retained `handy.log` window is **~80 seconds** (06:44:03 → 06:45:23). Every log line
from the wedge minute (06:41:22) had already been rotated away.

---

## 2. The cascade (each step proven)

1. **Session A ends normally.** Async stop task transcribes (158 ms), then submits the paste
   closure to the main thread: `TranscribeAction::stop` → `ah.run_on_main_thread(move || …)`
   (`actions/transcribe.rs`).
2. **The paste closure runs on the main thread and never returns.**
   Proof: `SessionTracker::finish_session` (which emits `PasteSucceeded`, `session.rs`) is the
   first statement after `utils::paste(...)` in that closure, and no `PasteSucceeded` was ever
   written for session A. The user still got the text, so `Cmd+V` was already sent —
   the block is *after* the keystroke, inside `clipboard::paste` /
   `send_paste_keys_and_verify` (150 ms sleep → `verify_paste_landed_macos` →
   `AXUIElementCopyAttributeValue` on the frontmost app, **no `AXUIElementSetMessagingTimeout`
   anywhere in the tree**) or on a Rust mutex inside that path (`get_settings`, line
   `enigo_state.0.lock()`).
3. **Main thread parks.** Stack (both samples, identical):
   `-[NSApplication run] → _DPSNextEvent → RunCurrentEventLoopInMode → __CFRunLoopRun →
   __CFRunLoopDoObservers → __CFRUNLOOP_IS_CALLING_OUT_TO_AN_OBSERVER_CALLBACK_FUNCTION__
   → tao event-dispatch frames → _pthread_cond_wait (__psynch_cvwait)`.
   That is tao's run-loop observer callback running app code that is now parked on a condvar
   (the same park primitive used by `std::sync::mpsc::Receiver::recv`).
4. **13 ms later** the user presses the hotkey. `send_external/send_input` enqueues `Command::Input`
   (unbounded channel → succeeds), `process_command` optimistically sets
   `Stage::Recording` + `active_use = true`, then `StageAction::StartRecording` →
   `start()` → `TranscribeAction::start`, which emits `RecordingStarted` (SessionTracker) and
   `ShortcutTriggered` — exactly the last two events in the log.
5. **`TranscribeAction::start` blocks at `set_tray_state(app, TrayIconState::Recording)`**
   (before `show_recording_overlay` and before `try_start_recording`).
   `tray::set_tray_state` takes the global `TRAY_LOCK` then calls `tray.set_icon(...)`;
   Tauri's tray uses the `run_item_main_thread!` macro (`tauri/src/menu/mod.rs`):
   `app_handle.run_on_main_thread(task)` **then blocking `rx.recv()`**.
   `send_user_message` only executes inline when the caller *is* the main thread, so a
   background caller needs the main loop to drain the queue — which it cannot.
   → **coordinator thread parked forever, holding `TRAY_LOCK`.**
6. Everything downstream follows mechanically:
   - `try_start_recording` never runs → `is_recording = false` (measured), no mic capture for the new session;
   - no further `app-state` emit → overlay keeps the last state the webview received;
   - all later hotkeys enqueue into a channel nobody reads → "hotkey does nothing";
   - `--cancel` reaches step 7 (`set_tray_state`) and blocks on `TRAY_LOCK`, so step 8
     `force_hide_recording_overlay` never runs → "--cancel has no effect";
   - the overlay NSPanel is never hidden at the OS level (`hide_recording_overlay`'s
     `run_on_main_thread(window.hide())` can't run either) → panel stays on screen;
   - `[visualizer]`, `sleep_wake` heartbeat and `retry_worker` keep logging because they are
     audio-thread / background-thread loops, **not** main-loop or coordinator work.

Cross-check on the `--cancel` deadlock: after running it, a fresh `sample` shows **two**
`tokio-runtime-worker` threads parked in the identical stack
(`… → thread-park → __psynch_cvwait`); the pre-existing one is the user's earlier `--cancel`
attempt stuck in the same `rx.recv()`.

---

## 3. Ranked hypotheses (evidence strength)

**H1 — HIGH. Main thread parked inside the main-thread paste closure; cascade via
`set_tray_state`'s main-thread roundtrip.** Proven: main thread permanently parked;
`PasteSucceeded` missing for session A; `--cancel` dies exactly at `set_tray_state`;
coordinator dies exactly where `set_tray_state` is called in `TranscribeAction::start`.
Unproven: the exact mutex/syscall inside `clipboard::paste` (binary is `strip = true`, all
app frames are `???`). Candidates in priority order:
  a. `enigo_state.0.lock()` / `get_settings` read lock inside `clipboard::paste` — a
     `parking_lot` park, which is what the sample shows (`__psynch_cvwait`, not `mach_msg`);
  b. `verify_paste_landed_macos` → `AXUIElementCopyAttributeValue` with **no messaging
     timeout** (unbounded cross-process call; would usually show as `mach_msg2_trap`, hence
     ranked below (a), but it is the only *unbounded* call in that path and must be fixed
     regardless).

**H2 — MEDIUM. The trigger is not the paste but another main-thread task.** Any
`run_on_main_thread` closure that blocks (overlay show/hide, `focus::restore_frontmost_app`,
`utils::paste`) produces the identical cascade. The missing `PasteSucceeded` points at the
paste closure specifically, but a blocked closure of any kind would look the same downstream.

**H3 — LOW / RULED OUT. Coordinator-only wedge (old `is_transcribing` slot, streaming
callback lock, engine supervisor hang).** Ruled out: worker is idle; zero mutex contention;
`--cancel` blocks at a *tray* call, which proves the main thread — not the coordinator — is
the root.

**H4 — RULED OUT. Log-level evidence of a panic.** No WARN/ERROR, and the coordinator's
`catch_unwind` recovery (which resets state to Idle) never ran, so no panic occurred.

---

## 4. Answers to the six questions

**Q1 — post-`RecordingStarted` path and where it wedges silently.**
`Command::Input` → `CoordinatorCore::process_command` (optimistic `Stage::Recording`,
`active_use=true`) → `StageAction::StartRecording` → `start()` (`transcription_coordinator.rs`)
→ `TranscribeAction::start` (`actions/transcribe.rs`): `tracker.start_session` (emits
`RecordingStarted`) → `emit(ShortcutTriggered)` → spawns model/VAD kicks → **`set_tray_state(app, Recording)`** → `show_recording_overlay` → `rm.try_start_recording`.
The silent wedge is `set_tray_state`: `tray::set_tray_state` holds `TRAY_LOCK`
(`parking_lot`) across `tray.set_icon` → `run_item_main_thread!` → `rx.recv()`. No log is
emitted on entry or exit of the blocking region, hence total silence.

**Q2 — why a press produces nothing, and which side shows "Processing".**
- Hotkey → `handy_keys` thread → `TranscriptionCoordinator::send_input` → **unbounded
  `mpsc` send succeeds** (logged, 1.5 µs) → coordinator thread never dequeues → no stop,
  no `[CMD-CHAN] processed command`, no `RecordingStopped`.
- The 30 s `PROCESSING_TIMEOUT` only applies to `Stage::Processing`. Here the core stage is
  `Recording` (optimistically set), so only `RECORDING_TIMEOUT`-style silence timers apply —
  and they are also computed **inside the coordinator loop**, which is parked, so they never
  fire either. There is no watchdog for a stuck *command handler*.
- The "Processing" pill is a **frontend** artifact: `useAppState` renders the last
  `app-state` event the webview received (Processing, from session A's stop). The Rust side
  never emitted `Recording`, and because the coordinator is parked it never will. WebKit is a
  separate process, so CSS animation keeps running while the Rust main thread is dead.

**Q3 — why `--cancel` is inert (measured live).**
`--cancel` → `tauri_plugin_single_instance` socket callback on a **tokio** thread →
`spawn_blocking(cancel_current_operation)` → steps 1–6 all complete in microseconds →
**step 7 `set_tray_state(Idle)` blocks on `TRAY_LOCK`**, held by the coordinator which is
itself blocked inside `set_tray_state`. Step 8 `force_hide_recording_overlay` never runs, so
the overlay is never force-hidden. Nothing is deadlocked on the TM/audio mutexes — the single
global `TRAY_LOCK` is the poison pill, and it is held across a main-thread roundtrip.

**Q4 — engine supervisor.**
- No lock in the supervisor blocks the app's stop/cancel/transcribe paths: `cancel()` bypasses
  the queue and kills the worker; it never touches the `TranscriptionManager` mutex.
- It *does* have deadlines (`HELLO_TIMEOUT` 60 s, `LOAD_TIMEOUT` 180 s, `CALL_FLOOR` ≥30 s,
  run floors 60/300 s) plus kill-and-respawn, so a wedged engine call cannot hang forever and
  is the one subsystem here with a real watchdog.
- Live check: worker pid 96118 had zero work in flight — completely idle. Not involved.

**Q5 — do the uncommitted diffs contain a fix? No.**
`git diff` = 19 files, but `rustfmt`-normalized comparison shows most are **formatting only**.
The real changes are a `settings::patch_settings` / `patch_settings_try` refactor (atomic
read-modify-write of the settings cache, replacing `get_settings_safe` + mutate +
`write_settings_safe` in `commands/{mod,history,models,transcription}.rs`) plus small edits in
`managers/audio.rs`, `audio_toolkit/vad/mod.rs`, `settings/store.rs`, `shortcut/mod.rs`.
Nothing touches `tray::set_tray_state`, `TRAY_LOCK`, `run_on_main_thread` usage in
`TranscribeAction::stop`, `clipboard::paste`, or AX verification. Commit `a79031a6`
("stop-deadlock BLOCKER found+fixed in review") is an *earlier* fix, already in HEAD.

**Q6 — recovery + next-occurrence diagnostics.**
Recovery (nothing here can be fixed from outside the process):
- `--cancel` will never work (it blocks on `TRAY_LOCK`); `--is-active-use` will keep saying
  "active-use" forever, so scripts waiting for idle will spin.
- The tray Quit / AppleScript quit go through the main thread → they will not work either.
  Only `kill -TERM 29205` (or `kill -9`) recovers. No data is lost: the wedged session never
  captured audio (`is_recording=false`) and session A's text was already pasted.
- Do **not** rebuild-install over the running instance expecting it to fix the old one.

Diagnostics to add (each targets a specific blindness proven here):
1. **Main-thread watchdog**: heartbeat thread + `AtomicU64` stamp updated by a
   `run_on_main_thread` tick; log `MAIN-THREAD-HANG elapsed=…s last_task=…` when stale > 5 s.
2. **Never hold `TRAY_LOCK` across a main-thread hop**: `set_tray_state` should copy the icon
   state, drop the lock, then `try_send` + `recv_timeout(250 ms)`; log on timeout. Same for
   `force_hide_recording_overlay`.
3. **Bound the AX calls**: `AXUIElementSetMessagingTimeout(ax_app, 1.0)` before
   `AXUIElementCopyAttributeValue`, and run `verify_paste_landed_macos` off the main thread
   (the paste closure only needs the keystroke ordered, not the verification).
4. **Move the paste off the main thread** entirely (`spawn_blocking`, re-enter main only for
   the final clipboard/overlay calls) — removes the whole class.
5. **Stop burning the log**: gate `[visualizer] bucket_0` behind `trace!` (or a debug flag) and
   raise `max_file_size`; 80 s of retention is useless for an hour-long wedge.
6. **Name threads** (`Builder::new().name("transcription-coordinator")`, `"paste"`, …) and ship
   an installed build with `strip = false` (or `debug = 1` + dSYM) so `sample`/`spindump`
   resolves Rust frames instead of `???`.
7. Add a `CMD-WATCHDOG`: if a command has been in `process_command` + its `StageAction` for
   > 5 s, log it from a supervisor thread (today only the *total* handler time is measured,
   after the fact).

---

## 5. Related docs

- `research/transcription-slot-wedge-diagnosis.md` — different wedge (slot stuck true).
- `research/stuck-state-investigation-streaming-live-captions.md` — different wedge (TM lock held by streaming callback).
- `research/stop-recording-freeze-investigation-report.md` — same *family* (stop path blocked on a synchronous UI call); that one was fixed by spawning the stop work before UI calls. The same principle was never applied to `set_tray_state`.
