# Investigation: Wake phrase ("Hey Minion"), router mode, and macOS notifications

**Date:** 2026-11-02 · **Scope:** stable-handy checkout (branch `main`) + external `Router-Actuator/boss_router.py`
**Status:** SOLVED (factual mapping from orchestrator questions)

---

## 1. Wake phrase handling

**There is no live wake-word / always-listening detection.** No occurrence of "salut" anywhere
(Handy repo, icons, or Router-Actuator). Handy is shortcut-activated; the "wake phrase" is text
spoken *inside* the recording and detected **after transcription**, on the transcript itself.

### Configuration (`src-tauri/src/settings/types.rs`, `AppSettings`)
- `trigger_phrases: Vec<String>` — canonical phrases, default `default_trigger_phrases()` →
  `["Hey Minion", "Hey Assistant"]` (types.rs ~line 958).
- `trigger_phrase_fallback_enabled: bool` (default **false**) — master switch for misheard-trigger recovery.
- `trigger_scan_seconds: u8` (clamped 3–30 when used), `trigger_scan_model: String` (small Whisper),
  `trigger_fallback_force_scan: bool` (debug: scan every recording).
- Frontend toggle: `src/components/settings/post-processing/PostProcessingSettings.tsx`
  (`trigger_phrase_fallback_enabled`), i18n label "Fallback trigger-phrase scan (Whisper)".

### Detection code — `src-tauri/src/actions/trigger_fallback.rs`
- `MINION_VARIANTS = ["minion", "minions", "million", "millions", "milion"]` — mishearing list.
- `suspicious_opening(text)` — is the opening plausibly a mangled trigger?
- `detect_canonical_trigger(text, phrases)` — does the transcript already start with a canonical phrase?
- `canonicalize_leading_trigger(text, canonical)` — rewrites the mangled opening to the canonical phrase.

### Flow into transcription → routing (all inside `TranscribeWithRouterAction::stop`,
`src-tauri/src/actions/router.rs`, single `tauri::async_runtime::spawn` block)
1. Shortcut start (`start()`): model load kick, recording start (`rm.try_start_recording`), overlay
   mode `OverlayMode::Router`.
2. `stop()`: sample capture → trigger samples saved **only if** `trigger_phrase_fallback_enabled`
   (first `trigger_scan_seconds` × 16 kHz of raw samples).
3. Primary transcription: `tm.transcribe_with_hook(processed_samples, …)`.
4. Trigger fallback gate: `suspicious_opening` && !`detect_canonical_trigger` && fast path not
   already matched → tokio `spawn_blocking` on `TriggerTranscriptionManager` (small model rescan) →
   `canonicalize_leading_trigger` rewrites `transcription_text`.
5. **Fast path** `write_for_me_prompt_with_phrases(text, phrases)` (router.rs, below the subprocess code):
   - phrase containing "minion" requires a write sub-prefix `["write for me", "write me", "write to me"]`;
     anything else (e.g. "Hey Assistant …") passes the whole remainder as prompt.
   - On match: single-shot **Ollama Cloud** via `crate::llm_client::send_chat_completion_with_schema`
     (provider hardcoded `ollama-cloud`, base_url `https://ollama.com/v1`; model/system prompt from
     `write_for_me_model` / `write_for_me_system_prompt` + `default_*` fallbacks) → `utils::paste(output)`
     after 500 ms + "Handy Write" notifications → **returns early, boss_router is never called.**
6. Otherwise: history `save_entry_with_vad(… routed=true)` → confirm overlay
   (`transcription-preview` event + `coordinator.set_confirming`) → routing (§3).

---

## 2. Router mode (`TranscribeWithRouterAction`)

Registered in `src-tauri/src/actions/mod.rs` ACTION_MAP as action id **`"transcribe_with_router"`**.

"confirm / edit / send" are the **frontend overlay** flow (`src/overlay/hooks/useRouterPreview.ts` +
`src/overlay/components/RouterResultDisplay.tsx`): preview → click-to-edit → auto-send countdown
(`ROUTING_COUNTDOWN_MS = 2000` ms, was 4500 before 2026-10-21 fix per learning-log.md) →
`commands.confirmRouting(text)` → backend `confirm_routing` (`src-tauri/src/commands/mod.rs`)
completes the oneshot `PendingRouting.confirm_tx`. Backend confirmation timeout: 30 s.

Exact sequence after transcription (in `stop()`'s async block):
1. `write_for_me_prompt_with_phrases` fast path (may early-return, see above).
2. `save_entry_with_vav`* → history, `routed=true`.
3. `set_confirming` + `transcription-preview` event.
4. Wait on `PendingRoutingState` oneshot (frontend confirm/edit/2 s auto-send or 30 s backend timeout).
5. `hide_recording_overlay`, `set_processing_with_binding`.
6. thread → `run_router_subprocess(router_script, text, datetime, env_file)`:
   `python3 boss_router.py --text <t> --datetime <dt> --json --handy` (120 s kill timeout, `--json`)
   via `build_router_command` (hardcoded `/Users/caffae/miniforge3/bin/python3`, PATH prefix,
   env vars from `router_env_file` = `/Users/caffae/Local-Projects-2026/Voice-Memo-to-Router/.env`).
7. `parse_router_json_output` → `RouterOutput { summary, handler_data: Vec<RouterHandlerData> }`;
   `RouterHandlerData { status: "✅"/"❌"/"🔁", handler, classification, file_path }`.
8. `hm.update_routing_result(entry_id, routing_json)`; emit `router-result` event.
9. **"routed to" notification** (see §3).
10. `run_pending_entry_verify` (journal verify, `ROUTER_VERIFY_DELAY = 60 s`, single-flight
    `ROUTER_VERIFY_INFLIGHT`; failure notes "Journal entry re-added after conflict" /
    "Journal verify failed — see history").
11. `coord.notify_processing_finished()` → coordinator Idle; optional `show_processing_overlay_with_mode(Router)`
    + 5 s delayed hide.

Defaults: `router_script_path = /Users/caffae/Local-Projects-2026/Router-Actuator/boss_router.py`,
`router_env_file = /Users/caffae/Local-Projects-2026/Voice-Memo-to-Router/.env` (`settings/defaults.rs`).

---

## 3. Notification mechanism

**Single sender: `send_macos_notification(title, message)` at the bottom of
`src-tauri/src/actions/router.rs`** — uses plain `osascript -e 'display notification "MSG" with title "TITLE"'`.
- **No icon parameter, no sound parameter, no custom app name.** AppleScript `display notification`
  accepts no icon option.
- No `tauri-plugin-notification` in `src-tauri/Cargo.toml`; no NSUserNotification/UNUserNotificationCenter code.
- The only other `osascript` users (`managers/audio.rs`, `actions/transcribe.rs`) do mute/volume
  AppleScript, not notifications.

**"Where it routed to" notification** (success branch of the router result thread):
```rust
let notification_text = if summary_text.len() > 100 {
    format!("Route: {}...", &summary_text[..100])
} else {
    format!("Route: {}", summary_text)
};
send_macos_notification("Handy Router", &notification_text);
```
`summary_text` is the joined handler list from boss_router.py's JSON, e.g. `✅ Daily (2026-11-02.md) | ✅ Direct Order`,
built by `parse_router_json_output` (`format!("{} {} ({})", status, handler_name, filename)`).
So the macOS notification literally begins with `Route: ✅ …`.

Other notifications in router mode: `"No speech detected"` (empty transcript), `"Handy Write"` /
`"Handy Write Error"` (fast path), `"Handy Router Error" <stderr/timeout>` (subprocess fail),
`"Transcription failed. Will retry automatically."`, `"No router path configured. Check Settings."`.

**macOS icon attribution:** the fork sets nothing custom. `display notification` banners are
attributed to the *responsible (parent) app* of the osascript child process — i.e. the Handy
bundle (`com.pais.handy`, `src-tauri/tauri.conf.json` productName `Handy`), whose icon comes from
the bundled `icons/icon.icns`. No helper binary, no alternate .icns, no config icon entry beyond the
standard `bundle.icon` list. (Banner icon = Handy app icon; no salute anywhere.)

---

## 4. "Direct Order"

**Not a Handy concept.** Only Handy-side match is the test name
`rejects_generic_direct_order` (router.rs tests): asserts `"Hey Minion, restart kimaki"` does **not**
take the write-for-me fast path (i.e. a generic command is *not* a "write" request).

It is a **boss_router.py classification** in the external subprocess
(`/Users/caffae/Local-Projects-2026/Router-Actuator/boss_router.py`):

```python
HANDLER_EMOJI_MAPPING   = { …, "direct_order": "🫡", }        # salute emoji — Telegram only
HANDLER_MAPPING         = { …, "direct_order": send_kimaki_command_main, }
HANDLER_READABLE_NAMES  = { …, "direct_order": "Direct Order", }
```

- Classifier: `call_decision_router` (`processing/decision_integration.py`; external decision
  router; optional `--preclassified` categories bypass round 1).
- Handler: `saving_modules/send_kimaki_command/send_kimaki_command.py` — strips the trigger and
  runs the cleaned text as a kimaki **CLI** prompt to Discord channel
  `KIMAKI_CHANNEL_ID = "1501841938037608509"` (retries once on known transient failure).
- JSON row handed back to Handy: `{status, classification: "direct_order", handler: "Direct Order",
  action, input_type, text_snippet, file_path, result_snippet…}` — Handy maps `handler` → readable
  name in the summary/notification; the 🫡 emoji is used **only** in the Telegram reply
  (`TELEGRAM_DAILY_LOG_BOT`), never in the macOS notification and never as an icon.

---

## 5. Icons

`src-tauri/icons/`: standard Tauri set — `icon.icns`, `icon.ico`, `icon.png`, `32x32.png`,
`64x64.png`, `128x128.png`, `128x128@2x.png`, `Square*Logo.png`, `StoreLogo.png`, `android/`, `ios/`, `logo.png`.
**No "salut"/salute asset anywhere** (variation selectors 🫡 live only in Python/Telegram strings).
`tauri.conf.json` `bundle.icon` lists the 5 standard entries. No `src/assets/` folder.

macOS notification icon: not chosen by the fork at all; see §3 attribution paragraph.

---

## 6. Pipeline ordering — routing vs. LLM post-processing

**The two paths are disjoint.**

- **Router path never runs `post_process.rs`.** History saves pass `post_process_requested = false`
  with the inline comment `// post_process (router doesn't post-process)`. `process_transcription_output`
  / `post_process_transcription` (`src-tauri/src/actions/post_process.rs`) are only called from the
  plain transcribe/post-process actions (`src-tauri/src/actions/transcribe.rs`), whose flow is
  transcription → `maybe_convert_chinese_variant` → `post_process_transcription` (LLM, structured
  output, `llm_client::send_chat_completion_with_schema`) → `utils::paste` on main thread.
- **Routing decision text dependency:** classification happens **inside boss_router.py on the
  (post-confirmation) transcript** — there is no routing signal before transcription, and Handy's
  Mac-side notification about routing can only fire **after** the subprocess exits (after
  boss_router's own LLM/text-correction work).
- The only Handy-side LLM round trip in router mode is the write-for-me **fast path**, which
  *replaces* routing (skips boss_router entirely) and is decided purely from the transcript.

**Call chain to quote:** `TranscribeWithRouterAction::stop` → `AudioRecordingManager::stop_recording`
→ `TranscriptionManager::transcribe_with_hook` → [trigger fallback:
`suspicious_opening` → `TriggerTranscriptionManager::transcribe` → `canonicalize_leading_trigger`]
→ `write_for_me_prompt_with_phrases` → (fast path: `llm_client::send_chat_completion_with_schema`
→ `utils::paste` → `send_macos_notification("Handy Write", …)`) | (router: `save_entry_with_vad`
→ `set_confirming` → frontend `confirmRouting` oneshot → thread → `run_router_subprocess` →
`parse_router_json_output` → `update_routing_result` → `emit("router-result")` →
`send_macos_notification("Handy Router", "Route: {summary}")` → `run_pending_entry_verify` →
`notify_processing_finished`).

---

## 7. Existing notes check

- `research/`: **no** docs on router notifications or wake phrases (index covers startup latency,
  upstream issues, custom words, cancel shortcut, stuck states, etc.).
- `learning-log.md`: §"Router Confirmation Countdown Too Slow" (2026-10-21) — `ROUTING_COUNTDOWN_MS`
  4500 → 2000 ms in `useRouterPreview.ts` / `RouterResultDisplay.tsx`; §"Router Filing Race
  Condition" entries (2026-06-15/17) on `router-result` overlay races and `is_active_use()` guard.
- `docs/`: error handling / transcription reliability only; nothing on notifications.

## Uncertainty flags
- macOS *banner attribution* (parent-app icon) is the documented AppleScript behavior for
  child-process osascript, but banner identity should be verified empirically on this machine.
- The decision router that produces `direct_order` lives in Router-Actuator
  (`call_decision_router`); its internals (which LLM/CLI) were out of scope and the
  `decision-classifying-router/` directory appeared empty.
