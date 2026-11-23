---
title: Handy (stable fork)
one_liner: Pamela's working fork of Handy, the free offline speech-to-text Mac app, carrying local fixes for live captions, overlays, and mic watchdogs.
status: active
updated: 2026-11-23
discord_channel: https://discord.com/channels/1529123294631166223
---

## Where it stands

- On branch `feature/voice-insert-api` (tip a636ba86): microphone retention — falling back to the default mic no longer erases the user's selected microphone from settings (persist_default_microphone_after_fallback deleted from managers/audio.rs), resolution split into resolve_microphone_device_pure + cached_resolution helpers with unit tests, cache-hit fast path restored, absent-mic warn rate-limited.
- MicrophoneSelector.tsx shows the retained-but-absent mic as a disabled dropdown option (new i18n key settings.sound.microphone.unavailable); docs updated (learning-log.md, usb-watchdog-wake-recovery.md fallback-masking note, DECISIONS.md ADR).
- Recently landed, runtime verification pending: AirPods Pro mute-gesture fix (upstream cjpais/Handy#2045, 696 lib + 59 integration tests pass), delayed-paste web-field detection (4fb6fe09), voice insert API Phase 1 (127.0.0.1:48213, 32 tests), live-captions pre-buffer fix (37560c58).
- Planned next: Pamela runtime-verifies delayed paste, AirPods stem presses, and mic-retention reconnect behavior in daily dictation, then Phase 2 — exact token placement via split transcription; Phase 3 — Obsidian "Link Dictate Co" plugin.

## Log

### 2026-11-23 — Mic retention across fallback

Fixed selected-mic loss on fallback: deleted persist_default_microphone_after_fallback in managers/audio.rs — it rewrote selected_microphone to the default whenever the chosen mic was absent, so the user's pick now survives until the device returns. Mic resolution split into resolve_microphone_device_pure + cached_resolution helpers with unit tests, the cache-hit fast path was restored, and the absent-mic warning is rate-limited (commits b87c62f5, 6f527e86, 0a746ca4, 251a3616, a636ba86, pushed on feature/voice-insert-api). Frontend: MicrophoneSelector.tsx shows the retained-but-absent mic as a disabled dropdown option (new i18n key settings.sound.microphone.unavailable); docs updated in learning-log.md, research/usb-watchdog-wake-recovery.md fallback-masking note, and a DECISIONS.md ADR.

Next: Pamela verifies the retained mic re-selects itself when it reconnects in daily dictation.

https://discord.com/channels/1486966847810043924/1574296533216530456

### 2026-11-21 — AirPods mute-gesture fix

Fixed "Cannot Control Mic with AirPods Pro" (upstream issue cjpais/Handy#2045): macOS's audioaccessoryd reroutes AirPods stem presses as mute commands to whichever app holds a record session, so the media controls stopped working while Handy ran. Fix B (8b8d0ea5 + 268e50b3) stops Bluetooth input mics from ever holding a standing stream — a `should_hold_standing_stream` predicate is applied at all 12 stream lifecycle sites including settings-driven reopens; Fix A (7515fcc4) adds mic_mute_gesture.rs, registering `AVAudioApplication.setInputMuteStateChangeHandler` once at startup so a stem press during recording routes to Command::StopIfRecording (stop-and-transcribe). Commits pushed; 696 lib + 59 integration tests pass; app rebuilt and reinstalled to /Applications/Handy.app via scripts/build-reinstall.sh with handler registration confirmed in live logs. Mechanics memo saved at research/airpods-cannot-control-mic-alert-hfp-mechanics.md.

Next: Pamela confirms AirPods stem presses work normally again and stop dictation during recording.

https://discord.com/channels/1486966847810043924/1573606590282600479

### 2026-11-20 — Browser text-field fix for delayed paste

Fixed delayed-paste detection failing in browsers: ax_focus.rs only descended one level under an exact AXWebArea top role (the Electron fix), while browsers expose wrapper chains like AXGroup→AXScrollArea→AXWebArea→AXTextArea, so web text boxes were classified NotEditable and the paste never fired (300s timeout). Commit 4fb6fe09 makes read_focused_element walk the focus chain recursively (depth cap MAX_AX_FOCUS_DEPTH=5, chain ≤6, cycle-guarded via pointer equality + CFEqual — AXUIElementEqual is not exported by modern HIServices, verified) and resolve_editability decides from the whole role chain, with unreadable/depth-capped ends falling back to Unknown so the paste fires immediately at arm; commit 1e9ffb48 cfg-gates the new types for non-macOS compile. Reviews: analytic pass caught the cfg bug, deep FFI pass verdict SOUND (all exit paths zero-leak, ABI bool correct, no double-release); pending_paste tests 15/15, cargo check/clippy/fmt clean, bun build green, both commits pushed. Runtime verification by Pamela is still pending — build-reinstall.sh was running at session end.

Next: Pamela runtime-verifies delayed paste in a browser text box and Discord after the reinstall.

https://discord.com/channels/1486966847810043924/1573165889254072411

### 2026-11-18 — Delayed paste crash + recognition fixes

The first field test of delayed paste surfaced two bugs, both fixed in commit 19f6668e (pushed): Handy crashed (EXC_BREAKPOINT, `_dispatch_assert_queue_fail` via TSMGetInputSourceProperty) because enigo 0.6.1's TIS/TSM calls assert the main queue on modern macOS and the delayed fire ran on a background watcher thread — the fire path now dispatches onto the main thread; and in Electron/chat apps (Discord, verified live) the focused element resolves to role AXWebArea rather than AXTextField, so ax_focus.rs now reads the role of a one-level nested AXFocusedUIElement inside web areas. Launcher windows that expose no pasteable roles (Alfred, Alfred-Preferences, Raycast, Spotlight) are covered by a PASTE_ANYWHERE_BUNDLE_IDS whitelist: paste immediately when the launcher was focused at arm time and an editable field is focused at fire time. Pending_paste tests 9/9, cargo check clean, bun build green; the crash fix is not yet runtime-verified by Pamela.

Next: rebuild + reinstall (scripts/build-reinstall.sh) and re-test the crash scenario in Discord.

https://discord.com/channels/1486966847810043924/1572247541817544865

### 2026-11-17 — Delayed paste feature

Added "Delayed paste" to `feature/voice-insert-api` (4 commits pushed to origin): when the focused element isn't an editable text field (checked via new macOS Accessibility FFI in ax_focus.rs, extracted from clipboard.rs), the transcription stays on a concealed clipboard and auto-pastes the moment an editable field is focused (250ms poll, 300s timeout, single slot, manual paste disarms it); the toggle lives in Settings→Advanced→Output and it's on by default. Two review-fix commits hardened the flow — peek instead of take for the frontmost app, pending survives busy states, arm probe moved to a helper thread — and corrected a generation-counter off-by-one that made armed pastes never fire. cargo check, pending_paste tests 7/7, and the full lib suite (682 passed / 0 failed) are green; no runtime testing by an agent — Pamela will verify manually with the installed app.

Next: manual runtime check of delayed paste in daily dictation.

https://discord.com/channels/1486966847810043924/1572247541817544865

### 2026-11-17 — Pre-existing lib tests fixed

Fixed the 4 pre-existing `cargo test --lib` failures on `feature/voice-insert-api` (e5a6de20): the three history tests were failing because the `setup_conn()` fixture hardcoded a stale `CREATE TABLE transcription_history` from before the `file_name_vad` migration (fixture now wired to the real MIGRATIONS chain) plus a trailing-space assertion artifact from f9aa0459; the shortcut-conflicts test was a production-behavior bug from 011fe1b1 where three reserved-shortcut lists had cfg-gated duplicates, leaving the Windows/Linux lists empty on macOS (populated lists ungated, empty stubs removed, `detect_conflicts()` behavior unchanged). Suite now 676 passed / 0 failed / 19 ignored; both root causes logged in learning-log.md.

Next: Phase 2 — exact token placement via split transcription.

https://discord.com/channels/1486966847810043924/1571704447476834335

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
