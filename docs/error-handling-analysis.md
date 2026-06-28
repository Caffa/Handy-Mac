# Handy Speech-to-Text Application: Error Handling & Robustness Analysis

## Executive Summary

The Handy application demonstrates **strong error handling architecture** with comprehensive recovery mechanisms, particularly in audio management and model handling. The codebase shows mature patterns including structured error types, automatic retry logic, watchdog systems, and graceful degradation. However, there are some gaps in frontend error boundaries and partial recovery scenarios that could be improved.

---

## 1. Rust Backend Error Handling

### 1.1 Error Types & Architecture

**Strengths:**

The application uses a well-designed error hierarchy in `src-tauri/src/errors.rs`:

```rust
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("Audio error: {message}")]
    Audio { message: String, #[source] source: anyhow::Error },
    
    #[error("Transcription engine panicked: {0}")]
    TranscriptionPanic(String),
    
    #[error("Timed out waiting for model to load")]
    TranscriptionLoadTimeout,
    
    #[error("Failed to load {engine} model {model_id}: {message}")]
    ModelLoadFailed { engine: String, model_id: String, message: String, #[source] source: anyhow::Error },
    // ... 15+ variants
}
```

**Key Features:**
- Uses `thiserror` for structured error definitions
- Implements `From<AppError> for String` for Tauri command compatibility
- Provides convenience constructors (e.g., `AppError::audio()`, `AppError::model_load()`)
- Separates user-facing messages from internal error sources

### 1.2 Result Propagation Patterns

**Excellent Pattern - Mutex Poison Recovery:**
```rust
// src-tauri/src/managers/audio.rs:25-37
fn lock_with_log<'a, T>(mutex: &'a Mutex<T>, name: &str) -> MutexGuard<'a, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            error!("Mutex '{}' was poisoned: {:?}", name, poisoned);
            warn!("Recovering from poisoned mutex '{}' - data may be inconsistent", name);
            poisoned.into_inner()
        }
    }
}
```

This pattern is consistently used across the audio manager (1536+ lines), ensuring the app continues even after thread panics.

**Excellent Pattern - RAII Cleanup Guards:**
```rust
// src-tauri/src/managers/model.rs:112-132
struct DownloadCleanup<'a> {
    available_models: &'a Mutex<HashMap<String, ModelInfo>>,
    cancel_flags: &'a Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
    model_id: String,
    disarmed: bool,
}

impl<'a> Drop for DownloadCleanup<'a> {
    fn drop(&mut self) {
        if self.disarmed { return; }
        // Cleanup is_downloading flag and cancel_flags
        // Ensures consistent cleanup on every error path
    }
}
```

### 1.3 Recovery Strategies

**1. USB Power Watchdog (`src-tauri/src/usb_watchdog.rs`)**

A sophisticated automatic recovery system for USB audio devices:

- **Failure Detection:** Tracks consecutive mic-open failures, silent transcriptions, and low audio levels
- **Automatic Power Cycling:** Uses `uhubctl` to power-cycle USB hub ports
- **Cooldown Protection:** 30-second minimum between cycles to prevent thrashing
- **Grace Period:** Post-cycle grace period prevents false positives during device re-enumeration
- **Multi-trigger Detection:**
  - Mic open failures (threshold: 2 consecutive)
  - Zero-sample recordings
  - Silent transcriptions (>10s duration)
  - Low audio levels (RMS < 0.001)

```rust
pub fn on_mic_open_failed(&self) -> bool {
    if !self.enabled.load(Ordering::SeqCst) { return false; }
    if self.cycling.load(Ordering::SeqCst) { return false; }
    
    let failures = self.consecutive_failures.fetch_add(1, Ordering::SeqCst) + 1;
    if failures < self.fail_threshold.load(Ordering::SeqCst) {
        return false; // Below threshold
    }
    
    self.power_cycle_blocking() // Trigger recovery
}
```

**2. Liveness Monitor (`src-tauri/src/managers/audio.rs:423-533`)**

Background thread that checks microphone stream health every 3 seconds:
- Detects "zombie" streams (open but not producing audio)
- Automatically restarts dead streams
- Shows USB-cycling overlay during recovery
- Integrates with USB watchdog for coordinated recovery

**3. Model Loading Recovery (`src-tauri/src/managers/transcription.rs:715-765`)**

Emergency fallback when transcription starts with no loaded model:
```rust
// Emergency fallback when engine not loaded
None => {
    warn!("Engine not loaded - attempting emergency load");
    // Try primary model
    if let Err(e) = self.load_model(&model_id) {
        // Try fallback models from hybrid mode
        for fallback in [&settings.hybrid_short_audio_model, ...] {
            if self.load_model(fallback).is_ok() { break; }
        }
    }
    // Re-acquire engine after load
}
```

**4. Streaming Transcription Cancellation**

```rust
pub fn cancel_streaming(&self) {
    self.cancel_streaming.store(true, Ordering::Release);
}

pub fn is_streaming_cancelled(&self) -> bool {
    self.cancel_streaming.load(Ordering::Acquire)
}
```

Prevents wasted work when user stops recording mid-stream.

**5. Engine Panic Recovery (`src-tauri/src/managers/transcription.rs:767-1020`)**

Uses `catch_unwind` to prevent transcription engine crashes from poisoning the mutex:
```rust
let result = catch_unwind(AssertUnwindSafe(|| -> Result<...> {
    match &mut engine { /* transcription logic */ }
}));

match result {
    Ok(inner_result) => {
        // Put engine back for reuse
        let mut engine_guard = self.lock_engine();
        *engine_guard = Some(engine);
        inner_result?
    }
    Err(panic_payload) => {
        // Engine panicked - do NOT put it back
        // Clear model ID to force reload on next attempt
        // Emit error event
    }
}
```

---

## 2. Frontend Error Handling

### 2.1 State Management

**Strengths:**

The Zustand stores (`src/stores/modelStore.ts`, `src/stores/settingsStore.ts`) implement:

- **Optimistic Updates:** UI updates immediately, rolls back on error
```typescript
// src/stores/settingsStore.ts:320-348
updateSetting: async <K extends keyof Settings>(key: K, value: Settings[K]) => {
    const originalValue = settings?.[key];
    set((state) => ({ settings: { ...state.settings, [key]: value } }));
    
    try {
        const updater = settingUpdaters[key];
        if (updater) await updater(value);
    } catch (error) {
        // Rollback on error
        if (settings) {
            set({ settings: { ...settings, [key]: originalValue } });
        }
    }
}
```

- **Event Listener Cleanup:** Prevents memory leaks and duplicate handlers
```typescript
_unlistenFns: Array<() => void>;
destroy: () => void;

initialize: async () => {
    get()._unlistenFns.forEach((fn) => fn()); // Clean up old listeners
    // ... register new listeners
}
```

### 2.2 User Feedback Mechanisms

**Toast Notifications (`src/App.tsx:99-158`)**

Comprehensive error event handling:
```typescript
// Microphone permission errors
listen<RecordingErrorEvent>("recording-error", (event) => {
    if (error_type === "microphone_permission_denied") {
        toast.error(t("errors.micPermissionDeniedTitle"), { description });
    } else if (error_type === "no_input_device") {
        toast.error(t("errors.noInputDeviceTitle"), { description });
    }
});

// Paste failures
listen("paste-error", () => {
    toast.error(t("errors.pasteFailedTitle"), { description: t("errors.pasteFailed") });
});

// Model loading failures
listen<ModelStateEvent>("model-state-changed", (event) => {
    if (event.payload.event_type === "loading_failed") {
        toast.error(t("errors.modelLoadFailed", { model: ... }), { description: ... });
    }
});
```

**Missing: Error Boundaries**

No React Error Boundaries found in the codebase. A component crash could potentially crash the entire UI.

### 2.3 Loading/Error States

**Model Store (`src/stores/modelStore.ts`)**

- `loading`: boolean for initial load
- `error`: string | null for error messages
- `downloadingModels`: Record for tracking downloads
- `verifyingModels`: Record for SHA256 verification
- `extractingModels`: Record for tar.gz extraction

**Settings Store (`src/stores/settingsStore.ts`)**

- `isLoading`: boolean
- `isUpdating`: Record<string, boolean> for per-field loading states
- Individual loaders for each setting being updated

---

## 3. Critical Failure Scenarios

### 3.1 ✅ Well Handled

| Scenario | Implementation | Location |
|----------|---------------|----------|
| **Microphone permission denial** | Detected via error message parsing, emits `recording-error` event with platform-specific instructions | `audio_toolkit/audio/recorder.rs:595-600`, `App.tsx:103-109` |
| **Audio device disconnection** | Device monitor thread polls every 2s, emits `device-list-changed` event, auto-restarts stream | `managers/audio.rs:546-694` |
| **Model download failure** | `DownloadCleanup` RAII guard ensures state cleanup, SHA256 verification deletes corrupt files, resume support via Range headers | `managers/model.rs:112-132`, `1060-1281` |
| **Model loading failure (OOM)** | `catch_unwind` prevents crash, unloads model, emits event, falls back to loading smaller model | `managers/transcription.rs:767-1020` |
| **Transcription crash/panic** | Engine mutex taken before transcription, on panic model not put back, cleared for reload | `managers/transcription.rs:984-1019` |
| **File system errors (history)** | SQLite migration system with schema verification, defensive column checks, auto-fix missing columns | `managers/history.rs:183-366` |
| **Network failures (download)** | Exponential backoff via resume support, timeout handling, progress events, cancellation support | `managers/model.rs:1133-1281` |

### 3.2 ⚠️ Partially Handled

| Scenario | Current Handling | Gap |
|----------|-----------------|-----|
| **Bluetooth audio dropout** | BT keep-alive keeps mic stream open | No detection of actual BT disconnections, relies on VAD timeout |
| **GPU acceleration failure** | Falls back to CPU in `transcribe-rs` | No user notification about fallback, could surprise user with slow performance |
| **Settings corruption** | Defaults applied for missing values | No validation of corrupted settings file, could cause undefined behavior |
| **History database corruption** | Migrations + schema verification | If DB is completely unreadable, app may fail to start |
| **USB watchdog uhubctl missing** | Attempts auto-install via Homebrew | If install fails, watchdog silently disabled, user not notified |

### 3.3 ❌ Missing or Needs Improvement

| Scenario | Risk | Recommendation |
|----------|------|----------------|
| **React component crashes** | UI could become unresponsive | Add Error Boundaries around major sections |
| **Partial WAV file write** | Corrupt audio files in history | Add checksums or write-then-rename pattern |
| **Model file corruption** | SHA256 only at download time | Periodic re-verification or on-load check |
| **Memory exhaustion** | Large model files + concurrent ops | Add memory pressure detection |
| **Deadlock in transcription** | 30s spin-wait could hang forever | Add absolute timeout with cancellation |
| **VAD model corruption** | Silero VAD file could be damaged | Add VAD model integrity check at startup |

---

## 4. Recovery Mechanisms

### 4.1 Automatic Retry Logic

| Component | Strategy | Location |
|-----------|----------|----------|
| **Microphone stream open** | USB watchdog cycles port after 2 failures | `usb_watchdog.rs:109-140` |
| **Model download** | Resume from byte offset via Range headers | `model.rs:1092-1151` |
| **Transcription (emergency)** | Fallback to hybrid mode alternate model | `transcription.rs:728-740` |
| **Liveness check** | Restart stream if no audio for 3s | `audio.rs:465-527` |

### 4.2 Fallback Behaviors

```
Transcription Flow:
1. Try currently loaded model
2. If not loaded → Load selected model
3. If load fails → Try hybrid short model
4. If that fails → Try hybrid long model
5. If all fail → Return error to user

Audio Device Selection:
1. Try configured device
2. If clamshell mode → Try clamshell microphone
3. If not found → Use system default
4. If no devices → Error with instructions
```

### 4.3 Graceful Degradation

- **GPU Unavailable:** Automatically uses CPU (via transcribe-rs)
- **VAD Disabled:** Still records, just without voice filtering
- **Pre-buffer Unavailable:** Falls back to on-demand recording
- **Smart-stop Disabled:** Uses fixed-duration trailing buffer
- **Streaming Disabled:** Falls back to post-recording transcription

### 4.4 State Recovery

**On App Startup:**
1. Clean up partial downloads (`.partial` files)
2. Clean up interrupted extractions (`.extracting` directories)
3. Verify database schema, auto-fix missing columns
4. Check which models are actually downloaded
5. Auto-select first available model if none selected

**After USB Power Cycle:**
1. Wait for device re-enumeration (5s timeout, 250ms polling)
2. Restart microphone stream
3. Show recovery overlay during process
4. Reset failure counters on success

---

## 5. Code Examples - Specific Issues

### 5.1 Issue: Floating Promise in Audio Stop

**Location:** `src-tauri/src/managers/audio.rs:1027-1036`

```rust
if let Some(rec) = lock_with_log(&self.recorder, "recorder").as_mut() {
    if *lock_with_log(&self.is_recording, "is_recording") {
        let _ = rec.stop(); // Error silently dropped
        *lock_with_log(&self.is_recording, "is_recording") = false;
    }
    let _ = rec.close(); // Error silently dropped
}
```

**Risk:** If `stop()` or `close()` fail, the stream may be in an inconsistent state.

**Recommendation:** Log errors and potentially retry.

### 5.2 Issue: Unbounded Spin-Wait

**Location:** `src-tauri/src/managers/transcription.rs:511-519`

```rust
while self.is_transcribing.load(Ordering::Relaxed) {
    if wait_start.elapsed() > max_wait {
        warn!("Timed out waiting for previous transcription to complete");
        return Err(AppError::TranscriptionBusy);
    }
    std::thread::yield_now();
}
```

**Risk:** Could spin forever if flag never clears (e.g., thread panic).

**Recommendation:** Use a proper synchronization primitive (condvar + timeout).

### 5.3 Issue: Missing Retry for Critical Paths

**Location:** `src-tauri/src/managers/model.rs:787-837`

```rust
fn update_download_status(&self) -> Result<()> {
    // If metadata read fails, silently ignores
    if partial_path.exists() {
        model.partial_size = partial_path.metadata().map(|m| m.len()).unwrap_or(0);
    }
}
```

**Risk:** Temporary I/O errors could corrupt download state tracking.

**Recommendation:** Add limited retries for metadata operations.

### 5.4 Issue: Frontend Error Swallowing

**Location:** `src/stores/settingsStore.ts:250-256`

```typescript
if (result.status === "ok") {
    // ... set settings
} else {
    console.error("Failed to load settings:", result.error);
    set({ isLoading: false }); // Error not surfaced to user
}
```

**Risk:** Settings failures could leave app in broken state without user notification.

**Recommendation:** Show error toast and offer retry/reset options.

---

## 6. Recommendations

### High Priority

1. **Add React Error Boundaries**
   ```typescript
   // Wrap major sections
   <ErrorBoundary fallback={<ErrorFallback />}>
       <SettingsPanel />
   </ErrorBoundary>
   ```

2. **Implement Model Verification on Load**
   - Check SHA256 before loading models, not just after download
   - Delete and re-download if corrupt

3. **Add Memory Pressure Detection**
   - Check available memory before loading large models
   - Warn user if system may run out of memory

4. **Improve Spin-Wait with Proper Synchronization**
   ```rust
   // Replace spin-wait with condvar
   let (lock, cvar) = &*self.transcription_condvar;
   let result = cvar.wait_timeout(lock.lock().unwrap(), max_wait);
   ```

### Medium Priority

5. **Add Partial Write Protection for Audio Files**
   - Write to temp file, fs::rename on success
   - Prevents corrupt WAV files in history

6. **Surface Settings Load Failures**
   - Show modal with "Reset to Defaults" option
   - Log details for debugging

7. **Add VAD Model Integrity Check**
   - Verify SHA256 of `silero_vad_v4.onnx` on startup
   - Re-extract from resources if corrupt

### Low Priority

8. **Add Periodic Health Checks**
   - Verify all downloaded models periodically
   - Clean up orphaned files

9. **Improve GPU Fallback Notification**
   - Show toast when falling back to CPU
   - Explain performance impact

10. **Add Metrics/Analytics for Recovery Events**
    - Track how often USB watchdog triggers
    - Monitor model load failures
    - Identify problematic hardware configurations

---

## 7. Overall Assessment

| Category | Score | Notes |
|----------|-------|-------|
| **Error Type Architecture** | ⭐⭐⭐⭐⭐ | Excellent use of `thiserror`, structured variants, clear separation |
| **Recovery Mechanisms** | ⭐⭐⭐⭐⭐ | USB watchdog, liveness monitor, panic recovery are sophisticated |
| **Resource Cleanup** | ⭐⭐⭐⭐⭐ | RAII guards consistently used, no leak patterns seen |
| **Frontend Error Handling** | ⭐⭐⭐⭐ | Good state management, toast notifications, missing error boundaries |
| **User Feedback** | ⭐⭐⭐⭐ | i18n support, platform-specific messages, could surface more failures |
| **Edge Case Coverage** | ⭐⭐⭐⭐ | Most scenarios handled, some missing as noted in Section 3.3 |
| **Testing** | ⭐⭐⭐ | Some unit tests for error detection, could use more integration tests |

**Overall: 4.5/5** - Production-ready error handling with room for minor improvements.

The codebase demonstrates mature Rust error handling patterns with excellent recovery capabilities. The USB watchdog and liveness monitoring systems are particularly impressive, showing real-world operational experience. The main areas for improvement are adding frontend error boundaries and surfacing more backend failures to users.
