# boss_router.py flow analysis — early "routed to X" notification feasibility

*Read-only investigation of `/Users/caffae/Local-Projects-2026/Router-Actuator/boss_router.py` (2065 lines) for the Handy "Route: {summary}" notification latency problem. Nothing modified.*

Call chain for `boss_router.py --text ... --datetime ... --json --handy`:
`main()` (L1895) → `run_router()` (L1503) → `route_content()` (L1006) → handlers via `HANDLER_MAPPING` (L137).

## 1. Program flow & stdout timing (Q1)

**Classification**: `route_content()` calls `call_decision_router()` (`processing/decision_integration.py:16`) with up to `MAX_ROUTING_RETRIES = 1` retries (boss_router.py L1117–1143):

```python
decision_result = call_decision_router(text=text, image_path=first_image, caption=..., preclassified=preclassified)
if decision_result.classifications:
    break
```

**Handler dispatch**: sequential `for classification in decision_result.classifications:` loop (L1257); handler resolved via `HANDLER_MAPPING` (L137–152, `"direct_order": send_kimaki_command_main`) or per-class overrides (`determine_handler_for_tiny_experiment`, `determine_handler_for_quantified_log`), then `result = handler(**kwargs)` (L1373).

**Stdout: NOTHING reaches real stdout until the very end, and the handler list is NEVER printed before handlers run.** Two layers delay it:

1. `run_router()` wraps the whole body in `redirect_stdout(stdout_capture), redirect_stderr(stderr_capture)` over `io.StringIO()` (L1554–1563). Every `print()` inside `route_content` — including the mid-flow JSON print — is **captured into a StringIO and discarded** in `--json` mode (it only lands in `result["messages"]`, which `main()` ignores).
2. Even inside `route_content`, the `json.dumps({"handlers": handler_data}, ...)` print (L1461) sits **after** the full handler loop plus `_apply_receipt_verification(handler_data)` (L1446).

The only real-stdout write is at the end of `main()` (L2050–2059), after `run_router` has returned (all handlers finished, history saved):

```python
if args.json:
    output = {"handlers": result.get("handler_data", [])}
    if result.get("full_text"): output["full_text"] = result["full_text"]
    if result.get("entry_id"):  output["entry_id"] = result["entry_id"]
    print(json.dumps(output, ensure_ascii=False))
```

So on the pipe Handy reads: **exactly one JSON document, printed only after all handler work is done**, followed by `sys.exit(0 if result["success"] else 1)`. (Real stderr, post-redirect-exit, only gets a few late lines like `"Routing history saved: entry_id=..."` L1832 and the `schedule_delayed_verify` warning L2047.)

## 2. Handler execution (Q2)

Handlers run **strictly sequentially** in one thread, synchronous; failures are collected into `failed_handlers`, no abort. Long waits:

- **`send_kimaki_command_main`** (`saving_modules/send_kimaki_command/send_kimaki_command.py:main`): regex trigger check (`contains_trigger`, "hey minion"), `strip_trigger`, then `subprocess.run([kimaki, "send", "--channel", ..., "--prompt", cleaned], timeout=SEND_TIMEOUT)` with `SEND_TIMEOUT = 60`. On quick nonzero exit: `time.sleep(RETRY_DELAY_SECONDS)` with `RETRY_DELAY_SECONDS = 40`, then one retry (timeouts never retried). **Worst case ≈ 102 s by itself** — nearly all of Handy's 120 s budget. This is the dominant latency source for `direct_order`.
- `determine_handler_for_tiny_experiment/quantified_log`, `extract_todos(text)` (LLM) for diary+todo mixed content, `perform_ocr` for image runs — all before/inside the loop.
- `_apply_receipt_verification(handler_data)` (boss_router.py L954) re-reads written files/notes for verification *after* all handlers.
- Before classification even starts, handy-speech text goes through `correct_transcription(..., source="handy")` (L1749–1753, LLM call).

Classification backend: `decision_integration.call_decision_router` shells out to `/Users/caffae/miniforge3/bin/python /Users/caffae/Local-Projects-2026/Basic-Minimal-Router/decision_router.py <text>` (`subprocess.run(..., check=True)`). There, `classify_with_rules` first tries the **direct-order fast path**: `contains_trigger` (regex) + `is_order_for_ai → call_llm(..., timeout=30)`; otherwise falls through to `classify_note` — Ollama tier-1 local `gemma3:12b` (single attempt, keep_alive 30s) falling back to Ollama Cloud `glm-5.3-flash:cloud` with backoff+jitter retries (Basic-Minimal-Router/router.py).

## 3. Per-handler stdout updates & final JSON schema (Q3)

**No per-handler progress on stdout.** Handler results accumulate in `handler_results` and are formatted once after the loop (`format_handler_result_json` / `format_handler_result_one_liner`). Non-JSON mode only prints its one-liner summary *after* all handlers too (L1482–1495, still captured).

Final JSON (`format_handler_result_json`, L874–927 + `main()` L2050–2059):

```json
{"handlers": [
  {"status": "✅|⚠️|❌",
   "classification": "direct_order",
   "handler": "Direct Order",        // HANDLER_READABLE_NAMES
   "action": "...",
   "input_type": "text",
   "text_snippet": "...",
   "file_path": null,
   "result": "<verbatim handler result, truncated to 200 chars>"}
 ],
 "full_text": "...",                  // omitted if empty
 "entry_id": "..."}                   // omitted if empty
```

Status derivation: `"✅" if ("✅" in result or "Successfully" in result or "Created" in result)` etc. (L896–900). Correction-mode rows are hand-built dicts (L1070–1079) with `"explanation"` instead of `"result"`.

## 4. Existing macOS-notification logic (Q4)

- **None in boss_router.py** — no osascript/`display notification` anywhere in it.
- One precedent exists: `processing/audio_processing/transcribe.py:48–60` `_send_notification(message, title)` → `subprocess.run(["osascript", "-e", 'display notification "..." with title "..."'])` (audio path only, not the `--text` Handy path).
- The notification that exists for `--handy` is **Telegram**, not macOS: `route_content` calls `_send_telegram_detached(handler_data, text_snippet, routing_success)` (L1468–1471; also L1093 in correction mode) — a fire-and-forget detached child (`subprocess.Popen(..., start_new_session=True)`, L330–337) that imports `send_telegram_result` from boss_router and posts a formatted reply built by `format_handler_reply_for_telegram` (L172). This is the in-repo template for a detached early notifier.

## 5. HANDLER_EMOJI_MAPPING (Q5)

Full mapping (L42–57):

```python
HANDLER_EMOJI_MAPPING = {
    "diary_entry": "📅", "quantified_log": "🍎", "project_update": "🗒️",
    "tiny_experiment": "🧪", "zettelkasten_entry": "📥", "fiction_story_idea": "👑",
    "read_later": "📖", "idea": "💡", "concerning": "🐶", "swipe": "👍",
    "emotional_rant": "😤", "correction": "✏️", "todo_item": "📋",
    "direct_order": "🫡",
}
```

- **Usage: Telegram only.** Sole use is `format_handler_reply_for_telegram` (L213): emoji matched by scanning readable names into the handler row, e.g. `✔️ 🫡 Direct Order`.
- Confirmed: `"direct_order": "🫡"` (L56), `HANDLER_READABLE_NAMES["direct_order"] = "Direct Order"` (L154–168), `HANDLER_MAPPING["direct_order"] = send_kimaki_command_main` (L151). Nothing macOS-side uses these dicts today.

## 6. Streaming feasibility (Q6)

**Not possible with today's printing.** A stdout-line reader on Handy's side gets its **first and only byte after the last handler finishes** — because of the `redirect_stdout(StringIO)` capture (L1563) plus the end-of-`main()` print (L2059). There is nothing to stream, from any point, until process exit is essentially reached.

Two minimal Python-side options:

**Option A — early flushed NDJSON line (needs a tiny Handy reader change).** In `route_content()`, immediately after the retry loop obtains non-empty `decision_result.classifications` (the `break` at L1132), print to the *real* stdout — plain `print()` is useless there because of the redirect, so it must use `sys.__stdout__`:

```python
print(json.dumps({"event": "classified",
                  "classifications": decision_result.classifications}),
      file=sys.__stdout__, flush=True)
```

Also emit the same line in the three alternate "classification known" spots: correction mode (L1062, `["correction"]`), router-failure diary fallback (L1145–1152, `["diary_entry"]`), and classification-failed (L1180s). Handy then splits stdout by line, fires its "Route: X" notification on an `{"event":"classified"}` line, and continues reading until the last line, which remains today's `{"handlers": ...}` doc. Zero risk to the current parse as long as "last JSON line = handlers" is the rule (or an earlier `exit()` keeps the current exactly-one-document behavior via stderr for the early signal).

**Option B — Python fires the macOS notification itself (zero Handy changes).** Right at the same points, after classification, spawn a detached osascript child — the repo already contains both needed patterns: `transcribe.py:_send_notification` (osascript) and `boss_router._send_telegram_detached` (`Popen(..., start_new_session=True)` so the parent never blocks):

```python
subprocess.Popen(["osascript", "-e",
    f'display notification "{emoji_line}" with title "Handy Router"'],
    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)
```

Notification text = `Route: 🫡 Direct Order` composed from `HANDLER_EMOJI_MAPPING` + `HANDLER_READABLE_NAMES`. Classification is typically available within seconds (regex trigger + one ≤30 s LLM eval for direct_order; gemma3 local attempt then cloud fallback for everything else), while `send_kimaki_command_main` then runs 40–100 s unbudgeted. If Handy keeps its own end-of-run "Route:" notification, either suppress it for those paths or accept a duplicate.

## 7. Other relevant files (Q7)

- `processing/decision_integration.py` — `DecisionResult` dataclass (`classifications`, `paths`, `keywords`, `datetime=None`); `call_decision_router` spawns `decision_router.py`; `parse_decision_output` maps `paths` → project/tiny_experiment/quantified_log; `is_error_response` treats "Error:"-prefixed output as failure.
- `Basic-Minimal-Router/decision_router.py` (`classify_with_rules`) — direct-order fast path returns `{"classifications": ["direct_order"], ...}` without running the full classifier.
- `Basic-Minimal-Router/router.py` / `direct_order.py` — LLM tiers (local gemma3:12b → cloud glm-5.3-flash), `call_llm(timeout=30)` for the order eval.
- `saving_modules/send_kimaki_command/send_kimaki_command.py` — the slowest handler (see §2).
- `utils/pending_entries.py` — `schedule_delayed_verify` explicates the contract: "child is fully detached (new session, stdio on DEVNULL) … Handy captures stdout and has a timeout on the parent process" (L269–271). Pre-existing test file `tests/test_handler_receipts.py` covers `_apply_receipt_verification`.

## Bottom line

Today the classification is known inside `route_content()` seconds after `call_decision_router()` returns — but the architecture captures *all* process stdout into `io.StringIO` (`run_router` L1554–1563) and prints a single handlers JSON as the last act of `main()` (L2059). The cheapest early-notify that changes nothing on Handy is **Option B** (detached osascript from Python at the two "classifications known" points); the cheapest change that keeps notifying logic on Handy is **Option A** (`sys.__stdout__` + `flush=True` NDJSON pre-line, with Handy consuming the last line as the final JSON).
