# Upstream Sync Plan — Handy fork (stable-handy) × cjpais/Handy

**Range:** `cdbc2239..9e57a0d7` (161 upstream commits, 2026-07-18 → 2026-10-15)
**Fork:** branch `main`, HEAD `644fa46f` (v0.12.2), +371 commits past merge-base `4b7bb4e5`
**Generated:** 2026-10-18 · Decision-gate field research (D3, D5): 2026-10-18 · Sources: /tmp/u-sync-upstream.md, /tmp/u-sync-fork.md, UPSTREAM_SYNC.md (stale), `HANDY-FORK/research/onnx-to-gguf-consolidation-research.md` (D3 evidence base)
**Working directory:** `/Users/caffae/Local-Projects-2026/Handy-Fork/stable-handy` (all code work is in the Handy-Mac submodule tree)

---

## 0. Executive Summary

**Goal:** Bring this macOS-focused fork up to upstream parity for the 161-commit range, while **preserving the fork's load-bearing macOS work** (AX paste verification, NSPanel overlay, preroll fixes, settings cache, coordinator state machine) and re-skinning upstream's *behavior* onto the fork's *structure* where cherry-picks cannot apply.

**Scope arithmetic (exactly 161, every sha in exactly one bucket):**

| Bucket | Count | Definition / members summary |
|---|---|---|
| **APPLY** | **61** | Ported in Phases 1–10 (the phase tables are the authoritative per-sha list). 10 of them behind decision gates D1–D4 (`c8262ab3`, `1b011a19`, `a42131d2`, `3985508a`, `cfcb6507`, `5197faf0`, `4150a4a8`, `9e57a0d7`, `a70ac84f` + matrix-dependent outcomes). |
| **PORTED** | **13** | Already present in fork: `20ada47d`, `9e534a3d`, `292db647`, `12f02e2a`, `c89b7bf3`, `a4348beb`, `df216832`, `2203a826`, `2cf157d9`, `09aaf4d3`, `db1aaac7`, `68af495e` — Sep 16–17 round + earlier; plus `7b2a28a3` (change present via fork's stronger `ddffa435`). |
| **VERIFY** | **6** | Likely already in fork — read-diff, then resolve to tiny-APPLY or S: `d55ea7ef` (+ bundled merge-fix `0e503672`), `86616891`, `57324065`, `e8c73ba7`, `f6fac42e`. |
| **SKIP-superseded** | **5** | `afbf44cd` (fork past 0.2.0 API), `b4453a29`, `d54c88eb`, `db003f38`, `549cbde3` (all superseded by `a42131d2`; behaviors verified post-port). |
| **SKIP-policy** | **76** | 66 named (Linux/Windows/Nix/translations/docs/releases/CI/README/PR-template/bumps, plus Windows-portable `a94b403e`/`76736d5a`, Windows-only `b1b2d9f9`, deferred-sidebar `cdf5028b`) **+ 10 un-itemized shas** present in the range but not individually analyzed in /tmp/u-sync-upstream.md — enumerated one-by-one in Phase 0 via `git log cdbc2239..9e57a0d7` (default SKIP-policy; anything surfacing as relevant is re-bucketed APPLY *before* the port starts). |
| **Total** | **161** | 61+13+6+5+76 ✓ |

**Strategy choice — "upstream-first, structure-preserving":**
The fork already adopted an upstream-first strategy (UPSTREAM_SYNC.md, July 2026). Therefore the **default on every open question is "adopt upstream behavior."** But the fork restructured four subsystems (settings → `settings/`, actions → `actions/`, overlay FE → hooks/, transcription in-process → supervisor client pending) and carries macOS-specific quality work upstream never had. So the merge discipline is:

1. **Greenfield/new files** (`engine_supervisor/`, `secure_input.rs`, `chunking.rs`, `sink.rs`, `lang_id.rs`, `chinese_script.rs`, `autostart.rs` (0902937b), `managers/transcription/{live,chunked,policy}.rs` (9e57a0d7), `paste_tx/`) → cherry-pick or port as fresh modules (A/C).
2. **Fork-restructured targets** (settings.rs, actions.rs, transcription.rs) → **path remapping + behavior port** (C), never textual application.
3. **Rival designs in same file** (paste) → **keep fork design**, adopt upstream items additively (gate D4).
4. **Skip-by-policy with one-line reasons** recorded in the registry — so re-auditing later is a table lookup.

**Execution mode:** dedicated branch `sync/2026-10-1`, tag before each phase, conventional commits, standing phase gate = `cargo check` + `bun run build` (+ `bun run lint` where FE touched) before the phase is closed and tagged.

**Total estimate = per-phase sums: ~26–43 focused blocks (≈30–60 min each).** Realistic wall-clock: the engine arc dominates — P6–P9 ≈ 12–20 blocks of that total; the low-risk runway P0–P5 ≈ 12–18 blocks (incl. D5's conditional 1–2). (Sums derived from per-phase estimates in §2: 1+1+(3–5)+(1–2)+(3–5)+(2–4 + D5 1–2)+(4–6)+(3–5)+(2–4)+(3–5)+(1.5–2.5).)

---

## 1. Merge Strategy Codes & Standing Rules

| Code | Meaning |
|------|---------|
| **A** | Cherry-pick applies clean (or Cargo-only) |
| **B** | Cherry-pick + manual conflict resolve (same shape, both sides changed it) |
| **C** | Manual re-skin/re-implement (patch does not apply — fork restructured or file fork-only) |
| **S** | Skip (policy / already present / superseded) — record one-line reason in registry |
| **D** | Decision gate — stop, ask user per §3 defaults |

**Standing phase gate (every phase):** `cargo check` in `src-tauri/`, `bun run build` (tsc + vite), `bun run lint` if FE touched. Targeted `cargo test` where tests exist in a touched area.

**Standing rules:**
- Reference code by **symbol, not line number** (Handy-Fork/AGENTS.md convention).
- i18n: only add keys to fork's **en/zh/zh-TW** `translation.json` (fork dropped other locales); locale-only hunks of any FE commit are trimmed.
- Every new backend setting: field in `settings/types.rs` + default in `settings/defaults.rs` (serde default) + `settingsStore.ts` updater + placement in fork's `Sidebar.tsx` `SECTIONS_CONFIG` + i18n keys.
- Every commit that upstream targeted at single-file `settings.rs` / `actions.rs` gets its hunk placed into the fork's module split (see Playbook §5.5).
- transcription.rs-touching hunks of early commits are **deferred to the arc phases** where noted, to avoid porting them twice (fork's transcription.rs is rewritten in Phases 6–9). Flagged so far: `29bd2c0d`, `e4ae0d44`, `258899a2`, `4cd49950`, `6fa85061`, `0bb0428e`.

---

## 2. Phase Breakdown

### Phase 0 — Pre-flight & registry rebuild (no upstream commits)

**Purpose:** The registry is stale (says last check 2026-07-21 / branch `cdbc223`, but a Sep 16–17 round ported ~12 commits). Fix the record before touching code, and establish a clean, verified baseline.

**Steps:**
1. `cd stable-handy && git fetch upstream` — confirm `upstream/main` HEAD = `9e57a0d7`.
2. `git log --oneline 644fa46f -20` + `git log --all --grep="<sha-prefix>"` to locate fork commit hashes for the already-ported set: `20ada47d` (earshot VAD), `9e534a3d`, `292db647` (HF harden → fork `38832ef1` mirror fallback), `12f02e2a` (channel select), `c89b7bf3` (default mic fallback), `a4348beb` (stream recovery), `df216832` (tail audio), `2203a826` (custom-word correction), `2cf157d9` (multi-word), `09aaf4d3` (moonshine streaming), `db1aaac7` (transcribe-cpp 0.2.3), `68af495e` (no HF env — verified present in `download_hf_model`).
3. **Mechanical enumeration of the 10 un-itemized shas:** `git log --oneline cdbc2239..9e57a0d7` minus every sha named in this plan → the ~10 remainder commits; inspect each briefly and assign SKIP-policy (expected: README/sponsors/PR-template/odds-and-ends) — any relevant one is re-bucketed APPLY *now*, before Phase 1 starts. This completes the "exactly one bucket per sha" requirement mechanically.
4. Rewrite UPSTREAM_SYNC.md "Current Status" block: Last Sync = Sep 16–17 port round (actual commit hashes), Last Upstream HEAD Examined = `9e57a0d7`, Fork version = 0.12.2, counts Synced/Skipped/Pending per this plan's §4.
5. Create working branch `sync/2026-10-1` from `main`; tag `pre-sync-0.12.2`.
6. Baseline verification (record output): `cargo check`, `cargo test` (audio_toolkit + transcription_coordinator_tests), `bun run build`, `bun run lint`. If baseline is red, STOP and report before any sync.

**Verification:** baseline green + registry block corrected + 10-sha enumeration complete.
**Rollback:** nothing to roll back (no source changes).
**Risk:** LOW. **Effort:** 1 block.

---

### Phase 1 — Trivial plumbing: Cargo bumps, deps, catalog verify (5 APPLY + 2 VERIFY)

**Purpose:** Zero-conflict groundwork; the Cargo-only rungs of the engine arc come first so later phases don't stack lock churn.

| Commit | Title | Strategy | Files | Notes |
|---|---|---|---|---|
| `2d526b15` | transcribe-cpp 0.2.4 (#2147) | A | Cargo.toml/lock | Cargo-only |
| `ffbc9504` | transcribe 0.3.0 (#2203) | A | Cargo.toml/lock | Cargo-only |
| `1bcbfc4c` | reqwest gzip/brotli/deflate features (#1548) | A/B | Cargo.toml | fork's reqwest lacks them; verify no dep clash with vendored updater/patched hf-hub |
| `d9615937` | js-yaml bump (quadratic parse) | A | package.json/lock | |
| `6d3239e0` | What's New parser compat (#1822) | B | `lib/compat.ts`, ErrorBoundary | small FE guard; if fork lacks the whats-new module, port only the ErrorBoundary hardening |
| `e8c73ba7` | catalog.json refresh | **verify→S** | catalog | fork catalog already lists handy-computer GGUF repos — diff then skip, record fork state in registry |
| `f6fac42e` | tauri-plugin-updater 2.10.1 | **verify→S unless D6 override** | vendored plugin | verify fork updater state first (vendored 2.10.0 + `dangerous-skip-verify`) → S by default; adopt crates.io 2.10.1 only if D6 outcome = bump |

**Verification:** standing gate; `cargo lock` resolves; `bun run build`.
**Rollback:** revert the phase commit range on `sync/2026-10-1`.
**Risk:** LOW. **Effort:** 1 block.

---

### Phase 2 — Backend small fixes: macOS core + model lifecycle + LLM + language/text (21 APPLY + 2 VERIFY)

**Purpose:** Bank all the independent low-risk behavioral gains before the big re-skins. Order inside the phase: macOS core → model lifecycle → LLM → language/text.

**Batch 2a — macOS core (strat A/B, small diffs):**

| Commit | Title | Strategy | Fork target symbols | Notes |
|---|---|---|---|---|
| `98a4d80c` | GGML_METAL_NO_RESIDENCY=1 (#1902) | A | `lib.rs` setup | macOS must-have; set before transcribe-cpp Metal init; `HANDY_METAL_RESIDENCY=1` opt-out |
| `cf49ab35` | run overlay on main thread (#1810) | B | `overlay.rs` `show_overlay_state`/`update_overlay_position` | add `run_on_main_thread` hop before NSScreen/cursor queries; **fork's `run_on_main_thread` one-shot position+size atomicity already exists — merge the hop into fork's flow, don't duplicate**; if clean application fails (fork monitor-targeting), hand-place the main-thread hop only |
| `29bd2c0d` | device listing off startup path (#2160) | B, hunk-deferred | `lib.rs` + `managers/transcription.rs` | port the `report_compute_devices()`-on-prewarm-thread shape; **lib.rs hunk lands now; transcription.rs hunk deferred to Phase 6** |
| `8a362e9e` | mute fix (#1760) | B | `managers/audio.rs` `get_mute()`/restore-mute | osascript output-muted probe; never strand muted; port into fork audio manager |
| `6cad594c` | settings window maximizable (#1778) | A | `lib.rs` 1 line | |
| `fbd4e15f` | Accessory launch when hidden (#2003) | B | `lib.rs` `apply_startup_activation_policy` | macOS-only; place between build() and run(); interacts with fork tray presence + `--start-hidden` |
| `0902937b` | SMAppService login item (#1825) | C | new `autostart.rs` + `lib.rs` + Cargo `objc2-service-management` | greenfield module; replaces tauri-plugin-autostart on macOS 13+; remove old launch-agent plist; keep tauri plugin for other platforms |
| `76b44d83` | recalibrate mic level meter (#1813) | B | `audio_toolkit/audio/visualizer.rs` | |

**Batch 2b — model lifecycle & coordinator-adjacent (strat B):**

| Commit | Title | Strategy | Fork target | Notes |
|---|---|---|---|---|
| `e4ae0d44` | recover when model files deleted (#1918) | B, hunk-deferred | `managers/model.rs` `loading_failed` events + FE ModelsSettings + modelStore invalidation | model.rs + FE hunks land now; **transcription.rs 46-line hunk deferred to Phase 6** rewrite |
| `dc5bdc9d` | unload model after empty recordings (#2106) | B | `actions/transcribe.rs` `FinishGuard` gains `Arc<TranscriptionManager>` → `maybe_unload_immediately` | sequential with next |
| `eea1f5f4` | refuse start when no transcribable model (#2161) | B | `actions/transcribe.rs` availability check before tray/overlay/mic | **must come after `dc5bdc9d`** |
| `258899a2` | redact transcription content from logs | B | `actions/`, `utils.rs`; transcription.rs hunk deferred to arc | 15 lines total; transcription.rs hunk joins Phase 6 |

**Batch 2c — LLM/post-processing (strat A/B, small):**
`148e5492` (strip `IGHL<...>IGH` reasoning blocks), `2211da65` (`stream:false`), `4223e7ac` (bounded reqwest error-chain reporting, sanitized), `14ae9c6b` (**Anthropic Messages API** — `PostProcessApiFormat` enum setting + provider migration + `length` finish-reason rejection; largest of the four, port the setting through the settings-remap playbook).

**Batch 2d — language/text (strat B/C; three commits are hunk-deferred to P6):**

| Commit | Title | Strategy | Notes |
|---|---|---|---|
| `4cd49950` | lang_id fallback + filler fixes (#1738) | C, **hunk-deferred to P6** | lands in P2: new `audio_toolkit/lang_id.rs` (whatlang/isolang deps), text.rs filler logic, settings field, FE FillerWordRemoval (skip i18n hunks). **Deferred to P6:** wiring the lang_id text-fallback into fork's transcription.rs language-param plumbing (`cpp_translation_task()`-adjacent) — do not wire it twice across the P6 rewrite |
| `8ef8dd43` | stop removing "Ha" as filler (#2156) | A/B | onto fork text.rs after `4cd49950` |
| `eb49dc02` | keep capitalization after leading-filler removal (#2157) | A/B | same |
| `6fa85061` | "no" language fix (#2001) | B, **hunk-deferred to P6** | lands in P2: language-coverage constants/logic in `managers/model.rs` + `LanguageSelector` constants (skip locale-file hunks by policy). **Deferred to P6:** the transcription.rs side of the language-coverage guard |
| `0bb0428e` | chinese script selector (#2186) | C, **hunk-deferred to P6** | lands in P2: fresh `chinese_script.rs` + `ChineseScript` setting (fork already has ferrous-opencc dep + zh conversion inside spelling_dictionaries.rs — wire module to existing conversion state), lang_id/text simplifications, FE selector. **Deferred to P6:** transcription.rs hooks (script-aware language-param handling) |
| `d55ea7ef`+`0e503672` | drop 'gpu' accelerator (merged form) | **VERIFY** | fork already at/above this API level (Backend::Auto + device binding); read net diff, record gap if any — likely S |

**Verification:** standing gate + `cargo test audio_toolkit` (filler/text tests if present) + manual smoke: record→transcribe, post-process, language selector, models page.
**Rollback:** per-batch conventional commits make surgical revert trivial.
**Risk:** LOW–MEDIUM (`cf49ab35`, `0902937b`, `14ae9c6b` are the touchiest). **Effort:** 3–5 blocks.

---

### Phase 3 — Frontend small fixes (8 APPLY + 1 VERIFY + 1 SKIP-policy)

**Purpose:** Independent FE gains; no backend coupling except bindings regeneration.

| Commit | Title | Strategy | Notes |
|---|---|---|---|
| `496a80cc` | history copy-success after clipboard write (#2011) | B | `HistorySettings.tsx` (fork's is larger, 783 lines — hand-place), `lib/clipboard.ts` + test |
| `345caa8f` | refresh custom sounds on selector open (#1941) | B | `Dropdown.tsx` open trigger |
| `4b3a969d` | streaming/translation filter buttons (#1815) | C | `ModelsSettings.tsx`; fork's ModelsSettings diverged — hand-place |
| `53db4cb6` | aria for setting names (#2238) | A/B | `Slider.tsx`, `ToggleSwitch.tsx` |
| `7c31572d` | reset settings scroll on section switch (#2082) | B | `App.tsx` |
| `330b2ddb` | macOS scrollbar styling (#2048) | A | `App.css` |
| `1cd92662` | onboarding horizontal overflow (#2050) | B | `App.css`, `Onboarding.tsx` |
| `86616891` | update checker overlay fix | **verify→S/B** | fork analysis says UpdateChecker already contains a version — diff-check first |
| `988521f8` | debug UI additions (#2052) | C | fork DebugSettings structurally different; hand-place into fork's debug/ components |
| `cdf5028b` | Sidebar settings refactor (#1720) | **SKIP-policy (deferred — reassess)** | big FE refactor colliding with fork's Accuracy/Usage sections; skip this round, reassess after sync (note in registry) |

**Verification:** standing gate **+ `bun run lint`**.
**Risk:** LOW (except `988521f8` placement). **Effort:** 1–2 blocks.

---

### Phase 4 — Paste / clipboard + Secure Input cluster (9 rows: 8 APPLY — 1 of them D4-gated — + 1 SKIP-policy)

**Purpose:** Enhance the paste path **while keeping the fork's AX verify-then-commit design as the default** (it is verified, load-bearing, and `macos_ax` is fork-owned). All upstream items here are additive; gated work: `a70ac84f` (D4: port vs idea-borrow vs skip) — `b1b2d9f9` is a Windows-policy skip under **all** D4 outcomes (its macOS-relevant substance, the concealment markers, is exactly D4's idea-borrow option).

**Order (dependency-driven):**

| # | Commit | Title | Strategy | Files/symbols | Notes |
|---|---|---|---|---|---|
| 1 | `3ed2b219` | restore clipboard images (#1231) | B | `clipboard.rs` `paste_via_clipboard`/`paste()` | read_image probe only on no-text path; restore images; clear when empty; preserve fork's Finder/Desktop fallback ordering |
| 2 | `aefc1319` | layout-aware paste key (#1911) | C→A | `input.rs` `send_paste_*` | TISCopyCurrentKeyboardLayoutInputSource + UCKeyTranslate; ANSI-V fallback; ~146 lines; new objc2-Carbon deps if vendored — check Cargo |
| 3 | `bc7facea` | paste delay max 5000ms | A | FE `PasteDelay.tsx` (fork: debug settings) | 1 line |
| 4 | `d001fcd9` | Secure Keyboard Entry warning (#1785) | C | **new `secure_input.rs`** (660) + `shortcut/handy_keys.rs` `register_cancel_fallback` + `tray.rs` `get_icon_path(theme, state, warning)` + `lib.rs` (menu item, command reg) + FE `SecureInputWarning.tsx` | greenfield module; wire into **fork's handy_keys manager thread shape** (differs ~352 lines from upstream); poll `CGEventSourceGetKeyboardEventSecureEventInputState`; fallback cancel shortcut |
| 5 | `00d25549` | preserve held Secure Input shortcuts (#2002) | B | `secure_input.rs` | **after d001fcd9** |
| 6 | `37a26fd6` | tray warning clear on deactivation | B | `secure_input.rs` + `tray.rs` | snapshot `warning_was_active` before clearing |
| 7 | `5ec2276a` | single-writer tray `set_tray_state` (#1952) | C | `tray.rs` rename `change_tray_icon`→`set_tray_state` + `actions/` + `lib.rs` + `utils.rs` + `secure_input.rs` + `shortcut/mod.rs` | whole-tree rename; touches the secure_input module just landed — do after #4–6 |
| 8 | `a70ac84f` | paste_tx receipt-sequenced paste (#1812) | **D4** | | gate: full opt-in port (a) / idea-borrow (b) / skip (c) — default (c), see §3 |
| 9 | `b1b2d9f9` | paste_tx windows fix (#1847) | **S (Windows policy, all D4 outcomes)** | | Windows-only commit; no macOS code; the macOS-relevant idea (NSPasteboard concealment markers) is captured by D4 option (b) — landing the commit is forbidden by fork's Windows-skip policy |

**Verification:** standing gate + `bun run lint` (FE touched) + smoke: normal paste into TextEdit, paste into Finder, paste with clipboard holding an image, Secure-Input banner appears when SKE active (can simulate with `secure keyboard on` via Carbon app), tray icon swap, cancel shortcut fallback works under SKE.
**Rollback:** `secure_input.rs` and tray rename are cleanly separable commits.
**Risk:** MEDIUM (tray rename touches many call sites; secure-input wiring into fork manager thread). **Effort:** 3–5 blocks.

---

### Phase 5 — Shortcut lifecycle hardening (7 APPLY + 1 conditional D5 vendor step)

**Purpose:** Batch the shortcut/ subsystem: vendor refresh first (D5, so the ports land against the 0.3.x crate API), then validation fixes, then the racing/parity work, then push-to-talk on top.

| Commit | Title | Strategy | Notes |
|---|---|---|---|
| **step 0 (D5)** | handy-keys vendor refresh: `0.2.4-handy.1` → **0.3.4** | **conditional APPLY (D5 default)** | Swap `vendor/handy-keys/` to 0.3.4; **re-apply fork's `pub fn reset_pressed_hotkeys()` + press-suppression diagnostics** (~20 ln mechanical re-application onto `manager.rs`); **drop fork's ~300 ln CGEventTap silent-death recovery in favor of upstream #31 (`7903fda`) `CallbackCtx`/`TapRunLoop` implementation (+ its unit tests)** — same failure, incompatible design, carrying both is untestable. API is additions-only 0.2.4→0.3.4 (fork call sites in `shortcut/handy_keys.rs` compile as-is). **Acceptance gate: macOS sleep/wake + recording-hotkey regression pass** (wake-from-sleep stuck keys, rapid start/stop, cancel spam, Secure-Input fallback); on failure revert the vendored-dir swap. Gains: modifier-only release fix, Globe key 0xB0, Pause/PrintScreen. |
| `f5c27e69` | validate_shortcut parses raw accelerator (#2158) | A/B | `shortcut/tauri_impl.rs`; self-contained |
| `a6eed754` | reject unknown binding IDs on reset (#2033) | B, path-remap | valid-ids list → fork `settings/store.rs` region of `reset_binding` + `settings/types.rs` if the list lives in settings. **Fork analysis: unknown-input rejection already satisfied in handler.rs (warns + skips) — verify reset path specifically since that's what a6eed754 fixes** |
| `141f981d` | compound key compact names (#1862) | B/C | shared mapping table `shortcut/mod.rs` + `shortcut/handy_keys.rs` + `shortcut/tauri_impl.rs` + FE `src/lib/utils/keyboard.ts` (+fork test); friendly labels separate |
| `16e5d48e` | suspend_all_shortcuts during rebind (#1811) | B/C | replace fork's `suspend_binding` per-binding command with `suspend_all_shortcuts()`; ⚠ API surface change: update `lib.rs` command list + `bindings.ts` |
| `b38987a9` | cancel-shortcut race fix (#2190) | B | `CANCEL_REQUESTED` AtomicBool + `CANCEL_REGISTERED` + `schedule_cancel_reconcile()`; deletes `tauri_impl::register_cancel_shortcut`/`unregister_*`, re-implements in mod.rs shared; **after `d001fcd9`** (interacts with `register_cancel_fallback`) — hence after Phase 4 |
| `c6fa60da` | toggle parity across busy-window presses (#1910) | **C — behavior port** | upstream patches a coordinator that structurally differs from fork's. Port the *behaviors* into fork's coordinator: (1) busy-window `PendingPress` remembered while pipeline busy (BusyAction Ignore/Remember/Forget), (2) external triggers (SIGUSR2/CLI — fork uses SIGUSR1/2 in `signal_handle.rs`) bypass keyboard debounce, (3) delayed-hide dropped when new session shows overlay, (4) other bindings ignored while a busy-window press is pending. Fork already has `CoordinatorCore`/`Stage` machine + tests — express as new `StageAction` rules |
| `c62a5fcd` | auto push-to-talk / HoldOrToggle (#1971) | C | **hard dep on c6fa60da behavior**: hold-vs-tap classify for remembered presses; new `ShortcutActivation` enum (replaces push_to_talk bool + migration); `shortcut/handler.rs` + FE PushToTalk/ShortcutActivation/HoldThreshold/GeneralSettings; settings-remap playbook |

**Verification:** standing gate + `cargo test transcription_coordinator_tests` (extend fork tests for new StageActions) + **D5 acceptance pass: macOS sleep/wake + recording-hotkey regression (must pass before trusting the 0.3.4 swap)** + smoke: rapid start/stop spam, cancel-spam during Processing, PTT vs toggle modes, record-rebind flow.
**Rollback:** per-commit; `c6fa60da`+`c62a5fcd` are one logical unit if the builder struggles to split; **D5 rollback = revert the vendored-dir swap to `0.2.4-handy.1` (single directory).**
**Risk:** MEDIUM–HIGH (coordinator behavior port; fork tests are the safety net; D5's upstream #31 is rewrite-grade — mitigated by its unit tests + the regression pass). **Effort:** 2–4 blocks + 1–2 for D5 (conditional, default-on).

---

### Phase 6 — Engine arc, part 1: engine_supervisor + transcribe-cpp 0.4.0 (2 APPLY; umbrella-gated D1)

**Purpose:** Adopt the biggest architectural change: all transcribe.cpp native code (backend init, device enumeration, model load, inference, streaming) moves into a **child worker process** (`engine_supervisor/`), so GPU faults/aborts kill only the worker. Fork's transcription.rs becomes a client of the supervisor.

**D1 gates this phase and P8/P9** (see §3 matrix): default = adopt full arc (row 1). If D1 = (b) partial, `c8262ab3` is S and `1b011a19` lands with in-process adaptation; if D1 = (c), both are S. The **Phase-2 deferred hunks** (`29bd2c0d`, `e4ae0d44`, `258899a2`, `4cd49950`, `6fa85061`, `0bb0428e` transcription.rs sides) land here, inside the rewrite.

| Commit | Title | Strategy | Detail |
|---|---|---|---|
| `c8262ab3` | isolate backend in own worker (#2208) | **C — largest single step in the plan** | New files `engine_supervisor/{mod,protocol,supervisor,worker}.rs` (~2.8k lines) port as fresh modules (upstream-only today). Cargo 0.3.0→0.3.1 + `serde` feature, `libc` global. Wire: `main.rs` worker dispatch (`--transcribe-worker`, `--cpu-only`), `lib.rs` supervisor init + command list, `commands/models.rs`, `commands/transcription.rs`, `memory.rs` (6), `shortcut/mod.rs` (18). **transcription.rs becomes supervisor client:** re-skin — keep fork's public surface (`start_stream`, `transcribe`, `cancel_stream`, `finalize_stream`, `StreamRouter` plumbing) but route runs through supervisor `Request/Response` protocol. Fork-only machinery to preserve during re-skin: `StreamingCancelFlag` (seq/flag atomics), stream perf counters `StreamPerf`, catch_unwind engine handling (note: catch-unwind no longer needed in-process once native code is in worker — keep for the in-process fallbacks), slot/watchdog force-release interplay with `managers/retry_worker.rs`, `suppressed_token_count` in `TranscriptionResult`, lazy_stream_close. **Fold in the six Phase-2 deferred hunks** (compute-device listing, loading_failed recovery, log redaction, lang_id wiring, language-coverage guard, chinese_script hooks — see §5.1). |
| `1b011a19` | transcribe-cpp 0.4.0 (#2265) | A (after c8262ab3) | Cargo 0.3.1→0.4.0; `engine_supervisor/worker.rs` adapt `model.capabilities()` → Result (small) |

**Design notes to give the builder:**
- Fork's `init_transcribe_backend()` / `resolve_gpu_device()` / `resolve_device_index()` / `get_available_accelerators()` move behind Hello/Load protocol or supervisor-assisted listing (29bd2c0d's deferred hunk lands here).
- `transcribe_cpp_run_plan()` + `cpp_translation_task()` (explicit target_language semantics) must survive — they are fork improvements; re-express as client-side run-option construction (this seeds Phase 9's vocabulary change).
- Keep `managers/transcription_mock.rs` tests compiling (mock the supervisor client trait if needed — introduce a small `EngineClient` trait so mock stays viable).
- Fork's `--debug` CLI flag and health.rs must not assume in-process panics anymore: supervisor crash = event + auto-restart via `retry_gpu()` semantics.

**Verification:** standing gate + `cargo test transcription` + smoke: batch transcribe (Metal), streaming live captions (feed/finalize paths), backend listing in Models settings, kill -9 the worker during load and verify app recovers (worker-relaunch UX), CPU-only fallback after simulated GPU fault (`HANDY` env or fault injection as available).
**Rollback:** the arc phases are the reason the branch + tags exist; `git reset --hard sync-phase-5-done` aborts the whole arc.
**Risk:** **HIGH** (behavior-level re-skin of a +2756-line file; streaming path regression risk). **Effort:** 4–6 blocks.

---

### Phase 7 — Engine arc, part 2: audio capture switch to handy-recorder (1 APPLY + re-port task; gated **D2 only**)

**Purpose:** Replace the fork's 1046-line `recorder.rs` + `resampler.rs` + `bin/cli.rs` with the `handy-recorder` crate + app-side `CaptureSink` (`audio_toolkit/audio/sink.rs`). **This is where the fork's preroll fixes live, so D2's plan must include re-applying them.** Independent of transcribe-cpp 0.4.x — runs under D2 regardless of D1 outcome (only its patch-shape relation to P8/P9 commits matters; see matrix).

| Commit | Title | Strategy | Detail |
|---|---|---|---|
| `a42131d2` | move to handy-recorder (#2188) | **C + required follow-up work** | Deletes `recorder.rs`, `recorder/tests.rs`, `resampler.rs`, `bin/cli.rs`; adds `sink.rs` (+tests). Cargo: removes `cpal`, `rubato`, `rtrb`; adds `handy-recorder = 0.1`; rodio → crates.io 0.23 (audio_feedback.rs). `managers/audio.rs` (567 lines) re-skin: device resolution `Option<Device>` → `MicrophoneResolution{device: Option<String>, from_cache}`; `recording_error_type()` mapping. `commands/audio.rs`, `actions/`, `device.rs`. **Mandatory re-port task (this is the heart of D2):** re-express, against `CaptureSink`/`RecorderConfig`/`VadSlot{Unloaded,Idle,Lent}`/`OpenMicrophone.needs_reopen()`/`EndReason`: ① `9e3b7f65` — commands drained at TOP of consumer loop; `apply_start()` before every chunk; `Cmd::Stop` deferred (`pending_stop`) until in-flight chunk processed; deferred stop finalized even if stream dead; pending_stop cleared on rapid Stop→Start. ② `ddffa435` — **full preroll slice emission**: NS-off path appends the full emitted `VadFrame::Speech` slice (pre-roll + current) into out_buf — the first-word fix; NS-on keeps append-current original. ③ live-captions seam: `router.feed(frame)` per-frame feed into StreamRouter — re-plumb via CaptureSink's VAD/level callbacks. ④ always-on mode + input channel selection + mute integration (`8a362e9e` behavior) as RecorderConfig options. Keep fork regression tests as sink-path equivalents (port `smoothed_vad_preroll_is_emitted_into_out_buf`, `re_onset_does_not_duplicate_hangover_frames` intent). |

**Superseded — port NONE of them** (`b4453a29` async cpal commands, `d54c88eb` rt-safe ring buffer, `db003f38` idle-skip level meter, `549cbde3` readiness handle): verify the *behaviors* post-port instead — (a) slow Bluetooth opens must not freeze main thread (b4453a29's concern; handy-recorder + `spawn_blocking` should cover — test), (b) audio feedback sounds still work on rodio 0.23, (c) recording UI must not show "recording" before first mic samples (549cbde3's concern; map to OpenMicrophone readiness/EndReason).

**Verification:** standing gate + `cargo test` (audio sink tests, smoothed tests) + **the preroll A/B harness**: record a sentence with NS-off, confirm first word intact in WAV (fork's learning-log method: raw-vs-vad samples via `examples/transcribe_wav.rs`); NS-on sanity; rapid stop→start; device hot-unplug → `needs_reopen()` recovery; live captions stream during long recording; mute toggle during recording.
**Rollback:** recorder.rs exists at tag `sync-phase-6-done`; full restore possible, but prefer forward-fix since phases 8–9 depend on this shape.
**Risk:** **HIGH** (regression would reintroduce the first-word-cut bug the fork spent Oct 2 fixing). **Effort:** 3–5 blocks.

---

### Phase 8 — Engine arc, part 3: VAD via transcribe-cpp + ONNX retirement + migration UI (3 APPLY; gated D1 × D3 — closure matrix §3)

**Purpose:** Move Silero VAD into transcribe.cpp (removes `vad-rs` + the ONNX VAD model), retire all ONNX engines (`transcribe-rs` deleted), and give ONNX-model users a migration UI. **Fate of all three commits depends on the D1×D2×D3 matrix — land as designed only in matrix rows 1/3/5.**

| Commit | Title | Strategy | Detail |
|---|---|---|---|
| `3985508a` | transcribe.cpp vad (#2267) | **C** | Cargo: remove `vad-rs`; resource swap `silero_vad_v4.onnx` → `silero-vad-v6.2-F32.gguf`. Rewrite `vad/silero.rs` onto `transcribe_cpp::{Model, VadSession, BackendMask::CPU}` (CPU-only in app process, poison-recovery). **Fork adaptation:** frame geometry 480/30ms → 512/32ms — but keep the fork's duration-based abstraction: update `frames_for_duration_ms()` (Silero = 512/32ms arm) rather than reverting to fixed frames; `vad/mod.rs` frame-count constants change accordingly (VAD_PREFILL_MS etc. are already in ms — mostly free). Keep Earshot arm untouched (earshot.rs score exposure comes in Phase 9). Keep fork's threshold-per-backend conversion logic. |
| `cfcb6507` | UI to migrate ONNX users to GGUF (#2277) | C (requires D3-adopt) | FE: new `onboarding/ModelMigration.tsx` (501) + `retiredModels.ts` (FE-side map first) + App.tsx wiring + ModelsSettings RetiredModelsNotice; BE: logging only. Land before `5197faf0` (FE flips to backend commands in it). |
| `5197faf0` | actually remove ONNX (#2279) | **C (gated D3)** | Removes `transcribe-rs`, `tar`, `flate2` deps; `managers/model.rs`: delete `EngineType`, add `RETIRED_ONNX_MODELS` + `get_retired_models()`/`delete_retired_model()` commands; transcription.rs: −520 transcribe_rs paths (fold into the Phase-6 re-skinned client + fork engine variants); settings: delete `OrtAcceleratorSetting`/`ort_accelerator` (**remap playbook** + serde migration for users who have it saved); `catalog/mod.rs`, `lib.rs`, `shortcut/mod.rs`, `build.rs` (−79 ORT lines; **adds explicit `libclang_rt.osx.a` link for ggml-metal on macOS — don't lose this**) ; `.github/workflows/build.yml` macOS jobs; FE `retiredModels.ts` → backend-listed, `AccelerationSelector.tsx` shrink. **Fork's engine removals:** Parakeet/GigaAM/SenseVoice/Moonshine ONNX variants leave `LoadedEngine`; their GGUF replacements come from the handy-computer catalog the fork already ships. Also delete the dead `vendor/transcribe-rs/` directory the fork analysis found. |

**If D3 = reject (keep ONNX dual-stack):** do NOT land any of these three as-is — the closure matrix (§3, rows 2/4/6) governs: `3985508a` → S (its substance is GGUF-VAD migration, which was designed for the all-GGUF world; re-skinning "onto `vad-rs`" means the fork simply keeps its VAD — nothing left to port), `cfcb6507` → S (migration UI pointless when ONNX stays), `5197faf0` → re-skin as "keep both engines" → effectively S (no RETIRED-table, no build.rs ORT cleanup — ORT remains a real dependency; `tar`/`flate2` stay needed for ONNX model archives; `ort_accelerator` setting remains). P6 and P9-`4150a4a8` still proceed under D1-adopt — see matrix.

**Verification:** standing gate + `cargo test` + smoke: fresh install onboarding still downloads a default GGUF model; **ONNX→GGUF migration flow** with a simulated old-model install (drop a Parakeet ONNX dir in place, see migration UI); translation + SenseVoice/GigaAM/Moonshine GGUF load paths; `build.rs` macOS link check (clean build from scratch, Metal inference works). (Under D3-reject: verify instead that translation + Moonshine/SenseVoice/GigaAM ONNX load paths still work with VAD unchanged.)
**Rollback:** per-commit within phase; `5197faf0` is the one-way door — if reverting after model catalog churn, revert catalog+FE+BE together.
**Risk:** HIGH (model-availability regressions hit every user's workflow). **Effort:** 2–4 blocks.

---

### Phase 9 — Engine arc, part 4: vocabulary API + long-form chunking (2 APPLY; requires D1-adopt — matrix §3 rows 1/2/3/4; rows 5–8 → downgraded/deferred)

**Purpose:** The two feature-commits on top of the arc: custom words through `RunOptions.vocabulary`, and VAD-based long-form chunking.

| Commit | Title | Strategy | Detail |
|---|---|---|---|
| `4150a4a8` | custom words via vocabulary API (#2284) | **C** | supervisor `protocol.rs`/`worker.rs`: `LoadedInfo.supports_vocabulary` (via `model.supports(Feature::Vocabulary)`); `RunOptions::vocabulary: Vec<String>` replaces `RunExtension::Whisper` initial_prompt for **batch AND streaming** paths; `FinalizedStreamText.custom_words_prompted` flag → **skip fork's fuzzy post-correction exactly when vocabulary was sent** — wire into fork's `finalize_stream()` and `process_transcription_text()`; **keep fork's n-gram post-correction as fallback** for non-vocabulary models (fork's text.rs is richer than upstream's). Fork re-skin: client-side run-options in `transcribe_cpp_run_plan()` + streaming run construction. |
| `9e57a0d7` | long-form chunking with VAD (#2282) | **C** | New `audio_toolkit/chunking.rs` (Chunker, 616) + `managers/transcription/` becomes dir with `chunked.rs`/`live.rs`/`policy.rs` — port as fork modules re-using the fork's client shape; `policy.rs` decode-strategy (`ModelTraits{streaming, long_form}`, `supports_long_form` on LoadedInfo — whisper true); Chunker shared between live recording and batch clips; `CancelMark`/`transcribe_since` so all chunk runs cancel together (integrate with fork's StreamingCancelFlag); chunk VAD lazy via `create_detector()` (from handy-recorder path); degrades gracefully to uncut runs; new `long_form_chunking: bool` setting (default true) via remap playbook + FE `LongFormChunking.tsx` in Advanced settings; VAD `SpeechScorer` trait + `check_whole_frames` in `vad/mod.rs`, `earshot.rs` score exposure (fork's earshot arm gains the trait impl); CLI `--long-form` (cli.rs) + actions/ + shortcut/mod.rs hunk. Fork: extend transcription_coordinator_tests for chunk-cancel behavior if the coordinator learns new cancel effects. |

**Hard deps:** `4150a4a8` ← 0.4.0 + c8262ab3; `9e57a0d7` ← c8262ab3 + 1b011a19 + 3985508a + 4150a4a8 + (patch-shape) a42131d2 + fork earshot.
**Phase-9 requirements if 0.4.0 is NOT adopted (D1 = (b)-in-process or (c)):** vocabulary strictly requires 0.4.0's `Feature::Vocabulary` + `RunOptions::vocabulary` (+ supervisor protocol for the flag) → **`4150a4a8` downgrades to "initial_prompt-only, no vocabulary" — a no-op against the fork (which already sends initial_prompt via the 0.2.3-era `RunExtension::Whisper` API) → S**; `9e57a0d7` requires 3985508a's `VadSession` scoring + protocol plumbing → without it, only a deferred heavy re-skin using vad-rs/earshot scoring on the fork pipeline is possible (**deferred, not default**). Under D3-reject + D1-adopt, both land but on the dual-stack: vocabulary on the transcribe-cpp half (`0.4.0` present), initial_prompt retained on the ONNX half (existing 0.2.3-era API) — partial but functional; see matrix rows 2/4/6.

**Verification:** standing gate + smoke: **custom words** — batch with vocabulary-capable model (whisper) → words honored, NO double-application with fuzzy correction; non-vocabulary model → fuzzy correction still runs; **streaming custom words**; **long-form** — 5+ min recording with `long_form_chunking=true` (chunks + cancel-all works: stop mid-long-stream), toggle false → old single-run path; 30s recording regression (chunker shouldn't fire).
**Rollback:** `9e57a0d7` reverts cleanly to Phase-8 tag if needed; setting defaults on, rollback = flip default until fix.
**Risk:** MEDIUM–HIGH (feature-correctness; fork's post-correction interplay is the subtle part). **Effort:** 3–5 blocks.

---

### Phase 10 — Overlay/live-captions reconciliation + closeout (4 APPLY + 1 VERIFY + pre-range note + docs)

**Purpose:** The adopt-if-clean overlay cluster, the live-captions loading indicator (which must be hand-integrated into fork hooks), verification of the duplicate-history fix, and documentation closeout.

| Commit | Title | Strategy | Detail |
|---|---|---|---|
| `16caad7a` | stop rendering animations while hidden (#1445) | **C → adopt-if-clean** | Upstream: RecordingOverlay renders null while hidden. Fork: re-implement at hook level — `useAppState` consumers gate the render (or a top-level `if (visible) … else null` in fork's coordinator component); ensure `useVisualizer`/`useLiveCaptions` listeners still run or are suspended deliberately. **Fallback if unclean:** skip with note "fork already hides via NSPanel orderOut + OVERLAY_SESSION; WebView cost accepted" |
| `b50b52a8` | theme reaches overlay (#1659) | C | `theme-changed` event + overlay FE theme application + dark CSS block; macOS title-bar widening only if applicable to NSPanel — careful: fork overlay is borderless panel; **evaluate if the fork's overlay needs the theme at all** (dark already). Adopt-if-clean. |
| `20a6685e` | overlay "Loading model…" (#2227) | C | Hand-integrate: coordinator emits `model-state-changed`? If fork lacks it, add event in coordinator (`transcription_coordinator.rs`); consume in `useOverlaySharedState`/`useLiveCaptions` to show loadingGeneric while Processing on cold load. Skip locale hunks; add en/zh/zh-TW strings. |
| `417dc6ac` | overlay loading preview after 2s (#1815-follow) | C | **after `20a6685e`** (same behavior family); only `loading_completed`/`loading_failed` end the wait |
| `57324065` | history dedupe (#2256) | **verify→likely S** | Fork creates history rows only post-final (`actions/transcribe.rs::stop()` → `hm.save_entry_with_vad`), so the double-insert condition shouldn't exist. Read fork's `HistorySettings.tsx` listener; if same-id replace is absent, apply the 12-line logic; else skip with verify note |

**Pre-range gap (no port row):** `5a7c0eac` "bump tauri version and Mac dock fixes (#1675)" — overlay-bottom/dock tracking — is **pre-range** (2026-07-15, verified ancestor of `cdbc2239`, outside `cdbc2239..9e57a0d7`). The fork already applied its work_area part per registry (**fork `544f28d2`**, "FIX: overlay Bottom position tracks macOS Dock via work_area", 2026-07-17). Optional follow-up only (diff `5a7c0eac` vs fork's `544f28d2` for any residual dock-position gap); no port row in this round.

**Closeout (§7):** finish UPSTREAM_SYNC.md registry rows for every actioned/decision/skip; learning-log entries; full final verification (§6).

**Risk:** MEDIUM (hand-integrations). **Effort:** 1.5–2.5 blocks + docs.

---

## 3. Decision Gates

Present to user **before starting the arc phases** (6–9); gate D5 now has a field-researched default (refresh) that must be applied at Phase 5 start; D4/D6 can be decided later (see "What can be safely deferred"). Each gate below carries: what it decides, why the fork/upstream divergence forces the choice, per-option consequences (work + user-facing gains/losses), and the recommendation with its honest uncertainty. The D1×D2×D3 closure matrix maps every permutation; original one-line defaults are kept under **Default.**

### D1 (umbrella) — Adopt the upstream engine architecture wholesale?
**Question:** Adopt engine_supervisor child-process isolation + transcribe-cpp 0.4.0 (+serde) for all ASR?
**Options (short):** (a) Full adopt — Phases 6, 8, 9 run as designed · (b) Partial: bump to 0.4.0 but keep in-process engines — `c8262ab3` S, `1b011a19` lands with in-process adaptation; `3985508a`/`4150a4a8` re-skinned in-process; `9e57a0d7` partial/deferred · (c) Stay at 0.2.3 — **skip Phases 6, 8, 9 ONLY** (Phase 7 `a42131d2` is independent of transcribe-cpp 0.4.x and stays governed solely by its own gate D2) — ~35% of range value forgone.
**Default:** (a).

**What it decides (plain terms):** How the speech engine runs inside the app. Today the fork loads models **in-process** — GGUF whisper via transcribe-cpp 0.2.3, and Parakeet/GigaAM/SenseVoice/Moonshine via transcribe-rs ONNX. Upstream (shipped in released v0.9.6–0.9.8) runs **every** model in a separate child process ("transcribe worker", `engine_supervisor/`) on transcribe-cpp 0.4.0. D1 decides whether the fork gets: (1) *crash isolation* — a GPU driver fault, `GGML_ASSERT` abort, or hang kills only a throwaway worker, the app itself (and your recording pipeline, health monitor, tray) survives and can relaunch the worker / fall back to CPU — or whether the whole app still dies in-process; and (2) the 0.4.0 API layer that P8's GGUF VAD and P9's vocabulary/chunking are built on.

**Why it exists:** Both sides heavily rewrote `managers/transcription.rs` from the same ancestor and the rewrites are incompatible: fork +2756 lines (its own `StreamRouter`/`StreamCmd`, `run_stream_worker()`/`finalize_stream()`, `StreamingCancelFlag` seq/flag atomics, run-plan + catch-unwind, `StreamPerf`, watchdog force-release, `suppressed_token_count`), upstream's `c8262ab3` rewrote the same file (1479 lines) and moved the engine out of process. Cherry-picks cannot apply — only *behavior* can be re-skinned onto fork structure (Playbook §5.1). That is 4–6 blocks of work on the fork's most sensitive file, so it deserves an explicit decision, not a silent default.

**Options & concrete consequences:**
- **(a) Full adopt** — 4–6 blocks (P6) + shapes P8/P9. Gains: crash isolation (the arc's headline value), vocabulary custom words, GGUF VAD, long-form chunking — the fork stops drifting from upstream, future syncs cheapen.
- **(b) 0.4.0 in-process** — ~2 blocks (P6 only, `1b011a19` + call-site adaptation). Gains: vocabulary + GGUF VAD still available in-process (0.4.0 API exists regardless of supervisor). Losses: GPU faults still take the whole app down; `9e57a0d7` chunking partial/deferred (its `CancelMark`/`transcribe_since` cancel plumbing assumes the supervisor).
- **(c) Stay 0.2.3** — 0 blocks here. Keeps today's `catch_unwind` guards and crash behavior; vocabulary = S (fork already sends `initial_prompt` at 0.2.3 — a no-op), GGUF VAD = S (hard-requires `Model::vad_info`/`vad_session_with`/`BackendMask`), chunking = deferred optional re-skin. Lowest risk this release, but the fork's divergence keeps compounding — the *next* sync pays more.

**Recommendation & honest uncertainty:** (a). Rationale: engine failures are this fork's #1 failure mode — fork already built `health.rs`, `usb_watchdog.rs`, and catch-unwind machinery around them; the supervisor is upstream's answer to the same disease and it has run in released upstream builds since early October. Evidence for: production-tested upstream-side; all fork load-bearing pieces have a mapped preservation plan (§5.1); per-phase tags keep exit cheap. Uncertainty: nobody has yet verified the fork's live-captions seam (per-frame feed into `StreamRouter`) across a child-process boundary — it's the least-tested mapping; P6's smoke tests target exactly that, and if it fails badly the fallback is (b), taken mid-arc, cheap thanks to tags. *What would change the answer:* demonstrated protocol instability under the fork's streaming cadence → (b); user priority "minimum change this release" → (c).

### D2 — handy-recorder switch
**Question:** Delete fork's `recorder.rs`/`resampler.rs` in favor of `handy-recorder` crate + `sink.rs`, re-porting the preroll/command-loop semantics on the new path?
**Options (short):** (a) Adopt + full preroll re-port (plan as written) · (b) Adopt, defer preroll re-port to a fast-follow with explicit known-regression note (NOT recommended — reintroduces the Oct-2 first-word-cut class of bug) · (c) Reject — keep fork recorder.rs; then `9e57a0d7`/`3985508a`-adjacent work needs fork-side `create_audio_recorder()`-shape adaptations instead of upstream's `create_detector()` shape.
**Default:** (a).

**What it decides (plain terms):** Which code captures your microphone and turns raw frames into VAD-processed audio. Fork: its own 1046-line `recorder.rs` + resampler, carrying bug fixes born from real incidents. Upstream: deleted that entire style of recorder and switched to the external `handy-recorder` crate with an app-side `CaptureSink`.

**Why it exists:** `a42131d2` (Oct 12) is a wholesale replacement: it deletes `recorder.rs`, `recorder/tests.rs`, `resampler.rs`, `bin/cli.rs` — files that exist only in the fork and host the fork's preroll fixes (`9e3b7f65` apply-start-before-chunk/command-drain, `ddffa435` full preroll-slice emission = the "first word cut off" fix, investigated 2026-10-02). Adopting the switch without re-porting those semantics silently regresses voice capture. It is gated separately from D1 only because of this fidelity risk — it needs neither 0.4.x nor the supervisor and is otherwise independent.

**Options & concrete consequences:**
- **(a) Adopt + full re-port** — 3–5 blocks. Keeps every fork capture behavior (command-drain/apply_start, pending_stop/finalize_stop, NS-off full preroll slice, `router.feed(frame)` seam, always-on/channel-select/mute) expressed on `CaptureSink`/`RecorderConfig`/`VadSlot`/`OpenMicrophone.needs_reopen()`/`EndReason`; Gains: upstream-maintained capture stack, device-ID resolution + permission-error mapping the fork hand-rolled, prerequisite shape for P8/P9 wiring. Acceptance test: WAV A/B harness (first word intact, NS-off).
- **(b) Adopt + defer re-port** — 2–3 blocks but a **known regression ships** (first-word truncation returns for NS-off users) until the fast-follow lands. Only tolerable if no NS-off users exist — not the case.
- **(c) Reject** — keep `recorder.rs` (zero regression risk now); permanent cost: the fork keeps maintaining its own capture stack solo, and P8/P9 commits that upstream wrote on top of handy-recorder's shape need heavier adaptation (matrix rows 3/4/6/8).

**Recommendation & honest uncertainty:** (a) with the re-port as a hard, non-skippable subtask. Rationale: the fork's fixes are *behaviors*, not code — they port; upstream's crate is where capture maintenance happens going forward. Uncertainty: how cleanly `CaptureSink` exposes what fork's recorder handed to the stream router and `emergency_save.rs` — the least-verified mapping in the plan (flagged in §5.2); the A/B harness is the protection. *What would change the answer:* if the A/B harness cannot be satisfied after honest effort (sink API lacks a needed seam), fall back to (c) with a registry note — nothing upstream-side is truly lost except crate alignment.

### D3 — Full ONNX removal (5197faf0)
**Question:** Delete transcribe-rs/ONNX entirely; all models GGUF via handy-computer catalog; retired-model migration UI for existing ONNX users?
**Options (short):** (a) Adopt fully (migration UI included) · (b) Adopt backend removal but skip migration UI (users self-recover — worse UX) · (c) Keep ONNX alongside (fork maintains the dual-stack; see matrix rows 2/4/6).
**Default:** (a).

**What it decides (plain terms):** Whether the app carries **two** model runtimes (transcribe.cpp GGUF + ONNX Runtime for Parakeet/GigaAM/SenseVoice/Moonshine) or **one** (GGUF only; VAD via transcribe-cpp too). Also swaps the macOS build's ORT packing for an explicit `libclang_rt.osx.a` link (`build.rs` −79 lines), and removes the `ort_accelerator` setting (serde-migrated).

**Why it exists:** The fork still lives in 5197faf0's pre-state: `transcribe-rs 0.3.8 [onnx]`, `vad-rs`, `silero_vad_v4.onnx`; upstream deleted all of it (Oct 15) in favor of `handy-computer/*-gguf` models, and the fork's catalog **already ships those GGUF repos**. Upstream's motive was never licensing (ORT is MIT) or binary size — the real drivers were **GPU fragmentation and packaging pain**: ONNX Runtime's AVX2 baseline crashed pre-Haswell Macs with illegal instructions (#2226), ORT leftovers broke Windows ARM64 code signing, and every platform needed its own ORT plumbing. The maintainer said this outright in [Discussion #1295](https://github.com/cjpais/Handy/discussions/1295) (2026-08-15, quoted): ONNX GPU acceleration **"is a mess for STT"**, community implementations are "all over the place", and **"I would rather just own it end to end in ggml."** Pushing every model through transcribe.cpp is stated as the future of the app — and transcribe.cpp itself is a [Mozilla.ai](https://blog.mozilla.ai/announcing-transcribe-cpp/)–backed "llama.cpp for STT" (MIT, Metal/Vulkan/CUDA, ~2k★, [github.com/handy-computer/transcribe.cpp](https://github.com/handy-computer/transcribe.cpp), announced 2026-06-30). Keeping ONNX means betting against the direction the engine's own authors named.

**Field data (2026-10-18 research → `HANDY-FORK/research/onnx-to-gguf-consolidation-research.md`):**
- **Nothing was dropped:** all 11 retired ONNX models have GGUF replacements (full id→replacement mapping in the research doc); translation survives (Whisper + Canary, now GGUF).
- **No accuracy-regression reports found post-switch.** Honest qualification: the removal (Oct 15) is **not yet in a tagged release** (latest v0.9.8 = Oct 3), so feedback is from main-branch/bleeding users — real but early. Praise exists for Parakeet Unified GGUF.
- **The loud post-switch issues were GPU device-selection bugs, not model-quality problems:** #2270 (outdated Intel driver → fixed by driver update), #2281 (wrong GPU auto-selected → fixed by device choice), #1884 (still open: `Auto` binding can pick a slower iGPU). These validate the fork's `transcribe_gpu_device` device-handle setting; #1884 is worth watching for fork users on dual-GPU Macs.

**Options & concrete consequences:**
- **(a) Full adopt** — inside P8's 2–4 blocks. Users with retired ONNX models get a guided migration UI (download the GGUF replacement with progress/retry; existing models stay on disk until the user deletes them — nothing force-deleted). Binary drops ORT (`onnxruntime`), `tar`/`flate2`; settings cleanly migrated; fork's custom-GGUF discovery (`discover_hf_cache_models`, `gguf_meta.rs` prober) survives untouched as an additive layer.
- **(c) Reject/dual-stack (matrix row 2/4/6)** — 0 P8 blocks saved in effort, but: vocabulary works only on the transcribe-cpp half (ONNX half keeps fork's `initial_prompt`), migration UI = S, `3985508a` = S (VAD stays `vad-rs` v4 ONNX), binary keeps ORT, and the fork maintains the ONNX engine paths solo as upstream deletes them around it — against the maintainer's stated direction.

**Recommendation & honest uncertainty:** **(a)** — now research-backed, not just policy-inferred. Rationale: maintainer's end-game statement (#1295) + Mozilla.ai backing of transcribe.cpp (the engine won't be an orphaned dependency) + zero accuracy-regression reports + all 11 replacements mapped + the fork's catalog already carrying them. Residual honesty: post-switch feedback predates a stable tag, and #1884 (`Auto` → iGPU) matters on dual-GPU Macs — the fork's exact-device binding already mitigates what #2281's fix addresses. Mitigation unchanged: old models stay on disk, migration is opt-in, so a disappointed user can stay. *What would change the answer:* credible accuracy regressions reported once the removal reaches a tagged release **before** our `5197faf0` lands → flip to (c) cheaply; **after** it lands, (c) means re-adding ONNX code the wrong way around. Therefore D3 must be decided before Phase 8 starts.

### D1×D2×D3 branch-closure matrix (what happens to Phases 6–9 in each permutation)

| Row | D1 | D2 | D3 | Phase 6 (`c8262ab3`+`1b011a19`) | Phase 7 (`a42131d2`) | P8 `3985508a` | P8 `cfcb6507` | P8 `5197faf0` | P9 `4150a4a8` | P9 `9e57a0d7` |
|---|---|---|---|---|---|---|---|---|---|---|
| 1 ★default | adopt | adopt | adopt | land as designed | land as designed | land (GGUF VAD) | land | land | land | land |
| 2 | adopt | adopt | **reject** | land as designed | land | **S — re-skin means keep VAD on `vad-rs`** (GGUF-VAD migration presumes the all-GGUF world; nothing portable left) | **S** (migration UI pointless when ONNX stays) | **re-skin as "keep both engines" → S** (no RETIRED table, no build.rs ORT cleanup, `tar`/`flate2`/`ort_accelerator` stay) | **land on transcribe-cpp half (0.4.0 present); ONNX half keeps fork's initial_prompt (existing 0.2.3-era API) — dual-path custom words, partial but functional** | land (chunk scoring via GGUF VadSession is independent of ONNX removal) |
| 3 | adopt | **reject** | adopt | land as designed | **S — keep fork `recorder.rs`** | land, **adapted onto fork shape** (`create_detector()` → fork `create_audio_recorder()`) | land | land | land (client-side run-options unaffected) | land, **heavy adaptation** to fork recorder shape |
| 4 | adopt | **reject** | **reject** | land as designed | S | S (keep `vad-rs`) | S | S ("keep both engines") | land (tcp half only; ONNX half keeps initial_prompt) | **partial/deferred** (re-skin to fork recorder shape + keep vad-rs/earshot scoring for chunks) |
| 5 | **b (0.4.0 in-process)** | adopt | adopt | **1b011a19 lands only; `c8262ab3` S** (capabilities()→Result adapted inside fork's in-process transcription.rs) | land | **re-skin in-process** (0.4.0 VadSession usable in-process) | land | land (transcribe_rs paths removed in-process; build.rs ORT cleanup applies) | **re-skin in-process** (`RunOptions::vocabulary` direct, no protocol layer) | **partial re-skin** (chunking onto fork pipeline; defer if heavy) |
| 6 | b | adopt | **reject** | 1b011a19 only | land | S (keep `vad-rs`) | S | S ("keep both engines") | re-skin: vocabulary on tcp in-process; ONNX keeps initial_prompt | partial/deferred (vad-rs-scoring re-skin) |
| 7 | **c (stay 0.2.3)** | adopt or reject | * | **S both** | land iff D2=adopt (independent of tcp 0.4.x) | **S — hard-requires the 0.4.0 API** (`Model::vad_info`/`vad_session_with`/`BackendMask`); v4 ONNX + `vad-rs` stay | S | S | **S / downgrade — "initial_prompt-only, no vocabulary": no-op vs fork (already does this at 0.2.3) → S** | **deferred optional re-skin** (chunk VAD scoring via vad-rs/earshot on fork pipeline; only if user asks) |
| 8 | c | **reject** | * | S both | S | S | S | S | S | deferred optional (same as row 7) |

**Matrix notes:**
- "Land as designed" = the Phase 6–9 detail sections. Phase-7 independence is explicit: `a42131d2` never needs transcribe-cpp 0.4.x.
- Whenever D3 = reject: fork keeps `transcribe-rs`, `tar`, `flate2`, `vad-rs`, `ort_accelerator`, ORT-full build.rs — record the deliberate divergence in the registry as "dual-stack kept by decision D3=(c) (date)".
- Whenever D2 = reject: P8/P9 commits that upstream wrote on top of handy-recorder shape need the `create_audio_recorder()` adaptation noted — heavier, not blocked.

### How the gates interact
- **D1 dominates.** D3 is only meaningful under D1 = (a)/(b): rows 1–6 all have D1 ≥ (b); under D1 = (c), ONNX removal is impossible anyway (`5197faf0` requires 0.4.0), so D3 collapses to "keep" by implication — asking D3 separately then would be noise. D3-adopt while rejecting 0.4.0 is a *contradictory state* and the matrix forbids it.
- **D1 also sets Phase 9's fate** (vocabulary needs 0.4.0 + supervisor; chunking needs 0.4.0 + GGUF VAD scoring), **and changes how the six Phase-2 deferred hunks rewire.**
- **D2 is independent of D1/D3** (the recorder swap needs neither 0.4.x nor supervisor) but constrains the *effort shape* of P8/P9 (rows 3/4/6/8: heavier fork-shape adaptation), not their possibility.
- **D4/D5/D6 are orthogonal** to D1–D3 and to each other — they can be decided independently and never cascade.

### What can be safely deferred
- **Must be answered before Phase 6 starts:** **D1** (it re-shapes the arc and the P2 deferred hunks land inside Phase 6's rewrite) and **D3** (needed before Phase 8's `3985508a`/`5197faf0` are attempted; reversible cheaply only before it).
- **Answered mid-arc at the latest — D2:** Phase 7 sits after Phase 6 in sequence; D2 can be decided any time before Phase 7 starts (deferring it does not block Phase 6).
- **Before Phase 5 starts — D5 (default = refresh):** the vendor-dir swap must precede the P5 shortcut ports (upstream wrote them against the 0.3.x API); acceptance = macOS sleep/wake + recording-hotkey regression pass; on failure, revert the swap (trivial rollback) — decided at P5 start, not later.
- **Freely deferrable:** **D4** (gates only one P4 row; P4 items 1–7 proceed regardless), **D6** (`f6fac42e` row is S-by-default; the bump can be taken any time later).

### D4 — paste_tx reliable paste
**Question:** Port upstream's receipt-sequenced paste as a separate opt-in `reliable_paste` setting, borrow only its ideas, or skip?
**Options (short):** (a) Full port as opt-in (default OFF — fork AX-verify stays default) · (b) Idea-borrow only: NSPasteboard concealment markers (`org.nspasteboard.ConcealedType/Transient/AutoGenerated`) + changeCount guard + quiet-period logic folded into fork's restore path · (c) Skip. `b1b2d9f9` stays S in all outcomes (Windows policy skip).
**Default:** (c) with (b) noted for later.

**What it decides (plain terms):** Whether the app gains an *alternative* paste engine beside the fork's existing one — or whether some of its ideas are folded into the fork's paste path.

**Why it exists:** The fork and upstream **independently solved the same bug class** (pasted text not landing / clipboard restore racing, upstream #502 family) with two complete, rival designs in the same file (`clipboard.rs`, diverged ~951 lines). Fork: **"AX verify-then-commit"** — inject paste keys via Enigo, then read the target app's AX value through raw `macos_ax` FFI and only restore the clipboard once the text verifiably landed. Upstream: **"receipt-sequenced"** (`a70ac84f`) — publish the transcript as a lazy NSPasteboard promise (`declareTypes:owner:`), use the consuming app's read as a receipt, gate clipboard restore on receipts + 200 ms quiet period + `changeCount` + 8 s cap, and stamp NSPasteboard concealment markers so clipboard-manager apps skip the transcription. Both designs work; they are not mergeable — one wins, or they coexist behind a setting.

**Options & concrete consequences:**
- **(a) Full port as opt-in** — ~2–3 blocks (new `paste_tx/` module + `reliable_paste` setting + wiring + FE debug panel). Gains: an alternate path for paste targets where AX reading fails (non-accessibility-enabled apps), OS-level receipt timing. Cost: real, and it duplicates maintenance of the app's highest-churn paste file.
- **(b) Idea-borrow** — ~0.5 block: add the three `org.nspasteboard.*` concealment markers to fork's publish path (clipboard managers — which read *any* copied content — stop capturing transcripts) + optionally the changeCount guard in restore.
- **(c) Skip** — 0 blocks; fork's verified design untouched.

**Recommendation & honest uncertainty:** (c) now, (b) on evidence. Rationale: the fork's design is verified in production and fork-owned; upstream's ships as opt-in upstream too; and port cost is real. Honest uncertainty: neither analysis surfaced fork-side incidents of clipboard managers capturing transcriptions — (b) insures against a failure mode **not yet observed** in this fork, which is why it isn't the default. *What would change the answer:* one credible "my clipboard-history app shows my transcription" report → do (b); repeated "paste doesn't land in app X" reports where AX verification fails → consider (a).

### D5 — handy-keys vendor refresh (default: **refresh to 0.3.4, conditional**)
**Question:** Refresh vendored `handy-keys` to upstream 0.3.4 (re-applying the fork's deltas on the new base), or keep the vendored 0.2.4-handy.1 copy?
**Options (short):** (a) ~~Keep vendored~~ — now the **fallback**, not the default · (b) **Refresh to 0.3.4 + re-apply `reset_pressed_hotkeys()` + adopt upstream #31 tap-recovery + REQUIRED macOS regression pass** — the default (APPLY-conditional in Phase 5).
**Default:** **(b) refresh**, gated on the acceptance pass; rollback = revert the vendored-dir swap.

> **D5 EXECUTION NOTE (2026-10-19, Phase 5 step 0):** Refresh executed as planned — vendored
> dir swapped to the crates.io 0.3.4 tarball (fork commit `04ad75d5`). Fork deltas re-applied
> onto the new `manager.rs`: `reset_pressed_hotkeys()` + press-suppression diagnostic warns;
> the `log` dependency re-added (0.3.4 ships with no logging facade). Fork's ~300-line
> CGEventTap silent-death recovery dropped in favor of upstream #31's `CallbackCtx`/`TapRunLoop`
> event-driven recovery inside 0.3.4. Automated gates green: vendored `cargo test --lib` 55/55
> (×3 stable, incl. #31's tap-recovery tests), app `cargo check`, app `cargo test --lib` 511
> pass (only the 4 documented pre-existing failures), `bun run build`. One vendored
> integration test (`macos_ipc_quiescence::idle_listener_sends_no_recurring_ipc`) could not be
> run meaningfully: it forbids ANY user input for 15 s while counting task-wide mach messages,
> and this Mac is in active interactive use — the failure was environment contamination, not
> listener IPC (re-run on an idle machine before release if desired).
> **⏳ REGRESSION-TEST-REQUIRED (open):** the physical macOS sleep/wake + recording-hotkey
> pass (wake-from-sleep stuck keys, rapid start/stop, cancel spam, Secure-Input fallback)
> MUST be done manually by the user; the agent deliberately did not run pmset/suspend.
> Until that pass, treat the 0.3.4 swap as provisionally trusted; rollback =
> `git revert 04ad75d5` (single vendored directory).

**What it decides (plain terms):** The crate that actually hears global hotkeys (including under Secure Input) moves from the fork's patched 0.2.4-based vendored copy to upstream 0.3.4 — a rewrite of the macOS listener core. End-user behavior should be identical or better; the risk is the rewrite itself.

**Why it exists — the 2026-10-18 vendor audit changed the recommendation.** Findings that overturned "keep vendored":
- **The vendor base is upstream 0.2.4** (crates.io tarball, `0.2.4-handy.1`, no `.git`) — the fork diverged **from 0.2.4, not 0.3.0** as assumed. Upstream moved 0.2.4→0.3.4 in 18 commits; all 4 Handy-side bump commits are Cargo-only (confirmed).
- **Public API 0.2.4→0.3.4: signatures identical, additions only** — the refresh is API-safe for the fork's `shortcut/` layer.
- **Fork deltas (what must survive):** ① **new `pub fn reset_pressed_hotkeys()`** + press-suppression/reset diagnostics in `manager.rs` (~60 ln) — the fork's wake-from-sleep stuck-hotkey recovery, **used by fork `shortcut/handy_keys.rs`** (three call sites) with **no upstream equivalent** → mechanical re-apply (~20 ln + optional warns); ② **~300 ln macOS listener rewrite**: CGEventTap silent-death recovery (1 s heartbeat `CALLBACK_COUNT`, tap recreation after ~5 s of zero callbacks while enabled, double-`Arc::from_raw` free fix); ③ rustfmt noise only.
- **Critical overlap:** upstream #31 (`7903fda`) "replace event tap polling with callback recovery" is a ~350 ln rewrite of the macOS listener (`CallbackCtx`/`TapRunLoop` + unit tests) solving the **same tap-death failure with an incompatible implementation** — carrying both would be untestable, so the fork's copy is **dropped in favor of upstream #31's**.
- **Other 0.3.x gains:** modifier-only release fix, Globe key (0xB0), Pause/PrintScreen keys, Windows/Linux fixes (orthogonal to a macOS fork but free).

**Options & concrete consequences:**
- **(b) Refresh (default)** — 1–2 blocks in Phase 5 as step 0, **before** the shortcut ports land (they were written upstream against the newer crate): swap `vendor/handy-keys/` to 0.3.4, re-apply `reset_pressed_hotkeys()` (~20 ln mechanical), rely on upstream #31's callback recovery + its unit tests instead of the fork's 300-ln copy. Gains: bug-for-bug coverage of the same tap-death failure **with tested code**, modifier-release + Globe-key + Pause/PrintScreen fixes, and a cheaper next sync.
- **(a) Keep vendored (fallback)** — 0 blocks now, but the fork carries a 500+ ln hand-maintained listener forever (two rival recovery designs coexisting in spirit, one fork-only, zero upstream tests), and every future shortcut port pays the 0.2.4-vs-0.3.x API gap.

**Recommendation & honest uncertainty:** **(b)** — the audit made "keep vendored" untenable: the vendored copy's own load-bearing trick (tap-death recovery) exists upstream as a tested, incompatible rewrite; maintaining two versions of that mechanism is the worst option. Required acceptance before trusting it: **a macOS sleep/wake + recording-hotkey regression pass** (wake-from-sleep stuck keys, rapid start/stop, cancel spam, Secure-Input fallback from P4). Residual risk, stated honestly: upstream #31 is new and rewrite-grade — its unit tests prove the mechanism, not our integration; the regression pass is the gate. *What would change the answer:* if the pass fails in ways #31's design can't be patched for, revert the vendored-dir swap (rollback is trivial — one directory) and stay on (a) with a registry note.

### D6 — Updater plugin 2.10.0 → 2.10.1
**Question:** Keep vendored tauri-plugin-updater 2.10.0 (with `dangerous-skip-verify`) or move to crates.io 2.10.1?
**Default:** Keep vendored.

**What it decides (plain terms):** Only the implementation source of the app's auto-update mechanism. No visible behavior change either way — unless 2.10.1 fixed a defect fork users actually hit (unknown; that is the honest gap).

**Why it exists:** The fork vendored the updater deliberately (2.10.0 + `dangerous-skip-verify`) for its stable-channel re-signing workflow; upstream meanwhile bumped to crates.io 2.10.1 in-range (`f6fac42e`). The Phase-1 `f6fac42e` row is therefore a VERIFY row: "confirm fork updater state → S unless D6 outcome = bump."

**Options & concrete consequences:**
- **(a) Keep vendored** — 0 blocks; the deliberate skip-verify + `HANDY_DISABLE_UPDATER`/portable gating keep working as built.
- **(b) Adopt 2.10.1** — ~1 block: swap the vendored dir for the crates dep and re-apply or re-evaluate `dangerous-skip-verify`; risk: the skip-verify path may now be patches-on-patches if the crate changed shape.

**Recommendation & honest uncertainty:** Keep. Thin evidence both ways: no updater defect is known to the fork that 2.10.1 fixes, and adopting touches the one plugin the fork forked *on purpose*. *What would change the answer:* the upstream 2.10.1 changelog showing a real fix for an updater symptom fork users report, or a future fork feature requiring the newer crate APIs. Zero urgency — defer freely.

### Non-gated defaults (no user input needed, recorded here):
- `7b2a28a3` = PORTED (change present via fork `ddffa435`, the stronger version with tests + tail_report).
- `b4453a29`/`d54c88eb`/`db003f38`/`549cbde3` = SKIP-superseded (by a42131d2; behaviors verified post-port).
- Translations/locales, Linux/Windows/Nix, README/Sponsors/PR templates, release commits, upstream version bumps = S by fork policy.
- `cdf5028b` Sidebar refactor = SKIP-policy/deferred (re-evaluate after sync; fork sidebar has Accuracy/Usage).
- What's New feature port = optional follow-up (not in this round unless user asks).

---

## 4. Skip List (registry-ready, one-line reasons)

Bucket totals: **PORTED 13 · VERIFY 6 · SKIP-superseded 5 · SKIP-policy 76 (66 named + 10 enumerated in Phase 0) · APPLY 61 (see phase tables) = 161 ✓**

**PORTED — already present in fork (record fork hash in Phase 0):**
`20ada47d` earshot VAD — ported Sep round · `9e534a3d` ported · `292db647` HF harden/mirror — fork `38832ef1` · `12f02e2a` channel select — ported · `c89b7bf3` default mic fallback — ported · `a4348beb` stream recovery — ported · `df216832` tail audio — ported · `2203a826` custom-word correction — ported · `2cf157d9` multi-word — ported · `09aaf4d3` moonshine streaming — ported · `db1aaac7` tcp 0.2.3 — in fork · `68af495e` no-HF-env — in `download_hf_model` (`ApiBuilder::from_env().with_token(None)`) · `7b2a28a3` VAD pre-roll hangover — covered by fork's stronger `ddffa435`.

**VERIFY — read-diff then resolve to tiny-APPLY or S, record fork state:**
`d55ea7ef` + `0e503672` drop-gpu accelerator (merged form — fork at/above this API level) · `86616891` update-checker overlay fix (likely in fork's UpdateChecker already) · `57324065` history dedupe (#2256 — likely unnecessary in fork: rows created post-final) · `e8c73ba7` catalog refresh (likely present) · `f6fac42e` updater 2.10.1 (fork vendored 2.10.0; S unless D6 says bump).

**SKIP-superseded by the arc:**
`afbf44cd` tcp 0.2.0 — fork past this API level (has `resolve_gpu_device()→Option<Device>`, `Backend::Auto`) · `b4453a29` async cpal commands — superseded by a42131d2 (behavior verified post-port) · `d54c88eb` rt-safe recorder — superseded · `db003f38` idle level-meter skip — superseded · `549cbde3` readiness handle — superseded (EndReason/needs_reopen covers).

**SKIP-policy — named (66):**
- Linux/Linux-specific: `d7fc6a07`, `fe5b23c4` (external script blocking, Linux), `0f32df74` (wtype modifier release), `46d6a2ae`, `b462aa39`, `e29520d6`, `d1180c42`, `0470d9ad`, `b428ae4c`, misc Linux-side fixes
- Windows: `b1b2d9f9` (windows paste fix — under ALL D4 outcomes), `a94b403e` (portable legacy cache retention), `76736d5a` (portable installer dialog), vsredist-era items
- Nix: `2c5c7601`, `b0266607`
- Misc policy/CI: `e1152d86`, `3f24f4b2`, `bf8756c3`, `838f8a24`, `8fd66912`, `48a3d8e9`, `2266b29b`, `96afb0cf`, `07a54e29`, `e449f69c`, `c7a68fed`, `9bcb6d9d`, `99052eef`, `eba5d9e2`, `8605699d`, `14cfed8a`
- handy-keys 0.3.1→0.3.4 bump commits — SKIP **as upstream commits**; superseded by **D5** (the crate content lands via Phase 5 step 0's vendored-dir swap to 0.3.4, which also re-applies the fork's `reset_pressed_hotkeys()` and adopts upstream #31 tap-recovery)
- Releases/version bumps (fork keeps own 0.12.x scheme + release process; CI don't apply to fork's build-reinstall + vendored updater): `17d6c763`, `3388983e`, `af48dd68`, `05e0aedd`, `14f6f0d3`, `468129fd`, `2281b8b2`, `59ec6959`, `5c77861f`, `70327582`, `390729a8`, `f602ef4f`, `286e66cb`, `80995b53`, `7a5f397d`
- Docs: `42afd1bf`, `2f7c2fca`, `8758dccb`, `8282c400`, `b8ad109d`, `8f9cf53c`, `05ca5f48`, `30062fa2`, `586a838c`, `73ab851c`, `f6b3f829`, `afe5a631`, README/Sponsors/PR-template commits
- Translations (fork keeps en/zh/zh-TW only): `f4e3587b`, `ed1bfb9b`, `2bdf9ac0`, `ea3c20a3`, `09a6a710`, `9aae694d`, `8abb8026`, `5276dd3c`, plus the locale-file hunks of `6fa85061` (its logic half is APPLY)
- FE deferral: `cdf5028b` Sidebar refactor — big refactor vs fork's Accuracy/Usage sections; skip this round, reassess after sync

**SKIP-policy — un-itemized in analysis docs (10):** enumerated one-by-one in Phase 0 (default SKIP-policy: README/sponsors/PR-template/odds-and-ends odds); each gets its row in the registry — re-bucket APPLY if any proves relevant.

**Pre-range / out of scope (not in `cdbc2239..9e57a0d7`, listed for orientation only — not counted in 161):**
- `cdbc2239` — range-excluded baseline commit (Add missing translations #1709); fork restructured translations anyway
- `4b7bb4e5` — merge-base (docs/audio)
- `5a7c0eac` — **pre-range** (2026-07-15, ancestor of `cdbc2239`): bump tauri version + Mac dock fixes (#1675). Fork already applied the work_area part (fork `544f28d2`). **Optional follow-up only** — diff `5a7c0eac` vs fork's `544f28d2` for any residual overlay bottom/dock-position gap; low priority; no port row this round
- `3cbbe582` (#1708, drop 'Direct' paste method from macOS UI) — **pre-range, out of scope**; already in fork per registry. **FYI pre-range gap (optional, low priority):** fork's `en/translation.json` may still expose the 'Direct' paste-method label that #1708 removed for macOS — a leftover i18n string, harmless; cleanup candidate if touched anyway in Phase 4

*(Every sha in `cdbc2239..9e57a0d7` lands in exactly one bucket; Phase 0's mechanical pass makes the 10-provisional enumeration explicit and final.)*

---

## 5. Conflict Playbook — 5 Top Collision Zones

### 5.1 `managers/transcription.rs` (+2756 fork drift; arc rewrites it completely)
**Preserve fork symbols/mechanisms** (do not lose under the re-skin):
- `StreamRouter` + `StreamCmd::{Feed, Finalize(reply), Cancel}` (open/take/clear/feed/is_open)
- `run_stream_worker()` / `finalize_stream()` incl. post-correction interplay + "no prompt in streaming" rule (superseded by vocabulary flag in Phase 9 — replace rule with `custom_words_prompted` semantics)
- `StreamingCancelFlag` (seq/flag atomics; fork-only `streaming_cancel_flag.rs`) → later integrates with 9e57a0d7's `CancelMark`
- `transcribe_internal()` run-plan, `transcribe_cpp_run_plan()`, `cpp_translation_task()` (explicit target_language semantics)
- `set_runtime_capabilities` + `model_capabilities.rs` runtime reconciliation
- `StreamPerf` perf counters; `suppressed_token_count` (TranscriptionResult); slot guard + watchdog force-release (`usb_watchdog.rs` interplay); `lazy_stream_close`
- **Language-side behaviors ported in Phase 2 (hunk-deferred), re-wired here:** lang_id text-fallback wiring into language-param handling (from `4cd49950`), language-coverage/'no'-language guard (from `6fa85061`), `chinese_script.rs` hooks into the transcription language pipeline (from `0bb0428e`) — port these hunks in Phase 6, once, onto the rewritten file
**Adopt upstream:** supervisor-client structure, `RunOptions.vocabulary`, `LoadedInfo{supports_vocabulary, supports_long_form}` through protocol, module split as `transcription/{live,chunked,policy}.rs` adapted to fork (fork may rename; keep files near upstream naming for future syncs).
**Rule:** every upstream hunk is expressed as a diff of *behavior* against the fork's current file (read fork fn → apply intent), never a textual patch.

### 5.2 `recorder.rs` → handy-recorder `sink.rs` (fork-only file deleted under you)
**Preserve (re-express on CaptureSink):** ① top-of-loop command drain + `apply_start()` before every chunk (`9e3b7f65`); ② NS-off full preroll slice emission (`ddffa435`) — THE first-word fix; ③ `pending_stop`/`finalize_stop` semantics incl. stream-dead finalize and rapid Stop→Start clear; ④ `router.feed(frame)` streaming seam; ⑤ always-on idle skip idea; ⑥ channel selection + mute integration; ⑦ smart-stop + emergency backup (`emergency_save.rs`) triggers at the same lifecycle points.
**Adopt upstream:** `OpenMicrophone{recorder, failed}` + `needs_reopen()`, `VadSlot{Unloaded,Idle,Lent}`, `EndReason`, `recording_error_type()` mapping (`PermissionDenied → microphone_permission_denied`, `DeviceUnavailable → no_input_device`), `CaptureSink` VAD+level callbacks, fork's `create_detector()` lazily from `create_capture_vad`.
**Acceptance:** preroll WAV A/B test must show the first word intact, NS-off.

### 5.3 `vad/{mod,silero,smoothed,earshot}.rs` (both sides rewrote; different frame regimes)
**Preserve fork:** duration-based timing (`VAD_PREFILL_MS/VAD_OFFLINE_HANGOVER_MS/VAD_STREAMING_HANGOVER_MS/VAD_ONSET_MS` + `frames_for_duration_ms()`), `VadBackend::{Silero,Earshot}` UI setting + per-backend sensitivity conversion (`hangover_frames * 30` → ms), `SmoothedVad` rework (`BufferedFrame{emitted,voiced}`, `mark_last_emitted()`, onset-skip, `tail_report()`), standalone `trim_trailing_silence()`, earshot backend.
**Adopt upstream (D1+D3-adopt rows):** `silero.rs` rewritten onto transcribe-cpp `VadSession` (CPU-only, GGUF v6.2), `SpeechScorer` trait + `check_whole_frames`, earshot score exposure, frame geometry **expressed through the fork's ms→frames converter** (Silero arm becomes 512/32ms). Under D3-reject rows: none of this lands; `vad-rs` stays (see matrix).
**Never:** revert to upstream's fixed-frame constants; never delete the Earshot arm.

### 5.4 `clipboard.rs` / paste (rival complete designs)
**Preserve fork:** `verify_paste_landed_macos()` + `macos_ax` module (AXUIElementCopyAttributeValue FFI; PAC-trap-safe CFString creation), `PasteMethod` enum incl. CtrlShiftV/ShiftInsert/ExternalScript, Enigo two-phase lock (prepare lock-free → send+verify), `restore_frontmost_app()`/`focus.rs::SavedFrontmostApp`, Finder/Desktop fallback, auto-submit variants, Linux chains.
**Add upstream (this plan):** 3ed2b219 image restore; aefc1319 translate-key; optional concealment markers (if D4=b).
**Do NOT:** introduce paste_tx as a rival default; never remove the AX verify gate.

### 5.5 Overlay + coordinator + settings remap (three smaller structural zones)
**Overlay — preserve fork:** NSPanel machinery (`tauri_panel!` RecordingOverlayPanel, `swizzle_can_become_key_window()`, `swizzle_mouse_tracking()` + NSTrackingArea), atomic gates (`OVERLAY_CAN_BECOME_KEY`, `OVERLAY_CURSOR_IN_PANEL`, `OVERLAY_FORCE_CLICK_THROUGH`, `OVERLAY_ENABLED`, `OVERLAY_SESSION` + `should_hide_with_session()`), `set_overlay_mouse_passthrough` click-through command, `OverlayScreenTarget` + `calculate_overlay_position`/`position_overlay_on_monitor` exact-sync bottom anchor, `run_on_main_thread` one-shot atomicity.
**Adopt-as-behavior:** cf49ab35 main-thread hop (fold into fork's one-shot path), 16caad7a null-render while hidden (hook level), b50b52a8 theme event (evaluate). (Pre-range `5a7c0eac` dock-position tracking: no action — fork's `544f28d2` work_area tracking already covers its substance; optional residual gap only.)
**Coordinator — preserve fork:** `AppState{Idle,Recording,Processing,UsbCycling,Confirming}` + `emit_app_state()`, `CoordinatorCore`/`Stage` machine + `transcription_coordinator_tests.rs`, recording-timeout warnings, `send_input` path, `reset_all_pressed_hotkeys` on sleep_wake.
**Settings remap (mechanical recipe for every settings commit):** field → `settings/types.rs` · default → `settings/defaults.rs` (+ serde default attr) · persistence path/version bump → `settings/store.rs` · FE updater → `settingsStore.ts` `settingUpdaters` · placement → fork's `SECTIONS_CONFIG` (Accuracy/Usage exist) · keys → en/zh/zh-TW only.

---

## 6. Final Verification Checklist

Run all after Phase 10 (and spot-run after each arc phase):

1. **Build matrix:** `cargo check` ✓ · `bun run build` (tsc+vite) ✓ · `bun run lint` ✓ · `cargo fmt --check` (or run) ✓
2. **Tests:** `cargo test` full (audio_toolkit incl. smoothed/sink/chunking, transcription coordinator tests, translation-mock tests) ✓
3. **Unit regressions:** preroll tests (sink-path equivalents of ddffa435) ✓ · coordinator StageAction tests incl. new pending-press rules ✓
4. **Feature smoke (manual, macOS):**
   - Cold start: no Dock icon when started hidden (fbd4e15f), tray icon correct; Login Item via SMAppService registers (0902937b)
   - Record → transcribe (Metal): ① batch quality unchanged, ② **first word intact NS-off**, ③ NS-on unchanged behavior
   - Live captions stream + finalize; cancel mid-stream; USB unplug/replug recovery (fork watchdog paths still fire)
   - Paste to: standard app, Finder, browser; image-only clipboard restore; SKE-app (simulate Secure Input) → warning + fallback cancel
   - Custom words: vocabulary-capable model honored without double fuzzy-correction; non-vocabulary path (initial_prompt + fuzzy) intact
   - Long-form: 5-min recording chunked + stop-cancel mid-run; toggle-off path
   - Post-process: OpenAI-compatible + Anthropic format; error toasts actionable (4223e7ac)
   - Models: GGUF download + load, custom GGUF discovery, retired-model migration notice (if D3-adopt), translation + Moonshine/SenseVoice/GigaAM path (GGUF under D3-adopt, ONNX under D3-reject), model-file-deletion recovery (e4ae0d44), availability check (eea1f5f4)
   - History: single row per transcription through the full session cycle
   - Settings: scroll reset, shortcuts rebind flow (suspend-all), PTT/toggle/hold modes, long_form toggle persists
   - Updater tray item still works against fork's vendored plugin
5. **No fork regression on** version scheme (stays 0.12.x), `HANDY_DISABLE_UPDATER`/portable gating, dangerous-skip-verify.

---

## 7. Post-Sync Documentation

**UPSTREAM_SYNC.md (at end, and incrementally):**
- New "Current Status": Last Sync Date = completion date; Last Upstream HEAD Examined = `9e57a0d7`; Fork Version = 0.12.x+; counts: Synced = PORTED 13 + landed APPLY (61 minus any gate-rejected rows); Skipped = 76 policy (incl. enumerated provisional) + 5 superseded + any gate-rejected APPLY rows; Verify rows resolved to their outcome.
- New section "2026-10 range (`cdbc2239..9e57a0d7`)": three tables — ✅ Synced (upstream sha → fork sha + note), ⏭️ Skipped (sha → one-line reason), ⏳ Decision-open (gates D1–D6 outcomes + date).
- Keep old registry sections beneath (do not rewrite history rows; strike the stale "Current Status" numbers only).

**learning-log.md entries (problems→root-cause→fix, bulleted, dated):**
1. Engine arc migration: supervisor protocol gotchas (stdout capture before native code, f32 frame channeling, capabilities→Result, serde NaN→JSON), plus any fork-specific fix.
2. handy-recorder preroll re-port: what changed on sink path; how fork semantics were re-expressed; A/B result.
3. ONNX removal (or D3-reject dual-stack rationale): serde-migration of settings changes, clang_rt.osx link note / ORT-kept note, model-path migration notes.
4. Vocabulary/long-form interplay with fork post-correction (the exact skip-when vocabulary-sent rule).
5. Any shortcut/coordinator behavior-port nuances (PendingPress ↔ fork Stage machinery).
6. handy-keys 0.2.4→0.3.4 vendor refresh (D5): `reset_pressed_hotkeys()` re-application, fork's 300-ln CGEventTap silent-death recovery replaced by upstream #31 (`7903fda`) callback recovery; record the macOS sleep/wake + recording-hotkey regression pass result (acceptance gate).

---

## 8. Hard Dependency Graph (condensed)

```
Phase 0 → (1 → 2 → 3 → 4 → 5)  low-risk runway
5 ─→ 6 (c8262ab3 → 1b011a19)           [D1]
6 ─→ 7 (a42131d2 + preroll re-port)    [D2 — independent of tcp 0.4.x; D2 may run with D1 rejected: under D1(c), run 7 before/instead of 6→8→9]
6,7 ─→ 8 (3985508a → cfcb6507 → 5197faf0) [D1 × D3]
8,6 ─→ 9 (4150a4a8 → 9e57a0d7)          [D1 × D3 per matrix]
9,5 ─→ 10 (overlay/live-captions/verify + closeout)
```
Intra-phase must-afters: `00d25549`+`37a26fd6` ← `d001fcd9`; `5ec2276a` after secure_input; `b38987a9` ← `d001fcd9`; `c62a5fcd` ← `c6fa60da`; `dc5bdc9d` → `eea1f5f4`; `417dc6ac` ← `20a6685e`; `5197faf0` ← `cfcb6507` (FE shape).

---

## 9. P6 Deferred-Hunk Ledger (live tracking)

These upstream commits contain transcription.rs hunks that cannot land until Phase 6 rewrites `managers/transcription.rs` onto the engine-supervisor client. The non-transcription hunks are applied in Phase 2; the transcription.rs-side wiring is recorded here so it is not lost.

| Upstream SHA | Phase 2 non-transcription work | Deferred transcription.rs hunk | Notes |
|---|---|---|---|
| `29bd2c0d` | `lib.rs` — move compute-device reporting off startup path onto prewarm thread (APPLIED NOW; transcription.rs hunk was a pure function extraction needed by the lib.rs call) | `managers/transcription.rs` `report_compute_devices()` shape / threading | Function extracted and called from startup + prewarm thread; no further P6 work required for this commit. **P6 close-out (2026-10-20):** confirmed — the lib.rs prewarm now calls the manager-based supervisor probe; nothing further needed. **P6 follow-up (`debbb66a`, 2026-10-20):** prewarm now calls the detached-handle `report_compute_devices_on(&EngineSupervisor)` under a brief lock instead — the TM mutex is no longer held across the worker probe (stop→transcribe TranscriptionBusy risk); the manager-based wrapper was removed as callerless. |
| `e4ae0d44` | `managers/model.rs` + FE ModelsSettings/modelStore — model-file-deletion recovery | `managers/transcription.rs` 46-line `loading_failed` recovery hunk | **APPLIED in P6 (`4c946fb0`):** supervisor-client load-failure path recovers via `forget_model_if_engine_dropped` (worker-gone detection keeps model-id/UI in step and emits `unloaded`); `loading_failed` emits kept in `load_model_with_device`. |
| `258899a2` | `actions/` + `utils.rs` — redact transcription content from logs | `managers/transcription.rs` hunk (log redaction) | **APPLIED in P6 (`4c946fb0`):** the rewritten client's result log line goes through `crate::utils::redact_text`. |
| `4cd49950` | `audio_toolkit/lang_id.rs`, `text.rs`, settings, FE FillerWordRemoval | `cpp_translation_task()`-adjacent lang_id text-fallback wiring | **P2 close-out (2026-10-18):** setting + two-tier filler logic + `OutputLanguageEvidence` + `detect_output_language` all landed (fork commit `28119924`; 8ef8dd43 `91ee3aee`, eb49dc02 `598c0504` on top). Master toggle IS wired end-to-end via `process_transcription_text(…, filler_word_removal_enabled, …)`. **APPLIED in P6 (`4c946fb0`):** real evidence chain built in the client — `resolve_output_language_evidence` + `with_model_detected_language` (Whisper audio LID via `Transcript.language`) + `TextDetected` fallback via `detect_output_language`; `process_transcription_text` now takes `&OutputLanguageEvidence` (number-words gate falls back to UI language only when evidence is Unknown). **Re-deferred:** deleting the legacy `filter_transcription_output` wrapper (text.rs) — still referenced by 4 text.rs unit tests; cosmetic follow-up, no behavior impact. |
| `6fa85061` | `managers/model.rs` + `LanguageSelector` constants | `cpp_translation_task()` language-coverage / 'no'-language guard | **P2 close-out (2026-10-18):** `canonical_language_code` + exact-base-preference in `effective_language` + tests landed (fork `8addac74`); FE picker matches base codes + aliases. **APPLIED in P6 (`4c946fb0`):** the transcription.rs-side guard is `canonical_language_code` comparison inside `resolve_output_language_evidence` (the 'no'→'nb' alias case, unit-tested). |
| `0bb0428e` | `audio_toolkit/chinese_script.rs`, setting, FE selector | script-aware language-param handling in transcription.rs | **P2 close-out (2026-10-18):** module + `ChineseScript` setting + FE selector landed (fork `c1a6ccc9`); conversion enforcement wired through the fork's `actions/post_process.rs::maybe_convert_chinese_variant` (explicit script wins, legacy zh-Hans/zh-Hant intents remain fallback, non-Chinese recognition language never converted — fail-closed). Upstream's `selected_language`→`zh`+script migration NOT adopted (fork's picker still exposes those intents; bare `zh` absent from fork's `LANGUAGES`). **APPLIED in P6 (`4c946fb0`):** `PreviewScript` live-preview conversion while streaming; Chinese-script conversion ordered before custom-word correction in the batch/stream post-processing; the transcription text path now keys off output-language evidence instead of the recognition language. **Re-deferred:** migrating `actions/post_process.rs::maybe_convert_chinese_variant` onto the evidence signal — after the transcription-path conversion it is a no-op safety net (nothing double-converts); optional follow-up. Fork tray_i18n hunk skipped (never had the hant/hans logic). |

*(Ledger updated during Phase 2; revisit before closing Phase 6.)*

### P7 inventory (2026-10-20, STEP 1 — written before any code)

Fork doomed files inventoried in full: `audio_toolkit/audio/recorder.rs` (1914 ln, fork-only,
inline tests), `audio_toolkit/audio/resampler.rs` (287 ln), `audio_toolkit/bin/cli.rs` (357 ln,
dead code — `[[bin]]` already commented out in Cargo.toml). Upstream copies (1046/351/371) were
NOT used as content source. handy-recorder 0.1.0 source read from the crates.io tarball
(6.5k ln): engine.rs (3-thread device/delivery/notify + watchdog + rtrb ring), delivery.rs
(command-drain-before-drain loop, pause-ack stop, finish_tail), sink.rs trait, error.rs
(ErrorKind taxonomy), device.rs (InputDevice{id,name,occurrence,backend,is_default,
id_is_stable,channels,is_monitor}), backend/cpal.

Classification key: **(a) upstream-fixed** (crate/upstream commit covers it), **(b) must
re-port** (fork custom, ported onto the new path), **(c) obsolete**.

#### Fork recorder.rs behaviors

| # | Fork behavior (symbol) | Class | Disposition on new path |
|---|---|---|---|
| 1 | `9e3b7f65` command drain at TOP of loop; `apply_start()` before first chunk | (a) | delivery.rs checks `DeliveryCmd` before every bounded drain; `begin()` processes already-queued ring audio; `slot` mutex serializes rapid Stop→Start |
| 2 | `9e3b7f65` `pending_stop` deferral (Stop waits for in-flight chunk; finalized even when stream dead; cleared on rapid Stop→Start) | (a) | engine `stop()`: pause-ack → drain-all → `finish_tail()` inside one call; `slot` state machine rejects double-stop; start() after failure rebuilds (manager retry loop) |
| 3 | `ddffa435` NS-off full preroll slice emission (first-word fix) | (b) | sink.rs: `VadFrame::Speech(emitted) => out.extend(emitted)` — upstream's CaptureSink already appends the full emitted slice; **verified identical to fork's no-NS branch** |
| 4 | `ddffa435` NS-on branch appends current ORIGINAL frame (emitted slice is denoised, unmappable) | (b) | sink.rs gains `noise_suppressor: Option<Arc<Mutex<NoiseSuppressor>>>`; VAD sees denoised chunk, out/on_audio get original chunk — port of `handle_frame` |
| 5 | `df216832` tail audio (stop-flag block forwarded before EOS) | (a) | engine stop(): boundary block + pause-ack + drain_all + resampler finish_tail |
| 6 | `d54c88eb` rt-safe callback | (a) | crate callback only writes rtrb ring (transport.rs) |
| 7 | `db003f38` idle level-meter skip | (a)/S | idle chunks discarded by crate; sink exists only during (standing) recordings |
| 8 | `b4453a29` cpal work off main thread | (a) | crate device thread + `OpenTimedOut` handling |
| 9 | `549cbde3` readiness handle | (a) | sink `ready` channel (first chunk) + `EndReason` |
| 10 | `a4348beb` stream recovery | (a) | crate watchdog (NoAudio 10s / Stalled 5s) → failure handler → `OpenMicrophone::needs_reopen` + manager rebuild (fork's liveness monitor kept on top) |
| 11 | NS runs BEFORE VAD; VAD sees denoised; originals kept for output | (b) | = row 4 (NoiseSuppressor into sink) |
| 12 | Smart-stop volume-aware trailing buffer (`extra_recording_buffer_ms`; SILENCE_RMS_MULTIPLIER 3.0, SILENCE_THRESHOLD_MS 500, MIN_BUFFER_MS 200, NOISE_ALPHA 0.05, DEFAULT_NOISE_FLOOR 0.005; state stays Recording during window; Stop cancels it) | (b) | re-expressed manager-side: stop_recording runs the trailing window while the recording keeps running (engine ring absorbs hiccups); sink publishes per-chunk RMS + VAD-noise-frame noise-floor EMA into shared capture state; constants move to managers/audio.rs |
| 13 | Noise-floor EMA from VAD-Noise frames (not during smart-stop window) | (b) | sink: Noise classification updates shared EMA; smart-stop window flag suppresses |
| 14 | Always-on pre-buffer (`with_pre_buffer_ms`, rolling raw 16 kHz buffer while idle, prepended raw at Start, VAD reset) | (b) | **standing-recording armed sink**: in always-on+prebuffer mode the manager keeps one recording active whose sink maintains the rolling buffer; hotkey press "arms" it via shared control (drain rolling → raw+out, VAD reset); hotkey release → stop() → re-arm a fresh standing sink. pre_recording_buffer_ms is opt-in (default 0) |
| 15 | `StreamRouter` per-frame feed — ALL resampled frames, pre-NS/pre-VAD, even when not recording | (b) | sink gains `on_raw_frame` pre-VAD callback (upstream feeds router post-VAD speech only — fork's live-captions streaming needs contiguous audio; keep fork seam) |
| 16 | 2.5s batch streaming callback (live captions fallback when no dedicated model; gates: streaming_enabled flag, ≥3 speech frames, 2.5s interval, router-not-active, cancel flag, close-generation guards; TM try_lock_for(2s)) | (b) | manager-side accumulator fed by sink `on_audio` (post-VAD emitted slices); same gates; `set_streaming_enabled` recreate-free toggle preserved |
| 17 | `last_voice_activity_ms` stamp (elapsed ms on VAD-speech frames) | (b) | sink stamps shared atomic on every Speech emission |
| 18 | max_level tracking (RMS·1e6 atomic; dead-mic detection `NO_AUDIO_THRESHOLD 0.001` → USB watchdog) | (b) | sink computes per-chunk RMS into shared atomic (all frames while recording) |
| 19 | emergency_save hooks (start_recording at Start; update_samples every 5s; complete_recording at stop; cancel_recording in manager) | (b) | sink holds `Option<Arc<EmergencyBackup>>`: start on construction/arm, 5s-throttled update on delivery thread; manager completes after stop returns samples (backup now covers the drain window — strictly better); cancel path unchanged |
| 20 | `is_stream_alive(3s)` + 500ms grace (liveness monitor + try_start_recording) | (b) | sink stamps shared `last_chunk_ms` per chunk; manager keeps exact fork logic (opened_at grace + timeout) |
| 21 | `needs_reopen` (stream_error flag / worker finished) | (a) | `OpenMicrophone::needs_reopen` (per-recorder `failed` flag; handler can't mark a replacement) |
| 22 | `device_name()` (device-change detection in try_start_recording) | (a) | `recorder.info().device.name` |
| 23 | `preferred_input_channel_count` (commands/audio.rs) | (a) | `InputDevice.channels` (None→1 on ALSA) |
| 24 | error classifiers `is_microphone_access_denied` / `is_no_input_device_error` + actions mapping | (a) | upstream `recording_error_type()` via `ErrorKind::{PermissionDenied→microphone_permission_denied, DeviceUnavailable→no_input_device}`; string fns deleted with recorder.rs |
| 25 | teardown barrier (EndOfStream wait before stream drop; CoreAudio heap corruption) | (a) | crate device-thread teardown + pause-ack |
| 26 | `FrameResampler` (rubato FFT, reset-between-recordings fixes, delay-line drain) | (a) | crate's internal FrameResampler (rubato 5.0), `resampler.reset()` at each `begin()` |
| 27 | `get_preferred_config` (native rate, F32>I16>I32 preference) | (a) | crate opens device at OS format and resamples; format preference is crate-internal |
| 28 | visualizer (window scaled to device rate) | (b, minor) | upstream's 16 kHz window-512 visualizer in sink (fork's device-rate scaling meaningless at fixed 16 kHz output) |
| 29 | inline error-classification tests | (a) | replaced by manager `recording_error_type` tests + crate suite |
| 30 | resampler.rs unit tests (reset/no-crosstalk) | (a) | crate tests cover its resampler; fork tests die with the file |
| 31 | bin/cli.rs (RecorderMode always/on-demand demo) | (c) | dead code; delete |

#### handy-recorder 0.1 coverage of in-range recorder commits
`df216832` tail audio, `a4348beb` recovery, `d54c88eb` rt-safe, `db003f38` idle-skip,
`b4453a29` async open, `549cbde3` readiness, `9e3b7f65` command-loop semantics: **all inside
the crate** (verified against engine.rs/delivery.rs/transport.rs/watchdog.rs source, rows
above). `ddffa435` preroll: upstream sink.rs carries the no-NS half; the NS-on half + NS
pipeline are fork-only → re-port. `12f02e2a` channel selection: `Channels::Only(n)` +
`open_microphone_on_channel` InvalidChannel fallback. `c89b7bf3` default-mic fallback: device
`None` + `MicrophoneResolution` fallback logic (upstream managers/audio.rs).

#### Deliberate deviations (recorded, not regressions)
1. Level events in always-on mode without pre-buffer: only during recordings (fork emitted
   continuously while the stream was open). Overlay is hidden when idle → not consumed.
   With pre-buffer enabled the standing sink restores continuous levels.
2. Batch streaming callback re-invocation during mid-recording silence: fork re-sent the same
   buffer every 2.5s even with no new speech; new path accumulates per emitted speech frame,
   so an unchanged buffer is not re-sent (same final text; fewer redundant transcriptions).
3. Sink panics now end the recording with `EndReason::SinkPanicked` (audio before the panic
   kept) instead of poisoning a shared worker — strictly better containment.

### P7 close-out addendum (2026-10-20)

Phase 7 complete (D2 adopt+re-port, single upstream row `a42131d2` landed as 4 fork commits):
`538a05c0` (STEP 1 inventory, written before code), `1f03472b` (the capture swap), `9d783219`
(test-target repair — also closes the P6 follow-up about parakeet/proptest not compiling),
`230dc995` (preroll acceptance tests). Tag `sync-2026-10-p7-done`. Registry rows with the full
re-port/disposition tables live in `UPSTREAM_SYNC.md` (Phase 7 Commit Notes).

Gate: `cargo check --all-targets` ✓ · `cargo test --lib` **544 passed / same 4 documented
pre-existing failures** (3× history schema, 1× conflicts machine-specific) · `bun run build` ✓ ·
integration: `capture_sink_preroll` 3 ✓, `parakeet_thresholds` 3 ✓, `proptest_audio` 15 ✓.

**Acceptance evidence (D2 proof, NS off):** the `transcribe_wav` harness cannot drive the
capture path (it feeds a WAV straight to the engine) and no GGUF model remains locally, so per
the plan's fallback the proof is sink-level: (a) scripted-VAD tests prove the full VAD-emitted
slice — including withheld pre-onset frames — opens the recording (ddffa435 mechanics) and the
NS-on branch keeps the current-original-frame behavior; (b) the env-gated `real_vad_preroll_slice`
test runs REAL Silero v4 + production SmoothedVad over real user recordings: recording opens
**420 ms / 420 ms / 210 ms before** the standalone detector's first voiced frame across 3/3
recordings — real leading preroll slices, first word intact. Manual mic pass: REGRESSION-TEST-REQUIRED.

**Unexpected finding during the swap (recorded, resolved):** fork `cpal = "0.16"` cannot coexist
with handy-recorder's handy-cpal (cpal 0.18.2) — cargo `links = "alsa"` conflict — so the planned
interim "dep + unwired sink" checkpoint was impossible; the whole arc landed as one coherent
commit instead (the plan's own fallback for this case).

**Carry-forward for Phase 8 (VAD GGUF + ONNX removal) and Phase 10 (overlay/live-captions
reconciliation):**
- `CaptureVad`/`VadSlot` are detector-agnostic (`Box<dyn VoiceActivityDetector>`); P8's backend
  swap touches only `create_capture_vad` in managers/audio.rs. The fork's user-facing VAD
  sensitivity setting (Silero-frame units → ms conversion) lives there too — preserve it in the
  P8 migration (upstream hardcodes 0.3/0.5 thresholds; the fork's setting must win).
- The standing capture (`refresh_standing_capture`) is the new home of idle-audio consumption:
  pre-hotkey buffer AND the 2.5s batch-captions fallback keep running while idle in always-on/BT
  mode. P10's overlay work should treat "standing" as the idle state (levels flow while standing;
  overlay is hidden when idle so this is not consumed today — recorded deviation #1).
- `set_streaming_enabled` is recreate-free (AtomicBool in `CaptureControl`) and now also
  starts/stops the standing capture; shortcut's live-captions toggle path is unchanged.
- try_start_recording now returns `RecordingReadiness` (upstream shape) and actions spawn
  readiness waiters; the fork's stale-Recording-state recovery and cancel escape hatch are kept.
- `RecordingState::Stopping` was added (upstream parity) — the smart-stop window runs under it;
  `is_recording()` reads the `recording_active` atomic mirror (never the state mutex) — b4453a29's
  lock-free concern finally fully addressed on the fork side.

### P6 close-out addendum (2026-10-20)

Phase 6 complete (D1 full adopt, matrix row 1): fork commits `1e85e476` (Cargo 0.3.1 + serde + libc),
`ad2ee077` (engine_supervisor greenfield + worker dispatch), `4c946fb0` (transcription.rs supervisor-client
re-skin + all six §9 deferred hunks), `9edb7b5b` (0.4.0 + capabilities Result). Tag `sync-2026-10-p6-done`.
Full dispositions in the ledger above; registry rows in `UPSTREAM_SYNC.md` (Phase 6 Commit Notes).

**P5 carry-forward verification:** Phase 6 did not touch `transcription_coordinator.rs`, `shortcut/handler.rs`,
or `signal_handle.rs` — `PendingPress` (busy-window remember/forget + cross-binding rule), `Stage::Recording.locked`,
`PendingPress.pressed_at` drain timing, `send_external_input` debounce exemption, and
`Command::Input{activation, hold_threshold}` all survived untouched, along with the fork's `Stage` machine, VAD
sliding deadlines, and `transcription_coordinator_tests.rs` (58 tests, all green).

**P6 follow-ups (2026-10-20):**
- **CI-mock decision (§6 "keep transcription_mock.rs tests compiling… introduce a small `EngineClient` trait if needed"): refreshed with stub bodies; trait seam NOT introduced.** `cfb09fdc` refreshes the mock to the full engine-supervisor pub surface. No `EngineClient` trait was needed: the real manager is a thin client over the cheap-clone `EngineSupervisor` handle, so the mock holds one verbatim and stubs every method. Deleting the mock is not an option unless the CI swap steps are removed too (ci.yml/test.yml copy it over `transcription.rs` and drop the `transcribe-rs` dep). CI-swap verification: `cargo check` green, clippy `-D warnings` zero new errors vs real-tree baseline (the mock keeps helpers orphaned by the swap reachable: model.rs language-coverage wiring, utils.rs Windows-emulation probe).
- **Prewarm probes off the manager lock** (`debbb66a`): the `29bd2c0d` prewarm call held the TM parking_lot mutex across a worker probe that can block up to the hello timeout (stop→transcribe `TranscriptionBusy` risk). Now: brief lock → `engine_supervisor()` clone → drop guard → `report_compute_devices_on`/`get_available_accelerators_on` on the detached handle. The callerless manager-based `report_compute_devices(tm)` wrapper was removed (see §9 ledger row for `29bd2c0d`); TM-based `get_available_accelerators` stays. Also fixed the stale `is_model_loaded()` "lease counter" comment (§9 `e4ae0d44`-adjacent reality: ONNX engine sits directly under the `onnx` mutex).
- Gate re-run after both: `cargo check` green; `cargo test --lib` 552 passed / same 4 documented pre-existing failures.

**Fork divergences recorded (deliberate, not deferrals):** upstream's glibc allocator hooks (`crate::memory`)
not ported into the worker (fork never carried memory.rs; no-ops on macOS). `describe_compute_devices` dropped
(fork has no `--list-devices` flag to serve; can return with that flag). Upstream's `LoadingGuard` simplification
NOT adopted (fork keeps the pending-model queue in the guard). Fork keeps its per-manager supervisor (main +
live captions each own a worker slot; upstream has a single manager). StreamPerf moved to `Arc<Mutex>` shared
between the stream worker loop and the supervisor's `on_progress` callback (feeds now report from the owner thread).

### P5 close-out addendum (2026-10-19)

Phases 3–5 complete (tags `sync-2026-10-p3-done` @ `aa55a46d`, `-p4-done` @ `5804f3a2`, `-p5-done` @ tag created at close). No new P6 deferred hunks arose: none of the Phase 3–5 rows contain transcription.rs hunks. Row tables and per-commit notes live in `UPSTREAM_SYNC.md` (Phase 3/4/5 Commit Notes).

**Carry-forward for Phase 6 (P6 re-skin must preserve):** the Phase 5 coordinator additions are now load-bearing — `PendingPress` (busy-window remember/forget + cross-binding rule), `Stage::Recording.locked` (tap-locked hold-or-toggle sessions; drain starts at the real press time via `PendingPress.pressed_at`, and `start()` preserves `since`/`locked` from the optimistic transition), `send_external_input` debounce exemption (signal_handle.rs), and the shortcut activation plumbing (`Command::Input{activation, hold_threshold}`). If Phase 6 touches `transcription_coordinator.rs` / the supervisor client wiring, these semantics must survive the re-skin, along with the fork's `Stage` machine, VAD sliding deadlines, and `transcription_coordinator_tests.rs` (58 tests incl. the 14 new busy-window/hold-or-toggle ones).

**Documented divergences (deliberate, not deferrals):** c6fa60da — ProcessingTimeout drops the pending press; upstream's OVERLAY_SHOW_GENERATION skipped (fork's OVERLAY_SESSION + is_active_use guards are stronger). c62a5fcd — upstream's 50ms RELEASE_GRACE pending-release machinery (X11 auto-repeat, #1539) not ported; immediate release resolution instead. D5 — REGRESSION-TEST-REQUIRED manual macOS sleep/wake + hotkey pass still open (see D5 EXECUTION NOTE in §3).

### P2 close-out addendum (2026-10-18)

Remaining Phase 2 rows finished this session, with fork commits: `4cd49950` partial → `28119924` (+ `91ee3aee`, `598c0504`), `6fa85061` partial → `8addac74`, `0bb0428e` partial → `c1a6ccc9`, `d55ea7ef`+`0e503672` → **S** (fork already at/above: `TranscribeAcceleratorSetting::Gpu` maps to `Backend::Auto` + stable `transcribe_gpu_device` identity; upstream's gpu→auto migration would be a no-op; catalog/build.rs merge content skipped by fork policy), `cf49ab35` → `cceadc15` (query-only main-thread hop; whole-body hop rejected — it would deadlock the fork's oneshot position confirmation). Also fixed a test-cfg break left by the `14ae9c6b` commit (`PostProcessProvider` test initializer missing `api_format`, in `llm_client.rs` `provider()`). Rows the orchestrator flagged as possibly-P2 (`53db4cb6`, `496a80cc`, `7c31572d`, `37a26fd6`, `5ec2276a`) are Phase 3/Phase 4 rows in this plan and were not actioned here.
Order above holds under the default gate outcome (matrix row 1); rejected-gate reshuffles follow §3's closure matrix (e.g., D1=(c) → run Phase 7 directly after Phase 5 and skip 6→8→9 per rows 7/8).