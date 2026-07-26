# transcribe-cpp GPU Initialization: Timeout & Cancellation Analysis

**Date:** 2026-07-23  
**Branch:** new-models  
**Target:** Understanding whether `Model::load_with()` (GPU init for GGUF/Parakeet models) can be timed out or cancelled.

---

## 1. The Critical Call Path

**File:** `src-tauri/src/managers/transcription.rs` lines 584-648

```rust
// line 604-612
let model_options = ModelOptions {
    backend,
    gpu_device,
};
let model = Model::load_with(&model_path, &model_options).map_err(|e| {
    let error_msg = format!("Failed to load whisper model {}: {}", model_id, e);
    emit_loading_failed(&error_msg);
    anyhow::anyhow!(error_msg)
})?;
```

This is a **synchronous blocking call** — it blocks the calling thread until the model is fully loaded and the GPU is initialized. It is spawned from a background `thread::spawn` at line 767 via `initiate_model_load()`.

---

## 2. `transcribe_cpp::ModelOptions` Struct Definition

**File:** `~/.cargo/registry/src/index.crates.io-.../transcribe-cpp-0.1.3/src/model.rs` lines 31-38

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelOptions {
    /// Which backend to request. Default `Backend::Auto`.
    pub backend: Backend,
    /// GPU device registry index. 0 means auto.
    pub gpu_device: i32,
}
```

**Only two fields: `backend` and `gpu_device`.** No timeout, no cancellation token, no progress callback.

---

## 3. C-Level FFI Struct

**File:** `~/.cargo/registry/src/index.crates.io-.../transcribe-cpp-sys-0.1.3/bindings/rust/sys/src/transcribe_sys.rs` lines 270-288

```rust
#[repr(C)]
pub struct transcribe_model_load_params {
    pub struct_size: u64,
    pub backend: transcribe_backend_request,
    pub gpu_device: ::std::os::raw::c_int,
}
```

**Only three fields.** The C API (`transcribe_model_load_file`) accepts this struct and has no timeout or abort callback parameter.

---

## 4. Does `CancelToken` Help?

**File:** `~/.cargo/registry/src/index.crates.io-.../transcribe-cpp-0.1.3/src/cancel.rs`

```rust
pub struct CancelToken {
    pub(crate) flag: Arc<AtomicBool>,
}
```

The `CancelToken` exists and works via an abort callback installed on a **`Session`** (not on `Model`). The key docstring:

> Cooperative cancellation for **in-flight runs and streams**.  
> A `CancelToken` wraps a shared atomic flag. Install it on a session, then call `CancelToken::cancel` from any thread to abort the **in-flight `run`/`feed`/`finalize`**: the native abort callback (polled between decode steps) sees the flag and stops.

**The cancel token is for transcription runs, NOT for model loading.** It is installed via `session.set_cancel_token()`, which comes *after* the model is already loaded. The native abort callback is polled "between decode steps" — during actual transcription compute, not during the `transcribe_model_load_file` C call.

---

## 5. Does Any Timeout Exist Around Model Loading?

**No.** Here's what exists vs. what's missing:

### What EXISTS (but doesn't help):
| Mechanism | Location | Purpose |
|---|---|---|
| `CancelToken` | `cancel.rs` | Abort in-flight `run`/`feed`/`finalize` on sessions |
| `CancellationToken` (tokio) | `model.rs` | Cancel HF model **downloads**, not GPU init |
| `processing_timeout` (30s) | `transcription_coordinator.rs` | Auto-reset if transcription hangs |
| `STREAM_FINALIZE_REPLY_TIMEOUT` (30s) | `transcription.rs:39` | Timeout for streaming finalize |
| Various `recv_timeout` | Multiple files | Prevent infinite blocking on channel receives |
| `tokio::time::timeout` | `settings/store.rs` | Flush debounce with 2s timeout |
| `try_lock_for(Duration)` | `commands/transcription.rs` | Mutex lock timeouts (2s/5s/10s) |

### What does NOT exist:
- **No timeout parameter on `ModelOptions`** or `transcribe_model_load_params`
- **No timeout wrapper** around `Model::load_with()` / `transcribe_model_load_file()`
- **No cancellation mechanism** for the GPU initialization phase
- **No progress callback** during model loading
- **No way to interrupt** a blocking GPU init from another thread

---

## 6. Workarounds Used Elsewhere for Blocking Operations

### Pattern A: `spawn_blocking` (Tauri async context)
```rust
// Used in commands/audio.rs, commands/history.rs, actions/transcribe.rs, etc.
tauri::async_runtime::spawn_blocking(move || {
    // blocking work here
});
```
This moves blocking work off the async runtime but doesn't add a timeout.

### Pattern B: `thread::spawn` + condvar signaling
```rust
// transcription.rs:767 — how model loading is triggered
thread::spawn(move || {
    self_clone.load_model(&settings.selected_model);  // blocks
    self_clone.loading_condvar.notify_all();
});
```
Model loading runs on its own thread. The coordinator waits on `loading_condvar` — but there's no timeout on that wait.

### Pattern C: `recv_timeout` for channel-based blocking
```rust
// audio/recorder.rs — prevent infinite blocking on channel recv
match resp_rx.recv_timeout(Duration::from_secs(5)) {
    Ok(samples) => samples,
    Err(_) => vec![], // timeout fallback
}
```

### Pattern D: `tokio::time::timeout` for async futures
```rust
// settings/store.rs
tokio::time::timeout(Duration::from_secs(2), writer.flush(app)).await
```

### Pattern E: Mutex lock timeouts
```rust
// commands/transcription.rs
tm.try_lock_for(Duration::from_secs(2))
    .ok_or("Transcription manager is busy (lock timeout after 2s)")?;
```

**None of these patterns are applied to model loading.** The `load_model_with_device()` call is fully synchronous with no cancellation or timeout.

---

## 7. Summary

| Question | Answer |
|---|---|
| Does transcribe-cpp have a timeout parameter? | **No.** `ModelOptions` has only `backend` and `gpu_device`. |
| Does `ModelOptions` expose timeout/cancel capabilities? | **No.** Neither timeout nor cancellation is available. |
| Does `CancelToken` help with model loading? | **No.** It only works on `Session` runs/streams (post-load). |
| Is there a timeout around model loading? | **No.** `Model::load_with()` is a synchronous blocking FFI call. |
| What are the workarounds? | Model loading is `thread::spawn`ed, but no timeout/cancel wraps the call. |

### Implications

If GPU initialization hangs (e.g., Vulkan/Metal device discovery failure, CUDA OOM during weight loading, driver crash), the `load_model_with_device()` call will block indefinitely. The only escape is:
1. The C library returns an error status (which Rust maps to `Error`)
2. The thread is killed externally (not currently implemented)
3. The process is killed

**To add timeout/cancel support, one would need to either:**
- Modify `transcribe-cpp` to add a timeout or abort callback to `transcribe_model_load_file` at the C level
- Use `std::thread::spawn` + `JoinHandle::join(Duration)` to wrap the load in a timeout (but the inner thread still blocks)
- Use a watchdog thread that kills the loading thread on timeout (unsafe, not recommended)

---

## Key Source Files

- `src-tauri/src/managers/transcription.rs` — `load_model_with_device()` (line 516), `initiate_model_load()` (line 754)
- `transcribe-cpp-0.1.3/src/model.rs` — `ModelOptions` struct, `Model::load_with()` 
- `transcribe-cpp-0.1.3/src/cancel.rs` — `CancelToken` (runs/streams only)
- `transcribe-cpp-0.1.3/src/session.rs` — `Session::set_cancel_token()` (runs/streams only)
- `transcribe-cpp-sys-0.1.3/bindings/rust/sys/src/transcribe_sys.rs` — C FFI structs
- `src-tauri/src/managers/model.rs` — `CancellationToken` for HF downloads (line 1831)
