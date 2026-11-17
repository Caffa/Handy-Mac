---
title: Handy (stable fork)
one_liner: Pamela's working fork of Handy, the free offline speech-to-text Mac app, carrying local fixes for live captions, overlays, and mic watchdogs.
status: active
updated: 2026-11-17
discord_channel: https://discord.com/channels/1529123294631166223
---

## Where it stands

- On branch `feature/voice-insert-api` (11 commits pushed to origin): Phase 1 of the voice insert API is done — an opt-in localhost HTTP server (127.0.0.1:48213, bearer token in settings) lets an external process inject a text token during an active recording; it splices into the final transcription after LLM post-processing and shows in live captions immediately.
- Voice insert is hardened after two review passes (recording gate, meter race, token cap raised 256→413, socket timeouts); 32 voice_insert tests pass, cargo/clippy/fmt/build clean at baseline.
- Fixed live captions clipping the front of utterances (37560c58): the arm-time pre-buffer drain in `CaptureSink::begin_armed` is now forwarded to the streaming `on_raw_frame` seam, with armed-edge detection ordered so the drain arrives before the arming chunk — streaming partials no longer miss ~1s of lead-in; regression test added.
- Planned next: Phase 2 — exact token placement via split transcription (transcribe before/after token, then join); Phase 3 — Obsidian "Link Dictate Co" plugin as a thin client over Handy's API (no bundled STT), with vault-synced bearer token.
- Carrying earlier fork fixes: missing-first-syllable hold on standing-stream rebuild, live-captions streaming rework, Bluetooth mic excluded from USB watchdog, purged British spelling dictionary (284 entries).

## Log

### 2026-11-16 — Live-captions pre-buffer fix

Fixed live captions sometimes clipping the front of an utterance: the always-on pre-buffer drained at arm time (`begin_armed` in src-tauri/src/audio_toolkit/audio/sink.rs) was never forwarded to the live-captions `StreamRouter` seam (`on_raw_frame`), so streaming partials missed ~1s of audio that was present in the final transcript. Commit 37560c58 (pushed to origin/feature/voice-insert-api) forwards the drain to `on_raw_frame` and moves armed-edge detection in `CaptureSink::process_chunk` above the raw-seam feed so frames arrive in order; regression test src-tauri/tests/capture_sink_preroll.rs proves the drain-before-arming-chunk ordering and fails on the old code. Reviewer confirmed no regressions — the streaming final is display-only, the authoritative transcript still comes from the batch transcription.

Next: Phase 2 — exact token placement via split transcription.

https://discord.com/channels/1486966847810043924/1571704447476834335

### 2026-11-16 — Voice insert API Phase 1

Built Phase 1 of the voice insert API on branch `feature/voice-insert-api` (base ee2bb114, 9 commits pushed to origin): an opt-in localhost HTTP API (127.0.0.1:48213, bearer token stored in settings) accepts a text token from an external process during an active recording, splices it into the final transcription at an approximate click-time anchor after LLM post-processing, and mirrors it into live captions immediately with alias-rendered coloring. New voice_insert Rust module (splice.rs, registry.rs, server.rs using httparse), a VoiceInsertApi card in the Integrations settings pane, scripts/test-voice-insert.sh, and an ADR in DECISIONS.md. Two review passes produced hardening commit a30688b7 (recording-gate, meter race, token cap 256→413, socket timeouts, parking_lot swap); 32 voice_insert tests pass with cargo/clippy/fmt/build at baseline, and the planned Obsidian plugin will be a thin client over Handy's API rather than bundling its own STT.

Next: Phase 2 — exact token placement via split transcription, then the Obsidian "Link Dictate Co" plugin.

https://discord.com/channels/1486966847810043924/1571480114150117437

### 2026-11-15 — Missing first syllable fix

Investigated reports of the first syllable missing from raw recordings; waveform forensics over 300 recent WAVs ruled out chunking, VAD trimming, and pre-buffer drain bugs (292/300 have the full 1s lead). Root cause: hotkey presses landing during the standing-stream rebuild window (sleep/wake, device change, USB watchdog) fell to the on-demand path with an empty pre-buffer. Added a bounded lock-free 300ms hold at the top of try_start_recording (should_hold_start_for_standing_rebuild / hold_for_standing_rebuild, epoch-stamped CaptureControl, ARM-RACE instrumentation) plus a publication-order fix stamping the down-epoch before clearing the flag; cargo fmt/clippy/check clean and 640 lib tests pass.

Next: confirm first words come through intact in daily dictation after sleep/wake and device swaps.

https://discord.com/channels/1486966847810043924/1571160823752163400

### 2026-11-14 — British spelling dictionary purge

Purged the US→British spelling dictionary (DWYL_DICTIONARY in audio_toolkit/spelling_dictionaries.rs) that mistranscribed "reflections" as "Reflexions" — the root cause was dated British targets like reflection→reflexion applied post-transcription when convert_us_to_british is on. Removed 9 dated pairs, 8 identity no-ops, and 31 duplicate keys (329→284 entries), fixed a missing normalize→normalise, and added a regression test. Audit saved at research/british-spelling-dictionary-audit.md.

Next: run convert_us_to_british on real dictation and check for remaining odd swaps.

https://discord.com/channels/1486966847810043924/1571176896878743584

### 2026-11-12 — Live captions + overlay click-through fix pass

Fixed the "live captions don't show" complaint: streaming states resolve lazily on frame delivery, overlay click-through persists through non-interactive states, and the streaming seam's TM lock is non-blocking. Ran plan, implementation, deep correctness review, and a plan-compliance review in one session.

Next: watch live captions in daily use for regressions.

https://discord.com/channels/1529123294631166223/1570274938374328332

### 2026-11-11 — Word-form trailing point

Kept the trailing spoken point in word form unless a number word follows.

### 2026-11-10 — Wake observer safety

Wake observers now dispatch off the main thread, fixing a display-wake segfault, and stopped miscasting the Arc::into_raw pointer as &Arc.

### 2026-11-09 — Bluetooth mic watchdog rework

Bluetooth mic flaps and failures stay out of USB watchdog accounting; the watchdog device is ignored in power-cycle entry points and wake-recovery USB probing, with liveness zero-RMS gating adjusted. Bluetooth smart-ignore documented in the learning log and DECISIONS.
