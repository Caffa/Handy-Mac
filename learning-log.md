# Handy-Fork Learning Log

## "Reflections" mistranscribed as "Reflexions" — post-processing dictionary rewrote correct words (2026-11-14)

### Problem

- Transcription output containing perfectly valid words ("reflections", "inflections", "kilogram", "tsar", "whiskey", "spelled") was rewritten by the US→British post-processing step into dated/incorrect British forms ("reflexions", "inflexions", "kilogramme", "tzar", "whisky", "spelt").

### Root Cause

- The DWYL-derived `DWYL_DICTIONARY` in `src-tauri/src/audio_toolkit/spelling_dictionaries.rs` contained dated British targets that modern British English no longer prefers (reflexion, inflexion, kilogramme, tzar, whisky, spelt — 9 pairs incl. inflected forms).
- The same dictionary carried 8 dead identity entries (us==uk, e.g. ("theatrical","theatrical")), 31 verbatim duplicate keys, and a broken entry ("normalise","normalise") that should have been ("normalize","normalise") — so "normalize" was never converted while organize/realize were.
- `convert_us_to_british` runs after transcription and trusted these entries blindly.

### Fix

- Purged the 9 dated pairs, 8 identity entries, and 31 later duplicates (kept first occurrence); replaced ("normalise","normalise") with ("normalize","normalise") plus normalizes/normalized/normalising forms. DWYL now has 284 clean entries.
- Flagged-but-approved pairs (adapter/adaptor, siphon→syphon, philter→philtre, fetus→foetus, sulphur, judgement, speciality, -ise/-our/-re) intentionally kept.
- Added `test_dated_entries_removed` guarding the purge; module doc updated with the 2026-11-14 purge note.

## Live captions streamed to nothing: on_audio closure resolved Tauri state before it was managed (2026-11-12)

### Problem

- Live captions never streamed partial text to the overlay even though the setting was enabled and speech frames were passing the VAD gate.
- The `[STREAM-CB]` / `[Live Captions]` log lines were missing entirely.

### Root Cause

- `AudioRecordingManager::new` eagerly resolved the live-captions managed state inside the `on_audio` closure constructor:
  - `app.try_state::<StreamingCancelFlag>()` / `LiveCaptionsStreamingCancelFlag`
  - `app.try_state::<Arc<Mutex<TranscriptionManager>>>()` / `LiveCaptionsTranscriptionManager`
- `initialize_core_logic` calls `AudioRecordingManager::new` BEFORE it calls `app_handle.manage(streaming_cancel_flag)`, `manage(transcription_manager)`, `manage(LiveCaptionsStreamingCancelFlag)`, and `manage(LiveCaptionsTranscriptionManager)`.
- Because Tauri managed state is never retroactively filled, the eager `try_state` captures returned `None` forever, and every speech frame silently dropped at the `let Some(cancel_flag)` / `let Some(tm)` guards.
- Regression from commit `1f03472b` (2026-10-20).

### Fix

- Added `StreamingSeamState` in `src-tauri/src/managers/audio.rs` with a `OnceLock<SeamHandles>` cache and a `resolve()` method that:
  - Returns the cached handles on subsequent frames.
  - On a cache miss, lazily looks up the four managed states from the frame delivery thread.
  - Retries (without caching) if any required state is not yet managed.
  - Logs a rate-limited `[LIVE-CAPTIONS]` warning once per 30 s while transient.
  - Emits a one-time `info!` when the seam finally resolves.
- `AudioRecordingManager::new` now constructs `Arc<StreamingSeamState>` and moves it into the `on_audio` closure; the eager `try_state` blocks are deleted.
- The closure destructures `seam.resolve()` once the interval/throttle gates have passed, preserving the original check order.

### Key Insight

Startup-constructed callbacks that consume Tauri managed state must resolve at use time, not construction time. Implicit ordering in `initialize_core_logic` drifts silently; `try_state` returning `None` once is permanent.

## Invisible 800×280 overlay region ate clicks during non-interactive transcribe states (2026-11-12)

### Problem

- During transcribe recording → transcribing → processing, and during USB-cycling, the overlay's transparent 800×280 fixed canvas blocked clicks and scrolls on the underlying application.
- The user could not interact with the app underneath the overlay even though no interactive DOM elements were visible.

### Root Cause

- `swizzle_mouse_tracking` in `src-tauri/src/overlay.rs` flips the NSPanel's `setIgnoresMouseEvents: false` whenever the cursor enters the panel and `OVERLAY_FORCE_CLICK_THROUGH` is false.
- `OVERLAY_FORCE_CLICK_THROUGH` was only armed for Router `processing` (and router result display on the React side). It was never armed for Transcribe non-interactive states.
- macOS `ignoresMouseEvents` is all-or-nothing for the whole panel; CSS `pointer-events: none` is insufficient. DOM `onMouseEnter` cannot fire while the panel is pass-through, so interactive elements cannot opt in until the panel is already accepting events.

### Fix

- In `show_overlay_state_main` (`src-tauri/src/overlay.rs`), extended the `should_force` predicate to keep click-through during Transcribe/TranscribeWithPostProcess states `"transcribing"`, `"processing"`, and `"usb-cycling"`.
- Left `recording` and `confirming` unforced so the cursor-enter flip still makes `.cancel-button` / `.recording-timeout-extend-button` and router textarea/cards clickable.
- Mirrored the predicate in the `setOverlayForceClickThrough` effect in `src/overlay/RecordingOverlay.tsx` so React state transitions cannot undo the Rust-side flag.

### Key Insight

macOS `ignoresMouseEvents` is all-or-nothing for the panel; DOM opt-in is impossible while pass-through. The correct scope is therefore the overlay state machine: force click-through whenever the current state has no interactive DOM, and let the enter-flip remain active for states that do.

## Segfault at display wake: ObjC ScreensDidWake observer ran app logic on the main thread (2026-11-10)

### Problem

- After sleep/display-wake, Handy stopped responding to the hotkey and the always-on mic stream was dead until the app was reopened
- Crash report `handy-2026-11-09-234707.ips`: SIGSEGV (faulting thread 0, faulting address `0x99`) at 23:47 local, three seconds after `Received native NSWorkspaceScreensDidWakeNotification` was logged; the process died and restarted 94s later (user reopen)

### Root Cause

- `start_native_wake_observer` stored `Arc::into_raw(app_handle.clone())` in an ObjC ivar. `Arc::into_raw` returns a pointer to the inner `AppHandle` (ArcInner + 0x10), NOT a pointer to an `Arc` handle — but the callbacks cast it back as `*const Arc<AppHandle>` and made `&Arc<AppHandle>` from it, one indirection level short
- Every subsequent use read the AppHandle's own first word (value 1) as the `ArcInner` pointer: crash 1 (old build) computed `&AppHandle = 1 + 0x10 = 0x11` and faulted reading `[0x11 + 0x88]` (address 0x99) inside `get_settings_safe`; crash 2 (after the thread-dispatch fix) faulted at `0x1` doing the `Arc::clone` atomic refcount add. Same word, same bug, deterministic on every wake-notification delivery — the ivar itself was never corrupted
- The callbacks also ran `on_screens_wake`/`on_system_wake` synchronously on the main thread inside the notification-center call-out (multi-second uhubctl probes, settings/state reads), and the design comment ("it only logs and calls on_system_wake, both of which are short and non-blocking") had been false since `on_screens_wake` gained that work

### Fix

- First attempt moved handler work to a thread (`dispatch_wake_handler`) — correct for the main-thread block, but the crash moved with the bug into `Arc::clone`
- Final fix removes the unsafe plumbing entirely: `static WAKE_APP_HANDLE: OnceLock<Arc<AppHandle>>` is set once at listener start; both ObjC callbacks contain zero `unsafe` code and only read the static and dispatch to the named thread. The ivar, `Arc::into_raw` leak, and ivar write are deleted
- Unit test `dispatch_wake_handler_runs_off_thread` asserts the handler runs on another thread with the Arc payload intact

### Key Insight

Two lessons: (1) `Arc::into_raw` gives you the **data pointer**; the only sound way back is `Arc::from_raw`, never a cast to `*const Arc<T>` — `&*ptr` then type-confuses the target's own bytes with the Arc header. (2) An ObjC notification callback is FFI: treat it like a signal handler — read a safe static, spawn a thread, return. Never keep state in the ObjC object or run settings/state/subprocess work inside the call-out frame.

## Bluetooth AirPods flaps spammed the USB watchdog with "device not found" cycles (2026-11-09)

### Problem

AirPods Pro connect/disconnect (input device flaps) caused spurious USB-watchdog warnings and uhubctl cycle attempts that always failed with "device not found in USB tree" — Bluetooth devices are invisible to `uhubctl`, so every attempted cycle was doomed, and each flap armed a fresh 180s one-shot bypass that skipped both the failure threshold AND the 30s cooldown on the next dead-looking recording.

### Root Cause Chain

1. Every AirPods input-device flap ran the same flap-arming path as a USB dock flap: `arm_wake_bypass_quiet(180s)` + `device_flap_bypass_armed` event.
2. Any subsequent zero-RMS / digital-silence / open-fail while the AirPods were the active mic counted as a failure; the armed bypass made the FIRST failure cycle immediately (`cycle_decision` bypass skips threshold and cooldown).
3. The cycle resolved the configured device against the USB tree via `uhubctl` — a Bluetooth name is never there → "device not found in USB tree" event, every time.
4. Separately, screens/system wake handlers probed the USB tree for a BT-configured device (always absent) and spawned a recovery thread that polled uhubctl for the full retry window before giving up.

### Fixes (all `src-tauri/src`)

1. **`audio_toolkit/audio/device.rs`** — the name heuristic extracted into `is_bluetooth_device_name` (shared, input/output agnostic); `is_bluetooth_output_device` is now a one-line delegate.
2. **`usb_watchdog.rs`** — `is_bluetooth_configured()` guard as the FIRST statement in `on_mic_open_failed`, `on_silent_transcription`, `force_power_cycle`, `arm_wake_bypass_quiet`, `arm_wake_bypass` (loud variant added so the wake path cannot emit a `bypass_armed` event for a bypass that was never armed), and `power_cycle_blocking_with` (belt-and-suspenders; also covers `wake_recovery_cycle_blocking`). Skip happens before any counter write, event, or uhubctl probe; guard never calls `is_uhubctl_available` so tests stay hermetic.
3. **`managers/audio.rs`** — flap arming additionally requires `any_non_bluetooth_device(&added_input, &removed_input)` (pure-BT flaps never arm; the `last_input_change_epoch_ms` stamp still fires so dead-recording aborts keep working for BT-caused flaps); mic-open failures are not counted when the attempted device name is positively Bluetooth (`MicrophoneResolution` now carries `device_name`; unknown/default names keep normal accounting); the three post-recording watchdog calls are gated on the selected mic not being positively Bluetooth.
4. **`sleep_wake.rs`** — `on_screens_wake`'s `bypass_gated` additionally requires a non-BT device name (no uhubctl probe, no arm); `spawn_wake_recovery_thread` early-returns at thread top for a BT name (zero subprocesses, zero "device absent after wake retry window" events).

### Invariant

Only a POSITIVE Bluetooth name match skips. `""`, `"Default"`, and unknown names take the exact pre-change path — for a non-BT watchdog device and non-BT failing mic, thresholds, cooldowns, grace, wake cycling, events, and uhubctl probes are byte-for-byte unchanged.

## Dock-flap digital-zero recordings defeated every watchdog duration gate (2026-11-09)

### Problem

Dock USB flap at wake → the RØDE stays enumerated and "delivering" but sends EXACT DIGITAL ZEROS. Fresh stream opens do NOT fix the hardware — only a USB hub port power cycle does (verified: manual uhubctl cycle restored signal; Nov 4 auto-cycles fixed identical silence). The user's short post-flap presses (0.3–2.9s) were ALL declined by the watchdog's 10s duration gates — `silent transcription ignored (duration X < 10.0s)` / `low audio level ignored` — so zero cycles fired despite `usb_watchdog_enabled=true`.

### Root Causes

1. **Exact zeros conflated with ambiguous low audio.** Both went through the same `max_level < NO_AUDIO_THRESHOLD` arm and inherited the 10s gate, but exact zeros are unambiguous dead signal — duration is irrelevant evidence-wise.
2. **Screens-wake bypass required literal device absence.** `on_screens_wake` only arms when `device_present_in_usb_tree` is false; the flap re-enumerates the tree in ~2.3s, so by the time the observer checked, the device was present and nothing armed.
3. **Zero-RMS abort refused the flap case.** The pre-arm-silent gate (muted-mic protection) requires a nonzero pre-arm window, but after a flap the stream is silent from the start — no window to observe.
4. **`last_standing_rebuild_epoch` initialized to 0** produced a garbage "idle 1794159154121ms" elapsed in the first device-list rebuild log.

### Fixes (all `src-tauri/src`)

1. **`managers/audio.rs`** — `last_standing_rebuild_epoch` stamps `epoch_ms()` at construction (new small `epoch_ms()` helper); `ZERO_SAMPLE_MAX_RMS` made `pub` as the single shared constant.
2. **`usb_watchdog.rs`** — new `on_digital_silence(duration_secs, max_rms)`: one evidence `warn!` then `on_mic_open_failed()`, deliberately with NO duration/enabled/cycling/grace gates (the absence of a duration gate is the point). `on_recording_finished(sample_count, duration, max_level)` now resets only on `sample_count > 0 && max_level > ZERO_SAMPLE_MAX_RMS` — n>0-but-zeros counts as dead.
3. **`managers/audio.rs`** (`collect_recording` accounting) — three mutually exclusive arms: exact zeros → `on_digital_silence` + `restart_microphone_if_needed` + native "Microphone delivered no signal — USB port cycled" notification on cycle; low-but-nonzero → existing `on_low_audio_level` 10s gate unchanged; else → `on_recording_finished(len, dur, max_level)`.
4. **`managers/audio.rs`** (`start_device_monitor`) — new `last_input_change_epoch_ms` stamped on every input diff (before all gating) + `last_flap_bypass_arm_epoch_ms`; when the watchdog settings gate passes and `should_arm_flap_bypass` allows (one arm per 60s), arms a 180s one-shot bypass via new `UsbWatchdog::arm_wake_bypass_quiet` and emits one distinguishable `WakeRecovery { stage: "device_flap_bypass_armed" }` (no double generic event). Pure predicates `device_change_recent` / `should_arm_flap_bypass` unit-tested.
5. **`managers/audio.rs`** (`start_liveness_monitor`) — `should_abort_dead_recording` gains `device_changed_recently`: within the 120s `DEVICE_FLAP_RECENT_WINDOW_MS` of a stamped flap, the abort fires even with a silent pre-arm window (flap substitutes for the zombie signature); muted mics with no flap stay protected.
6. No watchdog consumption changes needed: `on_mic_open_failed` already treats an armed bypass as an immediate one-shot cycle.

### DEFERRED — idle zero-RMS escalation (observe first)

An always-on standing stream that goes exact-zero while the app is IDLE is still invisible: the liveness monitor's recency checks stay green and nothing cycles until the user dictates. An idle-time escalation (periodic probe recording or sink-side zero-RMS timer) would fix that, but it adds a new background probe surface and risks cycling during legitimate muted-mic use. **Observe-first plan:** if the structured log shows evidence of idle exact-zeros (e.g. arm-time `max_rms 0` immediately after `device_flap_bypass_armed` with no user press in between, or repeated `zero_sample_abort` without an antecedent flap stamp), add a LogOnly `idle_zero_rms` `WakeRecovery` stage at the standing path first, and only then decide whether a cycle is warranted.

### Verification recipe

1. Flap the dock (or replug) while `usb_watchdog_enabled=true`, `cycle_on_wake=true`, device configured; wait for re-enumeration (~3s).
2. Within 2 minutes, dictate for 1–3 seconds.
3. Expected on that FIRST press: `device_flap_bypass_armed` in `handy-events.jsonl`, then `UsbWatchdogCycle { success: true }`, mic signal restored, native "Microphone delivered no signal — USB port cycled" notification.
4. After the 180s bypass TTL expires: the first dead press aborts (zero-RMS) and the SECOND press's failure crosses threshold-2 → cycle (or the flap arms a fresh bypass).
5. Muted mic with NO flap: a single short dead press must NOT cycle (pre-arm gate holds; `on_low_audio_level` keeps its 10s gate).

## Display-wake zombie standing stream on a never-sleeps Mac (2026-11-08)

### Problem

Desktop Mac with `SleepDisabled=1` — the system never sleeps, only the displays do. On a wake transition the dock's ASMedia USB controller power-loses and the whole USB tree re-enumerates ~2.3s later. Handy's always-on standing capture stream survived as a zombie "trickle": it stayed open and kept delivering digitally-silent chunks, so every recency-based liveness layer stayed green. The first dictation after wake armed on the dead standing stream → 0 samples → counted as "mic open failure #1 (threshold 2, wake_bypass false)" → no recovery — and the next press's *successful open* reset the failure counter before the threshold was ever reached. Separately, Cancel (Cmd+Esc) and empty-stop leaked the SessionTracker session (ghost "still active" for 31 min). The Oct-23 wake machinery never fired because wall-clock gap detection never sees a display-only sleep.

### Root Causes

1. **Recency-only liveness is blind to digital-silence delivery.** The liveness monitor's `chunks_stale` check treats fresh chunks as proof of life; a zombie stream delivering silence passes it.
2. **The watchdog failure counter reset on a mere stream open.** `on_mic_open_succeeded()` was called from both `start_microphone_stream` sites, so the press *after* a dead recording erased failure #1 — the threshold-2 gate could never be reached by repeated dead dictations.
3. **Session leak on cancel/empty stop.** `cancel_current_operation` never touched the tracker; the `Some(samples)`-but-empty branch of the stop path hid the overlay without `fail_session`, leaving the session "active" until the next `start_session` warned about it.
4. **Wake machinery was sleep-only.** `NSWorkspaceDidWakeNotification` and the wall-clock detector both require a system sleep; display wakes (and the dock power events that ride along) never reached any recovery path.

### Fixes

1. **`src-tauri/src/actions/transcribe.rs`** — the Some-empty branch of the `recording_result` match now calls `tracker.fail_session(sid, "Recording produced no audio samples")` (same pattern as the None branch) before hiding the overlay.
2. **`src-tauri/src/utils.rs`** — `cancel_current_operation` step 9 finalizes the tracker session via `fail_session(&sid, "Operation cancelled by user")` (idempotent, take()-based).
3. **`src-tauri/src/session.rs`** — `#[cfg(test)]` tests: fail finalizes + records failure; double-fail is a no-op.
4. **`src-tauri/src/usb_watchdog.rs`** — new `on_stream_open_confirmed()` clears only `post_cycle_grace`; `consecutive_failures` now resets solely via `on_recording_finished(>0)` or a completed cycle. `on_mic_open_succeeded` docs updated accordingly.
5. **`src-tauri/src/managers/audio.rs`** — both `start_microphone_stream` success sites call `on_stream_open_confirmed()` instead of `on_mic_open_succeeded()`.
6. **`src-tauri/src/managers/audio.rs`** (`start_device_monitor`) — an INPUT device-list change while idle now rebuilds the standing stream (`invalidate_device_cache()` + `restart_microphone_if_needed()`), rate-limited by `should_rebuild_standing_stream` (≥10s between rebuilds, never while recording), emitting `WakeRecovery { stage: "device_list_rebuild" }`. No hub cycling on this path.
7. **`src-tauri/src/managers/audio.rs`** (`start_liveness_monitor`) — zero-RMS abort: an armed recording past `ZERO_SAMPLE_ABORT_MS` (2500ms) with `capture.get_max_level()` still ≤ `ZERO_SAMPLE_MAX_RMS` (1e-6, digital silence; room tone ~0.001) is aborted, race-guarded by re-reading `armed` + `armed_at_ms`. Sequence: `WakeRecovery { stage: "zero_sample_abort" }` → `usb_watchdog.on_recording_finished(0, elapsed)` (may cycle per existing gates) → `utils::cancel_current_operation` (finalizes session + hides overlay; monitor thread holds no locks, no deadlock) → `restart_microphone_if_needed()`. Then one bounded auto-retry: Toggle mode + bound hotkey → ONE synthetic press via `handle_shortcut_event` on `spawn_blocking` (never inline — 2026-10-17 lesson), emitting `retry_started`; rebuild failure emits `retry_failed` + native notification; PushToTalk/HoldOrToggle → notification only.
8. **`src-tauri/src/logging.rs`** — `zero_sample_abort`, `device_list_rebuild`, `retry_started`, `retry_failed` mapped to Warn in the `WakeRecovery` level table.
9. **`src-tauri/src/sleep_wake.rs`** — `start_native_wake_observer` registers a second selector (`screensDidWake:` → `NSWorkspaceScreensDidWakeNotification`) on the same observer. New `on_screens_wake`: 120s self-dedupe (`LAST_SCREENS_WAKE_EPOCH`), same `should_arm_bypass` settings gate as `on_system_wake`, and **arms only when `device_present_in_usb_tree(&device_name)` is false** — a plain screensaver fade (device present) is log-only. On arm: `arm_wake_bypass(WAKE_BYPASS_TTL_SECS)` + `WakeRecovery { stage: "screens_wake_device_absent" }` + the shared `spawn_wake_recovery_thread` (thread body factored out of `on_system_wake`). Decision logic unit-tested in `screens_wake_decision`.
10. **`src-tauri/src/actions/router.rs` / `src/shortcut/mod.rs`** — `send_macos_notification` and the `handler` module made crate-visible for the retry path.

### Key Insight

"Recency proves a stream is *running*, not that it is *delivering*; an armed recording with zero max-RMS is dead regardless of heartbeats." Every liveness layer built on last-chunk timestamps shares one blind spot: a zombie device that delivers silence looks exactly as alive as one delivering speech. The only evidence that cannot lie is the content of what arrives — max-RMS over an armed window — paired with a failure counter that mere opens cannot reset.

### Verification recipe

1. Configure the USB watchdog for the dock-attached mic, always-on microphone enabled, Toggle activation.
2. Let displays sleep (or unplug/replug the dock) to force the tree re-enumeration, then dictate within ~10 s of wake.
3. Expected: the first press either works or cancels itself within ~3 s of arming, the overlay closes cleanly (no ghost session — check `get_session_history` shows the session failed, not stuck), the stream rebuilds, and recording restarts without a second press.
4. Grep `WakeRecovery` events in `handy-events.jsonl`: expect `screens_wake_device_absent` or `device_list_rebuild`, then `zero_sample_abort` and `retry_started` (or `retry_failed` + notification if the rebuild failed).
5. Cancel mid-recording (Cmd+Esc): the tracker session must finalize immediately instead of lingering.

### Deep-review addendum (2026-11-08, second pass)

Follow-up review of the fixes above confirmed two majors and found one new major; all fixed the same day. Symbols referenced as shipped.

- **Clone-owned threads ran full `Drop` (the big one, pre-existing since Aug).** `AudioRecordingManager` is `Clone + Drop`; the deferred startup open (`new()`), `start_device_monitor`'s standing rebuild, and `cancel_recording`'s recovery worker each moved a by-value clone into a thread, and the clone's `Drop` at thread end ran `stop_device_monitor()` + `liveness_stop.store(true)` — killing both monitors seconds after launch in always-on mode. This also made the device-list rebuild and zero-RMS abort fixes above inert (their threads died with the monitors). All three sites now capture only the `AppHandle` and resolve via `try_state::<Arc<AudioRecordingManager>>()` with a not-yet-managed skip guard; the manager is no longer `Clone` at all, permanently closing this bug class.
- **Guard-across-join.** `stop_device_monitor` and `Drop` held the `parking_lot` guard across `join()` (if-let scrutinee temporaries live to the end of the statement). Both now bind the `JoinHandle` out, then join. The self-join panic disappears with the clone removal (FIX 1), so no explicit self-join check was added.
- **Abort ordering.** The zero-RMS abort now runs `cancel_current_operation` FIRST (un-arms + hides) and watchdog accounting AFTER, because `on_recording_finished(0, …)` can block 6-15s in a power cycle; `armed`/`armed_at_ms` are re-checked immediately before cancel, and the coordinator must be `Idle` immediately before a synthetic press.
- **Retry budget.** `auto_retry_allowed` is one-shot: armed only by real delivery (stop path with samples, or the monitor seeing `max_level > ZERO_SAMPLE_MAX_RMS` on an armed recording), consumed by exactly one press (`retry_decision(budget, coordinator_idle)`), so a still-dead stream cannot loop retries.
- **Muted-mic false positive.** The standing path now records delivered max-RMS into `CaptureControl::pre_arm_max_rms` (reset on `disarm()`); `should_abort_dead_recording` aborts only on the zombie signature (nonzero pre-arm, digital-zero in-arm). Pre-arm-silent (muted / dead since boot) → no abort, no notification loop.
- **Session snapshot.** `cancel_current_operation` snapshots `current_session_id()` at entry, before any blocking step; step 9 fails that captured id, and `fail_session`'s mismatch put-back keeps a session started mid-cancel untouched.

## Router completion teardown clobbered a concurrent recording's overlay/visualizer (2026-11-04)

### Problem

When a router-mode transcription A was still filing/processing and the user started a second recording B, A's completion path could close or suppress B's visualizer even though B was a separate, still-active session. The user saw the recording pill, visualizer bars, cancel button, and live captions disappear mid-recording.

### Root Causes

1. **`Command::ProcessingFinished` had no session identity.** The coordinator's `process_command` arm only checked `Stage::Processing`; A's late finish therefore reset B's stage to Idle and emitted `AppState::Idle { session_id: B }`, which the frontend's stale guard accepted because the ids matched.
2. **`set_confirming` / `set_processing_with_binding` sampled `current_overlay_session()` at emit time.** If A's pipeline landed after B started, these setters stamped B's session id and emitted `Confirming`/`Processing`, causing the frontend to stop rendering the recording pill and visualizer bars.
3. **The frontend `router-result` listener was session-blind.** It set `routerResult` unconditionally; `RecordingOverlay` then hid the pill (`showPill = isVisible && !routerResult`) and forced click-through for up to `ROUTER_RESULT_DISPLAY_MS`.
4. **The router thread's delayed 5-second hide was unguarded for `set_tray_state(Idle)`** and re-sampled `OVERLAY_SESSION` inside the delayed closure, by which time B's id was current, so `hide_recording_overlay`'s guard passed.

### Fix

1. **`src-tauri/src/transcription_coordinator.rs`**:
   - Added `session_id: Option<u64>` to `Command::ProcessingFinished`. `Some(id)` only acts when `id == CoordinatorCore.current_session`; `None` preserves the legacy forced/untagged contract.
   - Added `current_session: u64` to `CoordinatorCore` and mirrored it from the thread loop right after `start()` allocates a new session.
   - Added the pure helper `router_emission_is_stale(owner_session, current_session)`.
   - Made `set_confirming` and `set_processing_with_binding` take `owner_session` and no-op when the overlay session has moved on, preventing stale emissions from overwriting shared AppState.

2. **`src-tauri/src/actions/transcribe.rs`**:
   - `FinishGuard` now carries an `Option<u64>` owner session captured at construction and passes it to `notify_processing_finished` in `Drop`.

3. **`src-tauri/src/actions/router.rs`**:
   - Captured `owner_session` at the top of the async stop block (before the session can be bumped by a newer recording).
   - Passed `owner_session` to `FinishGuard`, `set_confirming`, and `set_processing_with_binding`.
   - Tagged the router thread's `notify_processing_finished(Some(owner_session))` so it cannot free B's stage.
   - Guarded the 5-second delayed hide/tray thread with a session check: skip both calls if `current_overlay_session() != hide_session`.

4. **Frontend (`src/overlay/`)**:
   - New `src/overlay/lib/overlayView.ts` with pure `deriveOverlayView({ backendStateKind, backendIsVisible, backendIsRouter, routerResult })` enforcing a "recording wins" rule: `routerResultActive = routerResult !== null && !isRecording`.
   - `useRouterPreview` now accepts `isRecording` and drops `router-result` events while a recording is active (using a ref so the once-registered listener sees the latest prop).
   - `RecordingOverlay` consumes `deriveOverlayView` for `isVisible`, `isRouter`, `routerResultActive`, and `showPill`, and gates click-through and `LiveCaptionsBox`/`RouterResultDisplay` on `routerResultActive`.

### Key Insight

"Each recording an independent process" cannot mean literal concurrent pipelines — the coordinator deliberately serializes the mic. The actionable meaning is **session-scoped lifecycle events**: every teardown, state emission, and visual arbitration must carry the originating session id and no-op when a newer session has taken over. The notification channel remains A's feedback when B holds the overlay.

- Residual (out of scope, 2026-11-04): `transcription-preview` and `mic-level` events remain session-less; a stale router-result arriving while B is in Processing/Confirming (not Recording) still displays; `set_confirming`/`set_processing_with_binding` skip paths and the delayed 5s hide thread lack production-level tests.

## Post-wake USB recovery: deferred cycle + first-failure bypass (2026-10-23)

### Problem

After macOS wake-from-sleep, the USB microphone recovery path could:
- skip the power cycle entirely if Handy was busy when the device re-appeared,
- give up after only 10 seconds when the device was absent from `uhubctl`,
- require two consecutive mic-open failures before triggering a cycle even on the very first failure after wake, and
- invoke `force_power_cycle()` from the wake thread, which races with the cooldown guard and the liveness monitor.

### Root Causes

1. **Busy-skip was one-shot**: the wake thread checked `AppState` once after the 10s device wait and simply returned if busy. A recording that started during the wait aborted recovery permanently.
2. **Absent-device retry window was too short**: the device was only polled for 10s before giving up, even though USB hubs and macOS USB services can take much longer to re-enumerate after deep sleep.
3. **Threshold required two failures even post-wake**: `UsbWatchdog::on_mic_open_failed()` needed `consecutive_failures >= fail_threshold` (default 2) before cycling. The first post-wake mic-open failure therefore did not trigger immediate recovery.
4. **Duplicate wake notifications**: `NSWorkspaceDidWakeNotification` and the wall-clock polling detector could both fire for the same wake, and macOS sometimes delivers multiple native wake notifications in quick succession.
5. **`force_power_cycle()` from the wake thread**: this async entry point could collide with the liveness monitor's own restart logic and did not emit the structured `UsbWatchdogCycle` event.

### Fix

1. **`src-tauri/src/usb_watchdog.rs`**:
   - Added `wake_bypass_until_epoch: AtomicU64` and `arm_wake_bypass()` / `wake_bypass_active()` APIs. The bypass arms for 300s after wake and is consumed on the very next mic-open failure, making the first failure skip both threshold and cooldown.
   - Added a pure `cycle_decision()` helper so tests can verify the decision logic without spawning `uhubctl`.
   - Reworked `on_mic_open_failed()` to count the failure, check the bypass, consume it, and call `power_cycle_blocking_with(skip_cooldown)`.
   - Refactored `power_cycle_blocking()` into `power_cycle_blocking_with(skip_cooldown)`, keeping the `cycling` swap guard and all other mechanics identical. `power_cycle_blocking()` wraps the cooldown-respecting path; new `wake_recovery_cycle_blocking()` exposes that path to the wake thread.
   - Emitted the previously-unused structured `UsbWatchdogCycle { device, success }` event from both `power_cycle_blocking_with()` and `force_power_cycle()`'s spawned thread.

2. **`src-tauri/src/sleep_wake.rs`**:
   - Added 30s duplicate-wake suppression (`LAST_WAKE_EPOCH`) with a structured `duplicate_wake` event and a bypass TTL refresh.
   - Rewrote the wake-recovery thread into four bounded phases:
     - Phase 1: settle 2s, then poll `device_present_in_usb_tree()` every 1s up to 10s.
     - Phase 2: retry every 10s for up to 180s if the device is still absent.
     - Phase 3: poll `AppState` every 2s for up to 120s, deferring the cycle while busy.
     - Phase 4: run `wake_recovery_cycle_blocking()` and `restart_microphone_if_needed()` when idle.
   - Added structured `WakeRecovery` events for every stage (`wake_detected`, `device_wait`, `deferred_busy`, `deferred_gave_up`, `cycle_started`, `cycle_result`, `cycle_skipped`).
   - Added pure `is_idle()` helper covering `Idle { .. } | None` and unit-tested it for every `AppState` variant.

3. **`src-tauri/src/logging.rs`**:
   - Added `WakeRecovery { stage, reason, device_seen_secs }` structured event.
   - Mapped `deferred_gave_up`, `cycle_skipped`, and `cycle_result` with non-success reason to `Warn`; all other wake stages to `Info`.

4. **`src-tauri/src/managers/audio.rs`** (minimal hunk):
   - Made `start_liveness_monitor()` skip its dead-stream check while `usb_watchdog.is_cycling()` is true, preventing the monitor from fighting an in-progress wake/bypass cycle.

### Key Insight

Cooldown arbitrates between the wake thread and the bypass: the wake thread uses `wake_recovery_cycle_blocking()` (cooldown respected), while the post-wake first-failure bypass uses `power_cycle_blocking_with(true)` (cooldown skipped). Grace still wins over bypass (`cycle_decision()` checks `grace` first), so a freshly cycled mic is not immediately re-cycled. Replacing the async `force_power_cycle()` call with a blocking, idle-checked cycle removes the race with the liveness monitor and guarantees the device has re-enumerated before `restart_microphone_if_needed()` runs.

## Router Confirmation Countdown Too Slow (2026-10-21)

### Problem

- The transcribe-with-router confirmation overlay felt slow because it auto-sent every router route after a fixed 4.5-second delay.

### Root Cause

- `ROUTING_COUNTDOWN_MS` was hardcoded to `4500` ms in both `src/overlay/hooks/useRouterPreview.ts` and `src/overlay/components/RouterResultDisplay.tsx` (progress bar denominator), so every route waited 4.5 seconds before sending.

### Fix

- Shortened `ROUTING_COUNTDOWN_MS` from `4500` to `2000` in:
  - `src/overlay/hooks/useRouterPreview.ts`
  - `src/overlay/components/RouterResultDisplay.tsx`
- The countdown UI string (`overlay.confirming` / "Sending in") and timer display already derive seconds from the live `countdown` state, so no i18n hardcoded values needed changing.

## USB Watchdog Wake Recovery: zombie AlwaysOn stream silently skipped wake cycle (2026-10-21)

### Problem

- With `always_on: true` and the USB watchdog configured (RØDE VideoMic NTG), macOS wake-from-sleep did **not** trigger a USB power cycle
- The mic stream stayed zombie: open according to Handy, but not delivering audio
- The next dictation would either capture silence or fail to start until the app was restarted

### Root Causes

1. **`on_system_wake()` trusted `AudioRecordingManager::check_stream_health()`** to decide whether to cycle. For an idle always-on stream (no active recording sink), the handy-recorder crate never marks the stream failed — its `NoAudio` / `Stalled` watchdog only fires when callbacks stop while a sink is running. An idle post-wake stream therefore reports `(is_open=true, is_alive=true)`, so the cycle was skipped.

2. **`UsbWatchdog::on_mic_open_failed()` had no callers on the mic-open failure path**. `start_microphone_stream()` propagated open errors with `?` without counting them toward the watchdog threshold, and `start_liveness_monitor()` logged restart failures but never called `on_mic_open_failed()`.

### Fixes

1. **Added `device_present_in_usb_tree()`** in `src-tauri/src/usb_watchdog.rs` as a non-retriable snapshot of whether the configured device is currently visible to `uhubctl`.

2. **Rewrote the wake-recovery thread in `src-tauri/src/sleep_wake.rs`**:
   - Polls `device_present_in_usb_tree()` every 1s for up to 10s to wait for post-wake USB re-enumeration.
   - Re-checks `AppState` immediately before cycling; if a recording started during the wait, the cycle is skipped.
   - Cycles the USB hub unconditionally when the device is seen (no longer consults `check_stream_health`).
   - Emits `usb-power-cycle-failed` with reason `device absent after wake wait` if the device never re-appears.

3. **Updated `check_stream_health()` doc comment** to state explicitly that it is a diagnostic snapshot only, not a wake-recovery authority.

4. **Made `start_microphone_stream()` count open failures** in `src-tauri/src/managers/audio.rs`:
   - Calls `usb_watchdog.on_mic_open_failed()` once after the built-in cache-miss retry also fails.
   - If the watchdog returns `true` (a blocking power cycle completed), invalidates the device cache, re-resolves the mic, and tries to open one final time.
   - Added a diagnostics-only `warn!` when the selected mic is missing and the watchdog is enabled, but deliberately does **not** count the fallback as an open failure.

5. **Made `start_liveness_monitor()` count restart failures** in both the invariant self-heal branch and the main dead-stream restart branch by calling `usb_watchdog.on_mic_open_failed()` after each failed `start_microphone_stream()`.

### Key Insight

An idle-stream "alive" verdict is **meaningless** at the crate boundary because detectability requires a running sink. Wake recovery must therefore be unconditional (cycle after confirming device presence and verifying the app is idle), not gated on a health check that cannot see the failure it is trying to prevent.

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
- No AGC, no normalization, no auto-calibration existed anywhere in the pipeline (backend or frontend). The frontend `useVisualizer.ts` only has a low-audio _warning_ (threshold 0.05), not gain.

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

**Root cause:** putting an _adaptation_ variable (noise floor) in the _output path_ means cold-start values directly gate the output. Until the adaptation converges, the output is wrong. And at 0.1%/frame, convergence takes ~30 seconds.

**Fixed by removing `noise_floor` from the output entirely** and using:

1. A **fixed wide window** (`-70` to `-5`, 65dB range) — any mic's speech falls within the visible range from frame 1. A quiet mic at -55dB → normalized = 15/65 = 0.23 (visible, not zero).
2. A **peak-based gain boost** on the normalized value. Peak **snaps up instantly** (not EMA) when speech exceeds it, so the boost is computed on the very first speech frame. Peak decays slowly (`0.003`, half-life ~4s) so brief pauses don't collapse it.
3. Peak starts at **0.0** — so the first speech frame immediately sets it and bars fill. No cold-start delay.

**Key insight:** never put a slowly-adapting variable directly in the output path. Use a fixed window for the base (so cold-start is always visible) and apply adaptation as a _boost on top_ (so it only improves things, never gates them). Instant snap-up for peaks avoids the EMA convergence delay entirely.

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

## Cancel Button Main-Thread Deadlock (2026-07-17)

### Problem

- User clicks cancel button (or stop hotkey) during recording → app goes "not responding" (beachball), visualizer freezes at that instant, requires force-quit.
- Forensic evidence from `~/Library/Logs/com.pais.handy/handy-events.jsonl`: a `RecordingStarted` event with **no corresponding `RecordingStopped`**, no `ShortcutTriggered` for cancel, no `AppCrashed` — the structured logger (which needs the main thread) went silent the moment the cancel click arrived.
- Visualizer (separate audio thread) kept moving right up until the cancel click, then froze — signature of main-thread deadlock, not a crash.

### Root Cause

- `cancel_operation` was a **synchronous** Tauri command (`pub fn`, not `async`). In Tauri 2.x, sync commands run on the **main thread**.
- `cancel_current_operation` → `audio_manager.cancel_recording()` did `self.state.lock()` (parking_lot, **no timeout**, blocks forever) on the main thread.
- Once the main thread wedged: the webview event loop stopped (visualizer froze because `emit_levels` events stopped being delivered), the HandyKeys global shortcut event tap stopped (subsequent stop hotkey presses couldn't be delivered — explaining "stop didn't register"), and the structured logger stopped.
- This **invalidates** the #1 hypothesis in `research/overlay-freeze-bug-investigation-report-2025-07-09.md` ("Optimistic Cancel Desync"). That report assumed cancel _succeeded_ and the frontend hid too eagerly. The logs show cancel **never returned** — it deadlocked the main thread.

### Fix

1. **`commands/mod.rs`**: Made `cancel_operation` `async` + offloaded to `tauri::async_runtime::spawn_blocking`. The blocking cancel work now runs on a worker thread, never the main thread. Frontend `await commands.cancelOperation()` was already compatible (tauri-specta generates async functions regardless).
2. **`managers/audio.rs` `cancel_recording()`**: Replaced unbounded `self.state.lock()` with `self.state.try_lock_for(2s)`. On timeout, forces `is_recording=false` and returns (lets the app recover instead of hanging forever).
3. **Instrumentation**: Added `[CANCEL-TIMING]` logs to `cancel_current_operation` (8 steps timed) and `cancel_recording` (every lock acquisition + wait timed, with thread ids). If the freeze recurs, `grep CANCEL-TIMING handy.log` pinpoints the exact blocking call.

### Files Changed

- `src-tauri/src/commands/mod.rs` — `cancel_operation` async + `spawn_blocking`
- `src-tauri/src/utils.rs` — `[CANCEL-TIMING]` instrumentation on all 8 steps
- `src-tauri/src/managers/audio.rs` — `cancel_recording` bounded `try_lock_for(2s)` + instrumentation

### Lesson

- **Synchronous Tauri commands run on the main thread.** Any sync command that takes a `parking_lot::Mutex::lock()` with no timeout is a deadlock risk. The stop handler (`actions/transcribe.rs`) already does this correctly via `tauri::async_runtime::spawn` + `tm.try_lock_for(10s)` — the cancel handler was the inconsistency.
- `parking_lot::Mutex` only removes _poisoning_; it does **not** add timeouts. An unbounded `lock()` on the main thread is still a deadlock hazard. Always use `try_lock_for` on code paths reachable from the main thread.
- "Not responding" + visualizer freeze + zero log output = main-thread deadlock, not a crash. A crash terminates the process; a deadlock leaves it alive but frozen (requiring force-quit, which is why the next log entry is a fresh `AppStarted`).

## Lock Hazard Audit + 4 Fixes (2026-07-17)

### Problem

After fixing the cancel-button deadlock, a comprehensive audit of all unbounded `parking_lot::Mutex::lock()` calls in the Rust backend revealed 4 additional lock hazards — including a latent AB-BA deadlock that could still freeze the app in always-on mode even after the cancel fix.

### Root Causes + Fixes

**1. AB-BA lock-ordering inversion (CRITICAL):**

- `try_start_recording` takes locks **state → recorder** (nested, `audio.rs:1345-1347`).
- `stop_microphone_stream` took locks **recorder → state** (nested, opposite order) — it held `recorder.lock()` across `state.lock()`.
- The **liveness monitor** (`audio.rs:474`) wakes every 3s in always-on mode and calls `stop_microphone_stream`. If a recording start and a liveness check run concurrently → classic AB-BA deadlock.
- This explains why the original freeze happened during an always-on recording: the liveness monitor is only active in always-on/BT-keep-alive mode (`audio.rs:500`), so the inversion can't trigger in on-demand mode.
- **Fix:** Restructured `stop_microphone_stream` to drop the recorder lock before taking the state lock — no nested locks, no inversion possible.

**2. `update_microphone_mode` on main thread (CRITICAL):**

- Sync Tauri command (`commands/audio.rs:191`) ran on the main thread, called `rm.update_mode()` which can trigger `start_microphone_stream()` → USB power cycle (10+ seconds blocking).
- **Fix:** Made it `async` + `spawn_blocking`, mirroring the `cancel_operation` fix pattern.

**3. `paste` Enigo lock held across sleep (MEDIUM):**

- `paste` (`clipboard.rs:858`) acquired `enigo_state.0.lock()` then held it across `std::thread::sleep(paste_delay_ms)` inside `paste_via_clipboard`. Starves other Enigo users on the main thread.
- **Fix:** Split into `prepare_clipboard_for_paste` (clipboard write + sleep, no lock) and `send_paste_keys_and_verify` (key-sending, needs lock). Lock acquired only for the brief key-send phase.

**4. Unbounded transcription manager locks (MEDIUM):**

- `get_model_load_status` and `unload_model_manually` (`commands/transcription.rs`) used unbounded `.lock()` while `load_model` on another thread holds the same lock for seconds.
- **Fix:** Replaced with `try_lock_for(2s)` (status) and `try_lock_for(5s)` (unload), returning an error on timeout.

### Files Changed

- `src-tauri/src/managers/audio.rs` — Fix 1 (AB-BA inversion)
- `src-tauri/src/commands/audio.rs` — Fix 2 (async update_microphone_mode)
- `src-tauri/src/clipboard.rs` — Fix 3 (paste lock scope)
- `src-tauri/src/commands/transcription.rs` — Fix 4 (bounded try_lock_for)
- 5 additional files touched by `cargo fmt` (formatting-only, zero functional changes)

### Lesson

- **Lock-ordering inversions are silent killers.** Two functions each taking two locks in opposite order won't deadlock until they run concurrently — which may only happen under specific conditions (always-on mode enabling the liveness monitor). Always audit for consistent lock ordering when two functions take the same pair of locks.
- **The liveness monitor is the always-on-specific risk multiplier.** It's a background thread that takes manager locks every 3s, creating concurrent lock contention that on-demand mode never sees. Any new always-on-related lock analysis must account for the liveness thread.

## Settings "Forgetting Changes" — Cache-Bypass Clobber (2026-07-17)

### Problem

- User reports settings frequently "forget" changes after being saved. Changes vanish on app restart.

### Root Cause

- **Three sources of truth** for settings: `SettingsCache` (RwLock, in-memory), `SettingsWriter.pending` (500ms debounced disk-write queue), and `tauri-plugin-store` (disk JSON).
- The frontend uses `get_settings_safe` / `write_settings_safe` which correctly update the cache. But ~45 internal backend call sites (`managers/model.rs`, `managers/audio.rs`, `managers/transcription.rs`, `shortcut/mod.rs`, `actions/transcribe.rs`, `actions/router.rs`, `clipboard.rs`) used the non-safe `get_settings()` / `write_settings()` variants which **bypassed the cache**:
  - `get_settings()` read straight from `tauri-plugin-store`'s in-memory cache (STALE — didn't see unflushed cache changes)
  - `write_settings()` scheduled a debounced disk write but did NOT update `SettingsCache`
- **Clobber scenario**: user saves setting A (cache updated, disk write pending) → backend calls `get_settings()` (gets stale struct without A) → mutates → `write_settings()` (schedules disk write of stale struct) → debounce fires → A is gone from disk → restart loses A.
- Worst offender: `auto_select_model_if_needed()` in `managers/model.rs` fires on every model state change.
- Contributing: `flush_settings` on `ExitRequested` used `block_in_place` + `block_on` which can deadlock during runtime teardown → trailing 500ms write dropped on quit. And `let _ = store.save()` silently discarded disk-write errors.

### Fix (in `src-tauri/src/settings/store.rs` only)

1. `get_settings()` now reads from `SettingsCache` first (via `try_state`), falls back to plugin-store ONLY when cache isn't managed yet (early startup) — emits a `warn!` so we'd see it at runtime.
2. `write_settings()` now updates `SettingsCache` before scheduling the debounced disk write — mirrors `write_settings_safe` exactly.
3. `write_settings_safe` / `write_settings_immediate_safe` simplified to thin wrappers delegating to the non-safe variants (cache logic now lives in one place).
4. `flush_settings` hardened: early-return if no pending write; 2-second `tokio::time::timeout` guard around `block_on(flush)`; on timeout, writes cache snapshot directly via `write_settings_immediate`.
5. Diagnostic logging with `[settings]` prefix: `debug!` write trace (5 representative fields, no secrets), `info!` flush events, `warn!` fallback paths, `error!` disk-write failures (replaced all `let _ = store.save()`), `trace!` chatty cache hits.

### Lesson

- **A "safe" wrapper that callers can bypass is a footgun.** When two API variants exist (`foo` and `foo_safe`), every internal caller will eventually use the wrong one. The fix here was to make the non-safe variant behave like the safe one, so callers can't get it wrong — rather than auditing 45 call sites.
- **Debounced writes + multiple sources of truth = stale-struct clobber.** The debounce itself wasn't the bug (each write is a full-struct snapshot, so coalescing is fine); the bug was that a stale snapshot could be queued AFTER a fresh one because the non-safe path didn't read the fresh cache.
- **`let _ = result;` is a silent-failure antipattern** for persistence operations. Always log disk-write errors.
- **Diagnostic logging is cheap insurance.** The `[settings]` prefix lets us grep `~/Library/Logs/com.pais.handy/handy.log` for the full settings trace if this ever recurs.

## Lock-Ordering Invariant Test + CI (2026-07-17)

### Problem

- The 2026-07-17 Lock Hazard Audit found an AB-BA deadlock in `stop_microphone_stream` (recorder → state) vs `try_start_recording` (state → recorder). The fix restructured `stop_microphone_stream` to drop the recorder lock before taking the state lock.
- No automated test existed to catch regressions of this class.

### Fix

- Created `src-tauri/tests/lock_ordering.rs` with 5 tests:
  1. `test_lock_ordering_consistent_no_deadlock` — 16 threads × 500 iterations, all 4 lock paths (start, stop, cancel, lazy_close). Verifies no deadlock with the CURRENT (fixed) order.
  2. `test_concurrent_start_stop_stress` — 8 threads × 1000 iterations, alternating start/stop paths.
  3. `buggy_order_deadlock` (#[ignore]) — deliberately uses the OLD inverted order (recorder → state nested) to prove the test can detect AB-BA inversions.
  4. `test_stop_order_no_nesting` — single-threaded 1000-iteration check that stop order uses separate lock acquisitions.
  5. `test_lock_ordering_cross_product` — pairwise concurrent test of all 4 lock acquisition patterns.
- Created `.github/workflows/ci.yml` with 3 jobs: `rust-check`, `rust-test`, `frontend-build`.

### Key Insight

- Lock-ordering tests should replicate the lock STRUCTURE (which `parking_lot::Mutex<()>` pair), not the full production types. This makes tests independent of AppHandle, audio devices, or other heavy infrastructure.
- `std::thread::JoinHandle` has no `join_timeout()`. Use a helper that spawns a watcher thread + `mpsc::channel` with `recv_timeout` to detect hung threads.
- AB-BA deadlock tests must be `#[ignore]`d — they genuinely hang forever when the bug exists.

## VAD Pre-Roll Discard Bug — First Word Cut Off (2026-10-02)

### Problem
- First word of dictation often missing ("Add the visual…" → "The visual…"), even with 1000ms pre-recording buffer.
- Reported 3× across days; two earlier latency fixes (Cmd::Start ordering, 200ms sleep, wake reload) were real but not the cause.

### Investigation method (this is the reusable part)
- Bisect empirically: app saves raw WAV + VAD WAV (= transcription input) + transcripts in history.db.
- Find failing takes via re-dictation pairs: JOIN consecutive history entries ≤60s apart on overlapping text prefixes.
- Re-transcribe stored WAVs with the same model via a tiny example harness (examples/transcribe_wav.rs) → raw heard "Add", vad didn't → loss in VAD stage.
- Sample-exact alignment (64-sample block greedy match with resync) showed vad = raw minus one hole [hotkey..hotkey+0.5-0.8s] containing exactly the first word.

### Root cause
- handle_frame() matched VadFrame::Speech(_) and appended only the current 16ms frame, discarding SmoothedVad's ~464ms pre-roll emission at speech onset.
- Soft/short first words ("Add", "She", "Can't") can't trigger the 4-frame VAD onset themselves → they lived only in the discarded pre-roll.
- Regression from f6c2ac1c (2026-06-28 noise-suppression refactor, Speech(buf) → Speech(_)).

### Fix (ddffa435)
- handle_frame appends the full emitted slice when noise suppression is off (emitted frames are byte-identical to originals).
- smoothed.rs onset emission skips already-emitted hangover frames (else speech→pause→speech duplicates ~350ms mid-recording — caught by deep review).
- Regression tests in smoothed.rs; cargo test 471 pass.

### Key insights
- Energy-preserving ≠ content-preserving: VAD passed all onset-timing checks yet dropped words; only model-level raw-vs-vad A/B exposed it.
- Don't trim leading silence before ASR — in testing it caused hallucinations ("Big Brother"); leading silence helps these models.
- Known limitation: noise-suppression-enabled users keep old behavior (suppressed pre-roll frames can't map back to originals).

## ESC-Cancel Lag + Visualizer Dead After Cancel (2026-10-17)

### Problem
- Cmd+Escape cancel of an active recording felt like a multi-second freeze ("very bad lag"), and afterwards the visualizer didn't appear properly on the next recording.
- Event-log forensics (handy-events.jsonl) showed the smoking gun: `RecordingStarted` emitted, then 20ms later `RecordingFailed error="Already recording"` (sessions s-20261017T052618.527Z, s-20261016T062500.906Z) — the audio manager was wedged in `RecordingState::Recording` while the coordinator/UI thought otherwise.

### Root causes (three verified defects, one chain)
1. **Manager-thread starvation (the lag):** `handle_shortcut_event` (shortcut/handler.rs) dispatched `CancelAction::start` INLINE on the handy-keys manager thread. `cancel_current_operation` (utils.rs) step 2 `unregister_cancel_shortcut` does `recv_timeout(5s)` against that SAME thread's command queue — a guaranteed ~5s self-wait on every ESC — plus `cancel_recording`'s `try_lock_for` waits. The whole hotkey loop starved during the block. CLI `--cancel` never showed this because `commands/mod.rs` `cancel_operation` already runs on `spawn_blocking` (live repro: 35-40ms).
2. **Escape hatch leaves state wedged (the visualizer symptom):** `cancel_recording`'s `state.try_lock_for(2s)` failure branch stored `is_recording=false` and silently RETURNED, leaving the enum as `Recording`. Then `transcription_coordinator`'s `start()` consulted `is_recording()` (enum-only!) → emitted `AppState::Recording` for a start that actually failed "Already recording" → overlay visible with no capture → no mic-level events → micDeadWarning/no bars.
3. **Latent self-deadlock:** `schedule_lazy_close` held `state.lock()` while calling `stop_microphone_stream`, whose Phase 2 does `*self.state.lock() = Idle` (parking_lot non-reentrant → permanent lock deadlock 30s after every recording end when lazy_stream_close=true; comment claimed it doesn't take the lock — false).

### Fix (2026-10-17, uncommitted)
- handler.rs + both lib.rs cancel call sites: dispatch via `tauri::async_runtime::spawn_blocking` (parity with the CLI path) — manager thread never blocks.
- `cancel_recording` escape hatch: no silent return; detaches `recovery_after_state_lock_timeout` (bounded 3-phase worker: recorder stop without waiting for the reply → gated state heal `Recording && !is_recording` → on-demand stream close/lazy close parity with no locks held).
- `try_start_recording` check 2: self-heal `Recording` + atomic false → Idle (`[RECOVERY]` log), killing the "Already recording" retry loop.
- New `AudioRecordingManager::is_recording_effective()` (enum Recording AND atomic true) used ONLY by the coordinator's start-vs-rollback decision; `is_recording()` stays enum-only so ESC stays armed during a wedge.
- `schedule_lazy_close`: drop the state guard after the gen+Idle check, re-check `close_generation`, then close — deadlock gone.

### Key insights
- Two sources of truth (`is_recording` AtomicBool vs `RecordingState` enum) is the wedge's enabler: the escape hatch cleared one and left the other. Self-heal paths must gate on the atomic, which is always cleared/stored under the state lock.
- Never call a method that takes `state.lock()` while holding `state.lock()` — parking_lot is non-reentrant and will deadlock silently. Audit every "close" helper for hidden lock acquisition.
- Inline work on the hotkey manager thread must never do channel round-trips to itself or wait on locks; every second there reads as "app dead" to the user.
- Empirical checks that mattered: CLI repro measured the happy path fast (35-40ms) which disproved "cancel is inherently slow" and pointed at threading; events jsonl (RecordingStarted→Failed pairs) pinpointed the wedge without needing a debugger.

## Upstream Sync Cycle 2026-10 (cdbc2239..9e57a0d7) — Complete (2026-10-20)

### What was ported (one-line overview)
161 upstream commits triaged into 61 applied (P0–P10, 34 fork commits),
13 already-ported, 5 superseded, 76 policy-skipped, 6 verify-resolved; the
engine stack was rebuilt on engine_supervisor child-process isolation +
transcribe-cpp 0.4.0 + GGUF-only models + handy-recorder capture, and the
shortcut layer moved to handy-keys 0.3.4. Registry: `Handy-Fork/UPSTREAM_SYNC.md`;
plan + per-phase evidence: `research/upstream-sync-2026-10-plan.md`.

### Notable technical learnings
- **handy-recorder swap + preroll re-port:** fork's 1046-line recorder.rs died;
  the first-word-cut fix survives as sink-path tests (`capture_sink_preroll`) —
  NS-off re-emits the full `VadFrame::Speech` slice (pre-roll + current) into
  out_buf; commands drain at the TOP of the consumer loop with `pending_stop`
  deferred to after the in-flight chunk. Evidence that the re-port is faithful:
  the fork's A/B harness method (raw-vs-vad WAV via examples) is preserved in
  REGRESSION-TEST-REQUIRED.
- **VAD 512/32ms geometry:** transcribe-cpp's Silero uses 512-sample/32ms
  frames (vad-rs was 480/30ms). Keep duration-based abstractions
  (`frames_for_duration_ms` arms) — never fixed frame counts — so engine
  geometry changes are one-arm edits.
- **transcribe-cpp 0.4.0 + engine_supervisor:** native ASR now runs in a child
  worker (`--transcribe-worker`); GPU faults kill the worker, not the app.
  Process-boundary semantics that bit: stdout must be captured before native
  code initializes; capabilities cross the protocol as a `Result`;
  `f32` frames channel via typed IPC; serde NaN breaks JSON (guard scores).
  Fork keeps ONE supervisor per manager (main + live-captions), vs upstream's
  single manager.
- **Vocabulary API replaces initial_prompt:** `RunOptions.vocabulary` (0.4.0)
  + `LoadedInfo.supports_vocabulary`; skip the fork's n-gram fuzzy correction
  EXACTLY when vocabulary was sent (`custom_words_prompted`), both batch and
  streaming; non-vocabulary models keep initial_prompt-era fallback.
- **Chunking lives in the batch path:** fork WINS on live decode
  (StreamRouter = live-captions model); `Chunker` (40s+ gate, CJK-aware
  join) engages at stop; cancel = supervisor `CancelMark`/`transcribe_since`
  taken at the top of `transcribe_internal` PLUS `StreamingCancelFlag`
  between chunks. `transcription/policy.rs` must be a SIBLING file
  (`managers/transcription_policy.rs`) — a `pub mod policy` inside
  transcription.rs vanishes under the CI mock swap.
- **clap/CLI-arg dispatch lesson (P6):** worker dispatch happens in `main.rs`
  BEFORE Tauri builds an app — CLI args that select a process mode must be
  parsed before any single-instance/plugin machinery runs, or the second
  instance's arg handoff races the worker spawn.
- **bindings hand-regen pattern:** FE `bindings.ts` is generated by
  tauri-specta during a debug run; hand-porting Rust commands without a run
  leaves bindings stale — regen via the debug-run export and commit it with
  the command change (done for suspend-all, HoldOrToggle, long-form, vocabulary).
- **Overlay/lifecycle closure (P10):** hidden overlay must render null AND stop
  its interval loops (a 60ms decay timer re-rendered the hidden WebView
  ~16x/sec forever); model-load events already existed in the fork, so the
  loading indicator was pure FE wiring (only completed/failed end a load —
  `unloaded` races mid-load).

### OPEN items (REGRESSION-TEST-REQUIRED — manual mic/OS, no local GGUF E2E)
- D5: macOS sleep/wake + recording-hotkey pass on handy-keys 0.3.4.
- P7: first-word-intact preroll A/B (NS off), rapid stop→start, hot-unplug.
- P9: 5+ min chunked recording + cancel-all; 30s no-chunk; toggle off;
  streaming custom-words smoke.
- P8: ONNX→GGUF migration flow with simulated old install.
- P10: cold-start "Loading model…" indicator; hidden-overlay null render.
- Pre-existing `cargo fmt` drift (42 sites, predates the cycle) — optional
  cleanup before next release build.
