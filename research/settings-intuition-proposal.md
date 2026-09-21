# Handy Settings Reorganization Proposal

**Goal:** Reduce duplicate concepts, clearer labels, and group related controls so users can predict what each setting does.
**Design principles:** [interfaces.dev/cheat-sheet](https://interfaces.dev/cheat-sheet) — one word per flow, orient the reader, label toggles by the state they turn on, keep related controls adjacent, and never rely on color alone for status.

---

## 1. Mic readiness: merge two toggles into one slider

### Current state

| Setting                              | Tab                     | Meaning                                                |
| ------------------------------------ | ----------------------- | ------------------------------------------------------ |
| Always-On Microphone                 | Debug                   | Mic opens at startup and stays open forever.           |
| Keep Mic Open Between Transcriptions | Advanced → Experimental | After a recording, leaves the mic open for 30s.        |
| Bluetooth keep-alive                 | Hidden                  | Auto-keeps mic open when Bluetooth output is selected. |

### Problem

Both visible toggles solve the same user problem ("I don't want to wait for the mic to open"), but they are in different tabs and use different metaphors. A user can accidentally turn both on and not understand the difference.

### Proposed change

Replace the two toggles with a single control under **General > Recording** or **Accuracy > Voice Detection** (whichever feels closer to the recording flow):

**Control:** segmented slider or three-segment control
**Label:** "Keep microphone ready"
**Options:**

1. **Off** — close the mic when not recording (default, saves battery)
2. **For 30 seconds after recording** — keep it warm between transcriptions
3. **Always on** — keep the mic open continuously

**Helper text:** "Always on lets Handy capture audio before you press the shortcut. It uses more battery."

### Rationale (interfaces.dev)

- **One word per flow:** the flow is "when is the mic open?" Pick one label and keep it everywhere.
- **Orient the reader:** the slider makes the progression obvious from "off → warm → always on".
- **Hide the Bluetooth detail:** it is not a user-facing setting; it is an implementation detail that keeps Bluetooth output devices stable.

---

## 2. Pre/post buffer: rename and place next to each other

### Current state

| Setting              | Tab   | Meaning                                                                               |
| -------------------- | ----- | ------------------------------------------------------------------------------------- |
| Pre-Recording Buffer | Debug | Captures audio _before_ the hotkey press. Only works when Always-On Microphone is on. |
| Recording Buffer     | Debug | Captures audio _after_ the hotkey release (or smart stop).                            |

### Problem

Both are called "…Buffer", sit side by side in Debug, and both use ms sliders, but one is pre-trigger and the other is post-trigger. The user can't tell the difference without reading carefully.

### Proposed change

Move both into a single grouped card next to the new **Mic readiness** control (because one depends on it).

| New label        | Old name             | Behavior                                                      | Dependency                                              |
| ---------------- | -------------------- | ------------------------------------------------------------- | ------------------------------------------------------- |
| **Lead-in time** | Pre-Recording Buffer | Capture N ms before the shortcut is pressed.                  | Disabled unless "Keep microphone ready" is "Always on". |
| **Tail time**    | Recording Buffer     | Continue capturing up to N ms after the shortcut is released. | No dependency.                                          |

**Helper text for Lead-in time:** "Only available when the microphone is always on, because Handy needs a live buffer to pull from."

### UI treatment

- "Lead-in time" slider is **disabled with a visible note** when mic readiness is not "Always on".
- Put the two sliders in one card titled "Capture padding" so it is obvious they are a pair.

### Rationale (interfaces.dev)

- **One word per flow:** pick "lead-in / tail" or "pre-roll / post-roll" and use it consistently. Avoid the generic word "buffer" twice.
- **Keep related controls adjacent:** because lead-in depends on mic readiness, the user should see them together.
- **Never rely on color alone:** a disabled slider should also show a text explanation, not just greyed-out chrome.

---

## 3. Hard-coded 3-second silence pad: make it model-aware or expose it

### Current state

- Backend adds **3 seconds of leading silence** to every short clip before transcription.
- Applied to **all engines**: Whisper, Parakeet, Moonshine, SenseVoice, GigaAM, Canary, Cohere.
- Original comment says it reduces **Whisper** hallucinations and prevents clipping the first words.
- The claimed VAD trailing-silence cleanup (`trim_trailing_silence`) is **defined but never called**.
- Does **not** affect the live-captions/streaming path.

### Problem

The pad was tuned for Whisper, but every engine receives it. For non-Whisper models it may be unnecessary or even harmful (extra silence can confuse attention-based models or add hallucinations in other forms). There is no way to disable or inspect it.

### Proposed change

There are two reasonable directions. Pick one.

#### Option A — Make it engine-aware (recommended)

- Apply the 3s leading pad **only to Whisper-family engines**.
- For other engines, do not pad at all, or pad with a much shorter duration (e.g., 300ms).
- Rationale: the documented rationale is Whisper-specific, and there is no evidence other engines need it.

#### Option B — Turn it into a per-model setting

- Add a setting like "Short-clip silence pad" in the model selector or in Accuracy.
- Default it on for Whisper, off for everything else.
- Rationale: gives power users control and makes the model-specific default explicit.

#### Option C — Remove it and rely on VAD trimming

- Re-enable `trim_trailing_silence` (which already exists but is unused) and stop adding synthetic silence.
- Riskier, because the original bug was real Whisper hallucination. Should be A/B tested before shipping.

### Suggested action

Start with **Option A** (engine-aware gating) because it is the smallest, safest change and matches the documented intent. If you later get user reports that a non-Whisper engine needs padding, migrate to Option B.

### Rationale (interfaces.dev)

- **Don't expose internals that don't need a decision:** if only Whisper needs this, the user should not see a knob.
- **Orient the reader:** if it becomes a setting, its label must describe the _state it turns on_ and the model it affects.

---

## 4. Other consolidation opportunities (prioritized)

### 4.1 VAD sensitivity

**Current:** global control in Accuracy, per-model override in model selector, plus Debug locale strings.
**Proposal:** one global control in Accuracy. If per-model override is needed, surface it as an "Override global VAD" toggle inside the model card, not as a separate dropdown.

### 4.2 Word correction

**Current:** one mode dropdown plus three separate word lists (`custom_words`, `advanced_custom_words`, `word_replacements`) and a legacy boolean.
**Proposal:** one unified "Word fixes" card. The mode selector chooses which list is active; the inactive lists are hidden (not deleted from storage). Remove the legacy `use_advanced_custom_words` boolean after a migration cycle.

### 4.3 History retention

**Current:** `history_limit` and `recording_retention_period` both use `history_limit` when retention is set to "preserve latest N".
**Proposal:** rename the retention option to "Keep latest N recordings" and show the same limit field inline, so it is obvious they share one number.

### 4.4 Output behavior

**Current:** six scattered controls: paste method, typing tool, clipboard handling, auto-submit, trailing space, paste delay.
**Proposal:** group all of them under a single **Advanced > Output** card titled "After transcription". Order them logically:

1. Paste method
2. Clipboard handling (only if method copies)
3. Append trailing space
4. Auto-submit
5. Paste delay
6. Typing tool (Linux only, hidden on other platforms)

### 4.5 Post-processing

**Current:** toggle in Advanced → Experimental, full config in a separate Post Process tab.
**Proposal:** move the toggle into the Post Process tab. If Experimental mode must gate the tab, make the tab visible only when Experimental features are enabled, or show a "Post-processing is an experimental feature" banner inside the tab.

### 4.6 Language

**Current:** `selected_language` (transcription) vs `app_language` (UI).
**Proposal:** rename labels to **"Transcription language"** and **"Interface language"**. Keep them in separate sections so the distinction is obvious.

### 4.7 Shortcuts

**Current:** three record shortcuts: `transcribe`, `transcribe_with_post_process`, `transcribe_with_router`.
**Proposal:** keep one primary record shortcut and make post-processing/router behavior selectable modes, or at least group the three shortcuts under a "Recording shortcuts" card with clear sub-labels.

---

## 5. Suggested tab reorganization

| Tab           | Contents                                                    |
| ------------- | ----------------------------------------------------------- |
| **General**   | Language, shortcuts, sound, appearance                      |
| **Recording** | Mic readiness, lead-in/tail time, VAD sensitivity           |
| **Accuracy**  | Model selection, live captions, per-model overrides         |
| **Output**    | Paste behavior, auto-submit, post-processing                |
| **History**   | Retention, storage location                                 |
| **Advanced**  | Experimental toggles, debug logging, overlay, audio devices |

---

## 6. Implementation notes

- The backend settings keys can stay the same; this is primarily a frontend reorganization plus a small backend change for the engine-aware silence pad.
- i18n keys need new entries for the merged labels and helper text.
- The "Mic readiness" slider maps to existing booleans:
  - Off → `always_on_microphone = false`, `lazy_stream_close = false`
  - 30s → `always_on_microphone = false`, `lazy_stream_close = true`
  - Always on → `always_on_microphone = true`, `lazy_stream_close = true` (or don't care)

---

## 7. Open questions for you

1. Should the new **Mic readiness** control live in **General > Recording** or **Accuracy > Voice Detection**?
2. Do you want to expose the 3-second silence pad as a setting, or should we gate it to Whisper only?
3. Is the per-model VAD override actively used, or can we fold it into a single global control?
4. Do you want me to prototype the merged "Mic readiness" slider first, or the grouped buffer card first?
