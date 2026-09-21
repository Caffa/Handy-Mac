# Stuck-State Investigation — `feature/streaming-live-captions`

**Date:** 2026-08-26
**Branch:** `feature/streaming-live-captions` (HEAD `fb29b971`)
**Symptom:** After a transcription recording, text was NOT pasted and NOT added to history; pressing the recording hotkey afterwards does NOT start a new recording. App appears "stuck".

---

## TL;DR — Root cause (confirmed by logs)

The events log (`~/Library/Logs/com.pais.handy/handy-events.jsonl`) contains the exact stuck scenario. The **last** recording before the hang:

```
RecordingStarted 09:34:18
RecordingStopped 09:34:32  (duration_ms=10445, sample_count=80800)
TranscriptionStarted 09:34:32  (model_id: "unknown")
TranscriptionFailed 09:34:32  (duration_ms=1)  error="Another transcription is in progress. Please wait and try again."
```

`"Another transcription is in progress"` is `AppError::TranscriptionBusy` (`src-tauri/src/errors.rs:42`). The final batch transcription failed in **1 ms** because the `is_transcribing` slot was **already held** by a prior in-flight transcription that never released it. `model_id: "unknown"` confirms the failure happened before any model was resolved — i.e. at the `is_transcribing` wait in `transcribe_with_hook`/`transcribe`.

This is the classic "recording finished but never pasted, and the app can't start another recording" signature: the `is_transcribing` slot is stuck `true`, so every subsequent `transcribe()`/`transcribe_with_hook()` returns `TranscriptionBusy` immediately, and the coordinator's `start()` path never gets a usable transcription.

---

## Top root causes (ranked)

### 1. `is_transcribing` slot left `true` by a wedged/panicked streaming worker — the primary suspect

The `is_transcribing` flag is a `ParkingLotMutex<bool>` guarded by the RAII `TranscribingGuard` (`src-tauri/src/managers/transcription.rs:1896-1908`):

```rust
struct TranscribingGuard {
    flag: Arc<ParkingLotMutex<bool>>,
    condvar: Arc<ParkingLotCondvar>,
}
impl Drop for TranscribingGuard {
    fn drop(&mut self) {
        let mut flag = self.flag.lock();
        *flag = false;
        self.condvar.notify_all();
    }
}
```

The flag is only cleared when the `TranscribingGuard` is **dropped**. If the thread that owns the guard is wedged (blocked forever inside `transcribe_internal` → `transcribe-cpp` engine call, or a GPU/streaming call that never returns), the guard is never dropped and `is_transcribing` stays `true` forever. Every subsequent transcription then fails with `TranscriptionBusy` in ~1 ms — exactly what the log shows.

**Where the wedge can originate (live-captions branch):**

- The streaming callback (`src-tauri/src/managers/audio.rs:310-402`) runs `tm_guard.transcribe(samples)` inside `spawn_blocking`. If the engine call hangs (GPU context loss, sleep/wake, transcribe-cpp deadlock), the `TranscribingGuard` inside `transcribe()` is never dropped.
- The live-captions stream worker (`run_stream_worker`, `transcription.rs:996-1271`) holds the engine via `active_engine_lease` and calls `stream.feed()`/`stream.finalize()` in a loop. If `stream.feed()` blocks forever, the worker never returns the engine and never clears its lease.

**Why the app can't start a new recording:** `try_start_recording` (`audio.rs:1312`) only rejects if `state` is not `Idle`. But the coordinator's `start()` (`transcription_coordinator.rs:1371-1413`) checks `is_recording()` after dispatching; if the recorder state is stuck `Recording` (because `stop_recording` never completed its smart-stop wait), the coordinator rolls back to Idle but the recorder is still live. Combined with a stuck `is_transcribing`, the pipeline is permanently wedged.

### 2. Unbounded `is_loading` wait in the batch path — can hang forever if the load thread panics

`transcribe_internal` (`transcription.rs:1510-1522`):

```rust
{
    let mut is_loading = self.is_loading.lock().unwrap();
    while *is_loading {
        is_loading = self.loading_condvar.wait(is_loading).unwrap();
    }
    let engine_guard = self.lock_engine();
    if engine_guard.is_none() {
        return Err(AppError::ModelNotLoaded);
    }
}
```

This is an **unbounded** `condvar.wait()` with no timeout. The `is_loading` flag is cleared by the model-load thread (`initiate_model_load_for`, `transcription.rs:919-931`):

```rust
thread::spawn(move || {
    if let Err(e) = self_clone.load_model(&model_id) {
        error!("Failed to load model '{}': {}", model_id, e);
    }
    let mut is_loading = self_clone.is_loading.lock().unwrap();
    *is_loading = false;
    self_clone.loading_condvar.notify_all();
});
```

**If `load_model` panics** (it is NOT wrapped in `catch_unwind` — only the transcription engine call at line 1599 is), the spawned thread dies before clearing `is_loading`, and the `notify_all()` never runs. Every subsequent `transcribe_internal` then blocks forever on `condvar.wait()`. This is a hard hang, not just a busy-error. The streaming worker path (`run_stream_worker`, line 1016-1052) was already hardened with `wait_timeout(100ms)` + `try_recv` polling, but the **batch** path at line 1513-1516 was NOT given the same treatment — an inconsistency introduced by the live-captions work.

### 3. Coordinator `Processing` stage stuck / `FinishGuard` not firing

The coordinator transitions to `Stage::Processing` on stop (`transcription_coordinator.rs:1415-1438`) and relies on `FinishGuard` (`actions/transcribe.rs:53-68`) to send `ProcessingFinished` when the async stop task completes. If the async stop task is wedged (e.g. blocked on the `is_transcribing` wait at line 1513, or on `tm.try_lock_for(10s)` at line 430), `FinishGuard` never fires and the coordinator stays in `Processing` for up to `PROCESSING_TIMEOUT` (30s, `transcription_coordinator.rs:88`). During that window, `process_command`'s `Input` handler for a new press hits the `_ =>` arm and calls `try_recover_stop`, which returns `false` if the recorder isn't recording — so the new hotkey press is **ignored** (`transcription_coordinator.rs:572-578`):

```rust
} else {
    warn!(
        "Ignoring stop press for '{}': pipeline busy (stage={:?})",
        binding_id, self.stage
    );
    StageAction::None
}
```

So even after the 30s timeout auto-resets to Idle, if `is_transcribing` is still stuck `true`, the next recording's transcription fails with `TranscriptionBusy` again — the app appears permanently stuck.

---

## Exact code snippets (suspicious logic)

**A. `is_transcribing` slot — only cleared on guard drop** (`transcription.rs:1398-1430`):

```rust
{
    let mut is_transcribing = self.is_transcribing.lock();
    let max_wait = Duration::from_secs(30);
    let wait_deadline = std::time::Instant::now() + max_wait;
    while *is_transcribing {
        let remaining = wait_deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            warn!("Timed out waiting for previous transcription to complete");
            return Err(AppError::TranscriptionBusy);   // <-- 1ms failure in log
        }
        let _timed_out = self.transcribing_condvar.wait_for(&mut is_transcribing, remaining);
    }
}
{
    let mut flag = self.is_transcribing.lock();
    *flag = true;
}
let guard = TranscribingGuard { flag: ..., condvar: ... };  // cleared only on drop
```

**B. Unbounded `is_loading` wait in batch path** (`transcription.rs:1513-1516`):

```rust
let mut is_loading = self.is_loading.lock().unwrap();
while *is_loading {
    is_loading = self.loading_condvar.wait(is_loading).unwrap();  // no timeout
}
```

**C. Model-load thread not panic-safe** (`transcription.rs:919-931`):

```rust
thread::spawn(move || {
    if let Err(e) = self_clone.load_model(&model_id) { ... }  // load_model can panic
    let mut is_loading = self_clone.is_loading.lock().unwrap();
    *is_loading = false;
    self_clone.loading_condvar.notify_all();  // never reached if load_model panics
});
```

**D. Coordinator ignores new press while Processing** (`transcription_coordinator.rs:572-578`):

```rust
} else {
    warn!("Ignoring stop press for '{}': pipeline busy (stage={:?})", binding_id, self.stage);
    StageAction::None
}
```

---

## Recommended fix approach (do NOT implement yet)

1. **Make the batch `is_loading` wait bounded and panic-safe** (highest priority — this is the hard-hang path).
   - Wrap the model-load thread body in `catch_unwind` so a panic still clears `is_loading` and calls `notify_all()` (mirror the `StreamWorkerGuard` pattern).
   - Replace the unbounded `condvar.wait()` at `transcription.rs:1513-1516` with `wait_timeout(100ms)` + a deadline, returning `AppError::ModelNotLoaded` (or a new `ModelLoadTimeout`) instead of hanging forever. This matches the already-hardened streaming path.

2. **Add a watchdog for the `is_transcribing` slot.**
   - The `TranscribingGuard` is the only thing that clears the flag. Add a timeout: if a transcription holds the slot longer than a sane ceiling (e.g. 2× the 30s wait), force-clear it and log a `[WEDGE]` diagnostic. This directly prevents the "recording finished but never pasted + can't start again" symptom.
   - Consider a generation/lease token on the `is_transcribing` slot (like `active_engine_lease`) so a stale worker can't clear a newer worker's flag.

3. **Harden the streaming callback against engine hangs.**
   - The `spawn_blocking` streaming callback (`audio.rs:310-402`) holds the TM lock and calls `transcribe()`. If the engine call hangs, both the lock and the `is_transcribing` slot are stuck. Add a timeout around the engine call, or run it on a thread that can be abandoned and have its guard force-cleared by the watchdog in (2).

4. **Make the coordinator's Processing stage self-heal faster.**
   - When `ProcessingTimeout` fires, also force-clear the `is_transcribing` slot (via a new `TranscriptionManager::force_clear_transcribing()`), so the next recording doesn't immediately fail with `TranscriptionBusy`.

5. **Add a `[WEDGE]` diagnostic log** at the point where `TranscriptionBusy` is returned, including the current `is_transcribing` value and the last activity timestamp, so future occurrences are immediately identifiable.

---

## Logs / crash evidence found

**Primary evidence — `~/Library/Logs/com.pais.handy/handy-events.jsonl`** (last entries before hang):

```
{"always_on":true,"evt":"RecordingStarted","sid":"s-20260826T093418.376Z","ts":"2026-08-26T09:34:18.376Z"}
{"action":"transcribe","binding_id":"transcribe","evt":"ShortcutTriggered","ts":"2026-08-26T09:34:18.378Z"}
{"duration_ms":10445,"evt":"RecordingStopped","sample_count":80800,"sid":"s-20260826T093418.376Z","ts":"2026-08-26T09:34:32.784Z"}
{"evt":"TranscriptionStarted","model_id":"unknown","sid":"s-20260826T093418.376Z","ts":"2026-08-26T09:34:32.785Z"}
{"duration_ms":1,"error":"Another transcription is in progress. Please wait and try again.","evt":"TranscriptionFailed","lvl":"ERROR","model_id":"unknown","sid":"s-20260826T093418.376Z","ts":"2026-08-26T09:34:32.786Z"}
```

- `model_id: "unknown"` + `duration_ms: 1` = failed at the `is_transcribing` wait, before any model resolution.
- This exact `TranscriptionBusy` failure also appears on 08-21, 08-22, 08-24 — a recurring pattern, not a one-off.

**Recurring `TranscriptionBusy` failures in the same log:**

```
2026-08-21T02:12:26  "Another transcription is in progress" (2ms)
2026-08-22T04:07:57  "Another transcription is in progress" (3ms)
2026-08-24T04:29:54  "Another transcription is in progress" (0ms)
2026-08-26T09:34:32  "Another transcription is in progress" (1ms)  ← the stuck one
```

**Crash reports** (`~/Library/Logs/DiagnosticReports/Retired/handy-*.ips`, 2026-08-20) — both are **startup** crashes from the history-DB migration bug (`DatabaseTooFarAhead` / `duplicate column name: file_name_vad`), which was fixed by commit `c6732dc0`. These are **not** related to the current stuck-state bug, but confirm the app has a history of startup panics.

**`handy.log`** — dominated by `[visualizer]` and `[STREAM-CB-GATE] blocked: not recording` debug noise. No explicit panic/wedge lines captured for the stuck event (the relevant `[STREAM-CB]`/`[STOP-DIAGNOSTIC]` lines are gated to `debug!` and drowned out). The `[STREAM-CB-GATE] blocked: not recording` lines continuing after the stuck event confirm the recorder is no longer capturing but the app is not in a clean Idle state.

**Settings (`settings_store.json`):** `live_captions_enabled: true`, `live_captions_model: None` (falls back to main TM), `selected_model: parakeet-tdt-0.6b-v2`, `keyboard_implementation: handy_keys`, `push_to_talk: false`. So the live-captions streaming callback is wired to the **main** TM/cancel-flag pair (the `use_live_captions_model = false` branch in `audio.rs:247-255`), meaning the streaming callback and the final batch transcription share the same `is_transcribing` slot — making the wedge in root cause #1 directly able to block the final paste.

---

## Files reviewed

- `src-tauri/src/managers/audio.rs` (stop_recording, cancel_recording, try_start_recording, streaming callback wiring)
- `src-tauri/src/managers/transcription.rs` (transcribe/transcribe_with_hook, is_loading wait, stream worker, TranscribingGuard, model load)
- `src-tauri/src/transcription_coordinator.rs` (state machine, process_command, start/stop, try_recover_stop)
- `src-tauri/src/streaming_cancel_flag.rs`
- `src-tauri/src/shortcut/mod.rs`, `tauri_impl.rs`, `handler.rs`, `handy_keys.rs`
- `src-tauri/src/actions/transcribe.rs` (start/stop, FinishGuard, async stop task)
- `src-tauri/src/audio_toolkit/audio/recorder.rs` (streaming callback, stop_send)
- `src-tauri/src/session.rs`, `src-tauri/src/errors.rs`, `src-tauri/src/managers/transcription_retry.rs`
- Recent fix commits: `bfacd272` (wedged engine recovery), `b783bad8` (live captions TM/cancel-flag wiring), `a357944d` (coordinator hardening)
