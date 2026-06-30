use crate::managers::audio::AudioRecordingManager;
use crate::managers::transcription::TranscriptionManager;
use crate::shortcut;
use crate::TranscriptionCoordinator;
use log::{info, warn};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Manager};

// Re-export all utility modules for easy access
// pub use crate::audio_feedback::*;
pub use crate::clipboard::*;
pub use crate::overlay::*;
pub use crate::tray::*;

/// Centralized cancellation function that can be called from anywhere in the app.
/// Handles cancelling both recording and transcription operations and updates UI state.
pub fn cancel_current_operation(app: &AppHandle) {
    info!("Initiating operation cancellation...");

    // CRITICAL: Cancel streaming transcription FIRST (uses AtomicBool, no lock needed).
    // This must happen before any other operations to stop live captions immediately,
    // preventing wasted GPU work on partial audio that will be discarded anyway.
    if let Some(tm) = app.try_state::<Arc<Mutex<TranscriptionManager>>>() {
        tm.lock().unwrap().cancel_streaming();
        info!("Streaming transcription cancelled");
    }

    // Unregister the cancel shortcut asynchronously
    info!("Unregistering cancel shortcut...");
    shortcut::unregister_cancel_shortcut(app);

    // Cancel any ongoing recording
    let Some(audio_manager) = app.try_state::<Arc<Mutex<AudioRecordingManager>>>() else {
        warn!("AudioRecordingManager not available for cancellation");
        return;
    };
    let recording_was_active = audio_manager.lock().unwrap().is_recording();
    info!("Cancelling recording (was_active={})", recording_was_active);
    audio_manager.lock().unwrap().cancel_recording();

    // Update tray icon and force-hide overlay (bypass state check for cancel)
    info!("Updating UI state...");
    change_tray_icon(app, crate::tray::TrayIconState::Idle);
    force_hide_recording_overlay(app);

    // Unload model if immediate unload is enabled
    if let Some(tm) = app.try_state::<Arc<Mutex<TranscriptionManager>>>() {
        tm.lock().unwrap().maybe_unload_immediately("cancellation");
    }

    // Notify coordinator so it can keep lifecycle state coherent.
    if let Some(coordinator) = app.try_state::<TranscriptionCoordinator>() {
        info!("Notifying transcription coordinator");
        coordinator.notify_cancel(recording_was_active);
    } else {
        warn!("TranscriptionCoordinator not available");
    }

    info!("Operation cancellation completed - returned to idle state");
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
