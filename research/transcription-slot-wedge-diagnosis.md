# Transcription Slot Wedge — `feature/streaming-live-captions`

**Date:** 2026-08-26
**Branch:** `feature/streaming-live-captions` (HEAD `3dffacdb`)
**Symptom:** `TranscriptionFailed` with `"Another transcription is in progress"` (`AppError::TranscriptionBusy`, `errors.rs:42`), `model_id: unknown`, <3ms, after a normal idle gap — the `is_transcribing` slot is stuck `true` between recordings.

**Key architectural fact that shapes everything below:**
There are **two** `TranscriptionManager` instances (each its own `is_transcribing` slot + `Arc<Mutex<>>`):

- Main TM (`lib.rs:233`) — used by the **final batch** transcription (`transcribe_with_hook`, `router.rs:331` / `transcribe.rs:438`).
- Live-captions TM (`LiveCaptionsTranscriptionManager`, `lib.rs:237`, `lib.rs:279`) — used by the **streaming worker** and the streaming **batch-fallback callback**.

The `TranscriptionBusy` in the logs is a **final** transcription failing, i.e. the **main** TM's slot is stuck. It is distinct from the (benign) `TranscriptionBusy` the streaming callback swallows when the stream worker holds the engine.

---

## 1. Live-captions streaming callback — `audio.rs`

**Spawn:** `tauri::async_runtime::spawn_blocking(...)` (`audio.rs:310`). One blocking task per audio frame.

**Cancellation flag checked:** a `StreamingCancelFlag` (atomic) checked _without lock_ at entry (`audio.rs:287`, `:295`) and re-checked after acquiring the blocking thread (`:312`) and after transcription (`:352`). Also a `close_generation` re-check at `:322` and `:362` to discard stale frames.

**The critical call — `audio.rs:336-343`:**

```rust
let tm_guard = tm.lock();              // unbounded parking_lot lock (audio.rs:336)
...
let transcription_result = tm_guard.transcribe(samples);   // audio.rs:343
```

- `tm` is `Arc<Mutex<TranscriptionManager>>`. `tm_guard` is held for the **entire** `transcribe()` call, which includes `transcribe_internal`'s **engine call** (`transcription.rs:1755`).
- `transcribe()` internally acquires the `is_transcribing` slot (`TranscribingGuard`) and runs the engine.
- **Wedge risk:** if the engine call (`session.run(...)`, `:1755`) blocks forever (GPU context loss after sleep/wake, transcribe-cpp deadlock), `transcribe()` never returns:
  - `tm_guard` (the external manager mutex) is **held** — wedged.
  - The `TranscribingGuard` is never dropped — the `is_transcribing` slot stays **true**.
- `transcribe_internal` only drops the **engine** mutex (`engine_guard`) before the engine call (`:1689`). The **external** manager mutex held by the streaming callback's `tm_guard` is a _separate_ lock that is NOT released during the engine call.

**Error handling (`audio.rs:344-401`):** `Ok(empty)` skipped; `Err(ModelNotLoaded)` re-kicks `initiate_model_load()`; `Err(TranscriptionBusy)` logged at debug and swallowed (this is the _intended_ skip when the stream worker holds the engine); any other `Err` warns. The failure is never propagated to the coordinator.

**Can the worker hang inside the engine call?** Yes — and it holds BOTH the manager mutex AND the slot while hung. `catch_unwind` (`:1715`) only catches panics that _unwind_; a GPU/FFI call that blocks forever neither returns nor panics, so neither the guard nor the mutex is ever released.

**Any path leave the guard dropped/not-dropped?**

- **Not dropped:** the wedged engine call (above). This is the stuck-slot root cause.
- **Dropped correctly:** every normal return / `?` / panic-unwind in `transcribe()`/`transcribe_internal` releases `TranscribingGuard::drop`. The `TranscribingGuard` is a local in `transcribe()` (`:1487`) that lives to end of the `transcribe_internal` call — normal returns, `Err` returns, and the `catch_unwind` panic path all drop it. So the _only_ leak is a genuine non-unwinding hang.

---

## 2. Slot primitives — `transcription.rs`

**`try_acquire_transcribing_slot` (`:1425-1499`):**

- Loops while `*is_transcribing` is true.
- Each iteration: reads `is_transcribing_since`; if held ≥ `TRANSCRIBING_SLOT_FORCE_RELEASE_AFTER` (60s, `:57`), force-releases (`:1448-1456`): sets `flag=false`, `since=None`, `notify_all`.
- `cancel_check()` between waits; waits in 100 ms slices via `condvar.wait_for` (`:1471-1475`).
- On acquire: `flag=true`, `since=Some(now)`, returns `TranscribingGuard`.

**`TranscribingGuard::drop` (`:2026-2048`)** — the `3dffacdb` stale-guard fix:

- Clears the slot **only if** `*flag && (*timestamp == Some(acquired_at))` (`still_mine`, `:2033-2034`).
- If a later owner force-released the slot and re-acquired it with a new timestamp, the stale owner's drop becomes a no-op and does **not** clobber the new owner's flag/timestamp. This is the invariant any fix must preserve.

**Streaming worker `run_stream_worker` (`:1051-1326`):**

- Spawned with `thread::spawn` (`:1048`).
- Holds a `StreamWorkerGuard` (`:280-305`) that clears `active_stream_worker` / `active_engine_lease` / `stream_active` on ANY exit (normal, early, or panic-unwind) via compare_exchange token guards.
- **Important:** the stream worker does **NOT** hold the `is_transcribing` slot. It holds the _engine_ via `active_engine_lease` (`:1115-1124`) and the engine pool (taken at `:1125`). It loops on `rx.recv()` (`:1238`) processing `Feed`/`Finalize`/`Cancel`.
- **Wedge risk (separate from the slot bug):** if `stream.feed()`/`stream.finalize()` (`:1244`, `:1268`) block forever, the worker never returns the engine and never clears its lease → the model reports "unloaded"/engine unavailable. This produces `ModelNotLoaded`, not the stuck slot. It is a _second_ failure mode, not the one in the logs.

**How streaming and batch share the slot:**

- The `is_transcribing` slot is only touched by `transcribe()` (`:1548`) and `transcribe_with_hook()` (`:1514`) — i.e., **batch paths**. The streaming _worker_ bypasses it entirely.
- The live-captions **batch-fallback** callback (`audio.rs:343`) calls `transcribe()` → competes for the slot with the **final** `transcribe_with_hook()`. This is why a wedged streaming-callback transcription wedges the slot that the final path needs.

**Is the 60s watchdog sufficient? Its edge cases — this is the crux:**

The watchdog is **purely reactive**: it only executes inside `try_acquire_transcribing_slot` on the _waiter's_ thread. It is never driven by a timer. Consequences:

1. **It never runs during an idle gap.** If the slot is wedged and the user does nothing for >60s, the slot silently stays `true`. It recovers only when a _new_ waiter arrives.
2. **It can be starved by the manager-mutex hold.** In the streaming-callback wedge, the wedged `tm_guard` holds the external manager mutex. The **next** streaming callback blocks on the unbounded `tm.lock()` (`audio.rs:336`) and **never reaches** `try_acquire_transcribing_slot`, so the watchdog never runs. The final path uses `try_lock_for(10s)` (`transcribe.rs:430`) and also fails on the _mutex_ before it ever reaches the slot watchdog. **This is exactly the "never runs because the stuck worker never lets a waiter wake" edge case the user suspected — but the mechanism is the manager-mutex hold, not the condvar.** (The waiter's `wait_for(100ms)` does time out on its own, so the condvar itself is fine; the blocker is the _external mutex_ gate in front of the slot wait.)
3. **Even when it fires, it only frees the slot, not the engine or the manager mutex.** After the watchdog force-releases, the wedged streaming callback still holds the manager mutex and the engine (taken out of the pool at `:1681`). A concurrent final transcription would still block on `try_lock_for(10s)` or, if it got through, find `lock_engine().is_none()` → `ModelNotLoaded`. So the slot watchdog alone does **not** unblock the pipeline in the streaming-callback wedge.

---

## 3. Coordinator — `transcription_coordinator.rs`

- **Transitions:** Recording → Processing on stop (`:471`, `:535`); Processing → Idle via `ProcessingFinished` (from the async stop task's `FinishGuard`, `actions/transcribe.rs:53-68`) or via `ProcessingTimeout` (`:668-683`).
- **`ProcessingTimeout` (`:668-683`)**: on timeout it does `hard_cap_extension = ZERO`, `transition_to(Idle)`, `sync_state`, resets handy-keys pressed state. **It does NOT clear the `is_transcribing` slot.** The coordinator has no handle to (and no concept of) the slot. So the coordinator can go Idle while the main TM's slot is still `true`.
- **Does a stuck slot prevent the coordinator from becoming Idle?** No. The coordinator becomes Idle on its own 30s `PROCESSING_TIMEOUT` (`:88`) regardless of the slot. **The problem is the next recording**, not the coordinator: after Idle, the next `start()` → stop → `transcribe_with_hook()` hits the stuck slot and returns `TranscriptionBusy` in ~1ms. So the coordinator self-heals its _state_, but the _slot_ stays wedged, so every subsequent recording fails. This matches the "recurring across days" pattern in the logs.
- **`ProcessingTimeout` is not a slot recovery path** and cannot be, since it lacks access to the slot.

---

## 4. Minimal, safe fix strategy

Constraints:

- Must recover a single wedged worker without permanently blocking all future transcriptions.
- Must NOT force-release a legitimately long transcription early.
- Must NOT regress the `3dffacdb` stale-guard drop invariant (`still_mine` timestamp check).

### Root-cause statement

The slot watchdog is reactive-only and is gated behind the external manager mutex, which the streaming callback holds during the (potentially unbounded) engine call. A wedged engine call leaves the slot `true`, the manager mutex held, and the engine leased out, and **no timer exists to break that** — so the pipeline is permanently wedged until restart.

### Recommended fix (layered, each part independent and low-risk)

**A. Drive the slot watchdog from a timer thread (primary fix).**

- Extend the existing idle-watcher thread (`transcription.rs:388-450`, already spawned per manager, wakes every 10 s) to also, on each tick, check `is_transcribing_since` and force-release when held ≥ `TRANSCRIBING_SLOT_FORCE_RELEASE_AFTER` (60s), using the _identical_ logic already at `:1448-1456` (`flag=false`, `since=None`, `notify_all`).
- This requires only `&self` on the slot atomics/mutexes — **no external manager mutex** — so it runs even while the wedged streaming callback holds the manager mutex.
- **Why it does not regress `3dffacdb`:** the force-release sets `since=None` and notifies; the wedged owner's eventual `TranscribingGuard::drop` computes `still_mine = (*since == Some(acquired_at))` → `None == Some(...)` → `false` → **no-op**. A new owner that re-acquires gets a fresh timestamp and is never clobbered. This is exactly the invariant the `3dffacdb` fix established and tested.
- **Why it does not kill legitimate long transcriptions:** it fires only when the slot has been held >60s _and_ (recommended) `active_engine_lease == 0` and `!is_streaming()` — i.e., it skips while a streaming session is genuinely in flight (the stream worker never sets the slot anyway, so this is a defensive guard). A batch transcription of very long audio could in theory exceed 60s; in that case the cost is one concurrent engine attempt that transcribe-cpp's `compute_lock` will reject (graceful `TranscriptionBusy`), not corruption.

**B. Stop the streaming callback from blocking forever on the manager mutex.**

- Change the unbounded `tm.lock()` at `audio.rs:336` to `try_lock_for(...)` (e.g. 1–2 s) and, on timeout, drop the frame (skip) instead of blocking. Prevents a wedged callback from also blocking all _future_ streaming callbacks and compounding the wedge. This is a pure safety improvement with no effect on the healthy path.

**C. On recovery, force the wedged engine state to reload.**

- When the timer watchdog force-releases (part A), also clear `current_model_id` and reset `consecutive_engine_failures` so the next transcription treats the model as unloaded and rebuilds a fresh engine. Rationale: the wedged worker still physically owns the old engine (taken at `:1681` or leased at `:1125`); we cannot reclaim it, so we must ensure the _next_ run does not try to reuse a poisoned engine. `return_engine` (`:1330`) already guards this: after we clear `current_model_id`, the wedged worker's eventual return sees a model-id mismatch and drops its stale engine instead of returning it, and `StreamWorkerGuard` clears its lease flags on exit. This makes recovery self-consistent.
- Keep this within the same force-release block so it is atomic with the slot clear and still compatible with `3dffacdb`.

### Explicitly NOT recommended

- Do **not** make `ProcessingTimeout` (coordinator) clear the slot — it has no clean handle to the main TM's slot and would introduce cross-thread coupling.
- Do **not** add a `force_clear_transcribing()` that unlocks the manager mutex from another thread — parking_lot guards cannot be forcibly unlocked, and doing so would be unsafe.
- Do **not** lower the 60s threshold meaningfully — it protects legitimate long transcriptions; the timer-driven version already bounds the worst-case recovery to ~60s even during an idle gap.

### Files / lines referenced

- `src-tauri/src/managers/audio.rs:310, 336, 343` (streaming callback spawn, unbounded lock, transcribe call)
- `src-tauri/src/managers/transcription.rs:57` (`TRANSCRIBING_SLOT_FORCE_RELEASE_AFTER`), `:1448-1456` (reactive watchdog), `:1425-1499` (`try_acquire_transcribing_slot`), `:2026-2048` (`TranscribingGuard::drop`, `3dffacdb` invariant), `:388-450` (existing idle-watcher thread to extend), `:1051-1326` (`run_stream_worker`), `:1681,1689` (engine take + engine-lock release), `:1330` (`return_engine` model-id guard), `:1755` (unbounded engine call)
- `src-tauri/src/transcription_coordinator.rs:88` (`PROCESSING_TIMEOUT`), `:668-683` (`ProcessingTimeout` does not clear slot)
- `src-tauri/src/actions/transcribe.rs:430` (`try_lock_for(10s)`), `:369` (`finalize_stream` lock), `:449-453` (`TranscriptionBusy` diagnostic)
- `src-tauri/src/errors.rs:42` (`TranscriptionBusy`)

### Open item / honest uncertainty

The log shows a **<3ms** failure for the _final_ transcription. `transcribe_with_hook` uses `cancel_check = || false` (`:1533`), so its slot-wait should block up to 30s, not fail in <3ms. A <3ms `TranscriptionBusy` from the final path is only explainable by the manager-mutex `try_lock_for(10s)` path failing fast (unlikely — that's 10s) or by the slot being force-released-and-reacquired in a tight window, or by the streaming callback's `transcribe()` (cancel_check = `is_streaming_cancelled`) being the source during the stop window. The recurring across-days signature and the stuck-true symptom are unambiguous; the precise sub-10ms timing of a single occurrence is not fully pinned. Recommend adding the diagnostic the prior report suggested (log slot state at the `TranscriptionBusy` return sites) to confirm on the next occurrence.
