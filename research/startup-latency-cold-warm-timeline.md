# Handy startup latency: cold-start vs warm-start timeline

**Scope:** Read-only code analysis of `stable-handy` (branch `main`, HEAD `1cabf608`), focused on the
serial path between a transcribe-hotkey press and the first processed/recorded audio sample, plus the
wake-from-sleep regression. No code was changed.

**Method:** Static trace of `actions/transcribe.rs`, `managers/audio.rs`,
`audio_toolkit/audio/recorder.rs`, `managers/transcription.rs`, `sleep_wake.rs`, `lib.rs`,
`transcription_coordinator.rs`. Durations are *estimates* from in-code comments plus external
whisper.cpp / CoreAudio / ONNX-Runtime references; they are labelled and carry uncertainty. They are
not measurements taken on this machine.

---

## 0. TL;DR — direct answers

- **On-demand hotkey → first audio sample processed** is serial through: coordinator (≤30 ms debounce)
  → TM-lock model kick (blocks up to **2 s** if the manager mutex is contended) → VAD preload spawn →
  overlay → `try_start_recording` → device resolve + **synchronous VAD construction** + **cpal/CoreAudio
  stream open + play** + a **hard-coded 200 ms sleep** → `Cmd::Start` is only dequeued by the consumer
  thread *after the next audio chunk arrives*. Realistic serial wall-clock before the first *recorded*
  sample: **~250–500 ms on a built-in mic**, **~0.6–3 s on Bluetooth HFP/USB**, worst case higher if the
  TM mutex is held (the model-load kick adds up to 2 s *before* the mic even opens).

- **Always-on with `pre_recording_buffer_ms > 0`**: the stream is opened at app start (deferred +100 ms)
  and keeps running, so `pre_buffer` continuously holds the last N ms (`recorder.rs:1456-1466`). On
  hotkey, `need_stream_open` is normally false (`audio.rs:1507-1584`), so **no cpal open and no 200 ms
  sleep**; `Cmd::Start` prepends the pre-buffer (`recorder.rs:1596-1609`). Still serial/lazy: the
  `Cmd::Start` dequeue waits for the next audio chunk (≤ one buffer period, but the pre-buffer covers
  the *content*); VAD is only loaded when the stream first opens; the model is still (re)loaded
  asynchronously and the final transcription waits on it.

- **Wake-from-sleep** unconditionally calls `reload_model_on_next_use()` on **both** the main and
  live-captions `TranscriptionManager`s (`sleep_wake.rs:221-232`). The next `initiate_model_load_for`
  therefore ignores the "already loaded" fast path (`transcription.rs:1059-1066`) and performs a **full
  model reload from disk**, dropping the current engine first (`transcription.rs:809-812`). This makes
  the *first transcription after wake* as slow as a cold model load — deliberately, to escape a wedged
  Metal/GPU context.

- **Bugs vs design limitations** (detail in §6):
  - *Likely bugs / over-broad behaviour*: (a) the model-load kick can block the start path up to 2 s on
    a contended TM mutex; (b) the live-captions model is **not** pre-warmed at app startup even though
    the main model is — first hotkey after launch lazy-loads it; (c) `Cmd::Start` cannot be processed
    until an audio chunk arrives, which loses one buffer period and makes the 200 ms sleep a poor proxy
    for "stream is live"; (d) wake reload is unconditional even when the GPU context is healthy.
  - *Design limitations (intentional trade-offs)*: pre-buffer only exists when a stream is continuously
    open (always-on / BT keep-alive), and defaults to `0` with always-on `false`; the 200 ms stabilisation
    sleep; on-demand never pre-opens the mic; VAD is lazily constructed; engine is dropped before reload
    so reload time is fully exposed.

---

## 1. Startup initialisation — what is pre-warmed (and what is not)

`lib.rs` `initialize_core_logic` + the setup hook:

| Pre-warm | Where | Notes |
|---|---|---|
| transcribe-cpp backend / Metal-Vulkan context | `lib.rs:192` (`init_transcribe_backend`) | synchronous at startup; comment says this avoids paying "full cold-start cost" on first transcription |
| GPU/accelerator enumeration | `lib.rs:1008-1010` (detached thread) | `get_available_accelerators()` cached in a `OnceLock` |
| **Main ASR model load** | `lib.rs:1019-1025` (detached thread → `initiate_model_load()`) | purpose: "so it's ready when the user first presses the hotkey" |
| retry worker, USB watchdog, sleep/wake listener | `lib.rs:1029-1041` | |
| `AudioRecordingManager::new` | `lib.rs:228-230`, ctor at `audio.rs:568-651` | starts liveness monitor (`:626`), device monitor (`:629`); opens the mic stream **only** if always-on or BT (`:637-648`), deferred +100 ms |
| **VAD** | *not* pre-warmed | only constructed via `preload_vad` (`audio.rs:1147-1166`), which is called from `start_microphone_stream_inner` (`:1344`) and lazily from the hotkey path (`transcribe.rs:167-171`) |
| **Live-captions model** | *not* pre-warmed at startup | pre-warm exists only in the setting-change handlers (`shortcut/mod.rs:1585-1596`, `1648-1656`); on a fresh launch with a persisted dedicated live-captions model, it loads lazily on first hotkey (`transcribe.rs:147-158`) |

> The main model gets a startup pre-warm thread; the live-captions model does **not**. This asymmetry is
> the most concrete "bug-class" startup gap (see §6).

`AudioRecordingManager::new` intentionally does **not** eagerly load the model or VAD (confirmed at
`audio.rs:591-650`). The ctor comment (`:631-636`) explains the deferred stream open avoids a
main-thread CoreAudio SIGABRT during Tauri init.

---

## 2. Cold-start, on-demand mode — serial timeline

"Cold" = app launched moments ago (model pre-warm thread may still be running) and no mic stream open
(the default `always_on_microphone=false`, `defaults.rs:98`).

```
HOTKEY PRESS
│
├─ shortcut handler → TranscriptionCoordinator::send_input        (handler.rs:38-44)
│  └─ coordinator thread: 30 ms debounce on press                 (transcription_coordinator.rs:59,441-451)
│     └─ dispatch StartRecording → action.start() SYNCHRONOUSLY   (:1160-1174, 1467)
│
├─ TranscribeAction::start                                         (transcribe.rs:76-269)
│  ├─ session/log setup                                            (:81-99)
│  ├─ tm.try_lock_for(2 s) → clear_streaming_cancel + initiate_model_load
│  │     ⚠ BLOCKING UP TO 2 s if the TM mutex is held             (:124-164)
│  │       (initiate_model_load itself only spawns the load thread :1041-1118)
│  ├─ spawn VAD preload thread (races the sync path below)         (:166-171)
│  ├─ change_tray_icon + show_recording_overlay                    (:174-175)   ← overlay visible here
│  └─ rm.try_start_recording(binding)                              (:203)
│       │
│       ├─ state lock: verify Idle                                 (audio.rs:1470-1488)
│       ├─ need_stream_open = true (on-demand)                     (audio.rs:1585-1588)
│       ├─ start_microphone_stream()                               (audio.rs:1610)
│       │   └─ start_microphone_stream_inner()                     (audio.rs:1326-1389)
│       │       ├─ resolve_microphone_device                       (:993-1055)  cache hit ~0; else enumerate
│       │       ├─ preload_vad()?  ← SYNCHRONOUS, builds Silero/Earshot recorder + VAD  (:1344)
│       │       └─ recorder.open(device): spawn worker, build cpal stream, stream.play()
│       │            open() blocks on init_rx until worker replies (recorder.rs:310-568, 366-476)
│       ├─ std::thread::sleep(200 ms)  ← HARD-CODED              (audio.rs:1626)
│       └─ recorder.start() → sends Cmd::Start on channel (async)  (audio.rs:1640-1649; recorder.rs:571-578)
│
├─ [consumer thread] blocked on sample_rx.recv() (no timeout unless smart_stop)  (recorder.rs:1234-1242)
│     first cpal chunk arrives → processed with recording=FALSE
│     (visualizer/level_cb fires here: recorder.rs:1327-1330)
│     THEN cmd_rx.try_recv() dequeues Cmd::Start                    (recorder.rs:1574)
│       ├─ if pre_buffer>0: prepend pre-buffer                      (:1596-1609)
│       └─ recording = true                                         (:1612)
│
└─ FIRST RECORDED SAMPLE (handle_frame with recording=true)
```

Key serial observations:

1. **Up to 2 s before the mic even opens** if the TM mutex is contended (`transcribe.rs:124`). The
   comment says they "skip the kick rather than stall the start path", but the stall is bounded at
   2 s — not zero. A previous transcription holding the lock for seconds is the realistic trigger.
2. **VAD construction is synchronous inside the stream open** (`audio.rs:1344`) and contends with the
   spawned pre-warm at `transcribe.rs:167-171` (both take the recorder lock). Silero is a ~2 MB ONNX;
   session build is small but non-zero. Earshot is pure DSP (fast).
3. **cpal/CoreAudio open+play is inside the serial path** and `open()` blocks until the worker has
   built the stream and called `play()` (`recorder.rs:366-476`). The in-code comment
   (`audio.rs:1379-1383`) states `play()` returning does **not** guarantee samples are flowing; the
   first input callback fires ~10–200 ms later on macOS, longer on BT/USB.
4. **The hard-coded 200 ms sleep** (`audio.rs:1626`) runs *after* `play()` returns, adding a fixed cost
   irrespective of device.
5. **`Cmd::Start` ordering**: the consumer loop only drains commands *after* receiving a chunk
   (`recorder.rs:1234-1242` then `:1574`). So the chunk that arrives first is processed with
   `recording=false` and is not kept; the first *recorded* sample is at the next chunk. In practice this
   is ≤ one buffer period (≈10 ms at 512 frames/48 kHz, up to ~100 ms+ on Bluetooth), but it means the
   "200 ms sleep" is not what actually gates recording onset.
6. **Visualizer**: `level_cb` is fed in the consumer on each chunk (`recorder.rs:1327-1330`) and
   `emit_levels` throttles to ~30 FPS (`overlay.rs:1353-1381`). It cannot show bars before the stream
   delivers — the overlay appears at `transcribe.rs:175`, but bars are dead until step 5. This is the
   visible "dead-bars" window.

### Estimated serial durations (on-demand, cold)

| Segment | Built-in mic | Bluetooth (HFP) / USB | Source / basis |
|---|---|---|---|
| Coordinator debounce | ≤30 ms | ≤30 ms | `transcription_coordinator.rs:59` |
| TM-lock model kick | 0 (lock free) | 0 – **2000 ms** | `transcribe.rs:124` try_lock_for(2s) |
| Device resolve | ~0 (cache) / 10–200 ms (1st) | 10–300 ms | `audio.rs:993-1055`; enumerate timing log at `:1036-1040` |
| VAD build (Silero) | ~10–150 ms | ~10–150 ms | ONNX session init; ORT reports ~180–230 ms session creation for a 128 MB model, this is ~2 MB |
| cpal open + `play()` | ~10–100 ms | **200 ms – 2 s** | comment `audio.rs:1379-1383` (10–200 ms typical, longer BT/USB); A2DP↔HFP switch is why BT keep-alive exists (`audio.rs:616-623`) |
| Hard-coded sleep | 200 ms | 200 ms | `audio.rs:1626` |
| Wait for first chunk + Cmd dequeue | ~10–100 ms | ~20–200 ms | buffer period; BT callback comment in `recorder.rs:879-882` |
| **Total to first recorded sample** | **~250–500 ms** | **~0.5–3 s** | sum, order-of-magnitude |

*These are estimates, not measurements.* The dominant, easily-removed fixed cost is the 200 ms sleep;
the dominant variable cost is Bluetooth profile switching and the 2 s TM-lock ceiling.

---

## 3. Warm-start, on-demand mode

"Warm" = model pre-warm thread finished (or a prior recording already loaded it), but on-demand mode
means the stream is still closed between recordings unless `lazy_stream_close` is on.

- Stream is closed after each recording in on-demand (`audio.rs:2041-2049`) unless
  `lazy_stream_close` (`STREAM_IDLE_TIMEOUT = 30 s`, `audio.rs:27`, `schedule_lazy_close` at `:1093`)
  or BT keep-alive is active.
- Therefore the **warm-start path is essentially the cold-start path minus the model-load wait**: the
  cpal open, VAD (already in the recorder instance if reused), device resolve cache, and the 200 ms
  sleep all still apply on the first *post-close* recording.
  - Note: `stop_microphone_stream` closes the recorder (`audio.rs:1415-1419`) but does it destroy the
    `AudioRecorder`/VAD? `close()` (`recorder.rs:740-755`) tears down the stream and drops the device but
    the `AudioRecorder` object (and its VAD) stays in `self.recorder`; `preload_vad` only rebuilds when
    `recorder_opt.is_none()` (`audio.rs:1149`). So VAD is re-used after the first recording — warm
    starts skip VAD construction, but still open cpal + sleep 200 ms.
- If `lazy_stream_close=true` (default **false**, `defaults.rs:133`), the stream stays open for 30 s,
  so back-to-back recordings skip the cpal open too (`audio.rs:2045-2046`).
- Model side: `initiate_model_load` returns immediately when the model is loaded and not marked for
  reload (`transcription.rs:1059-1066`), so the hotkey path's TM lock is acquired quickly and no load
  happens. Final transcription proceeds immediately.

**Warm total to first recorded sample:** ~**220–400 ms** built-in (dominated by the 200 ms sleep +
one buffer period) or ~**0.2–1 s+** on Bluetooth.

---

## 4. Always-on mode with `pre_recording_buffer_ms > 0`

Config: `always_on_microphone=true` and `pre_recording_buffer_ms>0`. Defaults are `false` / `0`
(`defaults.rs:98,150`), so this must be opted into. `create_audio_recorder` reads the setting
unconditionally (`audio.rs:225,487-492`), so the buffer is armed whenever the stream is open — including
the BT keep-alive case in on-demand mode.

**Startup:** `AudioRecordingManager::new` defers a stream open +100 ms (`audio.rs:637-648`); the liveness
monitor sleeps 2 s before its first check to let that complete (`audio.rs:671-672`). Once the cpal stream
runs, the consumer fills `pre_buffer` with the last N ms of 16 kHz resampled audio on every chunk while
`!recording` (`recorder.rs:1456-1466`, sizing at `:1191-1196`).

**Hotkey press:**
```
TranscribeAction::start (transcribe.rs:183-198)
├─ spawn thread: play_feedback_sound_blocking(Start) → rm.apply_mute()   (non-blocking for start)
└─ rm.try_start_recording(binding)  (audio.rs:195)
     ├─ is_always_on branch: is_open? recorder.needs_reopen? device match? is_stream_alive(3000ms)?
     │     normally all good → need_stream_open = false        (audio.rs:1507-1588)
     │     (skips cpal open AND the 200 ms sleep)
     └─ recorder.start() → Cmd::Start queued                    (audio.rs:1640-1649)
[consumer] next chunk → try_recv → prepend pre_buffer → recording=true   (recorder.rs:1456-1466, 1596-1612)
```

**What is still serial/lazy in always-on:**

1. **`Cmd::Start` is still dequeued only after the next audio chunk** (`recorder.rs:1574`), so there is
   up to one buffer period of latency before `recording=true`. However the **pre-buffer prepend
   (`:1596-1609`) recovers up to `pre_recording_buffer_ms` of audio captured *before* the hotkey**, so
   no speech content is lost (up to that window).
2. **VAD is still only built when the stream first opens** (`audio.rs:1344`); in always-on this happens
   at startup, so warm recording is not VAD-gated — but if the stream had to be reopened (device
   change, USB cycle, wake), VAD + cpal + 200 ms sleep re-enter the serial path (`audio.rs:1511-1626`).
3. **Model load remains asynchronous** and is not pre-empted; the final transcription can still wait on
   `is_loading` up to `MODEL_LOAD_WAIT_TIMEOUT = 60 s` (`transcription.rs:78,1786-1802`).
4. **Fallback to the open path if the stream is dead**: `is_stream_alive(3000)` (`audio.rs:1570-1579`),
   device mismatch (`:1557-1567`), or `needs_reopen` (`:1515-1522`) all force `need_stream_open=true`,
   restoring cpal open + 200 ms sleep.

**The pre-buffer is content-level compensation for structural latency, not latency removal.** It only
exists while a stream is open, which is why it is gated on always-on (and incidentally works under BT
keep-alive).

---

## 5. Wake-from-sleep

`on_system_wake` (`sleep_wake.rs:205-311`), triggered by `NSWorkspaceDidWakeNotification` or the
wall-clock polling fallback (`:54-92`, `:107-201`):

1. `reset_hotkey_state_after_wake` (`sleep_wake.rs:211`; `handy_keys.rs:680`).
2. **Main TM** `reload_model_on_next_use()` → sets `AtomicBool` (`sleep_wake.rs:221-226`).
3. **Live-captions TM** same (`sleep_wake.rs:227-232`).
4. USB recovery only if `usb_watchdog_enabled && usb_watchdog_cycle_on_wake` (`:236`); else returns
   (`:238-250`). When enabled it sleeps 2 s then checks stream health, and if the stream is open but
   dead, runs `force_power_cycle()` — blocking uhubctl + settle (the learning log records a ~12 s
   settle). If the stream is not open (on-demand), it returns and lets the next recording open fresh
   (`:286-291`).

**Why the first recording after wake is slow** — the model reload is forced, not conditional:

- `initiate_model_load_for` reads `reload_pending` (`transcription.rs:1059`). When set, `already_loaded`
  is false even if the model is present (`:1061-1066`), so the load thread runs `load_model` (`:1105`).
- `load_model` **drops the current engine first** (`:809-812`) and then builds a new one, so reload time
  is fully exposed before any transcription can use it.
- For the **batch path**, this delays the *post-stop* transcription: `transcribe_internal` spins on
  `is_loading` with a 60 s ceiling (`:1786-1802`). The recording start itself is not blocked by the
  reload (the mic still opens normally).
- For the **live-captions streaming path**, `run_stream_worker` waits for `is_loading` and **drops
  frames that arrive before the load completes** (`transcription.rs:1201-1238`), so live preview is
  blank for the duration.
- Wake marks **both** TMs even if no live-captions model is configured. Marking is harmless in that
  case because `initiate_model_load_for` is only called when a model id is present — but it is
  over-broad.

The commit that introduced this is `bfacd272` ("FIX: recover wedged transcription engine on retry and
wake"), whose message explicitly says the engine could "return errors while still reporting 'loaded'",
so the reload is a correctness recovery for Metal/GPU context loss.

### Estimated wake reload durations (first use after wake)

| Model (fork catalog) | Size | Metal load (est.) | CPU load (est.) |
|---|---|---|---|
| Whisper Small (`ggml-small.bin`) | 465 MB (`model.rs:549`) | ~0.1–0.4 s | ~0.5–2 s |
| Whisper Medium (q4_1) | 469 MB (`:583`) | ~0.2–0.8 s | ~1–3 s |
| Whisper Turbo (`large-v3-turbo`) | 1549 MB (`:616`) | ~0.5–2 s | several s |
| Whisper Large (q5_0) | 1031 MB (`:649`) | ~0.4–1.5 s | several s |

Basis: whisper.cpp PR #1270 Metal benchmarks (M2 Ultra: small 62 ms, medium 161 ms, large 268 ms
load) and Core ML PR #566 (M1 Pro CPU: small 233 ms, medium 603 ms, large 1158 ms). Real fork loads are
typically higher because transcribe-cpp also builds compute buffers and may cold-init GPU state; the
GPU backend is pre-warmed at startup (`lib.rs:192`) but reload still re-reads weights. One upstream
report (whisper.cpp #2392) shows load times of tens of seconds on pathological GPU/CPU configurations,
so treat the upper bound as hardware-dependent.

---

## 6. Bugs vs design limitations

### Likely bugs / over-broad behaviour

- **B1 — Model-load kick can stall the start path up to 2 s.** `transcribe.rs:124` takes the TM mutex
  with `try_lock_for(2 s)` *before* the mic is opened. The comment frames this as non-blocking, but a
  contended mutex (a long transcription, a load, a GPU callback) delays recording start by up to 2 s.
  The recording start does not actually need the TM mutex; the kick could be moved off the critical path
  entirely (e.g. always spawn the kick thread and never block the coordinator).
- **B2 — Live-captions model is not pre-warmed at startup.** The main model is pre-warmed in `lib.rs`
  (`:1019-1025`) but the live-captions TM is only pre-warmed from setting-change handlers
  (`shortcut/mod.rs:1585`, `1654`). On a fresh launch with live captions enabled + a dedicated model,
  the first hotkey lazy-loads it (`transcribe.rs:147-158`), producing a much longer first-recording
  preview delay than the main path. Asymmetric with the stated intent of the main pre-warm.
- **B3 — `Cmd::Start` cannot be processed until an audio chunk arrives.** The consumer loop blocks on
  `sample_rx.recv()` (no timeout unless `smart_stop`) and drains `cmd_rx` only afterwards
  (`recorder.rs:1234-1242`, `:1574`). Consequences: (i) the first chunk is always processed with
  `recording=false`; (ii) the hard-coded 200 ms sleep (`audio.rs:1626`) is a crude proxy for readiness
  that does not actually guarantee the stream is producing; (iii) in on-demand mode the recorder's
  notion of "start" is coupled to mic hardware timing. A `recv_timeout` (like the `smart_stop` branch
  already uses) would let commands be drained promptly.
- **B4 — Wake reload is unconditional.** `sleep_wake.rs:221-232` marks both TMs for reload on every
  wake, even when the context is healthy and no transcription has failed. The recovery *mechanism*
  (failure-counted drop) already exists in `bfacd272` (`transcription.rs:2039-2044`); the wake path
  bypasses it and forces a reload regardless. This trades a guaranteed multi-hundred-ms-to-seconds
  first-transcription penalty on every wake for robustness, and could be narrowed (e.g. only mark stale
  if a probe/known-bad condition is detected, or reload lazily only when the next transcription fails).

### Design limitations (intentional trade-offs, not bugs)

- **D1 — Pre-buffer requires a continuously open stream.** `pre_buffer` fills only while `!recording`
  (`recorder.rs:1458`) and the stream only stays open in always-on or BT keep-alive. On-demand cannot
  capture pre-hotkey audio because cpal is not open yet. Defaults (`always_on_microphone=false`,
  `pre_recording_buffer_ms=0`, `defaults.rs:98,150`) mean most users get none. The feature is
  *correctly implemented* but *conditionally available*.
- **D2 — 200 ms hard-coded stabilisation sleep** (`audio.rs:1626`). Defensive and device-agnostic; it
  also gives CoreAudio a head start. Costly and arbitrary, but intentional (introduced in `067c1ce2`
  "add error handling and logging to prevent crashes on hotkey press").
- **D3 — On-demand never pre-opens the mic** (`AudioRecordingManager::new` opens only for always-on/BT,
  `audio.rs:637`). Keeps the macOS mic indicator and OS resource use off when idle; the price is the
  full open latency on every recording after a close.
- **D4 — VAD is lazily constructed** (`preload_vad` only when the recorder is absent,
  `audio.rs:1147-1166`). Could be pre-warmed at startup for on-demand mode; currently paid on first
  stream open (and it races the hotkey-path pre-warm spawn at `transcribe.rs:167-171`).
- **D5 — Engine is dropped before reload** (`transcription.rs:809-812`) to bound peak memory on large
  GGUFs. This makes reload latency fully exposed rather than overlap-able.
- **D6 — 100 ms delay before the on-demand audio feedback/mute** (`transcribe.rs:210`) runs on a spawned
  thread, so it does **not** block recording or transcription; it only postpones the start chime. Not a
  latency bug for capture.
- **D7 — Liveness/device monitors are poll-based** (2–3 s intervals, `audio.rs:626-629`, `:671-676`).
  They self-heal but cannot pre-empt a hotkey; a dead stream is repaired only after a hotkey falls into
  the open path.

---

## 7. Consolidated cold vs warm vs wake matrix

| Scenario | Model state | Mic stream | Serial extra cost vs warm | First-recording feel |
|---|---|---|---|---|
| Cold launch, on-demand | loading in bg | closed | cpal open + VAD + 200 ms + TM-lock(≤2 s); final transcription waits ≤60 s | overlay instant, bars after ~250–500 ms, text after model load |
| Warm, on-demand (stream closed) | loaded | closed | cpal open + 200 ms + one buffer | ~220–400 ms to bars |
| Warm, on-demand, `lazy_stream_close` (30 s) | loaded | open | one buffer (Cmd dequeue) | near-instant |
| Always-on + pre-buffer>0 | loaded/pre-warmed | open | one buffer only; pre-buffer restores pre-hotkey audio | near-instant, no speech loss |
| After wake (any mode) | forced reload | as per mode | **full model load** before transcription | recording may start normally, but final text/preview delayed by reload |

---

## 8. File:line index (primary citations)

- Startup pre-warm: `lib.rs:192`, `lib.rs:1008-1010`, `lib.rs:1019-1025`
- Audio manager ctor / deferred open: `managers/audio.rs:568-651`, `:626-629`, `:637-648`
- Liveness monitor: `managers/audio.rs:658-802` (2 s first sleep `:671-672`, 3 s interval `:676`)
- VAD preload: `managers/audio.rs:1147-1166`; sync call `:1344`
- `try_start_recording`: `managers/audio.rs:1465-1655`; aliveness/mode logic `:1507-1588`;
  200 ms sleep `:1626`; `recorder.start()` `:1640-1649`
- Stream open internals: `managers/audio.rs:1326-1389`; cpal open comment `:1379-1383`
- Lazy close: `managers/audio.rs:27`, `:1093-1114`, `:2041-2049`
- Recorder consumer loop / command drain: `audio_toolkit/audio/recorder.rs:1234-1242`, `:1574-1612`
- Pre-buffer fill/prepend: `recorder.rs:1456-1466`, `:1591-1610`; sizing `:1191-1196`
- Visualizer feed: `recorder.rs:1327-1330`; emit/throttle `overlay.rs:1353-1381`
- `TranscribeAction::start`: `actions/transcribe.rs:76-269`; TM kick `:124-164`; VAD spawn `:166-171`;
  overlay `:174-175`; always-on `:183-198`; on-demand `:199-221`; 100 ms feedback `:210`
- Live-captions startup lazy-load: `actions/transcribe.rs:147-158`
- Coordinator debounce/dispatch: `transcription_coordinator.rs:59`, `:424-552`, `:1160-1174`
- Model load/`reload_model_on_next_use`: `managers/transcription.rs:582-584`, `:1041-1118`,
  `:1059-1066`, `:745-812`; `is_loading` wait `:1786-1802` (`MODEL_LOAD_WAIT_TIMEOUT` `:78`)
- Streaming worker waits/drops: `managers/transcription.rs:1201-1238`
- Wake handler: `sleep_wake.rs:205-311`; reload marks `:221-232`; USB recovery `:234-310`
- Defaults: `settings/defaults.rs:98` (always-on false), `:150` (pre-buffer 0), `:133` (lazy close false)
- Model sizes: `managers/model.rs:549,583,616,649`

---

## 9. Uncertainty / caveats

- All millisecond figures are **estimates** derived from in-code comments and external benchmarks; no
  profiling was run. Actual values vary widely by Mac generation, macOS version, audio device, and
  whether Bluetooth is in HFP vs A2DP.
- The claim that a warm on-demand restart reuses the VAD is well-supported: the detector is held as
  `Arc<Mutex<Box<dyn VoiceActivityDetector>>>` on the `AudioRecorder` (`recorder.rs:112,207-208`),
  `Arc`-cloned into the worker at `open()` (`recorder.rs:347`), and `close()` never clears it
  (`recorder.rs:740-755`) nor does `stop_microphone_stream` clear `self.recorder`
  (`audio.rs:1405-1421`). `preload_vad` is a no-op once `self.recorder` is `Some` (`audio.rs:1148-1149`).
  So the ONNX/DSP VAD is genuinely re-used across stream reopens; only a `recreate_recorder` /
  backend/sensitivity change rebuilds it (`audio.rs:1250`, `:1726-1734`).
- The exact serial impact of the TM-lock kick (B1) depends on real mutex hold times during a GPU
  transcription, which the `[START-LOCK]` warning at `transcribe.rs:134-139` is designed to surface in
  logs; enabling debug logging would confirm the real distribution.
- ONNX-Runtime session-creation figures cited are for a 128 MB model; Silero VAD is ~2 MB, so its
  contribution is small but not precisely quantified here.
