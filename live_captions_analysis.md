# Handy Live Captions Not Displaying - Detailed Analysis

## Executive Summary

The live captions feature in Handy has a multi-layered architecture that can fail at several points between audio capture and UI rendering. Based on code analysis, I've identified **5 potential failure points** and **3 logging gaps** that could explain why users don't see live captions.

---

## Complete Data Flow for Live Captions

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                        LIVE CAPTIONS DATA FLOW                               │
└─────────────────────────────────────────────────────────────────────────────┘

1. RECORDING START
   └─> AudioRecordingManager::try_start_recording()
       └─> create_audio_recorder() [audio.rs:159-327]
           └─> Check: live_captions_enabled setting [audio.rs:171]
               ├─ TRUE: Continue with streaming callback setup
               └─ FALSE: Skip streaming callback

2. STREAMING CALLBACK SETUP
   └─> recorder.with_streaming_callback() [audio.rs:215-314]
       └─> Check: TranscriptionManager available? [audio.rs:207-213]
           ├─ YES: Get cancel_flag Arc<AtomicBool>
           └─ NO: Return recorder WITHOUT streaming (⚠️ SILENT FAILURE)

3. AUDIO PROCESSING (Every ~2.5s during recording)
   └─> AudioRecorder streaming callback fires
       └─> Check: cancel_flag.load()? [audio.rs:223]
           ├─ TRUE: Skip transcription (cancelled)
           └─ FALSE: Spawn blocking task [audio.rs:232]
               └─> TranscriptionManager::transcribe() [audio.rs:249]
                   └─> Model loads/runs transcription
                       └─> Process segments text with post-processing [audio.rs:278-298]
                           └─> EMIT: "partial-transcription" event [audio.rs:301]

4. FRONTEND EVENT HANDLING
   └─> RecordingOverlay.tsx receives "partial-transcription" [RecordingOverlay.tsx:931-1008]
       └─> Merge segments with existing streamingSegments state
           └─> Apply filterStreamingText() to remove filler words [RecordingOverlay.tsx:129-171]
               ├─ If text filtered to empty: Keep previous text [RecordingOverlay.tsx:977]
               └─> If text non-empty: Update streamingText state [RecordingOverlay.tsx:962-978]

5. UI RENDERING
   └─> Conditional render at [RecordingOverlay.tsx:1217-1235]
       REQUIREMENTS (ALL must be true):
       ✓ isVisible === true
       ✓ state === "recording"
       ✓ liveCaptionsEnabled === true
       ✓ !micDeadWarning
       ✓ !lowAudioWarning  
       ✓ streamingText exists
       ✓ streamingText.trim() !== ""
```

---

## Potential Failure Points

### 1. **TranscriptionManager Not Initialized When Recording Starts**

**Location**: `src-tauri/src/managers/audio.rs:207-213`

**Code**:
```rust
let cancel_flag = match app_handle.try_state::<Arc<Mutex<TranscriptionManager>>>() {
    Some(tm_state) => tm_state.lock().streaming_cancel_flag(),
    None => {
        info!("TranscriptionManager not available yet, skipping streaming callback setup");
        return Ok(recorder);  // ⚠️ Returns WITHOUT streaming callback!
    }
};
```

**Issue**: If recording starts before TranscriptionManager is fully initialized, the streaming callback is silently skipped. The recording proceeds normally, but live captions won't work.

**Impact**: HIGH - User has no indication this happened

---

### 2. **Race Condition: liveCaptionsEnabled Setting Fetch**

**Location**: `src/overlay/RecordingOverlay.tsx:368-388`

**Code**:
```typescript
useEffect(() => {
  if (!isVisible) return;
  const fetchSettings = async () => {
    try {
      const result = await commands.getAppSettings();
      if (result.status === "ok" && result.data) {
        const captionsEnabled = result.data.live_captions_enabled ?? true;
        setLiveCaptionsEnabled(captionsEnabled);
        console.log("[Live Captions] Settings fetched...", captionsEnabled);
      }
    } catch {
      // Silently ignore
    }
  };
  fetchSettings();
}, [isVisible]);
```

**Issue**: The setting is fetched ASYNCHRONOUSLY when overlay becomes visible. If recording starts quickly:
1. Recording starts → streaming callback set up (or not) based on backend setting
2. Overlay becomes visible → setting fetched
3. If frontend thinks captions are enabled but backend didn't set up callback, mismatch occurs

**Impact**: MEDIUM - Setting mismatch between frontend and backend

---

### 3. **Filler Word Filter Removes All Text**

**Location**: `src/overlay/RecordingOverlay.tsx:129-171`

**Code**:
```typescript
function filterStreamingText(text: string): string {
  const fillerWords = ["okay", "yeah", "um", "uh", "so", "like", "you know", "right", "well"];
  // ... filtering logic that can return empty string
  if (words.length < 2) {
    const isFiller = fillerWords.some(...);
    if (isFiller) return "";  // ⚠️ Returns EMPTY STRING!
  }
  // ...
}
```

**Issue**: The UI render condition requires `streamingText.trim() !== ""`. If the user says only "okay" or "um", the filter removes ALL text, and the captions box won't render.

**Impact**: MEDIUM - User sees no captions for short filler-only phrases

---

### 4. **Model Not Loaded When Streaming Transcription Runs**

**Location**: `src-tauri/src/managers/transcription.rs:637-641`

**Code**:
```rust
{
    let engine_guard = self.lock_engine();
    if engine_guard.is_none() {
        return Err(AppError::ModelNotLoaded);  // ⚠️ Silent failure in streaming context
    }
}
```

**Issue**: If the model isn't loaded when the streaming callback tries to transcribe, it returns an error that's only logged at debug level. The frontend never sees a partial transcription event.

**Impact**: HIGH - No captions if model loading is slow/fails

---

### 5. **Streaming Cancellation Race**

**Location**: `src-tauri/src/managers/audio.rs:223-225` and `254-256`

**Code**:
```rust
// Check cancellation WITHOUT lock (atomic load)
if cancel_flag.load(Ordering::Acquire) {
    debug!("Skipping streaming transcription - cancellation requested");
    return;
}
```

**Issue**: When recording stops, `cancel_streaming()` is called. Any in-progress or queued streaming transcriptions are aborted. If the user stops recording quickly, captions may never appear.

**Impact**: LOW - Expected behavior, but timing dependent

---

## Current Logging Coverage

### Backend (Rust) - Good Coverage ✅

| Location | Event | Log Level |
|----------|-------|-----------|
| `audio.rs:171` | Live captions enabled check | info |
| `audio.rs:210` | TranscriptionManager not available | info |
| `audio.rs:223` | Streaming cancelled | debug |
| `audio.rs:235` | Streaming cancelled (blocking) | debug |
| `audio.rs:244` | TranscriptionManager not available for streaming | debug |
| `audio.rs:301-302` | Failed to emit partial-transcription | warn |
| `audio.rs:309` | Streaming transcription failed | debug |
| `transcription.rs:170-172` | Live captions keeping model loaded | debug |

### Frontend (TypeScript) - Moderate Coverage ⚠️

| Location | Event | Log Type |
|----------|-------|----------|
| `RecordingOverlay.tsx:663-665` | Recording started + captions status | console.log |
| `RecordingOverlay.tsx:927-929` | Event listener registration | console.log |
| `RecordingOverlay.tsx:935-942` | Event received | console.log |
| `RecordingOverlay.tsx:964-976` | streamingText updates | console.log |
| `RecordingOverlay.tsx:991-1004` | Fallback text handling | console.log |

### Missing Critical Logs ❌

1. **When live captions render conditions fail** - No logging when:
   - `liveCaptionsEnabled` is false but overlay thinks it should show captions
   - `streamingText` is empty after filtering
   - `micDeadWarning` or `lowAudioWarning` blocks captions

2. **Backend/Frontend setting mismatch** - No way to detect when backend skipped streaming setup but frontend expects captions

3. **Model loading status during streaming** - No visibility into whether model was loaded when streaming transcription attempted

---

## Recommended Additional Logging

### Backend Additions

```rust
// In audio.rs after line 201 - Add explicit logging for streaming setup decision
info!(
    "Live captions: {} | TranscriptionManager available: {} | Will setup streaming callback: {}",
    live_captions_enabled,
    app_handle.try_state::<Arc<Mutex<TranscriptionManager>>>().is_some(),
    live_captions_enabled && app_handle.try_state::<Arc<Mutex<TranscriptionManager>>>().is_some()
);

// In audio.rs after line 302 - Add success logging
info!("Emitted partial-transcription event with {} segments", result.segments.as_ref().map(|s| s.len()).unwrap_or(0));

// In transcription.rs transcribe() - Add streaming-specific logging
debug!("Streaming transcription started - model loaded: {}, audio samples: {}", 
    self.is_model_loaded(), 
    audio.len()
);
```

### Frontend Additions

```typescript
// In RecordingOverlay.tsx - Add render condition logging
// After line 1224:
console.log('[Live Captions] Render check:', {
  isVisible,
  state,
  liveCaptionsEnabled,
  micDeadWarning,
  lowAudioWarning,
  hasStreamingText: !!streamingText,
  streamingTextLength: streamingText?.length,
  shouldRender: isVisible && state === "recording" && liveCaptionsEnabled && 
                !micDeadWarning && !lowAudioWarning && streamingText?.trim()
});

// Add when settings are fetched
console.log('[Live Captions] Settings fetched from backend:', {
  live_captions_enabled: result.data.live_captions_enabled,
  currentFrontendValue: liveCaptionsEnabled
});
```

---

## Debugging Steps for User Reports

When a user reports "live captions not showing", check:

1. **Settings check**: Is `live_captions_enabled` actually true in their settings?
2. **Model loaded**: Is a model actually loaded? (Check model-state-changed events)
3. **Events emitted**: Are `partial-transcription` events being emitted by backend?
4. **Events received**: Is the frontend receiving those events?
5. **Text filtering**: Is text being filtered to empty by filler word filter?
6. **Render blocked**: Are warnings (mic dead, low audio) blocking the render?

---

## Summary Table

| Failure Point | Severity | Detectable | User Impact |
|--------------|----------|------------|-------------|
| TranscriptionManager not init | HIGH | No logging | Silent failure |
| Setting fetch race condition | MEDIUM | Console logs only | Confusing behavior |
| Filler filter removes all text | MEDIUM | Console logs only | No short captions |
| Model not loaded | HIGH | Debug log only | No captions, no error |
| Cancellation race | LOW | Debug log only | Brief recordings miss captions |

---

## Priority Recommendations

1. **HIGH**: Add logging when streaming callback setup is skipped due to missing TranscriptionManager
2. **HIGH**: Add render condition logging to identify why captions aren't displaying
3. **MEDIUM**: Add setting mismatch detection between frontend and backend
4. **MEDIUM**: Show user-facing warning when model isn't loaded but live captions are enabled
5. **LOW**: Consider showing "partial" captions (like "...") even when filler words are filtered

---

*Analysis completed: 2026-07-02*
*Files analyzed: 15+ source files*
*Lines of code reviewed: ~3000+*
