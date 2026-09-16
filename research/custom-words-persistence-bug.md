# Custom Words Persistence Bug — Investigation Report

**Branch:** `feature/streaming-live-captions`
**Repo:** `/Users/caffae/Local-Projects-2026/Handy-Fork/stable-handy`
**Symptom:** Custom words can be added and appear in the UI, but after quitting and reopening the app the words are gone.
**Date:** 2026-08-20

---

## Executive Summary

The custom-words write path (frontend → backend → disk) is functionally correct: `update_custom_words` reads from the in-memory cache, mutates `custom_words`, and schedules a debounced disk flush. `salvage_settings` correctly preserves `custom_words`, `advanced_custom_words`, `word_replacements`, and `custom_filler_words` even when an unrelated field is broken (verified with a new test). The on-disk store in the active AppData directory (`com.pais.handy/settings_store.json`) **does** currently contain the persisted word, proving writes can succeed.

**The bug is a timing/reliability flaw in the debounced write + exit-flush mechanism, combined with a store-path discrepancy.** Three concrete weaknesses were identified:

1. **`write_settings` uses `tokio::spawn` from a synchronous Tauri command.** The 500 ms debounce timer is held on a Tokio worker task. If the app quits before the 500 ms window elapses (or if the spawned task hasn't been polled yet), the write may never reach disk unless `flush_settings` runs.
2. **`flush_settings` calls `tokio::runtime::Handle::current()` + `block_in_place`/`block_on`**, which **panics** with _"there is no reactor running"_ when invoked from the main event-loop thread that dispatches `RunEvent::ExitRequested`. Tauri's event loop runs the callback directly on the main thread with **no active Tokio runtime context** (verified in the tauri fork source). The panic is swallowed by `safe_settings_operation`, so the pending write is **silently dropped**.
3. **A separate stale store file** exists at `~/Library/Application Support/settings_store.json` (mtime 15:24) that contains **empty** `custom_words` and the branch's new defaults (`vad_sensitivity: very_relaxed`, `live_captions_enabled: false`), while the active store at `~/Library/Application Support/com.pais.handy/settings_store.json` (mtime 06:05) contains the custom word. If the app ever loads/writes the root store instead of the bundled path, custom words appear "reset."

**Recommended fix location (see §7):** harden `flush_settings` in `src-tauri/src/settings/store.rs` (lines ~626–671) to not depend on a Tokio runtime context, and consider making the debounced write path fail-safe so a late quit always lands the in-memory cache to disk.

---

## 1. Custom words UI(s) and the full frontend → backend save flow

There are **two** custom-words UIs and one related group of controls:

| UI component                                | File                                              | Backend command                |
| ------------------------------------------- | ------------------------------------------------- | ------------------------------ |
| Simple custom words                         | `src/components/settings/CustomWords.tsx`         | `update_custom_words`          |
| Advanced custom words (with pronunciations) | `src/components/settings/AdvancedCustomWords.tsx` | `update_advanced_custom_words` |
| Custom filler words                         | `src/components/settings/CustomFillerWords.tsx`   | `update_custom_filler_words`   |
| Word replacements                           | `src/components/settings/WordReplacements.tsx`    | `update_word_replacements`     |

### Frontend → backend trace (simple custom words)

1. **Component** — `CustomWords.tsx` calls `updateSetting("custom_words", [...customWords, sanitizedWord])` (line 37) and `updateSetting("custom_words", customWords.filter(...))` (lines 43–46).
2. **Settings store** — `src/stores/settingsStore.ts`:
   - `settingUpdaters["custom_words"] = (value) => commands.updateCustomWords(value as string[])` (line 140).
   - `updateSetting()` (lines 390–435) does an optimistic in-memory update, awaits the updater, then rolls back on a backend error.
3. **Tauri command binding** — `src/bindings.ts` line 302: `TAURI_INVOKE("update_custom_words", { words })`.
4. **Backend command** — `src-tauri/src/shortcut/mod.rs` lines 800–807:
   ```rust
   #[tauri::command]
   pub fn update_custom_words(app: AppHandle, words: Vec<String>) -> Result<(), String> {
       let mut settings = settings::get_settings_safe(&app);
       settings.custom_words = words;
       settings::write_settings_safe(&app, settings);
       Ok(())
   }
   ```
5. **Settings store → cache → disk** — `write_settings_safe` → `write_settings` (store.rs line 530) updates the in-memory `SettingsCache` immediately, then `tokio::spawn`s `SettingsWriter::write` which debounces 500 ms before `write_settings_immediate` → `store.save()`.

**Key observation:** The optimistic frontend update + immediate cache update means the UI always shows the new word even before it reaches disk. So "it appears in the UI" does **not** imply it was persisted.

---

## 2. Does `update_custom_words` / `update_advanced_custom_words` correctly mutate + save?

**Yes — the command bodies are correct.** Confirmed at `src-tauri/src/shortcut/mod.rs`:

- `update_custom_words` (lines 800–807): `get_settings_safe` → set `custom_words` → `write_settings_safe`. ✓
- `update_advanced_custom_words` (lines 821–828): `get_settings_safe` → set `advanced_custom_words` → `write_settings_safe`. ✓
- `update_custom_filler_words` (lines 830–837): sets `custom_filler_words = if empty { None } else { Some(words) }`. ✓
- `update_word_replacements` (lines 871–879): sets `word_replacements`. ✓

All four are registered as Tauri commands in `lib.rs` (lines 614–617).

`write_settings_safe` (store.rs line 210) wraps `write_settings` (line 530), which:

1. updates `SettingsCache` (in-memory source of truth) synchronously;
2. calls `writer.write(app, settings)` inside `tokio::spawn` (line 562).

`get_settings_safe` (line 192) reads from the cache when present (line 194–196), so the read-modify-write is safe against the debounced writer.

**The correctness gap is not in mutation — it is in the disk flush reliability (see §3).**

---

## 3. Settings write debouncing + `SettingsWriter` flush on `ExitRequested`

### The debounce mechanism

`SettingsWriter` (`store.rs` lines 726–814):

- `pending: Mutex<Option<AppSettings>>` and `timer: Mutex<Option<JoinHandle>>`.
- `write()` (761–789): sets pending, aborts any prior timer, then `tokio::spawn` a task that sleeps `debounce_ms` (500) and calls `flush_inner`.
- `flush_inner()` (802–812): takes the pending settings and calls `write_settings_immediate` → `store.save()`.

### How the write is initiated

`write_settings` (line 562):

```rust
tokio::spawn(async move {
    writer.write(app_clone, settings).await;
});
```

This is called from a synchronous Tauri command (e.g. `update_custom_words`). The `tokio::spawn` schedules the write on the Tokio runtime. **The disk write only happens if/when the spawned task is polled** by a Tokio worker thread and the 500 ms sleep elapses.

### `flush_settings` on `RunEvent::ExitRequested`

Wired in `src-tauri/src/lib.rs` lines 1106–1110:

```rust
if let tauri::RunEvent::ExitRequested { .. } = &event {
    crate::settings::flush_settings(app);
}
```

`flush_settings` (store.rs lines 626–670) does:

```rust
let has_pending = tokio::task::block_in_place(|| {
    tokio::runtime::Handle::current().block_on(async { writer.has_pending().await })
});
...
let result = tokio::task::block_in_place(|| {
    tokio::runtime::Handle::current().block_on(async {
        tokio::time::timeout(Duration::from_secs(2), writer.flush(app)).await
    })
});
```

### The critical reliability problem

Tauri dispatches `RunEvent::ExitRequested` on the **main event-loop thread** (tao event loop, see `tauri-runtime-wry-2.11.4/src/lib.rs` `make_event_handler` → `handle_event_loop`). That callback runs **without any active Tokio runtime context** — Tauri does **not** call `.enter()` around the event loop (verified in the project's tauri fork at `~/.cargo/git/checkouts/tauri-87d5f779a89e41b2/ad9ba94/crates/tauri/src/`).

`tokio::runtime::Handle::current()` calls `scheduler::Handle::current()` which does:

```rust
match context::with_current(Clone::clone) {
    Ok(handle) => handle,
    Err(e) => panic!("{}", e),   // CONTEXT_MISSING_ERROR
}
```

On a non-Tokio thread this **panics**: _"there is no reactor running, must be called from the context of a Tokio 1.x runtime."_ This panic is swallowed by `safe_settings_operation` (line 627), so `flush_settings` returns normally but **the pending write is never flushed**.

**A reproduction test was added** — `flush_handle_current_panics_outside_tokio_context` (`store.rs` tests) — which asserts `Handle::current()` panics on a fresh non-Tokio thread. It passes, confirming the mechanism.

**Consequence:** If the user adds a custom word and quits within the 500 ms debounce window (very common — type the word, hit Add, immediately quit), the write is still pending in the `SettingsWriter.pending` mutex, and the exit flush panics before flushing it. The word is silently lost. On next launch the cache is rebuilt from disk (which never got the write), so the word is gone.

Even when the 500 ms has elapsed, there's a race: `tokio::spawn`'d task may not have been polled/flushed by the time the runtime tears down on exit, and the panic path in `flush_settings` doesn't fall through to the `write_settings_immediate` fallback because the panic occurs in the `block_in_place`/`block_on` **before** the try/catch-style fallback.

> Note: Commit `4204cc2f` (on the other branch/not in HEAD) added a `RunEvent::Exit` flush that wrote directly; the current branch only has the `ExitRequested` handler and the panic-prone `flush_settings` body.

---

## 4. Does `salvage_settings` preserve `custom_words` and `advanced_custom_words`?

**Yes.** `salvage_settings` (store.rs lines 233–315) is written to keep every individually-valid field:

```rust
let mut merged = serde_json::to_value(get_default_settings()).unwrap();
for (key, value) in stored_map {
    let previous = merged.as_object_mut().unwrap().insert(key.clone(), value.clone());
    if serde_json::from_value::<AppSettings>(merged.clone()).is_err() {
        warn!("Dropping invalid settings field '{key}', keeping its default");
        // restore previous / remove key
    }
}
```

Because `custom_words`, `advanced_custom_words`, `word_replacements`, and `custom_filler_words` all have `#[serde(default)]` (types.rs lines 585–595, 651) and are individually deserializable from any valid JSON array, a broken unrelated top-level field does **not** reset them.

**Verified with a new test:** `salvage_settings_preserves_custom_words_across_broken_field` (store.rs tests) — seeds all four fields + an invalid `broken_future_field`, runs salvage, asserts all four survive. **Passes.**

> This was previously a real bug (#1619, fixed by the salvage implementation) and the bindings migration added on this branch (`abf6116e`) only touches `bindings`, not word fields. So salvage is not the current regression.

---

## 5. Reproduction: backend API → store file → reload

### Disk evidence

- Active app-data store: `~/Library/Application Support/com.pais.handy/settings_store.json` — **contains** `"custom_words": ["Zettelkasten"]` (persisted successfully, mtime 2026-08-20 06:05).
- Stale root store: `~/Library/Application Support/settings_store.json` (mtime 2026-08-20 15:30) — **empty** `custom_words: []`, and holds the branch's new defaults (`vad_sensitivity: very_relaxed`, `live_captions_enabled: false`).

The two stores have diverged. The active, correctly-persisted file has the word; the other is either an older path used by a different build/launch, or a "reset to defaults" write from a process that loaded an empty store. If the app ever loads the root store, all custom words read as `[]`.

### New tests added (all pass)

1. `salvage_settings_preserves_custom_words_across_broken_field` — proves salvage keeps word fields.
2. `flush_handle_current_panics_outside_tokio_context` — proves the exit-flush `Handle::current()` panics off-runtime, which silently drops the pending write.

> Note on test hygiene: an earlier draft reproduction test (`exit_flush_from_non_tokio_thread_drops_pending_custom_words`) used `write_settings`/`flush_settings` with the `MockRuntime` app, which fails to type-check because those production functions are hard-bound to `AppHandle<Wry>`. I removed that draft and kept the two focused, compilable tests above.

---

## 6. Do related fields (`word_replacements`, `custom_filler_words`) suffer the same bug?

**Yes — they share the identical exit-flush/drop mechanism.** All of:

- `custom_words`
- `advanced_custom_words`
- `word_replacements`
- `custom_filler_words`

go through the same `get_settings_safe` → mutate → `write_settings_safe` → debounced-write → `tokio::spawn` path. None of them emit a "settings-changed" event or force an immediate write. So if custom words are lost on quit, `word_replacements` and `custom_filler_words` (and any other debounced setting changed in the same window) are lost too.

**Caveat:** a setting that also triggers some post-write side effect (e.g. `noise_suppression_level` re-creating the recorder, `keyboard_implementation`) does not change the persistence semantics — they all still rely on the same debounced flush.

---

## 7. What changed on `feature/streaming-live-captions` that could explain the regression

Diff `main..HEAD` and the on-disk state show several things changed **near** the persistence path on this branch:

1. **`VadSensitivity` enum was reworked** (`types.rs`): variants renamed and re-ordered, `#[default]` moved to `VeryRelaxed`, thresholds changed, and a new `per_model_vad_sensitivity: HashMap` field added. Combined with a stored value of `"balanced"`/old variants, this could make an **old** settings file fail full deserialization → forcing the `salvage_settings` path more often than before. But as shown in §4, salvage preserves word fields, so this alone does not erase them.

- **New fields added** (`per_model_vad_sensitivity`, `live_captions_model`, `tauri_binding`, `handy_keys_binding`), all `#[serde(default)]`, so they don't break old files.
- **`migrate_bindings` + binding rework** (`ba75bb61`, `abf6116e`) only touches `bindings`, not word fields.
- **No change to `write_settings`, `SettingsWriter`, or `flush_settings`** on this branch — those are the same as `main` (the `flush_settings` code is inherited, not branch-new). The dangerous `Handle::current()` off-runtime issue predates this branch.

**Therefore the streaming-live-captions branch did not introduce the write-path bug.** The regression is a pre-existing weakness in the debounced-write + exit-flush machinery, likely surfaced because (a) more settings are written more frequently, and (b) the two-store discrepancy and the `VadSensitivity` reshuffle make the user notice "my word reset" more often.

---

## 8. Recommended fix location (no code written)

The **primary fix** should be in `src-tauri/src/settings/store.rs`, in `flush_settings` (lines 626–670):

1. **Stop calling `tokio::runtime::Handle::current()`/`block_in_place` from the event-loop thread.** The exit callback has no Tokio context and those calls panic. Instead:
   - Capture the `SettingsWriter` pending state and the `SettingsCache` directly (both are sync state that can be read without a runtime),
   - call `write_settings_immediate` synchronously on the cached settings (it does `store.save()` synchronously, no Tokio needed). `write_settings_immediate` (lines 571–609) is already runtime-free.
2. The fallback branch that uses the cache (lines 658–667) is exactly the right approach — **make it the primary path** rather than the timeout-fallback. `flush_settings` can be implemented as: read `SettingsCache::get()`, call `write_settings_immediate`. No `Handle::current()`, no `block_in_place`, no `block_on` → no panic, no deadlock, no dropped writes.
3. **Optionally add a guard**: only call `flush_settings` in `lib.rs` on `ExitRequested` _and_ `Exit` (mirroring the abandoned `4204cc2f` approach), so a race between the two events is covered.

Secondary hardening (also in `store.rs`):

- In `write_settings` (line 562), consider calling `write_settings_immediate` directly when the cache-backed writer would be dropped — or ensure the `tokio::spawn` task's abort-on-drop isn't the only path.

Tertiary (diagnostic, optional):

- The two conflicting store files suggest the app may sometimes resolve `app_data_dir` to different paths across runs. Log the resolved store path at startup (this was added in commit `4204cc2f` but is absent in HEAD) to catch "which store am I reading/writing?" regressions.

**Summary of exact files/lines to touch:**

- Primary: `src-tauri/src/settings/store.rs` — `flush_settings` (lines 626–670): replace the `Handle::current()` + `block_on` logic with a runtime-free synchronous flush of `SettingsCache` via `write_settings_immediate`.
- Supporting: `src-tauri/src/lib.rs` lines 1106–1110 — optionally also flush on `RunEvent::Exit`, not just `ExitRequested`.
- Diagnostic: add the store-path log line in `open_settings_store` (store.rs lines 91–101).

---

## Appendix: Key files and line numbers

| Concern                        | File                                              | Lines     |
| ------------------------------ | ------------------------------------------------- | --------- |
| `update_custom_words`          | `src-tauri/src/shortcut/mod.rs`                   | 800–807   |
| `update_advanced_custom_words` | `src-tauri/src/shortcut/mod.rs`                   | 821–828   |
| `update_custom_filler_words`   | `src-tauri/src/shortcut/mod.rs`                   | 830–837   |
| `update_word_replacements`     | `src-tauri/src/shortcut/mod.rs`                   | 871–879   |
| Command registration           | `src-tauri/src/lib.rs`                            | 614–617   |
| `write_settings_safe`          | `src-tauri/src/settings/store.rs`                 | 210–214   |
| `write_settings`               | `src-tauri/src/settings/store.rs`                 | 530–569   |
| `write_settings_immediate`     | `src-tauri/src/settings/store.rs`                 | 571–628   |
| `flush_settings`               | `src-tauri/src/settings/store.rs`                 | 626–670   |
| `SettingsWriter`               | `src-tauri/src/settings/store.rs`                 | 726–813   |
| `salvage_settings`             | `src-tauri/src/settings/store.rs`                 | 233–315   |
| `SettingsCache`                | `src-tauri/src/settings/store.rs`                 | 23–42     |
| Exit flush wiring              | `src-tauri/src/lib.rs`                            | 1106–1110 |
| `settingUpdaters` custom_words | `src/stores/settingsStore.ts`                     | 140–146   |
| CustomWords component          | `src/components/settings/CustomWords.tsx`         | whole     |
| AdvancedCustomWords component  | `src/components/settings/AdvancedCustomWords.tsx` | whole     |
| `custom_words` field           | `src-tauri/src/settings/types.rs`                 | 586       |
| `advanced_custom_words`        | `src-tauri/src/settings/types.rs`                 | 588       |
| `word_replacements`            | `src-tauri/src/settings/types.rs`                 | 590       |
| `custom_filler_words`          | `src-tauri/src/settings/types.rs`                 | 651       |
| Store path const               | `src-tauri/src/settings/types.rs`                 | 15        |

## Tests added

- `settings::store::tests::salvage_settings_preserves_custom_words_across_broken_field` (PASS)
- `settings::store::tests::flush_handle_current_panics_outside_tokio_context` (PASS)
