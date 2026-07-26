//! Keyboard shortcut management module
//!
//! This module provides a unified interface for keyboard shortcuts with
//! multiple backend implementations:
//!
//! - `tauri`: Uses Tauri's built-in global-shortcut plugin
//! - `handy_keys`: Uses the handy-keys library for more control
//!
//! The active implementation is determined by the `keyboard_implementation`
//! setting and can be changed at runtime.

pub mod conflicts;
mod handler;
pub mod handy_keys;
mod tauri_impl;

use log::{debug, error, info, warn};
use parking_lot::Mutex;
use serde::Serialize;
use specta::Type;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_autostart::ManagerExt;

use conflicts::ConflictInfo;

use crate::managers::transcription::TranscriptionManager;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
use crate::settings::APPLE_INTELLIGENCE_DEFAULT_MODEL_ID;
use crate::settings::{
    self, get_settings, AutoSubmitKey, ClipboardHandling, CustomWord, KeyboardImplementation,
    LLMPrompt, OverlayPosition, OverlayScreenTarget, PasteMethod, ShortcutBinding, SoundTheme,
    TypingTool, APPLE_INTELLIGENCE_PROVIDER_ID,
};
use crate::tray;

// Note: Commands are accessed via shortcut::handy_keys:: in lib.rs

/// Initialize shortcuts using the configured implementation
pub fn init_shortcuts(app: &AppHandle) {
    let user_settings = settings::load_or_create_app_settings_safe(app);

    // Check which implementation to use
    match user_settings.keyboard_implementation {
        KeyboardImplementation::Tauri => {
            tauri_impl::init_shortcuts(app);
        }
        KeyboardImplementation::HandyKeys => {
            if let Err(e) = handy_keys::init_shortcuts(app) {
                error!("Failed to initialize handy-keys shortcuts: {}", e);
                // Fall back to Tauri implementation and persist this fallback
                warn!("Falling back to Tauri global shortcut implementation and saving fallback to settings");

                // Update settings to persist the fallback so we don't retry HandyKeys on next launch
                let mut settings = settings::get_settings_safe(app);
                settings.keyboard_implementation = KeyboardImplementation::Tauri;
                settings::write_settings_safe(app, settings);

                tauri_impl::init_shortcuts(app);
            }
        }
    }
}

/// Register the cancel shortcut (called when recording starts)
pub fn register_cancel_shortcut(app: &AppHandle) {
    let settings = get_settings(app);
    match settings.keyboard_implementation {
        KeyboardImplementation::Tauri => tauri_impl::register_cancel_shortcut(app),
        KeyboardImplementation::HandyKeys => handy_keys::register_cancel_shortcut(app),
    }
}

/// Unregister the cancel shortcut (called when recording stops)
pub fn unregister_cancel_shortcut(app: &AppHandle) {
    let settings = get_settings(app);
    match settings.keyboard_implementation {
        KeyboardImplementation::Tauri => tauri_impl::unregister_cancel_shortcut(app),
        KeyboardImplementation::HandyKeys => handy_keys::unregister_cancel_shortcut(app),
    }
}

/// Register a shortcut using the appropriate implementation
pub fn register_shortcut(app: &AppHandle, binding: ShortcutBinding) -> Result<(), String> {
    let settings = get_settings(app);
    match settings.keyboard_implementation {
        KeyboardImplementation::Tauri => tauri_impl::register_shortcut(app, binding),
        KeyboardImplementation::HandyKeys => handy_keys::register_shortcut(app, binding),
    }
}

/// Unregister a shortcut using the appropriate implementation
pub fn unregister_shortcut(app: &AppHandle, binding: ShortcutBinding) -> Result<(), String> {
    let settings = get_settings(app);
    match settings.keyboard_implementation {
        KeyboardImplementation::Tauri => tauri_impl::unregister_shortcut(app, binding),
        KeyboardImplementation::HandyKeys => handy_keys::unregister_shortcut(app, binding),
    }
}

// ============================================================================
// Binding Management Commands
// ============================================================================

#[derive(Serialize, Type)]
pub struct BindingResponse {
    success: bool,
    binding: Option<ShortcutBinding>,
    error: Option<String>,
}

/// Check a shortcut for conflicts with platform-specific reserved shortcuts.
///
/// Returns a list of conflicts (may be empty). This does NOT block registration;
/// it only provides information for the user to decide whether to proceed.
#[tauri::command]
#[specta::specta]
pub fn check_shortcut_conflicts(binding: String) -> Vec<ConflictInfo> {
    conflicts::detect_conflicts(&binding)
}

/// Apply a cancel binding update to settings.
/// Returns the updated ShortcutBinding.
fn apply_cancel_binding_update(
    settings: &mut crate::settings::AppSettings,
    id: &str,
    new_binding: String,
) -> ShortcutBinding {
    let mut b = settings.bindings.remove(id).unwrap_or_else(|| {
        settings::get_default_settings()
            .bindings
            .get(id)
            .cloned()
            .unwrap_or_else(|| ShortcutBinding {
                id: id.to_string(),
                name: "Cancel".to_string(),
                description: "Cancels the current recording.".to_string(),
                default_binding: "escape".to_string(),
                current_binding: "escape".to_string(),
            })
    });
    b.current_binding = new_binding;
    settings.bindings.insert(id.to_string(), b.clone());
    b
}

#[tauri::command]
#[specta::specta]
pub fn change_binding(
    app: AppHandle,
    id: String,
    binding: String,
) -> Result<BindingResponse, String> {
    // Reject empty bindings — every shortcut should have a value
    if binding.trim().is_empty() {
        return Err("Binding cannot be empty".to_string());
    }

    // Serialize binding updates to prevent TOCTOU races
    let lock: tauri::State<'_, Arc<Mutex<()>>> = app.state();
    let _lock = lock.lock();

    let mut settings = settings::get_settings_safe(&app);

    // Get the binding to modify, or create it from defaults if it doesn't exist
    let binding_to_modify = match settings.bindings.get(&id) {
        Some(binding) => binding.clone(),
        None => {
            // Try to get the default binding for this id
            let default_settings = settings::get_default_settings();
            match default_settings.bindings.get(&id) {
                Some(default_binding) => {
                    warn!(
                        "Binding '{}' not found in settings, creating from defaults",
                        id
                    );
                    default_binding.clone()
                }
                None => {
                    let error_msg = format!("Binding with id '{}' not found in defaults", id);
                    warn!("change_binding error: {}", error_msg);
                    return Ok(BindingResponse {
                        success: false,
                        binding: None,
                        error: Some(error_msg),
                    });
                }
            }
        }
    };

    // If this is the cancel binding, just update the settings and return
    // It's managed dynamically, so we don't register/unregister here
    if id == "cancel" {
        let b = apply_cancel_binding_update(&mut settings, &id, binding.clone());
        settings::write_settings_safe(&app, settings);
        return Ok(BindingResponse {
            success: true,
            binding: Some(b),
            error: None,
        });
    }

    // Defensive: cancel should never reach the registration path
    debug_assert!(id != "cancel", "cancel binding should have been handled above");

    // Unregister the existing binding
    if let Err(e) = unregister_shortcut(&app, binding_to_modify.clone()) {
        let error_msg = format!("Failed to unregister shortcut: {}", e);
        error!("change_binding error: {}", error_msg);
    }

    // Validate the new shortcut for the current keyboard implementation
    if let Err(e) = validate_shortcut_for_implementation(&binding, settings.keyboard_implementation)
    {
        warn!("change_binding validation error: {}", e);
        return Err(e);
    }

    // Check for platform-specific shortcut conflicts and emit warning if found
    let conflicts = conflicts::detect_conflicts(&binding);
    if !conflicts.is_empty() {
        let conflict_names: Vec<String> = conflicts
            .iter()
            .map(|c| format!("{} ({})", c.name, c.platform))
            .collect();
        warn!(
            "Shortcut '{}' conflicts with system shortcuts: {}",
            binding,
            conflict_names.join(", ")
        );

        // Emit warning event to frontend
        let _ = app.emit(
            "shortcut-conflict-warning",
            serde_json::json!({
                "shortcut": binding,
                "conflicts": conflicts,
            }),
        );
    }

    // Create an updated binding
    let mut updated_binding = binding_to_modify;
    updated_binding.current_binding = binding;

    // Register the new binding
    if let Err(e) = register_shortcut(&app, updated_binding.clone()) {
        let error_msg = format!("Failed to register shortcut: {}", e);
        error!("change_binding error: {}", error_msg);
        return Ok(BindingResponse {
            success: false,
            binding: None,
            error: Some(error_msg),
        });
    }

    // Update the binding in the settings
    settings.bindings.insert(id, updated_binding.clone());

    // Save the settings
    settings::write_settings_safe(&app, settings);

    // Return the updated binding
    Ok(BindingResponse {
        success: true,
        binding: Some(updated_binding),
        error: None,
    })
}

#[tauri::command]
#[specta::specta]
pub fn reset_binding(app: AppHandle, id: String) -> Result<BindingResponse, String> {
    let binding = settings::get_stored_binding(&app, &id);
    change_binding(app, id, binding.default_binding)
}

/// Temporarily unregister a binding while the user is editing it in the UI.
/// This avoids firing the action while keys are being recorded.
#[tauri::command]
#[specta::specta]
pub fn suspend_binding(app: AppHandle, id: String) -> Result<(), String> {
    if let Some(b) = settings::get_bindings(&app).get(&id).cloned() {
        if let Err(e) = unregister_shortcut(&app, b) {
            error!("suspend_binding error for id '{}': {}", id, e);
            return Err(e);
        }
    }
    Ok(())
}

/// Re-register the binding after the user has finished editing.
#[tauri::command]
#[specta::specta]
pub fn resume_binding(app: AppHandle, id: String) -> Result<(), String> {
    if let Some(b) = settings::get_bindings(&app).get(&id).cloned() {
        if let Err(e) = register_shortcut(&app, b) {
            error!("resume_binding error for id '{}': {}", id, e);
            return Err(e);
        }
    }
    Ok(())
}

// ============================================================================
// Keyboard Implementation Switching
// ============================================================================

/// Result of changing keyboard implementation
#[derive(Serialize, Type)]
pub struct ImplementationChangeResult {
    pub success: bool,
    /// List of binding IDs that were reset to defaults due to incompatibility
    pub reset_bindings: Vec<String>,
}

/// Change the keyboard implementation with runtime switching.
/// This will unregister all shortcuts from the old implementation,
/// validate shortcuts for the new implementation (resetting invalid ones to defaults),
/// and register them with the new implementation.
#[tauri::command]
#[specta::specta]
pub fn change_keyboard_implementation_setting(
    app: AppHandle,
    implementation: String,
) -> Result<ImplementationChangeResult, String> {
    let current_settings = settings::get_settings_safe(&app);
    let current_impl = current_settings.keyboard_implementation;
    let new_impl = parse_keyboard_implementation(&implementation);

    // If same implementation, nothing to do
    if current_impl == new_impl {
        return Ok(ImplementationChangeResult {
            success: true,
            reset_bindings: vec![],
        });
    }

    info!(
        "Switching keyboard implementation from {:?} to {:?}",
        current_impl, new_impl
    );

    // Unregister all shortcuts from the current implementation
    unregister_all_shortcuts(&app, current_impl);

    // Update the setting
    let mut settings = settings::get_settings_safe(&app);
    settings.keyboard_implementation = new_impl;
    settings::write_settings_safe(&app, settings);

    // Initialize new implementation if needed (HandyKeys needs state)
    if new_impl == KeyboardImplementation::HandyKeys {
        if initialize_handy_keys_with_rollback(&app)? {
            // Shortcuts already registered during init
            return Ok(ImplementationChangeResult {
                success: true,
                reset_bindings: vec![],
            });
        }
    }

    // Register all shortcuts with new implementation, resetting invalid ones
    let reset_bindings = register_all_shortcuts_for_implementation(&app, new_impl);

    // Emit event to notify frontend of the change
    let _ = app.emit(
        "settings-changed",
        serde_json::json!({
            "setting": "keyboard_implementation",
            "value": implementation,
            "reset_bindings": reset_bindings
        }),
    );

    info!("Keyboard implementation switched to {:?}", new_impl);

    Ok(ImplementationChangeResult {
        success: true,
        reset_bindings,
    })
}

/// Get the current keyboard implementation
#[tauri::command]
#[specta::specta]
pub fn get_keyboard_implementation(app: AppHandle) -> String {
    let settings = settings::get_settings_safe(&app);
    match settings.keyboard_implementation {
        KeyboardImplementation::Tauri => "tauri".to_string(),
        KeyboardImplementation::HandyKeys => "handy_keys".to_string(),
    }
}

// ============================================================================
// Validation Helpers
// ============================================================================

/// Validate a shortcut for a specific implementation
fn validate_shortcut_for_implementation(
    raw: &str,
    implementation: KeyboardImplementation,
) -> Result<(), String> {
    match implementation {
        KeyboardImplementation::Tauri => tauri_impl::validate_shortcut(raw),
        KeyboardImplementation::HandyKeys => handy_keys::validate_shortcut(raw),
    }
}

/// Parse a keyboard implementation string into the enum
fn parse_keyboard_implementation(s: &str) -> KeyboardImplementation {
    match s {
        "tauri" => KeyboardImplementation::Tauri,
        "handy_keys" => KeyboardImplementation::HandyKeys,
        other => {
            warn!(
                "Invalid keyboard implementation '{}', defaulting to tauri",
                other
            );
            KeyboardImplementation::Tauri
        }
    }
}

/// Unregister all shortcuts for the current implementation
fn unregister_all_shortcuts(app: &AppHandle, implementation: KeyboardImplementation) {
    let bindings = settings::get_bindings(app);

    for (id, binding) in bindings {
        // Skip cancel shortcut as it's dynamically registered
        if id == "cancel" {
            continue;
        }

        let result = match implementation {
            KeyboardImplementation::Tauri => tauri_impl::unregister_shortcut(app, binding),
            KeyboardImplementation::HandyKeys => handy_keys::unregister_shortcut(app, binding),
        };

        if let Err(e) = result {
            warn!(
                "Failed to unregister shortcut '{}' during switch: {}",
                id, e
            );
        }
    }
}

/// Register all shortcuts for a specific implementation, validating and resetting invalid ones
fn register_all_shortcuts_for_implementation(
    app: &AppHandle,
    implementation: KeyboardImplementation,
) -> Vec<String> {
    let mut reset_bindings = Vec::new();
    let default_bindings = settings::get_default_settings().bindings;
    let mut current_settings = settings::get_settings_safe(app);

    for (id, default_binding) in &default_bindings {
        // Skip cancel shortcut as it's dynamically registered
        if id == "cancel" {
            continue;
        }

        // Skip post-processing shortcut when the feature is disabled
        if id == "transcribe_with_post_process" && !current_settings.post_process_enabled {
            continue;
        }

        let mut binding = current_settings
            .bindings
            .get(id)
            .cloned()
            .unwrap_or_else(|| default_binding.clone());

        // Validate the shortcut for the target implementation
        if let Err(e) =
            validate_shortcut_for_implementation(&binding.current_binding, implementation)
        {
            info!(
                "Shortcut '{}' ({}) is invalid for {:?}: {}. Resetting to default.",
                id, binding.current_binding, implementation, e
            );

            // Reset to default
            binding.current_binding = default_binding.current_binding.clone();
            current_settings
                .bindings
                .insert(id.clone(), binding.clone());
            reset_bindings.push(id.clone());
        }

        // Register with the appropriate implementation
        let result = match implementation {
            KeyboardImplementation::Tauri => tauri_impl::register_shortcut(app, binding),
            KeyboardImplementation::HandyKeys => handy_keys::register_shortcut(app, binding),
        };

        if let Err(e) = result {
            error!(
                "Failed to register shortcut '{}' for {:?}: {}",
                id, implementation, e
            );
        }
    }

    // Save settings if any bindings were reset
    if !reset_bindings.is_empty() {
        settings::write_settings_safe(app, current_settings);
    }

    reset_bindings
}

/// Initialize HandyKeys if not already initialized, with rollback on failure
fn initialize_handy_keys_with_rollback(app: &AppHandle) -> Result<bool, String> {
    if app.try_state::<handy_keys::HandyKeysState>().is_some() {
        return Ok(false); // Already initialized, caller should continue
    }

    if let Err(e) = handy_keys::init_shortcuts(app) {
        error!("Failed to initialize HandyKeys: {}", e);
        // Rollback to Tauri
        let mut settings = settings::get_settings_safe(app);
        settings.keyboard_implementation = KeyboardImplementation::Tauri;
        settings::write_settings_safe(app, settings);
        tauri_impl::init_shortcuts(app);
        return Err(format!(
            "Failed to initialize HandyKeys: {}. Reverted to Tauri.",
            e
        ));
    }

    // init_shortcuts already registered shortcuts
    Ok(true)
}

// ============================================================================
// General Settings Commands
// ============================================================================

#[tauri::command]
#[specta::specta]
pub fn change_ptt_setting(app: AppHandle, enabled: bool) -> Result<(), String> {
    let mut settings = settings::get_settings_safe(&app);
    settings.push_to_talk = enabled;
    settings::write_settings_safe(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_audio_feedback_setting(app: AppHandle, enabled: bool) -> Result<(), String> {
    let mut settings = settings::get_settings_safe(&app);
    settings.audio_feedback = enabled;
    settings::write_settings_safe(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_audio_feedback_volume_setting(app: AppHandle, volume: f32) -> Result<(), String> {
    let mut settings = settings::get_settings_safe(&app);
    settings.audio_feedback_volume = volume;
    settings::write_settings_safe(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_sound_theme_setting(app: AppHandle, theme: String) -> Result<(), String> {
    let mut settings = settings::get_settings_safe(&app);
    let parsed = match theme.as_str() {
        "marimba" => SoundTheme::Marimba,
        "pop" => SoundTheme::Pop,
        "custom" => SoundTheme::Custom,
        other => {
            warn!("Invalid sound theme '{}', defaulting to marimba", other);
            SoundTheme::Marimba
        }
    };
    settings.sound_theme = parsed;
    settings::write_settings_safe(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_translate_to_english_setting(app: AppHandle, enabled: bool) -> Result<(), String> {
    let mut settings = settings::get_settings_safe(&app);
    settings.translate_to_english = enabled;
    settings::write_settings_safe(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_convert_us_to_british_setting(app: AppHandle, enabled: bool) -> Result<(), String> {
    let mut settings = settings::get_settings_safe(&app);
    settings.convert_us_to_british = enabled;
    settings::write_settings_safe(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_selected_language_setting(app: AppHandle, language: String) -> Result<(), String> {
    let mut settings = settings::get_settings_safe(&app);
    settings.selected_language = language;
    settings::write_settings_safe(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_overlay_position_setting(app: AppHandle, position: String) -> Result<(), String> {
    let mut settings = settings::get_settings_safe(&app);
    let parsed = match position.as_str() {
        "none" => OverlayPosition::None,
        "top" => OverlayPosition::Top,
        "bottom" => OverlayPosition::Bottom,
        other => {
            warn!("Invalid overlay position '{}', defaulting to bottom", other);
            OverlayPosition::Bottom
        }
    };
    settings.overlay_position = parsed;
    settings::write_settings_safe(&app, settings);

    // Keep the cached overlay-enabled flag in sync so emit_levels stops
    // (or resumes) emitting on the next audio callback.
    crate::overlay::update_overlay_enabled_cache(parsed != OverlayPosition::None);

    // Update overlay position without recreating window
    // Use default Transcribe mode since overlay is likely not visible
    // (position change is a settings operation, not during active recording)
    // State doesn't matter here since mode is Transcribe (always min height)
    crate::overlay::update_overlay_position(
        &app,
        "recording",
        &crate::overlay::OverlayMode::Transcribe,
    );

    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_overlay_screen_target_setting(app: AppHandle, target: String) -> Result<(), String> {
    let mut settings = settings::get_settings_safe(&app);
    let parsed = match target.as_str() {
        "cursor" => OverlayScreenTarget::Cursor,
        "side_screen" => OverlayScreenTarget::SideScreen,
        other => {
            warn!(
                "Invalid overlay screen target '{}', defaulting to cursor",
                other
            );
            OverlayScreenTarget::Cursor
        }
    };
    settings.overlay_screen_target = parsed;
    settings::write_settings_safe(&app, settings);
    crate::overlay::update_overlay_position(
        &app,
        "recording",
        &crate::overlay::OverlayMode::Transcribe,
    );
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_debug_mode_setting(app: AppHandle, enabled: bool) -> Result<(), String> {
    let mut settings = settings::get_settings_safe(&app);
    settings.debug_mode = enabled;
    settings::write_settings_safe(&app, settings);

    // Emit event to notify frontend of debug mode change
    let _ = app.emit(
        "settings-changed",
        serde_json::json!({
            "setting": "debug_mode",
            "value": enabled
        }),
    );

    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_start_hidden_setting(app: AppHandle, enabled: bool) -> Result<(), String> {
    let mut settings = settings::get_settings_safe(&app);
    settings.start_hidden = enabled;
    settings::write_settings_safe(&app, settings);

    // Notify frontend
    let _ = app.emit(
        "settings-changed",
        serde_json::json!({
            "setting": "start_hidden",
            "value": enabled
        }),
    );

    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_autostart_setting(app: AppHandle, enabled: bool) -> Result<(), String> {
    let mut settings = settings::get_settings_safe(&app);
    settings.autostart_enabled = enabled;
    settings::write_settings_safe(&app, settings);

    // Apply the autostart setting immediately
    let autostart_manager = app.autolaunch();
    if enabled {
        let _ = autostart_manager.enable();
    } else {
        let _ = autostart_manager.disable();
    }

    // Notify frontend
    let _ = app.emit(
        "settings-changed",
        serde_json::json!({
            "setting": "autostart_enabled",
            "value": enabled
        }),
    );

    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_update_checks_setting(app: AppHandle, enabled: bool) -> Result<(), String> {
    let mut settings = settings::get_settings_safe(&app);
    settings.update_checks_enabled = enabled;
    settings::write_settings_safe(&app, settings);

    let _ = app.emit(
        "settings-changed",
        serde_json::json!({
            "setting": "update_checks_enabled",
            "value": enabled
        }),
    );

    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn update_custom_words(app: AppHandle, words: Vec<String>) -> Result<(), String> {
    let mut settings = settings::get_settings_safe(&app);
    settings.custom_words = words;
    settings::write_settings_safe(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_word_correction_threshold_setting(
    app: AppHandle,
    threshold: f64,
) -> Result<(), String> {
    let mut settings = settings::get_settings_safe(&app);
    settings.word_correction_threshold = threshold;
    settings::write_settings_safe(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn update_advanced_custom_words(app: AppHandle, words: Vec<CustomWord>) -> Result<(), String> {
    let mut settings = settings::get_settings_safe(&app);
    settings.advanced_custom_words = words;
    settings::write_settings_safe(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn update_custom_filler_words(app: AppHandle, words: Vec<String>) -> Result<(), String> {
    let mut settings = settings::get_settings_safe(&app);
    settings.custom_filler_words = if words.is_empty() { None } else { Some(words) };
    settings::write_settings_safe(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_use_advanced_custom_words_setting(
    app: AppHandle,
    enabled: bool,
) -> Result<(), String> {
    let mut settings = settings::get_settings_safe(&app);
    // Backward compatibility: map boolean to mode
    settings.word_correction_mode = if enabled {
        settings::WordCorrectionMode::Pronunciation
    } else {
        settings::WordCorrectionMode::WordBias
    };
    settings.use_advanced_custom_words = enabled;
    settings::write_settings_safe(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_word_correction_mode(
    app: AppHandle,
    mode: settings::WordCorrectionMode,
) -> Result<(), String> {
    let mut settings = settings::get_settings_safe(&app);
    settings.word_correction_mode = mode;
    // Keep use_advanced_custom_words in sync for backward compatibility
    settings.use_advanced_custom_words = mode == settings::WordCorrectionMode::Pronunciation;
    settings::write_settings_safe(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn update_word_replacements(
    app: AppHandle,
    replacements: Vec<settings::WordReplacement>,
) -> Result<(), String> {
    let mut settings = settings::get_settings_safe(&app);
    settings.word_replacements = replacements;
    settings::write_settings_safe(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_extra_recording_buffer_setting(app: AppHandle, ms: u64) -> Result<(), String> {
    let mut settings = settings::get_settings_safe(&app);
    settings.extra_recording_buffer_ms = ms;
    settings::write_settings_safe(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_pre_recording_buffer_setting(app: AppHandle, ms: u64) -> Result<(), String> {
    let mut settings = settings::get_settings_safe(&app);
    settings.pre_recording_buffer_ms = ms;
    settings::write_settings_safe(&app, settings);

    // Recreate the recorder if the stream is open to apply the new setting.
    // The pre-recording buffer is only read when the recorder is created,
    // so we need to recreate it for the change to take effect.
    //
    // The AudioRecordingManager has internal locks for state and recorder,
    // so we don't need an outer Mutex here.
    if let Some(rm) =
        app.try_state::<std::sync::Arc<crate::managers::audio::AudioRecordingManager>>()
    {
        if rm.is_stream_open() {
            info!(
                "Applying pre-recording buffer change ({}ms): recreating recorder",
                ms
            );

            // recreate_recorder() now self-heals: if the stream was open and
            // should still be running (always-on or BT keep-alive), it
            // automatically restarts the stream after recreation. No need for
            // manual stop/restart here.
            if let Err(e) = rm.recreate_recorder() {
                error!(
                    "Failed to recreate recorder after pre-recording buffer change: {}",
                    e
                );
                return Err(format!(
                    "Failed to apply pre-recording buffer setting: {}",
                    e
                ));
            }

            info!("Recreated recorder with pre-recording buffer: {}ms", ms);
        }
    } else {
        log::warn!(
            "AudioRecordingManager not initialized, skipping pre-recording buffer recreation"
        );
    }

    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_paste_delay_ms_setting(app: AppHandle, ms: u64) -> Result<(), String> {
    let mut settings = settings::get_settings_safe(&app);
    settings.paste_delay_ms = ms;
    settings::write_settings_safe(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_paste_method_setting(app: AppHandle, method: String) -> Result<(), String> {
    let mut settings = settings::get_settings_safe(&app);
    let parsed = match method.as_str() {
        "ctrl_v" => PasteMethod::CtrlV,
        "direct" => PasteMethod::Direct,
        "none" => PasteMethod::None,
        "shift_insert" => PasteMethod::ShiftInsert,
        "ctrl_shift_v" => PasteMethod::CtrlShiftV,
        "external_script" => PasteMethod::ExternalScript,
        other => {
            warn!("Invalid paste method '{}', defaulting to ctrl_v", other);
            PasteMethod::CtrlV
        }
    };
    settings.paste_method = parsed;
    settings::write_settings_safe(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn get_available_typing_tools() -> Vec<String> {
    #[cfg(target_os = "linux")]
    {
        crate::clipboard::get_available_typing_tools()
    }
    #[cfg(not(target_os = "linux"))]
    {
        vec!["auto".to_string()]
    }
}

#[tauri::command]
#[specta::specta]
pub fn change_typing_tool_setting(app: AppHandle, tool: String) -> Result<(), String> {
    let mut settings = settings::get_settings_safe(&app);
    let parsed = match tool.as_str() {
        "auto" => TypingTool::Auto,
        "wtype" => TypingTool::Wtype,
        "kwtype" => TypingTool::Kwtype,
        "dotool" => TypingTool::Dotool,
        "ydotool" => TypingTool::Ydotool,
        "xdotool" => TypingTool::Xdotool,
        other => {
            warn!("Invalid typing tool '{}', defaulting to auto", other);
            TypingTool::Auto
        }
    };
    settings.typing_tool = parsed;
    settings::write_settings_safe(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_external_script_path_setting(
    app: AppHandle,
    path: Option<String>,
) -> Result<(), String> {
    let mut settings = settings::get_settings_safe(&app);
    settings.external_script_path = path;
    settings::write_settings_safe(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_clipboard_handling_setting(app: AppHandle, handling: String) -> Result<(), String> {
    let mut settings = settings::get_settings_safe(&app);
    let parsed = match handling.as_str() {
        "dont_modify" => ClipboardHandling::DontModify,
        "copy_to_clipboard" => ClipboardHandling::CopyToClipboard,
        other => {
            warn!(
                "Invalid clipboard handling '{}', defaulting to dont_modify",
                other
            );
            ClipboardHandling::DontModify
        }
    };
    settings.clipboard_handling = parsed;
    settings::write_settings_safe(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_auto_submit_setting(app: AppHandle, enabled: bool) -> Result<(), String> {
    let mut settings = settings::get_settings_safe(&app);
    settings.auto_submit = enabled;
    settings::write_settings_safe(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_auto_submit_key_setting(app: AppHandle, key: String) -> Result<(), String> {
    let mut settings = settings::get_settings_safe(&app);
    let parsed = match key.as_str() {
        "enter" => AutoSubmitKey::Enter,
        "ctrl_enter" => AutoSubmitKey::CtrlEnter,
        "cmd_enter" => AutoSubmitKey::CmdEnter,
        other => {
            warn!("Invalid auto submit key '{}', defaulting to enter", other);
            AutoSubmitKey::Enter
        }
    };
    settings.auto_submit_key = parsed;
    settings::write_settings_safe(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_post_process_enabled_setting(app: AppHandle, enabled: bool) -> Result<(), String> {
    let mut settings = settings::get_settings_safe(&app);
    settings.post_process_enabled = enabled;
    settings::write_settings_safe(&app, settings.clone());

    // Register or unregister the post-processing shortcut
    if let Some(binding) = settings
        .bindings
        .get("transcribe_with_post_process")
        .cloned()
    {
        if enabled {
            let _ = register_shortcut(&app, binding);
        } else {
            let _ = unregister_shortcut(&app, binding);
        }
    }

    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_experimental_enabled_setting(app: AppHandle, enabled: bool) -> Result<(), String> {
    let mut settings = settings::get_settings_safe(&app);
    settings.experimental_enabled = enabled;
    settings::write_settings_safe(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_post_process_base_url_setting(
    app: AppHandle,
    provider_id: String,
    base_url: String,
) -> Result<(), String> {
    let mut settings = settings::get_settings_safe(&app);
    let label = settings
        .post_process_provider(&provider_id)
        .map(|provider| provider.label.clone())
        .ok_or_else(|| format!("Provider '{}' not found", provider_id))?;

    let provider = settings
        .post_process_provider_mut(&provider_id)
        .ok_or_else(|| format!("Provider '{}' not found after initial lookup", provider_id))?;

    if provider.id != "custom" {
        return Err(format!(
            "Provider '{}' does not allow editing the base URL",
            label
        ));
    }

    provider.base_url = base_url;
    settings::write_settings_safe(&app, settings);
    Ok(())
}

/// Generic helper to validate provider exists
fn validate_provider_exists(
    settings: &settings::AppSettings,
    provider_id: &str,
) -> Result<(), String> {
    if !settings
        .post_process_providers
        .iter()
        .any(|provider| provider.id == provider_id)
    {
        return Err(format!("Provider '{}' not found", provider_id));
    }
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_post_process_api_key_setting(
    app: AppHandle,
    provider_id: String,
    api_key: String,
) -> Result<(), String> {
    let mut settings = settings::get_settings_safe(&app);
    validate_provider_exists(&settings, &provider_id)?;
    settings.post_process_api_keys.insert(provider_id, api_key);
    settings::write_settings_safe(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_post_process_model_setting(
    app: AppHandle,
    provider_id: String,
    model: String,
) -> Result<(), String> {
    let mut settings = settings::get_settings_safe(&app);
    validate_provider_exists(&settings, &provider_id)?;
    settings.post_process_models.insert(provider_id, model);
    settings::write_settings_safe(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn set_post_process_provider(app: AppHandle, provider_id: String) -> Result<(), String> {
    let mut settings = settings::get_settings_safe(&app);
    validate_provider_exists(&settings, &provider_id)?;
    settings.post_process_provider_id = provider_id;
    settings::write_settings_safe(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn add_post_process_prompt(
    app: AppHandle,
    name: String,
    prompt: String,
) -> Result<LLMPrompt, String> {
    let mut settings = settings::get_settings_safe(&app);

    // Generate unique ID using timestamp and random component
    let id = format!("prompt_{}", chrono::Utc::now().timestamp_millis());

    let new_prompt = LLMPrompt {
        id: id.clone(),
        name,
        prompt,
    };

    settings.post_process_prompts.push(new_prompt.clone());
    settings::write_settings_safe(&app, settings);

    Ok(new_prompt)
}

#[tauri::command]
#[specta::specta]
pub fn update_post_process_prompt(
    app: AppHandle,
    id: String,
    name: String,
    prompt: String,
) -> Result<(), String> {
    let mut settings = settings::get_settings_safe(&app);

    if let Some(existing_prompt) = settings
        .post_process_prompts
        .iter_mut()
        .find(|p| p.id == id)
    {
        existing_prompt.name = name;
        existing_prompt.prompt = prompt;
        settings::write_settings_safe(&app, settings);
        Ok(())
    } else {
        Err(format!("Prompt with id '{}' not found", id))
    }
}

#[tauri::command]
#[specta::specta]
pub fn delete_post_process_prompt(app: AppHandle, id: String) -> Result<(), String> {
    let mut settings = settings::get_settings_safe(&app);

    // Don't allow deleting the last prompt
    if settings.post_process_prompts.len() <= 1 {
        return Err("Cannot delete the last prompt".to_string());
    }

    // Find and remove the prompt
    let original_len = settings.post_process_prompts.len();
    settings.post_process_prompts.retain(|p| p.id != id);

    if settings.post_process_prompts.len() == original_len {
        return Err(format!("Prompt with id '{}' not found", id));
    }

    // If the deleted prompt was selected, select the first one or None
    if settings.post_process_selected_prompt_id.as_ref() == Some(&id) {
        settings.post_process_selected_prompt_id =
            settings.post_process_prompts.first().map(|p| p.id.clone());
    }

    settings::write_settings_safe(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn fetch_post_process_models(
    app: AppHandle,
    provider_id: String,
) -> Result<Vec<String>, String> {
    let settings = settings::get_settings_safe(&app);

    // Find the provider
    let provider = settings
        .post_process_providers
        .iter()
        .find(|p| p.id == provider_id)
        .ok_or_else(|| format!("Provider '{}' not found", provider_id))?;

    if provider.id == APPLE_INTELLIGENCE_PROVIDER_ID {
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        {
            return Ok(vec![APPLE_INTELLIGENCE_DEFAULT_MODEL_ID.to_string()]);
        }

        #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
        {
            return Err("Apple Intelligence is only available on Apple silicon Macs running macOS 15 or later.".to_string());
        }
    }

    // Get API key
    let api_key = settings
        .post_process_api_keys
        .get(&provider_id)
        .cloned()
        .unwrap_or_default();

    // Skip fetching if no API key for providers that typically need one
    if api_key.trim().is_empty() && provider.id != "custom" {
        return Err(format!(
            "API key is required for {}. Please add an API key to list available models.",
            provider.label
        ));
    }

    crate::llm_client::fetch_models(provider, api_key).await
}

#[tauri::command]
#[specta::specta]
pub fn set_post_process_selected_prompt(app: AppHandle, id: String) -> Result<(), String> {
    let mut settings = settings::get_settings_safe(&app);

    // Verify the prompt exists
    if !settings.post_process_prompts.iter().any(|p| p.id == id) {
        return Err(format!("Prompt with id '{}' not found", id));
    }

    settings.post_process_selected_prompt_id = Some(id);
    settings::write_settings_safe(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_mute_while_recording_setting(app: AppHandle, enabled: bool) -> Result<(), String> {
    let mut settings = settings::get_settings_safe(&app);
    settings.mute_while_recording = enabled;
    settings::write_settings_safe(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_append_trailing_space_setting(app: AppHandle, enabled: bool) -> Result<(), String> {
    let mut settings = settings::get_settings_safe(&app);
    settings.append_trailing_space = enabled;
    settings::write_settings_safe(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_lazy_stream_close_setting(app: AppHandle, enabled: bool) -> Result<(), String> {
    let mut settings = settings::get_settings_safe(&app);
    settings.lazy_stream_close = enabled;
    settings::write_settings_safe(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_app_language_setting(app: AppHandle, language: String) -> Result<(), String> {
    let mut settings = settings::get_settings_safe(&app);
    settings.app_language = language.clone();
    settings::write_settings_safe(&app, settings);

    // Refresh the tray menu with the new language
    tray::update_tray_menu(&app, &tray::TrayIconState::Idle, Some(&language));

    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_show_tray_icon_setting(app: AppHandle, enabled: bool) -> Result<(), String> {
    let mut settings = settings::get_settings_safe(&app);
    settings.show_tray_icon = enabled;
    settings::write_settings_safe(&app, settings);

    // Apply change immediately
    tray::set_tray_visibility(&app, enabled);

    Ok(())
}

/// Save accelerator settings, re-apply globals, and unload the model so it
/// reloads with the new backend on next transcription.
fn apply_and_reload_accelerator(app: &AppHandle, s: settings::AppSettings) {
    settings::write_settings_safe(app, s);
    crate::managers::transcription::apply_accelerator_settings(app);

    let Some(tm) = app.try_state::<std::sync::Arc<parking_lot::Mutex<crate::managers::transcription::TranscriptionManager>>>() else {
        log::warn!("TranscriptionManager not initialized, skipping model unload after accelerator change");
        return;
    };
    if tm.lock().is_model_loaded() {
        if let Err(e) = tm.lock().unload_model() {
            log::warn!("Failed to unload model after accelerator change: {e}");
        }
    }
}

#[tauri::command]
#[specta::specta]
pub fn change_transcribe_accelerator_setting(
    app: AppHandle,
    accelerator: settings::TranscribeAcceleratorSetting,
) -> Result<(), String> {
    let mut s = settings::get_settings_safe(&app);
    s.transcribe_accelerator = accelerator;
    apply_and_reload_accelerator(&app, s);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_ort_accelerator_setting(
    app: AppHandle,
    accelerator: settings::OrtAcceleratorSetting,
) -> Result<(), String> {
    let mut s = settings::get_settings_safe(&app);
    s.ort_accelerator = accelerator;
    apply_and_reload_accelerator(&app, s);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_transcribe_gpu_device(app: AppHandle, device: i32) -> Result<(), String> {
    let mut s = settings::get_settings_safe(&app);
    s.transcribe_gpu_device = device;
    apply_and_reload_accelerator(&app, s);
    Ok(())
}

/// Return which accelerators and GPU devices are available for this build.
///
/// First-call cost is dominated by enumerating GPU devices through the
/// whisper.cpp Metal/Vulkan backend, which loads dynamic libraries and
/// probes hardware. Run it on the blocking pool so the webview thread
/// stays responsive — see also the startup pre-warm in `lib.rs`.
#[tauri::command]
#[specta::specta]
pub async fn get_available_accelerators(
) -> Result<crate::managers::transcription::AvailableAccelerators, String> {
    tauri::async_runtime::spawn_blocking(crate::managers::transcription::get_available_accelerators)
        .await
        .map_err(|e| format!("get_available_accelerators task failed: {}", e))
}

#[tauri::command]
#[specta::specta]
pub fn change_vad_sensitivity_setting(app: AppHandle, sensitivity: String) -> Result<(), String> {
    let vad_sensitivity = match sensitivity.as_str() {
        "very_quick" => settings::VadSensitivity::VeryQuick,
        "quick" => settings::VadSensitivity::Quick,
        "balanced" => settings::VadSensitivity::Balanced,
        "relaxed" => settings::VadSensitivity::Relaxed,
        "very_relaxed" => settings::VadSensitivity::VeryRelaxed,
        _ => return Err(format!("Invalid VAD sensitivity: {}", sensitivity)),
    };
    let mut settings = settings::get_settings_safe(&app);
    settings.vad_sensitivity = vad_sensitivity;
    settings::write_settings_safe(&app, settings);

    // Recreate the recorder so the new VAD sensitivity takes effect immediately.
    // vad_threshold and vad_hangover_frames are read in create_audio_recorder()
    // and baked into the VAD at construction time — without this, the change
    // only takes effect on next app restart.
    if let Some(rm) =
        app.try_state::<std::sync::Arc<crate::managers::audio::AudioRecordingManager>>()
    {
        if let Err(e) = rm.recreate_recorder() {
            warn!(
                "Failed to recreate recorder after VAD sensitivity change: {}",
                e
            );
        } else {
            debug!(
                "Recorder recreated for VAD sensitivity change ({})",
                sensitivity
            );
        }
    }

    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_live_captions_enabled_setting(app: AppHandle, enabled: bool) -> Result<(), String> {
    let mut settings = settings::get_settings_safe(&app);
    settings.live_captions_enabled = enabled;
    settings::write_settings_safe(&app, settings.clone());

    // Pre-warm the model when live captions is enabled so it's ready
    // for the first recording. Without this, the model loads lazily
    // on first hotkey press, causing a delay before live captions appear.
    if enabled {
        if let Some(transcription_manager) = app.try_state::<Arc<Mutex<TranscriptionManager>>>() {
            transcription_manager.lock().initiate_model_load();
            debug!("Live captions enabled — pre-warming transcription model");
        }
    }

    // Toggle streaming transcription at runtime via the Strategy pattern.
    // Instead of recreating the entire AudioRecorder (which tears down the mic
    // stream and requires manual restart), we just toggle the streaming_enabled
    // flag on the existing recorder. This eliminates Bug 1 where toggling live
    // captions could leave the always-on mic stream dead.
    //
    // If the recorder doesn't have a streaming callback (e.g., TranscriptionManager
    // wasn't available at startup), set_streaming_enabled() will fall back to
    // recreating the recorder, which now self-heals the stream (RAII guard).
    if let Some(rm) =
        app.try_state::<std::sync::Arc<crate::managers::audio::AudioRecordingManager>>()
    {
        if let Err(e) = rm.set_streaming_enabled(enabled) {
            warn!(
                "Failed to toggle streaming transcription after live captions toggle: {}",
                e
            );
        } else {
            debug!(
                "Streaming transcription toggled for live captions (enabled={})",
                enabled
            );
        }
    } else {
        log::warn!(
            "AudioRecordingManager not initialized, skipping live captions streaming toggle"
        );
    }

    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_overlay_scale_setting(app: AppHandle, scale: f64) -> Result<(), String> {
    // Validate scale: only allow 1.0 or 2.0
    if scale != 1.0 && scale != 2.0 {
        return Err(format!(
            "Invalid overlay scale: {}. Must be 1.0 or 2.0",
            scale
        ));
    }
    let mut settings = settings::get_settings_safe(&app);
    settings.overlay_scale = scale;
    settings::write_settings_safe(&app, settings);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_noise_suppression_enabled_setting(
    app: AppHandle,
    enabled: bool,
) -> Result<(), String> {
    let mut settings = settings::get_settings_safe(&app);
    settings.noise_suppression_enabled = enabled;
    settings::write_settings_safe(&app, settings);

    // Recreate the recorder so the noise suppression toggle takes effect immediately.
    // The noise suppressor is attached via recorder.with_noise_suppressor() at
    // recorder creation time — without this, the change only takes effect on next
    // app restart.
    if let Some(rm) =
        app.try_state::<std::sync::Arc<crate::managers::audio::AudioRecordingManager>>()
    {
        if let Err(e) = rm.recreate_recorder() {
            warn!(
                "Failed to recreate recorder after noise suppression toggle: {}",
                e
            );
        } else {
            debug!(
                "Recorder recreated for noise suppression toggle (enabled={})",
                enabled
            );
        }
    }

    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_noise_suppression_level_setting(app: AppHandle, level: String) -> Result<(), String> {
    let noise_level = match level.as_str() {
        "low" => settings::NoiseSuppressionLevel::Low,
        "medium" => settings::NoiseSuppressionLevel::Medium,
        "high" => settings::NoiseSuppressionLevel::High,
        _ => return Err(format!("Invalid noise suppression level: {}", level)),
    };
    let mut settings = settings::get_settings_safe(&app);
    settings.noise_suppression_level = noise_level;
    settings::write_settings_safe(&app, settings);

    // Recreate the recorder so the new noise suppression level takes effect immediately.
    // The noise suppressor level is read in create_audio_recorder() and baked
    // into the suppressor at construction time — without this, the change only
    // takes effect on next app restart.
    if let Some(rm) =
        app.try_state::<std::sync::Arc<crate::managers::audio::AudioRecordingManager>>()
    {
        if let Err(e) = rm.recreate_recorder() {
            warn!(
                "Failed to recreate recorder after noise suppression level change: {}",
                e
            );
        } else {
            debug!(
                "Recorder recreated for noise suppression level change ({})",
                level
            );
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::get_default_settings;

    #[test]
    fn apply_cancel_binding_inserts_when_missing() {
        let mut settings = get_default_settings();
        settings.bindings.remove("cancel");
        assert!(!settings.bindings.contains_key("cancel"));

        let result = apply_cancel_binding_update(&mut settings, "cancel", "cmd+backspace".to_string());

        assert_eq!(result.current_binding, "cmd+backspace");
        assert!(settings.bindings.contains_key("cancel"));
        assert_eq!(settings.bindings["cancel"].current_binding, "cmd+backspace");
    }

    #[test]
    fn apply_cancel_binding_overrides_existing() {
        let mut settings = get_default_settings();
        assert_eq!(settings.bindings["cancel"].current_binding, "escape");

        let result = apply_cancel_binding_update(&mut settings, "cancel", "cmd+shift+x".to_string());

        assert_eq!(result.current_binding, "cmd+shift+x");
        assert_eq!(settings.bindings["cancel"].current_binding, "cmd+shift+x");
    }

    #[test]
    fn apply_cancel_binding_preserves_other_bindings() {
        let mut settings = get_default_settings();
        let original_transcribe = settings.bindings["transcribe"].clone();

        apply_cancel_binding_update(&mut settings, "cancel", "cmd+backspace".to_string());

        assert_eq!(
            settings.bindings["transcribe"].current_binding,
            original_transcribe.current_binding
        );
    }
}
