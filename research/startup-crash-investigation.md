# Startup Crash Investigation — `abort()` / SIGABRT on `feature/streaming-live-captions`

## TL;DR (lead with the answer)

**The crash stack is almost certainly unreliable (garbage/corrupted) and should NOT be read as a causal chain.** The frames interleave unrelated subsystems (`default_post_process_models` allocation, `VecDeque::grow`/`BTreeMap` allocator internals, `symphonia` MP4 atom parsing, specta `run::export`, and the router `stop` closure) that have **no plausible causal relationship**. You cannot get from "router stop 5-second delayed-overlay-hide thread" to "post-process-models allocation" to "symphonia MP4 parsing" in one call path.

The realistic root cause is **macOS native heap corruption → SIGABRT**, *which the code itself explicitly documents* in `src-tauri/src/audio_toolkit/audio/recorder.rs:431-440`:

> *"Pause the stream before dropping to prevent heap corruption. On macOS, cpal::Stream::pause() is asynchronous — CoreAudio's IO thread may still be executing the last callback invocation when pause() returns. If we drop immediately, internal buffers are deallocated while the callback is still in-flight, causing nanov2 malloc heap corruption → SIGABRT."*

The `abort()` happens in the **allocator** (malloc's `abort()` on detected heap corruption) inside an unrelated allocation — here, `default_post_process_models()` (types.rs:925) which is one of the first main-thread allocations at startup. The other stack frames are where the corrupt allocator's bookkeeping happened to point, and the router `stop` closure frame is a red herring from the corrupted/unwindable stack.

Both Hypotheses B and C, as stated, are **not** supported as root causes:
- **B (OOM from default-construction allocations):** The default construction allocates only a handful of small strings (see allocation audit). It cannot OOM/abort on its own. It is merely the *victim* allocation that triggers the allocator's corruption check.
- **C (router stop runs on main thread at launch):** The router `stop` path is only reachable via `ACTION_MAP` dispatch (shortcut event / CLI / signal), and line 733's delayed-hide thread requires the *entire* record→transcribe→route→subprocess-complete→5s-sleep pipeline to have run. That cannot happen ~4s after a fresh launch. The frame is garbage from heap corruption.

---

## 1. What I found for Hypothesis B — allocation audit of the default-construction path

### `default_post_process_models()` — types.rs:920-929
```rust
pub(crate) fn default_post_process_models() -> HashMap<String, String> {
    let mut map = HashMap::new();
    for provider in default_post_process_providers() {   // builds ~9 providers
        map.insert(provider.id.clone(), default_model_for_provider(&provider.id));
    }
    map
}
```
- `default_post_process_providers()` (types.rs:820-903) builds a `Vec` of ~9 `PostProcessProvider` structs (each ~4 small `String`s + 2 bools + an `Option<String>`). Total: ~40 small heap allocations.
- The returned `HashMap` has ~9 entries of `(String, String)`. Tiny.

### `get_default_settings()` — defaults.rs:8-187
Constructs one `AppSettings` on the main thread. Relevant heap allocation surface:
- `bindings: HashMap` with 4 entries (transcribe, transcribe_with_post_process, transcribe_with_router, cancel). **Note the duplicate `transcribe_with_router` insert** (defaults.rs:62-73 and defaults.rs:85-97). Because it's a `HashMap::insert`, the second insert silently **overwrites** the first — no duplicate entry results. It is wasteful (one dead `ShortcutBinding` is allocated and immediately dropped) but **not a correctness or OOM problem**. The dead allocation is ~5 small strings, immediately freed.
- `post_process_providers` (9), `post_process_api_keys` (~9), `post_process_models` (~9), `post_process_prompts` (1 with a ~1KB string), `spelling_dictionary: SpellingDictionary::default()` (an enum, zero heap), plus ~40 scalar/String fields.

**Conclusion for B:** Total one-shot default construction is on the order of **a few hundred small heap allocations / a few KB**. This is orders of magnitude below anything that could cause OOM or an `abort()`. `get_default_settings()` is called many times at startup (in `load_or_create_app_settings`, `get_settings`, `init_shortcuts`/`init` paths, and the specta `get_default_settings` command), but each call's allocations are freed (or cloned into the cache once). There is **no** runaway/quadratic allocation. `default_post_process_models` is a *victim*, not the *cause*, of the abort.

> Note on the duplicate binding: it is harmless for startup (HashMap dedups), but it is confusing and should be cleaned up. See Recommendations.

---

## 2. What I found for Hypothesis C — why router stop appears in the stack

### The `stop` closure at router.rs:733
`TranscribeWithRouterAction::stop` (router.rs:224-918):
1. Unregisters cancel shortcut, sets `StreamingCancelFlag`.
2. Spawns the whole record→save-WAV→transcribe→route pipeline on `tauri::async_runtime::spawn` (router.rs:275).
3. Only after the **router subprocess** (`run_router_subprocess`, router.rs:577) completes **successfully** does it reach line 733:
   ```rust
   std::thread::spawn(move || {
       std::thread::sleep(Duration::from_secs(5));
       utils::hide_recording_overlay(&app_delayed);
       change_tray_icon(&app_delayed, TrayIconState::Idle);
   });
   ```
   This is a **fire-and-forget 5-second delayed overlay-hide**, spawned on a **dedicated std::thread** (not the main thread).

### Why it cannot run at launch on the main thread
- `TranscribeWithRouterAction::stop` is only reachable through `ACTION_MAP` (actions/mod.rs:67-81) via `handle_shortcut_event` (shortcut/handler.rs:29-73), which itself is reached from:
  - Tauri global-shortcut callback (tauri_impl.rs:107-117) — only after user presses a key,
  - HandyKeys manager thread (handy_keys.rs:141) — only after a registered hotkey fires,
  - CLI/signal (signal_handle.rs, lib.rs:793-799) — `--toggle-transcription`/SIGUSR1/SIGUSR2 send **`start`** (`send_input(..., is_pressed=true, push_to_talk=false)`), which only triggers `start()`, not `stop()`.
- Line 733 additionally requires the **full record→transcribe→route→subprocess-complete** pipeline plus a 5s sleep. A fresh launch ~4s in has not even opened a microphone stream (unless always-on/BT, which still doesn't start router recording automatically).
- Therefore **router `stop` at launch is impossible**. The frame is an artifact of the corrupted stack.

---

## 3. Root cause hypothesis

**Primary (most likely): macOS native heap corruption → allocator `abort()`.**
- The abort is `SIGABRT` inside allocator internals (`VecDeque::grow`, `BTreeMap` balancing) — the classic signature of malloc/nanov2 detecting a corrupted heap, not of Rust's panic-unwind (Rust's `panic = "unwind"` in release, `abort` only via the allocator or an explicit `abort()`).
- The code itself documents the exact mechanism in recorder.rs:431-440 (cpal stream drop racing CoreAudio's IO callback → heap corruption → SIGABRT).
- The unrelated `symphonia_format_isomp4::atoms::AtomIterator::read_atom` frame is telling: symphonia is pulled in **only via `rodio`** (Cargo.lock: `rodio` → `symphonia`; `rodio` is a git fork used in `audio_feedback.rs:5,140`). `symphonia`'s MP4 atom parser is exercised when **playing `.wav`/audio feedback**. This is *audio pipeline* code — consistent with a heap corruption originating in the audio path (cpal/CoreAudio), not in settings defaults.
- `default_post_process_models` (types.rs:925) is simply the first/allocation the corrupt allocator hits at startup on the main thread.

**Secondary (contributing concurrency pressure at startup):**
- `AudioRecordingManager::new` (managers/audio.rs:445-511) starts a microphone stream at startup when **Bluetooth output is detected** (`bt_keep_alive`) or `always_on_microphone`. On a launch with BT audio, a cpal stream is created/dropped on the startup critical path, and the liveness monitor (every 3s) / device monitor may also restart streams — all interacting with the new streaming-callback machinery added on this branch (recorder.rs streaming worker, `StreamingCancelFlag`, `StreamRouter`). This raises the chance of the documented pause/drop race.

---

## 4. Recommended fixes

### Fix the actual heap-corruption (root cause)
1. **`src-tauri/src/audio_toolkit/audio/recorder.rs:431-440`** — The existing comment documents the risk; the current mitigation is `stream.pause(); sleep(100ms); drop(stream)`. This is a race *mitigation*, not a *fix*. Investigate a deterministic handshake so the CoreAudio callback has fully drained before the stream is dropped (e.g. keep the `cpal::Stream` alive in a scoped guard owned by the consumer thread until after the callback's buffer period is confirmed idle, or route through a proper drain barrier). The 100ms fixed sleep is fragile across buffer sizes/sample rates.
2. **`src-tauri/src/managers/audio.rs:445-511` (startup)** — On startup with `bt_keep_alive`/`always_on_microphone`, avoid creating the mic stream inside `new()` on the main thread; defer `start_microphone_stream()` to a background thread (mirror the pattern used for model pre-load at lib.rs:1009-1016) so the CoreAudio device lifecycle never races with the main-thread allocation/settings work during the first seconds.
3. **Reduce churn of `recreate_recorder()`/`start_microphone_stream()`** in the liveness monitor (managers/audio.rs:527+) and device hot-plug monitor — each stop/start of a cpal stream is a drop risk. Add a mutex/cooldown so the monitors cannot restart a stream concurrently with a `stop_recording()`/drop.

### Cleanups (correctness/hygiene, not the crash)
4. **`src-tauri/src/settings/defaults.rs:75-97`** — Remove the **duplicate `transcribe_with_router` insert**. The second insert overwrites the first; the dead first `ShortcutBinding` allocation is wasted and the code is misleading. Keep a single entry.

### Diagnostic hardening (prove the root cause)
5. **Add an `abort`/`SIGABRT` handler that dumps all thread names + a backtrace** before re-raising, so the next crash shows which thread (e.g. the CoreAudio/worker thread) was corrupting memory, instead of the victim thread's corrupted stack. The existing panic hook (logging.rs) only captures Rust panics, not allocator `abort()`.
6. **Enable MallocStackLogging / run under `MallocGuardEdges` / ASan** in the repro to catch the *first* bad write rather than the symptom. Given the code comment, the audio drop path is the prime suspect to instrument.

---

## 5. Key evidence references
- Crash signature (SIGABRT, allocator internals, unrelated symphonia/MP4 frame): **heap corruption**, not OOM.
- recorder.rs:431-440 — code-author comment: cpal stream drop → nanov2 heap corruption → SIGABRT.
- Cargo.lock: `rodio` (git fork) → `symphonia`; audio_feedback.rs:5,140 uses `rodio::play` — explains the symphonia frame via the audio path.
- defaults.rs:62-97 — duplicate `transcribe_with_router` insert (overwrite, harmless to startup, wasteful).
- router.rs:275 (async spawn) + :733 (5s delayed-hide thread) — stop pipeline cannot run at a fresh launch.
- signal_handle.rs:16-22, lib.rs:793-799 — CLI/signal paths only send `start` (`is_pressed=true`), never `stop`.
- Profile: `panic = "unwind"` in release (Cargo.toml:145) — confirms `abort()` is the allocator, not Rust panic-abort.
