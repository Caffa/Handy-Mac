# Transcription Reliability Improvements

## Overview

This document describes the robustness improvements made to Handy's transcription system, particularly for live captions mode. The changes ensure that audio is never lost, errors are properly classified, and failed transcriptions can be retried automatically or manually.

## Problem Statement

When using Handy's live captions mode, users experienced:
1. **Lost audio**: If transcription failed, the recording was lost
2. **Silent failures**: Errors would slip through without proper notification
3. **Race conditions**: Streaming transcription could conflict with final transcription
4. **No recovery path**: Failed transcriptions couldn't be retried

## Solutions Implemented

### 1. Audio Preservation (Always Save First)

**What changed**: Audio is now saved to disk BEFORE attempting transcription, guaranteeing no recording is ever lost.

**Files changed**:
- `src-tauri/src/actions.rs`: Modified error handling to always save WAV before transcription
- `src-tauri/src/managers/transcription_retry.rs`: New module for retry management

**How it works**:
```
Before:
  Record → Transcribe → Save (if success)

After:
  Record → Save WAV → Transcribe → Update history (if success) or Add to retry queue (if failure)
```

### 2. Transcription Retry Queue

**New module**: `src-tauri/src/managers/transcription_retry.rs`

A persistent queue that tracks failed transcriptions with:
- **Disk persistence**: Queue survives app restarts (stored as JSON in app data dir)
- **Automatic retry**: Exponential backoff (5s → 10s → 20s → 40s...)
- **Manual retry**: UI support for retrying specific entries
- **Fallback models**: Automatically tries backup models if primary fails

**RetryableTranscription fields**:
```rust
pub struct RetryableTranscription {
    pub id: String,                          // Unique identifier
    pub audio_path: PathBuf,                 // Where audio is saved
    pub timestamp: i64,                      // When recorded
    pub model_id: String,                    // Primary model
    pub fallback_models: Vec<String>,        // Backup models to try
    pub current_model_index: usize,          // Which model to use next
    pub retry_count: u32,                     // Attempts made
    pub max_retries: u32,                     // Max attempts (default 3)
    pub last_failure: TranscriptionFailure,   // Why it failed
    pub next_retry_at: Option<i64>,          // When to retry (exponential backoff)
    pub history_entry_id: Option<i64>,        // Linked history entry
}
```

### 3. Error Classification System

**New enum**: `TranscriptionFailure`

Different failures require different retry strategies:

```rust
pub enum TranscriptionFailure {
    // Might succeed on retry with different model
    ModelLoadFailure { model_id, error },
    InferenceFailure { model_id, error },
    
    // Unlikely to succeed on retry (engine crash)
    EnginePanic { model_id },
    
    // Resource issues (might succeed after wait)
    Timeout { model_id, duration_secs },
    ResourceUnavailable { resource, error },
    
    // Not an error - no retry needed
    SilentAudio,
    
    // Unknown error - retry with caution
    Unknown { error },
}

impl TranscriptionFailure {
    pub fn should_auto_retry(&self) -> bool {
        match self {
            Self::InferenceFailure { .. } => true,  // ✅ Retry
            Self::Timeout { .. } => true,           // ✅ Retry
            Self::ResourceUnavailable { .. } => true, // ✅ Retry
            Self::Unknown { .. } => true,            // ✅ Retry
            Self::ModelLoadFailure { .. } => false, // ❌ No retry
            Self::EnginePanic { .. } => false,      // ❌ No retry
            Self::SilentAudio => false,             // ❌ No retry
        }
    }
    
    pub fn should_try_fallback_model(&self) -> bool {
        match self {
            Self::InferenceFailure { .. } => true,
            Self::Timeout { .. } => true,
            Self::ModelLoadFailure { .. } => true,
            Self::Unknown { .. } => true,
            Self::ResourceUnavailable { .. } => false, // Not model's fault
            Self::EnginePanic { .. } => false,         // Engine unstable
            Self::SilentAudio => false,
        }
    }
}
```

### 4. Fallback Model Chain

When hybrid mode is enabled, the retry system automatically tries alternative models:

```rust
// In actions.rs error handling:
let fallback_models = {
    let settings = get_settings(&ah);
    let mut models = Vec::new();
    if settings.hybrid_mode_enabled {
        if let Some(short_model) = &settings.hybrid_short_audio_model {
            if short_model != &settings.selected_model {
                models.push(short_model.clone());
            }
        }
        if let Some(long_model) = &settings.hybrid_long_audio_model {
            if long_model != &settings.selected_model 
                && !models.contains(long_model) {
                models.push(long_model.clone());
            }
        }
    }
    models
};

retry_queue.add_failed_transcription(
    wav_path,
    model_id,
    fallback_models,  // Will try these if primary fails
    failure,
    post_process,
    None,
    history_entry_id,
)?;
```

### 5. Streaming Transcription Cancellation

**Problem**: Live captions streaming transcription runs every 2.5 seconds during recording. If the user stops recording while streaming transcription is in progress, the final transcription would find the engine locked, fail, and save an empty history entry.

**Solution**: Added cancellation flag to `TranscriptionManager`:

```rust
pub struct TranscriptionManager {
    // ... existing fields ...
    
    /// Flag to cancel streaming transcription when recording stops.
    /// When set, the streaming callback should skip transcription and return early.
    cancel_streaming: Arc<AtomicBool>,
}

impl TranscriptionManager {
    /// Request cancellation of streaming transcription.
    pub fn cancel_streaming(&self) {
        self.cancel_streaming.store(true, Ordering::Release);
    }
    
    /// Clear cancellation flag for new recording.
    pub fn clear_streaming_cancel(&self) {
        self.cancel_streaming.store(false, Ordering::Release);
    }
    
    /// Check if cancelled.
    pub fn is_streaming_cancelled(&self) -> bool {
        self.cancel_streaming.load(Ordering::Acquire)
    }
}
```

**In streaming callback** (audio.rs):
```rust
// Check if streaming was cancelled (recording stopped)
if tm.is_streaming_cancelled() {
    debug!("Skipping streaming transcription - cancellation requested");
    return;
}

// Transcribe the audio samples
match tm.transcribe(samples) {
    Ok(result) if !result.text.is_empty() => {
        // Check again after transcription in case cancelled mid-work
        if tm.is_streaming_cancelled() {
            debug!("Discarding streaming transcription result - cancelled");
            return;
        }
        // Emit partial transcription event to frontend
        app_handle.emit("partial-transcription", &result.text);
    }
    // ...
}
```

**In actions.rs**:
```rust
fn start(&self, app: &AppHandle, ...) {
    // Clear cancellation flag when starting new recording
    let tm = app.state::<Arc<TranscriptionManager>>();
    tm.clear_streaming_cancel();
    // ... rest of recording start logic
}

fn stop(&self, app: &AppHandle, ...) {
    // Cancel streaming when recording stops
    let tm = app.state::<Arc<TranscriptionManager>>();
    tm.cancel_streaming();
    // ... rest of recording stop logic
}
```

### 6. History Entry Integration

Failed transcriptions are saved to history with empty text and the history entry ID is stored in the retry queue:

```rust
// Save entry with empty text so user can retry
if wav_saved {
    let entry_result = hm.save_entry(
        file_name,
        String::new(),  // Empty text
        post_process,
        None,
        None,
        None,
        false,
    );
    
    // Track for retry
    if let Ok(entry) = entry_result {
        retry_queue.add_failed_transcription(
            wav_path,
            model_id,
            fallback_models,
            failure,
            post_process,
            None,
            Some(entry.id),  // Link to history
        )?;
    }
}
```

## TypeScript API

New commands exposed to frontend:

```typescript
// Get all pending retry entries
const entries = await commands.getRetryQueue();

// Manually trigger retry for specific entry
await commands.retryTranscription(entryId);

// Remove entry from queue
await commands.removeFromRetryQueue(entryId);

// Clear all pending retries
await commands.clearRetryQueue();

// Get count of pending retries
const count = await commands.getRetryQueueCount();
```

## Future Enhancements

### Background Retry Worker (Low Priority)

A background worker that periodically checks the retry queue and processes entries:

```rust
// Pseudocode for future implementation
fn start_retry_worker(app: AppHandle) {
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_secs(60));
            
            if let Some(queue) = app.try_state::<Arc<TranscriptionRetryQueue>>() {
                if let Some(entry) = queue.get_next_retry() {
                    if entry.is_ready() {
                        // Load audio, retry transcription
                        // Update history on success
                        // Mark failed on error
                    }
                }
            }
        }
    });
}
```

### Recovery UI (Medium Priority)

Add a section in History Settings showing failed transcriptions:
- List of pending retries with timestamps
- Retry button for individual entries
- "Retry All" button
- Ability to delete unwanted entries

### Audio Validation (Medium Priority)

Enhance audio validation to distinguish:
- **Silent audio**: Detected by max_level < threshold, no retry needed
- **Corrupted audio**: WAV file doesn't match expected format
- **Failed transcription**: Audio is valid but model failed

## Testing Recommendations

1. **Test failure scenarios**:
   - Force transcription failure with env var `HANDY_FORCE_TRANSCRIPTION_FAILURE=1`
   - Verify audio is saved and entry appears in retry queue
   - Retry from queue and verify success

2. **Test live captions cancellation**:
   - Enable live captions
   - Start recording, let streaming transcription run
   - Stop recording quickly
   - Verify final transcription succeeds (no race condition)

3. **Test app restart recovery**:
   - Start transcription, force failure
   - Quit app before retry
   - Restart app
   - Verify retry queue is restored from disk

## Files Changed

### New Files
- `src-tauri/src/managers/transcription_retry.rs` - Retry queue manager
- `src-tauri/src/commands/transcription_retry.rs` - Tauri commands

### Modified Files
- `src-tauri/src/lib.rs` - Register retry queue manager and commands
- `src-tauri/src/managers/mod.rs` - Export retry module
- `src-tauri/src/commands/mod.rs` - Export retry commands
- `src-tauri/src/managers/transcription.rs` - Add cancellation flag
- `src-tauri/src/managers/audio.rs` - Check cancellation in streaming callback
- `src-tauri/src/actions.rs` - Use retry queue on failure, set/clear cancellation
- `src-tauri/Cargo.toml` - Add uuid dependency
- `src/bindings.ts` - Auto-generated TypeScript types

## Commits

1. **FEAT: Add transcription retry queue with automatic fallback model support**
   - Created TranscriptionRetryQueue manager
   - Added TranscriptionFailure enum
   - Integrated into error handling

2. **FEAT: Add streaming transcription cancellation for live captions**
   - Added cancel_streaming flag to TranscriptionManager
   - Check cancellation before/after streaming transcription
   - Set/clear cancellation on recording start/stop

3. **FEAT: Regenerate TypeScript bindings for retry queue API**
   - Auto-generated types for frontend integration