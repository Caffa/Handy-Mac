# Architecture Decisions

## 2026-11-23: Retain selected microphone preference when it disconnects, auto-reselect on reconnect

### Status

**Accepted**

### Context

USB docks, sleep-wake, and hub power events can make the user's chosen input microphone disappear briefly. Previously Handy erased `selected_microphone` from settings the moment the device could not be enumerated, falling back to the system default. The 2-second device monitor already detects reconnects and rebuilds the stream, but because the preference was already cleared, the rebuild stayed on the default microphone.

### Decision

Always retain the user's selected microphone preference when it is temporarily absent. `managers/audio.rs::resolve_microphone_device()` still falls back to the system default for recording, but no code path writes `selected_microphone = None` on fallback. The frontend shows the retained choice as a disabled dropdown option with a tooltip explaining it will be used again when it reconnects. The cache-hit fast path is preserved so on-demand recording starts skip an extra device-list round-trip. A single `warn!` is emitted the first time a given absent microphone is encountered; subsequent opens while the same mic is absent log at `debug` to avoid per-dictation warn spam.

Auto-reselect behavior depends on microphone mode:
- In always-on or BT-output keep-alive mode, the 2-second device monitor calls `invalidate_device_cache()` + `restart_microphone_if_needed()` on reconnect, rebuilding the standing stream on the selected mic within ~2 seconds.
- In on-demand mode, `restart_microphone_if_needed()` does not hold the stream open; the next recording will re-resolve the preference and pick up the reconnected mic automatically.

### Reconsider when

- Users request an opt-in "revert to default when selected mic disconnects" setting.

## 2026-11-21: Bluetooth input devices never hold a standing stream

### Status

**Accepted**

### Context

With an always-on mic (or the BT-output keep-alive), the fork held a standing record session open 24/7. When the input device is AirPods/Beats, that held session is exactly what makes macOS reroute every stem press into a mute command aimed at Handy: `audioaccessoryd` → `audiomxd toggleInputMuteForRecordingProcess:` fails (no `AVAudioApplication` handler), and the user gets the "Cannot Control Mic with AirPods …" rejection banner + chime attributed to Handy, on every press (upstream issue cjpais/Handy#2045). The held mic also pins the headset in HFP call quality all day and mutes AVRCP media-key delivery. Product-category consensus (Sprecho, Epilude, Typeflux, MacParakeet) is to never hold a BT headset mic open. Full mechanics: research/airpods-cannot-control-mic-alert-hfp-mechanics.md.

### Decision

All hold/open/keep decisions route through one pure predicate, `should_hold_standing_stream(is_always_on, bt_keep_alive, bt_input)` in `managers/audio.rs`: hold only when (always-on or BT keep-alive) AND the input device is not Bluetooth (name-classified via the existing `is_bluetooth_device_name` heuristic, selected device first, then host default — `audio_toolkit::is_bluetooth_input_active`). `wants_stream_held_open()` consumes it everywhere: startup deferred open, liveness monitor self-heal/zombie branches, `wants_standing_capture`, `update_mode`, `recreate_recorder`, stop/cancel dispositions (shared `settle_stream_after_stop`), cancel-timeout recovery, and `restart_microphone_if_needed` (which now stops stray streams instead of reopening). A Bluetooth input never holds the stream; always-on mode with BT input silently behaves as on-demand (hotkey still opens per dictation). As a belt-and-suspenders for presses that reach us while a recording IS active, the `AVAudioApplication` input-mute handler (`mic_mute_gesture.rs`, macOS 14+) treats the gesture as stop-and-transcribe and returns "handled" so the rejection banner never plays.

Known pre-existing quirk (not fixed here): mid-session default-device flips during the A2DP↔HFP transition keep the same device and only change its format, so no input device-list change fires and the standing stream is not rebuilt (Biscotti #88; research memo §2).

### Reconsider when

- Apple ships a mic fast-wake/standby API (removing the need to choose between open-on-demand latency and holding BT in HFP).
- Users report the on-demand BT open latency (~0.4–2 s HFP switch) as worse than the stem-control loss.

## 2026-11-16: Voice insert API (localhost HTTP → live-token splice into transcription)

### Status

**Accepted** (Phase 1)

### Context

External tools (scripts, an eventual Obsidian plugin) need to inject text into an ongoing dictation — e.g. drop a wikilink `[[Char - Alice|Alice]]` into the transcript at the right moment without the user typing it. Final transcription in Handy is whole-audio re-transcription producing a flat string (no timestamps), and live captions are display-only previews with two backend paths: the streaming path (append-only committed prefix) and a 2.5s batch fallback whose rewrites are non-monotonic (VAD drops silence), so anything "inserted into the display" once gets wiped by the next rewrite.

### Decision

- **Transport: localhost-only, hand-rolled HTTP/1.1 parsed with `httparse`** (the only new dependency). Bind 127.0.0.1 only, one request per connection (`Connection: close`, no keep-alive), `Transfer-Encoding` rejected with 501, per-request handling panic-isolated via `catch_unwind` → 500. `tiny_http` is unmaintained; axum/hyper drag a server framework in for three endpoints (POST /insert, GET /state, GET /health). Auth is a bearer token (sha256 over pid + clock nanos + counter, persisted in settings, mirrored into a shared `Arc<RwLock<String>>` so regeneration applies without restart). The server gates on `AppState::Recording` only — Confirming/UsbCycling answer 409. A bind failure logs and continues app startup; the server never blocks boot.
- **Registry keyed by `SessionId`, owned outside the coordinator.** `VoiceInsertRegistry` is managed Tauri state beside `SessionTracker`; the actions hook `begin`/`finalize`/`take_final` into the recording lifecycle and the coordinator never learns about it. `begin` replaces any previous Active wholesale — self-cleaning on cancel/timeout/USB-cycle; no teardown path to forget.
- **Splice strictly AFTER `process_transcription_output`.** The LLM post-process and Chinese-variant conversion run on the raw transcription and never see token text, so they cannot rewrite, translate, or "clean" it. History entries stay token-free. Known intentional behavior change: a recording with tokens but no speech still pastes (the dead-mic skip is bypassed when tokens exist).
- **Phase 1 anchors are approximate fractions**: char fraction against the display meter's monotonic max (batch-fallback lengths are non-monotonic, so the meter takes a max), falling back to elapsed-time fraction when no display text was seen, word-boundary-snapped (5-char forward scan, CJK-safe by char indexing). Every live-captions emission re-splices from the same base string (`splice_display` at `min(chars_anchor, len)`), which defeats the 2.5s rewrite without any frontend state.
- **Colored alias display is frontend-only.** Live captions render `[[Target|Alias]]` as a colored inline `Alias` (indigo accent, brackets stripped); the final pasted text keeps the full wikilink syntax.

Split-transcription — transcribe the audio before and after each token's time anchor separately and join with the token, giving exact placement — is the planned Phase 2 follow-up once timestamps are available in `TranscriptionResult`.

### Reconsider when

- The Phase 2 Obsidian plugin lands (its real usage decides whether /insert needs batching, a queryable token list, or structured anchors).
- Timestamps appear in final transcription output (switch the splice from fraction anchors to exact time placement via split-transcription).

## 2026-11-14: Hotkey start holds up to 300ms for a standing-stream rebuild (bounded, lock-free) instead of arming an empty pre-buffer on-demand sink

### Status

**Accepted**

### Context

When the always-on ("standing") mic stream is rebuilding after a system wake, input device change, or USB-watchdog cycle, a hotkey press during the rebuild window fell to the on-demand path and armed with an empty pre-hotkey buffer — the first syllable's lead-in was clipped from the raw WAV (observed reopen latency: ~110–230ms).

### Decision

`try_start_recording` calls `hold_for_standing_rebuild` before taking any lock: a bounded, lock-free hold (10ms polls, 300ms budget) gated by the pure predicate `should_hold_start_for_standing_rebuild` (standing wanted + stream down + `standing_down_epoch_ms` stamped within the last 2s). If the standing stream comes back, it is armed with its pre-buffer intact; on deadline the existing on-demand path runs unchanged. Every `standing_active` teardown goes through `mark_standing_down` so the down-stamp is never missed, and `begin_armed` emits INFO/WARN (`ARM-RACE`) drain diagnostics. No mutex is held across the wait — the hold reads atomics and instant mutex reads only, so concurrent stop/cancel is never blocked.

Known limitation: a zombie standing stream that never tears down (still delivering digital silence) never stamps the down-epoch and is not caught by the hold — it stays `should_abort_dead_recording`'s domain (zero-RMS abort + USB cycle). Additionally, `epoch_ms()` is SystemTime-based and non-monotonic — a clock step can cause a spurious ≤300ms hold (backward step) or skip the hold for one press (forward step >2s); consistent with existing `epoch_ms()` usage elsewhere.

### Reconsider when

- Rebuild times shrink materially (the 300ms budget and 2s recency window become dead weight — tighten or remove the hold).
- Users report post-wake hotkey press latency (the hold is felt; consider lowering the budget or gating on a readiness signal instead of polling).

## 2026-11-09: USB watchdog smart-ignores Bluetooth devices by name heuristic

### Status

**Accepted**

### Context

Bluetooth headsets (AirPods etc.) that expose an input device flap on connect/disconnect. The USB watchdog treated those flaps and any subsequent dead-looking recording exactly like USB dock flaps: it armed the one-shot wake bypass and attempted uhubctl power cycles that could never succeed ("device not found in USB tree") because Bluetooth devices do not appear in the USB tree.

### Decision

Classify devices by name only, using the existing `is_bluetooth_device_name` heuristic (substring/startswith match on AirPods/Beats/WH-/WF-/JBL/Bose/Jabra/Marshall/Galaxy Buds/FreeBuds/Bluetooth — the same logic already used for the BT output keep-alive). A POSITIVE match on (a) the configured watchdog device name or (b) the attempted/selected input mic name skips all watchdog accounting, bypass arming, wake probing, and power cycling. Empty, "Default", or unrecognized names keep the previous path exactly.

Guard-before-probe invariant: every skip happens as the first statement of the entry point, before any counter write, event emission, or uhubctl invocation — so a BT device costs zero subprocesses and zero events.

### Reconsider when

- CoreAudio transport-type detection becomes reliably available from Rust (kAudioDevicePropertyTransport) — replace the name heuristic with the authoritative transport classification and delete the substring list.
- The heuristic misfires in practice (e.g. a USB device whose marketing name contains "Bose"/"JBL", or a BT mic named something not in the list) — either extend the list or wire in the transport probe for the ambiguous case only.
- A user legitimately wants wake/flap cycling for a device whose name the heuristic considers Bluetooth (they would need a setting; deliberately not added now to keep scope minimal).

## 2026-06-25: Spelling Dictionary Source for US-to-British Conversion

### Status

**Accepted**

### Context

The Handy app provides US-to-British English spelling conversion for speech-to-text transcriptions. The previous implementation used the `varcon` crate (v1.0.5), which is based on the VARCON project (v2020.12.07).

### Problem

The varcon-based solution had several issues:

1. **Archaic spellings**: Varcon contains outdated British spellings like "waggon" (modern British uses "wagon") and "instal" (incorrect - both variants use "install")

2. **Semantic ambiguity errors**: Varcon suggests converting words like "check" → "cheque", "tire" → "tyre", "program" → "programme" without considering context. In British English:
   - "check" (verify) and "cheque" (payment) are different words
   - "tire" (weary) and "tyre" (wheel) are different words
   - "program" (software) and "programme" (TV show) are different words

3. **Manual maintenance burden**: Required constant manual curation of an exclusion list to remove problematic entries

4. **Stale upstream**: Last updated in 2020, no recent maintenance

### Decision

Replace varcon with two swappable dictionary sources:

1. **DWYL Dictionary** (default) - Curated list from [dwyl/english-words](https://github.com/dwyl/english-words)
   - ~180 common spelling pairs
   - Human-curated, less noise
   - Well-maintained (12k+ stars)
   - Excludes archaic spellings by design

2. **CSpell Dictionary** - Extracted from [CSpell dictionaries](https://github.com/streetsidesoftware/cspell-dicts)
   - More comprehensive coverage
   - Actively maintained (1.5M+ weekly downloads)
   - SCOWL-based filtering excludes archaic words
   - Community-maintained upstream

### Exclusions Applied to Both Dictionaries

Both dictionaries explicitly exclude:

| US Word | UK Word   | Reason                                   |
| ------- | --------- | ---------------------------------------- |
| check   | cheque    | Semantic ambiguity (verify vs payment)   |
| tire    | tyre      | Semantic ambiguity (weary vs wheel)      |
| program | programme | Semantic ambiguity (software vs TV show) |
| catalog | catalogue | Computing context prefers "catalog"      |
| dialog  | dialogue  | Computing context prefers "dialog"       |
| wagon   | waggon    | Archaic - modern British uses "wagon"    |
| install | instal    | Incorrect - both variants use "install"  |

### Implementation

- Created `src/audio_toolkit/spelling_dictionaries.rs` module
- Defined `SpellingDictionary` enum with `Dwyl` and `Cspell` variants
- Both dictionaries use static `HashMap` for O(1) lookup
- Added `spelling_dictionary` setting to `AppSettings`
- Removed varcon dependency from Cargo.toml

### Consequences

**Positive:**

- No more manual exclusion list maintenance
- Archaic spellings excluded by upstream filtering
- User can switch between dictionaries via settings
- Smaller codebase (removed ~100 lines of exclusion logic)

**Negative:**

- Less comprehensive than varcon's 10,000+ clusters
- Need to update dictionaries periodically from upstream

**Neutral:**

- Performance unchanged (both use HashMap lookup)
- Same conversion logic for plurals/past tense/-ing forms

### Alternatives Considered

1. **nspell + wooorm/dictionaries**: Would require runtime dictionary comparison, slower startup
2. **Custom curated list**: Maintenance burden moved to us
3. **Keep varcon with more exclusions**: Ongoing maintenance, doesn't solve staleness

### References

- Research document: `/research/us-british-spelling-conversion-solutions.md`
- DWYL source: https://github.com/dwyl/english-words/blob/master/uk-us-dict.txt
- CSpell dicts: https://github.com/streetsidesoftware/cspell-dicts
