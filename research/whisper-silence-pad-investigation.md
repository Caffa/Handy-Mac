# Investigation: The 3-Second Silence Pad in `stop_recording`

**Branch:** `feature/streaming-live-captions`
**File:** `src-tauri/src/managers/audio.rs:1786`
**Date:** 2026-08-21

## TL;DR

The 3-second leading-silence pad is **applied unconditionally to every model and engine** before final (batch) transcription. It is **NOT** conditional on model type. There is **no per-engine gating** — Whisper, Parakeet, Moonshine, SenseVoice, GigaAM, Canary, and Cohere all receive the identical padded buffer at `audio.rs:1787-1795`. The pad does **not** affect the live-captions/streaming path. Critically, the "VAD-based `trim_trailing_silence`" cleanup the comment claims to rely on is **defined but never called** anywhere in the codebase.

---

## 1. Is the pad conditional on model/engine type? — **NO**

The padding block at `audio.rs:1786-1795` sits in `stop_recording`, at the point where the VAD-filtered `processed_samples` are prepared before being returned. The code contains **no `match` on `engine_type`, no model ID check, and no capability check**:

```rust
// audio.rs:1786
let min_samples = WHISPER_SAMPLE_RATE * 3; // 3 seconds minimum
let padded_processed = if s_len > 0 && s_len < min_samples {
    let needed_silence = min_samples - s_len;
    let mut padded = vec![0.0f32; needed_silence];
    padded.extend_from_slice(&processed_samples);
    padded
} else {
    processed_samples
};
```

The `AudioRecordingManager` that owns `stop_recording` (a lower-level recorder layer, `audio.rs`) has **no knowledge of which model is selected** — model selection lives in `ModelManager`/`TranscriptionManager` (`managers/model.rs`, `managers/transcription.rs`). So the pad cannot be (and is not) engine-aware at this layer.

**Where it flows:** `router.rs:283` and `transcribe.rs:352` both call `rm.stop_recording(...)`, get `(padded_processed, raw_samples)`, then call `tm.transcribe_with_hook(processed_samples, ...)` (router.rs:331, transcribe.rs:432). The same padded buffer is passed to the engine-agnostic dispatch at `transcription.rs:1600-1718`, where the `match &mut engine` branches (`TranscribeCpp`, `Parakeet`, `Moonshine`, `MoonshineStreaming`, `SenseVoice`, `GigaAM`, `Canary`, `Cohere`) each receive the same `&audio` with **no further padding or silence trimming per engine** (transcription.rs:1638-1716).

**Conclusion:** Every engine family receives the leading 3-second zero pad. The pad's stated rationale ("reduce **Whisper** hallucinations") is engine-specific, but its application is engine-blind.

---

## 2. Stated purpose — from the source comment and git history

Current comment at **`audio.rs:1776-1784`**:

> Pad short audio with silence at the BEGINNING to reduce Whisper hallucinations and prevent clipping of the first words.
> Very short clips (< 3s) padded with silence at the end would cause Whisper to miss the first words. By padding at the start, we give Whisper context and ensure the captured speech timing is preserved.
> A 3-second minimum gives Whisper enough context to produce a good transcription without hallucinating. The VAD-based `trim_trailing_silence` in the transcription pipeline further cleans up any trailing silence.

**Note on the last sentence:** the claim that "`trim_trailing_silence` in the transcription pipeline further cleans up trailing silence" is **factually false in the current code**. `trim_trailing_silence` is defined at `audio_toolkit/vad/mod.rs:45` and re-exported at `audio_toolkit/mod.rs:24`, but a full grep shows it is **never called** anywhere (the only reference besides its definition is the comment at `audio.rs:1782`). So there is **no downstream trailing-silence cleanup** in the transcription pipeline — the pad is the _only_ silence-injection mechanism.

### Git history shows the pad evolved through three distinct rationales (from the earliest):

| Commit                    | Date       | Change                                         | Stated purpose                                                                                                    |
| ------------------------- | ---------- | ---------------------------------------------- | ----------------------------------------------------------------------------------------------------------------- |
| `a60cf578` (earliest pad) | –          | initial pad (`if s_len < WHISPER_SAMPLE_RATE`) | "Pad if very short"                                                                                               |
| `4a7f5e12`                | 2025-02-17 | 1s → 1.025s                                    | "Add a little extra padding to avoid some failures"                                                               |
| `6a7f0189`                | 2026-05-13 | 1.25s → **3s**                                 | "Increase minimum audio padding from 1.25s to 3s to reduce **Whisper** hallucinations"                            |
| `ce8745ee`                | 2026-06-10 | end-pad → **leading**-pad                      | "FIX: prevent audio clipping at beginning/end of short sentences" (switched from appending to prepending silence) |
| `df65f4ac`                | 2026-07-27 | separate raw vs processed                      | "FEAT: save raw + VAD-filtered audio separately for WAV recordings"                                               |

The 3-second value and the "reduce Whisper hallucinations" framing originate specifically in commit **`6a7f0189`** (2026-05-13). The rationale was authored with Whisper in mind — the well-documented Whisper failure mode of hallucinating repetitive/silence-degenerate text on very short or silence-heavy input.

---

## 3. Existing issues/comments/TODOs about the pad causing harm — **None found.**

- **No TODO / FIXME / XXX / HACK** mentions the pad in `audio.rs` or `transcription.rs`.
- The only "hallucination" mentions in the entire `src/` tree are the two lines of the pad comment (`audio.rs:1777`, `audio.rs:1782`).
- Grep of `README.md`, `CHANGELOG.md`, and `docs/` for "hallucinat / 3 second / pad" returned **no** related notes.
- There is **no code** that trims the padded silence before the engine sees it, and **no code or comment** documenting the pad as harmful or hurtful to quality for any model.

The notable **gap**: while the pad's original rationale is Whisper-specific, **no code or comment acknowledges that applying 3s of silence to non-Whisper engines (Parakeet, Moonshine, SenseVoice, GigaAM, Canary, Cohere) may be unnecessary or harmful.** That is a documentation/design omission, not a flagged defect.

---

## 4. Does the pad length vary? — **No; fixed at 3 seconds for all configurations.**

- `min_samples = WHISPER_SAMPLE_RATE * 3` (audio.rs:1786) where `WHISPER_SAMPLE_RATE = 16000` (audio.rs:116) → exactly **48,000 samples = 3.0 s**.
- It is **hard-coded**. There is no settings field, no `const`, and no per-configuration override.
- The only related **settings** are `extra_recording_buffer_ms` and `pre_recording_buffer_ms` (`settings/types.rs:659,661`, both default **0** at `settings/defaults.rs:148-149`). These affect the **recorder's capture length**, not the pad. `pre_recording_buffer_ms` prepends _real mic audio_ (`recorder.rs:1466-1476`), not synthetic zeros — distinct from this pad.

**So: 3.0 s of leading zeros, for every short clip, on every engine, regardless of model family, language, or configuration.**

---

## 5. Model-specific handling of leading/trailing silence

There is **no model-specific silence handling** anywhere in the transcription pipeline:

- **Batch path** (all engines): each engine receives the same `&audio` and the same already-padded buffer. `transcription.rs:1600-1718` shows no engine-specific padding/trimming before `session.run()`, `parakeet.transcribe_with()`, `moonshine.transcribe()`, etc.
- **Streaming/live-captions path**: the `StreamRouter` (`transcription.rs:128-184`) just forwards PCM frames via `StreamRouter::feed` with **no padding**. The streaming callback at `audio.rs:342` calls `tm.transcribe(samples)` directly — **no 3s pad**. `finalize_stream` (transcription.rs:1295) flushes buffered audio. So live captions (which is where streaming-capable models run) never see the pad; the pad only hits the final batch transcription.

**Which models does the pad actually touch?**

- **Whisper family** (TranscribeCpp, `model.rs:535-689`) — the pad was designed for these. Rationale is positive (Whisper hallucinates on short/silence-poor input).
- **Parakeet V2/V3** (`model.rs:698-766`) — a separate engine, no evidence it needs the pad, no code path that benefits/trims.
- **Moonshine / MoonshineStreaming** (`model.rs:794,828,862,896`) — same, engine-blind.
- **SenseVoice** (`model.rs:936`), **GigaAM** (`model.rs:972`), **Canary** (`model.rs:1012,1056`), **Cohere** (`model.rs:1097`) — same, engine-blind.

Because the pad is applied before the engine-agnostic dispatch, all of these non-Whisper engines are padded identically — with **no code or comment establishing whether that is beneficial or harmful** for each. The historical rationale only ever mentions Whisper.

---

## 6. Any code/comment mentioning hallucination, silence, padding, or trimming?

| Term                              | Location(s)                                                                   | Meaning                                                                                                                   |
| --------------------------------- | ----------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------- |
| "hallucinations / hallucinating"  | `audio.rs:1777,1782` (pad comment)                                            | The Whisper-hallucination rationale                                                                                       |
| "Pad / padding"                   | `audio.rs:1776-1795`, `resampler.rs:70,78`, `vad/mod.rs:119`                  | The leading pad; resampler zero-pad for chunk alignment; VAD hangover pad                                                 |
| `trim_trailing_silence`           | `vad/mod.rs:34-136` (**definition only**), `audio_toolkit/mod.rs:24` (export) | Defined but **never called** — the comment's claimed "cleanup" is absent                                                  |
| VAD leading/trailing buffer       | `vad/mod.rs:51-65`                                                            | The VAD `trim_trailing_silence` function keeps 300ms hangover after last speech + 200ms leading buffer (but it is unused) |
| "min_samples / 3 seconds minimum" | `audio.rs:1786-1788`                                                          | The pad constant                                                                                                          |

**Key contradiction:** `trim_trailing_silence` (vad/mod.rs:45) does exist and implements a proper VAD-based trim (keeps 200ms leading + 300ms hangover), but it is **dead code** — never invoked. The pad comment at `audio.rs:1782` asserts this cleanup runs "in the transcription pipeline", which is not true in the current codebase.

---

## Summary & assessment

1. **Pad is engine-blind (applies to every model).** The 3s leading silence is injected in the low-level recorder layer, before model dispatch, with no model/capability gate.
2. **Purpose** is explicitly Whisper-motivated (reduce Whisper hallucinations / avoid first-word clipping), hardened over 4 commits since 2025.
3. **No open issues/TODOs/comments** flag the pad as harmful. But the pad's **claimed VAD trailing-silence cleanup does not actually run** (`trim_trailing_silence` is dead code).
4. **Pad length is fixed at 3.0 s** for every configuration; the recorder's `pre_buffer_ms`/`extra_buffer_ms` settings are unrelated.
5. **No model-specific silence paths** exist — Whisper, Parakeet, Moonshine, SenseVoice, GigaAM, Canary, and Cohere all receive identical padding in batch mode; the streaming/live path bypasses the pad entirely.
6. **Net verdict on evidence:** the pad was calibrated for Whisper. There is **no code/comment/evidence either that it helps or hurts the non-Whisper engines** — this is an unexamined side effect. Because the rationale is Whisper-only while application is engine-blind, Parakeet and the ONNX families may be receiving 3s of leading silence they neither need nor were designed for. Since `trim_trailing_silence` is never applied, any trailing or leading silence the pad introduces is handed to the engines verbatim.

**Relevant files:** `src-tauri/src/managers/audio.rs:1776-1795` (pad), `src-tauri/src/managers/transcription.rs:1600-1718` (engine-agnostic dispatch), `src-tauri/src/audio_toolkit/vad/mod.rs:34-136` (dead `trim_trailing_silence`), `src-tauri/src/managers/model.rs:27-39,706-1097` (EngineType + model catalog), `src-tauri/src/audio_toolkit/audio/recorder.rs:1466-1476` (real-mic pre-buffer, unrelated).
