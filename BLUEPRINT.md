---
title: Handy (stable fork)
one_liner: Pamela's working fork of Handy, the free offline speech-to-text Mac app, carrying local fixes for live captions, overlays, and mic watchdogs.
status: active
updated: 2026-11-13
discord_channel: https://discord.com/channels/1529123294631166223
---

## Where it stands

- Fork of Handy (github.com/Caffa/Handy-Mac) — offline, cross-platform speech-to-text desktop app.
- Live-captions streaming reworked: states resolve lazily on frame delivery, the transcribe overlay keeps click-through during non-interactive states, and the streaming seam uses a non-blocking TM lock.
- Wake observers dispatch off the main thread; the display-wake segfault and an Arc pointer miscast are fixed.
- Bluetooth mic is excluded from USB watchdog accounting via an is_bluetooth_device_name heuristic, with zero-RMS liveness gating.

## Log

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
