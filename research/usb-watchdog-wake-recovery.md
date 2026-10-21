# USB Watchdog Wake Recovery — Zombie AlwaysOn Stream Investigation

**Status: SOLVED (2026-10-21)**

**Date:** 2026-10-21
**Scope:** macOS sleep/wake USB power-cycle recovery for always-on microphones
**Symbols:** `on_system_wake`, `UsbWatchdog::force_power_cycle`, `AudioRecordingManager::check_stream_health`, `UsbWatchdog::on_mic_open_failed`, `start_microphone_stream`, `start_liveness_monitor`

---

## Symptom

With the USB watchdog enabled and a configured device (RØDE VideoMic NTG), Handy's always-on microphone was **not** power-cycled when the Mac woke from sleep. The stream appeared open but was effectively zombie (no audio). Subsequent dictations would capture silence or fail until the app was restarted.

---

## The Old Gate Chain in `on_system_wake`

`src-tauri/src/sleep_wake.rs::on_system_wake()` spawned a recovery thread that did:

1. Sleep 2s.
2. Call `AudioRecordingManager::check_stream_health()` → `(is_open, is_alive)`.
3. Skip if `!is_open`.
4. Skip if `is_alive`.
5. Otherwise call `UsbWatchdog::force_power_cycle()`.

The intent was to avoid unnecessary cycles when the stream was healthy. In practice it created the opposite bug.

---

## Why `check_stream_health` Is Blind for Idle AlwaysOn Streams

`check_stream_health` in `src-tauri/src/managers/audio.rs` computes:

```rust
let failed = recorder.lock().as_ref().is_some_and(|m| m.needs_reopen());
let sink_running = self.is_recording() || self.standing_active.load(...);
let chunks_fresh = sink_running && { ... recent chunk timestamp ... };
let stream_alive = !failed && (!sink_running || chunks_fresh);
```

The handy-recorder crate flips its internal `failed` flag only via its own `NoAudio` / `Stalled` watchdog, which measures callback activity **while a sink is armed**. When the always-on stream is idle (no active recording), the crate discards audio and no sink is running, so there are no callbacks to count. A post-wake zombie idle stream therefore returns `(is_open=true, is_alive=true)`.

At the boundary of the crate we cannot reliably detect a dead idle stream. Wake recovery must be unconditional, gated only on device presence and application idle state.

---

## Dead `on_mic_open_failed` Callers

`UsbWatchdog::on_mic_open_failed()` was the intended entry point for automatic power cycling, but it had no path into the mic-open failure flow:

- `start_microphone_stream()` propagated `handy_recorder::Error` with `?` and never called `on_mic_open_failed()`.
- `start_liveness_monitor()` had two `start_microphone_stream()` failure branches (invariant self-heal and main dead-stream restart) that only logged.

The watchdog's consecutive-failure counter could only be incremented by `on_recording_finished(0 samples)` or `on_silent_transcription()`, neither of which fires for a stream that is simply open-but-dead on wake.

---

## Fallback-Masking Note (Deliberate Residual Gap)

When the selected microphone is missing, `resolve_microphone_device()` falls back to the system default and sets `unavailable_selected_microphone`. A diagnostics-only `warn!` was added, but this fallback is **not** counted as an open failure. Reasoning:

- A successful later recording resets the failure counter anyway.
- Treating the fallback as an open failure would cause the watchdog to power-cycle during benign transitions (dock/undock, clamshell mode changes) where the selected mic legitimately disappears.

This is a documented residual gap, not a bug.

---

## Fix Description

### 1. Device-presence helper

`src-tauri/src/usb_watchdog.rs` gained:

```rust
pub fn device_present_in_usb_tree(name: &str) -> bool {
    resolve_device(name).is_some()
}
```

No retries inside the helper; callers poll.

### 2. Unconditional wake recovery with wait + `AppState` guard

`on_system_wake()` now spawns a thread that:

- Sleeps 2s.
- Polls `device_present_in_usb_tree()` every 1s for up to 10s (post-wake re-enumeration can lag).
- Re-checks `AppState` via `TranscriptionCoordinator::get_state()` immediately before cycling; skips if not `Idle`.
- Calls `force_power_cycle()` when the device is seen.
- Emits `usb-power-cycle-failed` with reason `device absent after wake wait` if the device never appears.

`check_stream_health()` was removed from the wake path and its doc comment was updated to state it is diagnostic-only.

### 3. Open-failure counting + retry

`start_microphone_stream()` now:

- Calls `usb_watchdog.on_mic_open_failed()` once after the cache-miss retry also fails.
- If the watchdog returns `true` (blocking cycle completed), invalidates the cache, re-resolves, and tries one final open.
- Updates `resolution` through whichever attempt succeeds so the fallback-persist block uses the correct device.

### 4. Liveness-monitor restart counting

`start_liveness_monitor()` calls `usb_watchdog.on_mic_open_failed()` in both restart-failure branches.

---

## Verification

- `cargo check` green.
- `grep` confirms no `check_stream_health()` call remains in `sleep_wake.rs`.
- `on_mic_open_failed` has three callers in `managers/audio.rs`.
- `device_present_in_usb_tree` exists and is used by the wake recovery thread.
- `force_power_cycle()` still calls `restart_microphone_if_needed()` on success (unchanged).

---

## Related

- `learning-log.md` entry: **USB Watchdog Wake Recovery: zombie AlwaysOn stream silently skipped wake cycle (2026-10-21)**

## Follow-ups (not blockers)

These review findings are known limitations of the current wake-recovery fix and are documented here for future work:

- `device_present_in_usb_tree()` polls uhubctl via `Command::output()` with no timeout; a wedged USB stack at wake could hang the wake-recovery poll loop (pre-existing exposure, newly amplified). A timeout wrapper would help.
- If the uhubctl binary is missing, the wake path burns the full 10s wait and reports "device absent after wake wait" instead of naming the real cause (could check `is_uhubctl_available()` first).
- The failure counter is not reset on a successful stream open (only grace clears), so a stale count=1 from a long-ago flaky open can combine with one new genuine failure to reach threshold 2.
- The silent-fallback `warn!` fires before the fallback open result is known, so it can print when the fallback open also failed.
- Cosmetic: wake presence-wait log prints loop index, not true elapsed seconds (excludes the initial 2s delay).
