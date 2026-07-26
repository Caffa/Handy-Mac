use crate::managers::audio::AudioRecordingManager;
use crate::shortcut;
use crate::transcription_coordinator::{emit_app_state, AppState, CancelSignal};
use crate::TranscriptionCoordinator;
use log::{info, warn};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::{AppHandle, Manager};

// Re-export all utility modules for easy access
// pub use crate::audio_feedback::*;
pub use crate::clipboard::*;
pub use crate::overlay::*;
pub use crate::tray::*;

/// Centralized cancellation function that can be called from anywhere in the app.
/// Handles cancelling both recording and transcription operations and updates UI state.
///
/// FIXED: Added defensive checks to ensure we don't try to cancel when already idle,
/// and proper logging of state transitions for debugging freeze issues.
/// INSTRUMENTED: Each step is timed so that if the freeze recurs, the log shows
/// exactly which call blocked.
pub fn cancel_current_operation(app: &AppHandle) {
    info!(
        "[CANCEL-TIMING] cancel_current_operation START on thread {:?}",
        std::thread::current().id()
    );
    let total_start = std::time::Instant::now();

    // CRITICAL: Cancel streaming transcription FIRST (uses AtomicBool, no lock needed).
    // This must happen before any other operations to stop live captions immediately,
    // preventing wasted GPU work on partial audio that will be discarded anyway.
    // IMPORTANT: Use the streaming_cancel_flag managed separately in app state
    // instead of tm.lock().cancel_streaming() to avoid blocking.
    // The streaming callback holds the TM lock during transcription (seconds),
    // and this cancel handler would block waiting for that lock, freezing the UI.
    // By using the Arc<AtomicBool> directly, we can cancel without waiting.
    {
        let t = std::time::Instant::now();
        if let Some(cancel_flag) = app.try_state::<Arc<AtomicBool>>() {
            let was_already_cancelled = cancel_flag.swap(true, Ordering::AcqRel);
            if was_already_cancelled {
                info!("Streaming transcription was already cancelled");
            } else {
                info!("Streaming transcription cancelled via Arc<AtomicBool>");
            }
        } else {
            warn!("Streaming cancel flag not available in app state");
        }
        info!(
            "[CANCEL-TIMING] step 1 (streaming cancel flag swap) took {:?}",
            t.elapsed()
        );
    }

    // Unregister the cancel shortcut (synchronously now, not async)
    {
        let t = std::time::Instant::now();
        info!("Unregistering cancel shortcut...");
        shortcut::unregister_cancel_shortcut(app);
        info!(
            "[CANCEL-TIMING] step 2 (unregister_cancel_shortcut) took {:?}",
            t.elapsed()
        );
    }

    // 3. Cancel recording FIRST — must never fail
    let recording_was_active = {
        let t = std::time::Instant::now();
        let active = if let Some(audio_manager) = app.try_state::<Arc<AudioRecordingManager>>() {
            let active = audio_manager.is_recording();
            if active {
                info!("Cancelling active recording");
                audio_manager.cancel_recording();
            } else {
                info!("No active recording to cancel, but proceeding with cleanup");
            }
            active
        } else {
            warn!("AudioRecordingManager not available for cancellation");
            false
        };
        info!(
            "[CANCEL-TIMING] step 3 (cancel_recording) took {:?}",
            t.elapsed()
        );
        active
    };

    // 4. Notify coordinator (non-blocking, ignores closed channel)
    {
        let t = std::time::Instant::now();
        if let Some(coordinator) = app.try_state::<TranscriptionCoordinator>() {
            coordinator.notify_cancel(recording_was_active);
        } else {
            warn!("TranscriptionCoordinator not available");
        }
        info!(
            "[CANCEL-TIMING] step 4 (notify_cancel) took {:?}",
            t.elapsed()
        );
    }
    {
        let t = std::time::Instant::now();
        if let Some(cancel_signal) = app.try_state::<CancelSignal>() {
            cancel_signal.send_cancel();
            info!("Cancel signal sent via CancelSignal flag");
        }
        info!(
            "[CANCEL-TIMING] step 5 (send_cancel) took {:?}",
            t.elapsed()
        );
    }

    // 5. Emit Idle state (safe — uses let _ = on emits)
    {
        let t = std::time::Instant::now();
        emit_app_state(app, &AppState::Idle);
        info!(
            "[CANCEL-TIMING] step 6 (emit_app_state Idle) took {:?}",
            t.elapsed()
        );
    }

    // 6. UI cleanup LAST — wrapped so a tray panic can't abort the cancel.
    // The tray icon change has 14 .expect() calls that can panic; wrapping
    // it ensures the cancel completes even if the tray is in a bad state.
    {
        let t = std::time::Instant::now();
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            change_tray_icon(app, crate::tray::TrayIconState::Idle);
        }));
        info!(
            "[CANCEL-TIMING] step 7 (change_tray_icon) took {:?}",
            t.elapsed()
        );
    }
    {
        let t = std::time::Instant::now();
        force_hide_recording_overlay(app);
        info!(
            "[CANCEL-TIMING] step 8 (force_hide_recording_overlay) took {:?}",
            t.elapsed()
        );
    }

    info!(
        "[CANCEL-TIMING] cancel_current_operation COMPLETE, total {:?}",
        total_start.elapsed()
    );
}

/// Show the recording overlay in "USB cycling" mode.
/// Used during dead-stream recovery when the USB watchdog is power-cycling
/// the hub port so the user sees visual feedback instead of a frozen app.
pub fn show_usb_cycling_overlay(app: &AppHandle) {
    show_overlay_state(app, "usb-cycling", &OverlayMode::Transcribe);
}

/// Check if using the Wayland display server protocol
#[cfg(target_os = "linux")]
pub fn is_wayland() -> bool {
    std::env::var("WAYLAND_DISPLAY").is_ok()
        || std::env::var("XDG_SESSION_TYPE")
            .map(|v| v.to_lowercase() == "wayland")
            .unwrap_or(false)
}

/// Check if running on KDE Plasma desktop environment
#[cfg(target_os = "linux")]
pub fn is_kde_plasma() -> bool {
    std::env::var("XDG_CURRENT_DESKTOP")
        .map(|v| v.to_uppercase().contains("KDE"))
        .unwrap_or(false)
        || std::env::var("KDE_SESSION_VERSION").is_ok()
}

/// Check if running on KDE Plasma with Wayland
#[cfg(target_os = "linux")]
pub fn is_kde_wayland() -> bool {
    is_wayland() && is_kde_plasma()
}

/// Returns true when running an x64 Windows binary under ARM64 emulation.
/// GPU acceleration is disabled in this configuration because emulated
/// Vulkan/Metal interop is unreliable.
#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
pub fn is_windows_x64_emulated_on_arm64() -> bool {
    std::env::var("PROCESSOR_IDENTIFIER")
        .map(|v| v.to_uppercase().contains("ARM"))
        .unwrap_or(false)
}

#[cfg(not(all(target_os = "windows", target_arch = "x86_64")))]
pub fn is_windows_x64_emulated_on_arm64() -> bool {
    false
}
