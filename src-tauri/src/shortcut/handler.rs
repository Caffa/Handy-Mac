//! Shared shortcut event handling logic
//!
//! This module contains the common logic for handling shortcut events,
//! used by both the Tauri and handy-keys implementations.

use log::warn;
use std::sync::Arc;
use tauri::{AppHandle, Manager};

use crate::actions::ACTION_MAP;
use crate::managers::audio::AudioRecordingManager;
use crate::settings::get_settings_safe;
use crate::transcription_coordinator::is_transcribe_binding;
use crate::TranscriptionCoordinator;

/// Handle a shortcut event from either implementation.
///
/// This function contains the shared logic for:
/// - Looking up the action in ACTION_MAP
/// - Handling the cancel binding (only fires when recording)
/// - Handling push-to-talk mode (start on press, stop on release)
/// - Handling toggle mode (toggle state on press only)
///
/// # Arguments
/// * `app` - The Tauri app handle
/// * `binding_id` - The ID of the binding (e.g., "transcribe", "cancel")
/// * `hotkey_string` - The string representation of the hotkey
/// * `is_pressed` - Whether this is a key press (true) or release (false)
pub fn handle_shortcut_event(
    app: &AppHandle,
    binding_id: &str,
    hotkey_string: &str,
    is_pressed: bool,
) {
    let settings = get_settings_safe(app);

    // Transcribe bindings are handled by the coordinator.
    if is_transcribe_binding(binding_id) {
        if let Some(coordinator) = app.try_state::<TranscriptionCoordinator>() {
            coordinator.send_input(
                binding_id,
                hotkey_string,
                is_pressed,
                settings.shortcut_activation,
                std::time::Duration::from_millis(settings.hold_threshold_ms),
            );
        } else {
            warn!("TranscriptionCoordinator is not initialized");
        }
        return;
    }

    let Some(action) = ACTION_MAP.get(binding_id) else {
        warn!(
            "No action defined in ACTION_MAP for shortcut ID '{}'. Shortcut: '{}', Pressed: {}",
            binding_id, hotkey_string, is_pressed
        );
        return;
    };

    // Cancel binding: only fires when recording and key is pressed
    if binding_id == "cancel" {
        let Some(audio_manager) = app.try_state::<Arc<AudioRecordingManager>>() else {
            warn!("AudioRecordingManager not initialized for cancel shortcut");
            return;
        };
        if audio_manager.is_recording() && is_pressed {
            // Offload the synchronous cancel path to a blocking thread.
            // cancel_current_operation() can block on parking_lot locks and,
            // crucially, calls unregister_cancel_shortcut() which does a
            // recv_timeout round-trip to this same hotkey loop — running it
            // inline would self-wait for ~5s and starve the shortcut thread.
            // commands/mod.rs::cancel_operation already uses the same pattern.
            let app2 = app.clone();
            let action2 = Arc::clone(action);
            let bid = binding_id.to_string();
            let hk = hotkey_string.to_string();
            tauri::async_runtime::spawn_blocking(move || {
                action2.start(&app2, &bid, &hk);
            });
            // Accepted race: a transcribe hotkey pressed within ~40ms of ESC
            // (humanly impossible) could interleave; the same race already
            // exists in the spawned CLI path.
        }
        return;
    }

    // Remaining bindings (e.g. "test") use simple start/stop on press/release.
    if is_pressed {
        action.start(app, binding_id, hotkey_string);
    } else {
        action.stop(app, binding_id, hotkey_string);
    }
}
