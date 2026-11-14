---
title: Handy (stable fork)
one_liner: Pamela's working fork of Handy, the free offline speech-to-text Mac app, carrying local fixes for live captions, overlays, and mic watchdogs.
status: active
updated: 2026-11-15
discord_channel: https://discord.com/channels/1529123294631166223
---

## Where it stands

- Fork of Handy (github.com/Caffa/Handy-Mac) — offline, cross-platform speech-to-text desktop app.
- Missing-first-syllable bug fixed: hotkey presses landing during the standing-stream rebuild window (sleep/wake, device change, USB watchdog) now hold up to 300ms lock-free so recording starts on the rebuilt stream instead of an on-demand path with an empty pre-buffer.
- Live-captions streaming reworked: states resolve lazily on frame delivery, the transcribe overlay keeps click-through during non-interactive states, and the streaming seam uses a non-blocking TM lock. A live-captions model picker defaults to Moonshine Streaming Small.
- Bluetooth mic is excluded from USB watchdog accounting via an is_bluetooth_device_name heuristic, with zero-RMS liveness gating.
- US→British spelling dictionary purged of dated targets (reflexion, tzar, etc.) that caused "Reflexions"-style mistranscriptions; deduped to 284 entries with a regression test.

## Log

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
