# Handy Codebase Research Report: Audio Processing Analysis

## Project Overview

**What is Handy?**

Handy is a **free, open source, cross-platform desktop speech-to-text application** built with Tauri (Rust backend + React/TypeScript frontend). It provides offline speech transcription using local Whisper and Parakeet models.

### Key Features:
- **Offline Speech Recognition**: Uses Whisper models or Parakeet V3 for transcription
- **Voice Activity Detection (VAD)**: Silero VAD for filtering silence
- **Audio Feedback**: Configurable sound notifications for start/stop recording
- **Bluetooth Audio Support**: Special handling to prevent audio dropouts with BT headsets
- **Multiple Recording Modes**: Always-on vs On-demand microphone modes

### Technology Stack:
- **Frontend**: React + TypeScript + Tailwind CSS
- **Backend**: Rust with Tauri 2.x
- **Audio I/O**: CPAL (Cross-Platform Audio Library)
- **Speech Recognition**: whisper-rs, transcribe-rs
- **Audio Playback**: rodio
- **VAD**: Silero VAD via vad-rs

---

## Audio Processing Code Locations

### 1. Audio Feedback System
**File**: `/src-tauri/src/audio_feedback.rs`

The audio feedback system handles playback of start/stop recording sounds using the `rodio` library.

```rust
// Key structures:
pub enum SoundType {
    Start,
    Stop,
}

// Volume control (line 92-139):
pub fn play_audio_file(
    path: &std::path::Path,
    selected_device: Option<String>,
    volume: f32,  // <-- Volume parameter from settings
) -> Result<(), Box<dyn std::error::Error>> {
    // ... device selection logic ...
    let stream_handle = stream_builder.open_stream()?;
    let mixer = stream_handle.mixer();
    let sink = rodio::play(mixer, buf_reader)?;
    sink.set_volume(volume);  // <-- Volume applied here
    sink.sleep_until_end();
    Ok(())
}
```

**Relevant Settings** (from `/src-tauri/src/settings.rs`):
- `audio_feedback: bool` - Enable/disable audio feedback
- `audio_feedback_volume: f32` - Volume level (default: 0.5)
- `sound_theme: SoundTheme` - Theme selection (Custom, Modern, Classic, etc.)

### 2. Audio Recording Manager
**File**: `/src-tauri/src/managers/audio.rs` (1078 lines)

This is the main audio management module that handles:
- Microphone stream lifecycle
- Recording state management
- Bluetooth keep-alive functionality
- USB watchdog for dead microphone recovery
- Mute while recording

**Key Methods:**

```rust
// Lines 492-537: Start microphone stream with retry logic
pub fn start_microphone_stream(&self) -> Result<(), anyhow::Error>

// Lines 630-655: Stop microphone stream
pub fn stop_microphone_stream(&self)

// Lines 692-807: Start recording with stream health checks
pub fn try_start_recording(&self, binding_id: &str) -> Result<(), String>

// Lines 857-987: Stop recording and return audio samples
pub fn stop_recording(&self, binding_id: &str) -> Option<Vec<f32>>

// Lines 36-123: System volume mute control
fn set_mute(mute: bool)
```

**Critical Audio Processing Code (Lines 969-983) - Short Audio Padding:**

```rust
// Pad short audio to reduce Whisper hallucinations.
// Very short clips (< 3s) padded with silence cause Whisper to
// hallucinate repetitive text. A 3-second minimum gives Whisper
// enough context to produce a good transcription without
// hallucinating. The VAD-based trim_trailing_silence in the
// transcription pipeline further cleans up any trailing silence.
let s_len = samples.len();
let min_samples = WHISPER_SAMPLE_RATE * 3; // 3 seconds minimum
if s_len > 0 && s_len < min_samples {
    let mut padded = samples;
    padded.resize(min_samples, 0.0);
    Some(padded)
} else {
    Some(samples)
}
```

### 3. Audio Recorder (Low-Level)
**File**: `/src-tauri/src/audio_toolkit/audio/recorder.rs` (917 lines)

Core audio capture implementation using CPAL.

**Key Features:**
- Multi-threaded audio capture with dedicated worker thread
- Resampling from device sample rate to 16kHz (Whisper sample rate)
- Voice Activity Detection integration
- Audio level tracking for visualizer
- Smart-stop (volume-aware trailing buffer)

**Smart-Stop Implementation (Lines 321-338):**

```rust
/// Volume-aware stop: continues recording for up to `max_buffer_ms`
/// after the hotkey is released, but stops early when the microphone
/// level drops below the noise floor for a sustained period.
pub fn smart_stop(&self, max_buffer_ms: u64) -> Result<Vec<f32>, Box<dyn std::error::Error>> {
    let (resp_tx, resp_rx) = mpsc::channel();
    if let Some(tx) = &self.cmd_tx {
        tx.send(Cmd::SmartStop {
            max_buffer_ms,
            reply_tx: resp_tx,
        })?;
    }
    Ok(resp_rx.recv()?) // wait for the samples
}
```

**Smart-Stop Tuning Constants (Lines 63-90):**

```rust
// SILENCE_RMS_MULTIPLIER: A frame is considered "voice" when its RMS
//   exceeds `noise_floor * SILENCE_RMS_MULTIPLIER`.  3× means that
//   even modest speech (3× louder than background noise) keeps the
//   buffer open, while pure silence or steady ambient noise closes it.
const SILENCE_RMS_MULTIPLIER: f32 = 3.0;

// SILENCE_THRESHOLD_MS: How long the volume must stay below the
//   threshold before we decide the user has finished speaking.
//   300 ms ≈ the length of a very short pause; it avoids cutting off
//   natural micro-pauses in continuous speech.
const SILENCE_THRESHOLD_MS: u64 = 300;

// MIN_BUFFER_MS: The shortest time we *always* wait before considering
//   an early stop.  Guarantees we capture trailing consonants or a
//   brief final syllable that might dip below threshold for a few ms.
const MIN_BUFFER_MS: u64 = 100;
```

### 4. VAD (Voice Activity Detection) Module
**File**: `/src-tauri/src/audio_toolkit/vad/mod.rs` (111 lines)

**Key Function: trim_trailing_silence (Lines 42-110)**

This function trims trailing silence from audio before transcription:

```rust
pub fn trim_trailing_silence(audio: &[f32], vad_path: &str, threshold: f32) -> Vec<f32> {
    const FRAME_MS: u32 = 30;
    const FRAME_SAMPLES: usize = (constants::WHISPER_SAMPLE_RATE as usize * FRAME_MS as usize) / 1000;
    // Keep 150ms of audio after the last detected speech frame
    // to avoid clipping final consonants/tails of words
    const HANGOVER_FRAMES: usize = 5;
    const HANGOVER_SAMPLES: usize = HANGOVER_FRAMES * FRAME_SAMPLES;
    
    // ... VAD processing logic ...
    
    // Pad the cut point with a small hangover to avoid clipping
    let trimmed_len = (last_speech_frame_end + HANGOVER_SAMPLES).min(audio.len());
    audio[..trimmed_len].to_vec()
}
```

**Usage in Transcription** (`/src-tauri/src/managers/transcription.rs`, Lines 568-582):

```rust
// Trim trailing silence from audio before transcription.
// Critical for Whisper (hallucinates on silence) AND for autoregressive
// transducer models (Parakeet TDT) whose decoder free-runs language
// model continuations into trailing silence.
let audio = match self.app_handle.path().resolve(
    "resources/models/silero_vad_v4.onnx",
    tauri::path::BaseDirectory::Resource,
) {
    Ok(vad_path) => {
        let path_str = vad_path.to_str().unwrap_or("");
        // Use 0.5 threshold to match Python implementation.
        // Lower values (0.3) were too aggressive and trimmed soft trailing words.
        trim_trailing_silence(&audio, path_str, 0.5)
    }
    // ...
};
```

### 5. Audio Resampler
**File**: `/src-tauri/src/audio_toolkit/audio/resampler.rs` (99 lines)

Uses `rubato` library for resampling audio from device sample rate to 16kHz.

```rust
pub struct FrameResampler {
    resampler: Option<FftFixedIn<f32>>,
    chunk_in: usize,
    in_buf: Vec<f32>,
    frame_samples: usize,
    pending: Vec<f32>,
}

// Constant chunk size for resampling
const RESAMPLER_CHUNK_SIZE: usize = 1024;
```

### 6. Transcription Manager
**File**: `/src-tauri/src/managers/transcription.rs` (1184+ lines)

Handles transcription using various engines (Whisper, Parakeet, Moonshine, etc.)

**Key Code - Hybrid Mode Audio Length Handling (Lines 515-538):**

```rust
// Determine which model to use (hybrid mode or standard).
// Hybrid mode picks a different model based on audio length:
// short audio uses the "short audio model", long audio uses the "long audio model".
let effective_model_id = if settings.hybrid_mode_enabled {
    let audio_duration_secs = audio.len() as f64 / 16000.0;
    if audio_duration_secs < settings.hybrid_threshold_secs {
        debug!(
            "Hybrid mode: audio is {:.1}s (< {}s threshold), using short audio model",
            audio_duration_secs, settings.hybrid_threshold_secs
        );
        settings.hybrid_short_audio_model.clone()
            .unwrap_or(settings.selected_model.clone())
    } else {
        // ... long audio model ...
    }
} else {
    settings.selected_model.clone()
};
```

---

## Potential Issues Related to Audio Clipping/Timing

### 1. **Short Audio Padding (Lines 969-983 in audio.rs)**

**Issue**: Audio shorter than 3 seconds is padded with silence at the END.

```rust
let min_samples = WHISPER_SAMPLE_RATE * 3; // 3 seconds minimum
if s_len > 0 && s_len < min_samples {
    let mut padded = samples;
    padded.resize(min_samples, 0.0);  // <-- Pads with zeros (silence) at the END
    Some(padded)
}
```

**Potential Problem**: This padding happens AFTER recording stops. If the issue is that the BEGINNING of sentences is being clipped, this code is not the cause - but it might mask the issue by adding silence at the end.

### 2. **Trailing Silence Trimming (VAD)**

**Location**: `/src-tauri/src/audio_toolkit/vad/mod.rs`, Lines 42-110

The `trim_trailing_silence` function:
- Processes audio in 30ms frames
- Keeps 150ms (5 frames) of "hangover" after last detected speech
- Uses a threshold of 0.5 for speech detection

**Potential Issue**: If VAD is too aggressive (threshold too high), it might:
- Trim actual speech at the end of sentences
- Cut off trailing words or final consonants

**Current Settings**:
- Frame size: 30ms
- Hangover: 150ms (5 frames)
- Threshold: 0.5

### 3. **Smart-Stop Buffer Timing**

**Location**: `/src-tauri/src/audio_toolkit/audio/recorder.rs`, Lines 63-90

**Configuration**:
- `SILENCE_RMS_MULTIPLIER`: 3.0x noise floor
- `SILENCE_THRESHOLD_MS`: 300ms of silence before stopping
- `MIN_BUFFER_MS`: 100ms minimum buffer

**Potential Issue**: If the user's voice drops below the threshold during speech (e.g., trailing off at the end of a sentence), the smart-stop may cut off the last words.

### 4. **Audio Stream Initialization Timing**

**Location**: `/src-tauri/src/managers/audio.rs`, Lines 579-628

```rust
fn start_microphone_stream_inner(&self) -> Result<(), anyhow::Error> {
    // ...
    // This timing covers through cpal's stream.play() returning — i.e. the
    // point cpal surfaces as "stream running." It does NOT guarantee the
    // host audio device is producing samples yet; the first input callback
    // fires asynchronously one buffer period later (hardware dependent,
    // typically ~10–200ms on macOS, longer on Bluetooth/USB).
    info!(
        "Microphone stream initialized in {:?}",
        start_time.elapsed()
    );
    Ok(())
}
```

**Potential Issue**: There's a 200ms stabilization delay before recording starts (Line 779), but this is AFTER the stream is opened. The actual audio capture may start with a delay.

### 5. **Recording Start Sequence**

**Location**: `/src-tauri/src/actions.rs`, Lines 486-510

In on-demand mode:
```rust
// Small delay to ensure microphone stream is active
std::thread::sleep(std::time::Duration::from_millis(100));
```

This 100ms delay occurs AFTER `try_start_recording()` is called, meaning the recording may start capturing audio before the stream is fully stable.

### 6. **Frame Resampler Buffer Handling**

**Location**: `/src-tauri/src/audio_toolkit/audio/resampler.rs`, Lines 66-84

```rust
pub fn finish(&mut self, mut emit: impl FnMut(&[f32])) {
    // Process any remaining input samples
    if let Some(ref mut resampler) = self.resampler {
        if !self.in_buf.is_empty() {
            // Pad with zeros to reach chunk size
            self.in_buf.resize(self.chunk_in, 0.0);
            // ...
        }
    }
    // Emit any remaining pending frame (padded with zeros)
    if !self.pending.is_empty() {
        self.pending.resize(self.frame_samples, 0.0);
        emit(&self.pending);
        self.pending.clear();
    }
}
```

**Potential Issue**: The resampler pads incomplete frames with zeros at the end during `finish()`. This could potentially add silence at the end of recordings.

---

## Configuration Settings Affecting Audio Behavior

### From `/src-tauri/src/settings.rs`:

| Setting | Type | Default | Description |
|---------|------|---------|-------------|
| `audio_feedback` | bool | false | Enable audio feedback sounds |
| `audio_feedback_volume` | f32 | 0.5 | Volume level for feedback sounds |
| `sound_theme` | SoundTheme | Modern | Theme for start/stop sounds |
| `always_on_microphone` | bool | false | Keep mic stream always open |
| `extra_recording_buffer_ms` | u64 | 0 | Extra buffer time after hotkey release |
| `lazy_stream_close` | bool | false | Delay mic stream close after recording |
| `hybrid_mode_enabled` | bool | false | Use different models for short/long audio |
| `hybrid_threshold_secs` | f64 | 10.0 | Threshold for short/long audio classification |
| `mute_while_recording` | bool | false | Mute system output during recording |

---

## Summary of Audio Flow

```
1. User presses shortcut
   ↓
2. try_start_recording() called
   ↓
3. Microphone stream opened (if not already)
   ↓
4. Audio feedback plays (Start sound)
   ↓
5. Recording begins (VAD filters incoming audio)
   ↓
6. User releases shortcut
   ↓
7. stop_recording() called
   ↓
8. Smart-stop buffer (if enabled) or immediate stop
   ↓
9. Audio feedback plays (Stop sound)
   ↓
10. Audio padded to 3 seconds if shorter (with silence at END)
   ↓
11. trim_trailing_silence() removes trailing silence
   ↓
12. Audio sent to transcription engine
```

---

## Key Files Summary

| File | Purpose | Lines |
|------|---------|-------|
| `audio_feedback.rs` | Audio playback for feedback sounds | 142 |
| `managers/audio.rs` | Audio recording manager | 1078 |
| `audio_toolkit/audio/recorder.rs` | Low-level audio capture | 917 |
| `audio_toolkit/vad/mod.rs` | VAD and silence trimming | 111 |
| `audio_toolkit/audio/resampler.rs` | Audio resampling | 99 |
| `managers/transcription.rs` | Transcription orchestration | 1184+ |
| `actions.rs` | Action handlers for shortcuts | 1204+ |
| `settings.rs` | Configuration settings | 1100+ |

---

## Observations for Sentence Clipping Investigation

1. **Padding is at the END**: The 3-second minimum padding adds silence at the END of short audio, which would NOT cause beginning-of-sentence clipping.

2. **No Leading Silence Trimming**: The codebase only trims TRAILING silence, not leading silence. There's no equivalent `trim_leading_silence` function.

3. **VAD may be the culprit**: If speech is being clipped, the VAD threshold (0.5) or the smart-stop logic might be too aggressive, cutting off the beginning of quiet speech.

4. **Stream initialization delay**: The 200ms stabilization delay and the asynchronous nature of CPAL callbacks could cause the first few milliseconds of audio to be missed.

5. **Frame resampling**: The resampler processes audio in 1024-sample chunks, which could potentially cause boundary issues at the start/end of recordings.
