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
pub mod tauri_impl;

use log::{debug, error, info, warn};
use parking_lot::Mutex;
use serde::Serialize;
use specta::Type;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager};

use conflicts::ConflictInfo;

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
use crate::settings::APPLE_INTELLIGENCE_DEFAULT_MODEL_ID;
use crate::settings::{
    self, get_settings, AutoSubmitKey, ChineseScript, ClipboardHandling, CustomWord,
    KeyboardImplementation, LLMPrompt, OverlayPosition, OverlayScreenTarget, PasteMethod,
    ShortcutActivation, ShortcutBinding, SoundTheme, TypingTool, VadBackend,
    APPLE_INTELLIGENCE_PROVIDER_ID,
};
use crate::tray;
use crate::LiveCaptionsTranscriptionManager;

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
                settings::patch_settings(app, |s| {
                    s.keyboard_implementation = KeyboardImplementation::Tauri;
                });

                tauri_impl::init_shortcuts(app);
            }
        }
    }
}

/// Whether the recording lifecycle currently wants the cancel shortcut.
/// Written synchronously by start/stop, so it always holds the latest request.
static CANCEL_REQUESTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether the cancel shortcut is actually registered with the backend.
/// The lock also serializes reconciliation passes.
static CANCEL_REGISTERED: Mutex<bool> = Mutex::new(false);

/// Register the cancel shortcut (called when recording starts)
pub fn register_cancel_shortcut(app: &AppHandle) {
    // Track recording lifecycle independently of the current implementation so
    // switching implementations mid-recording cannot leave stale fallback state.
    crate::secure_input::register_cancel_fallback(app);

    CANCEL_REQUESTED.store(true, std::sync::atomic::Ordering::SeqCst);
    schedule_cancel_reconcile(app);
}

/// Unregister the cancel shortcut (called when recording stops)
pub fn unregister_cancel_shortcut(app: &AppHandle) {
    crate::secure_input::unregister_cancel_fallback(app);

    CANCEL_REQUESTED.store(false, std::sync::atomic::Ordering::SeqCst);
    schedule_cancel_reconcile(app);
}

/// Apply the requested cancel state off the calling thread. Start and stop run
/// on the shortcut handler thread, which must not block on registration, so the
/// work is spawned. Spawned tasks may run in any order (a very short recording
/// can see its unregister task run before its register task), so each pass
/// applies the latest request instead of the action that scheduled it.
fn schedule_cancel_reconcile(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        reconcile_cancel_shortcut(&app);
    });
}

fn reconcile_cancel_shortcut(app: &AppHandle) {
    let mut registered = CANCEL_REGISTERED.lock();
    let requested = CANCEL_REQUESTED.load(std::sync::atomic::Ordering::SeqCst);
    if requested == *registered {
        return;
    }

    let Some(cancel_binding) = get_settings(app).bindings.get("cancel").cloned() else {
        return;
    };

    if requested {
        match register_shortcut(app, cancel_binding) {
            Ok(()) => *registered = true,
            Err(e) => error!("Failed to register cancel shortcut: {}", e),
        }
    } else {
        match unregister_shortcut(app, cancel_binding) {
            Ok(()) => *registered = false,
            Err(e) => error!("Failed to unregister cancel shortcut: {}", e),
        }
    }
}

/// Register a shortcut using the appropriate implementation.
/// The implementation-specific binding string is copied into
/// `current_binding` before dispatching to the backend so existing
/// implementation code only needs to read `current_binding`.
pub fn register_shortcut(app: &AppHandle, binding: ShortcutBinding) -> Result<(), String> {
    let settings = get_settings(app);
    let mut active_binding = binding.clone();
    active_binding.current_binding = binding
        .binding_for(settings.keyboard_implementation)
        .to_string();
    match settings.keyboard_implementation {
        KeyboardImplementation::Tauri => tauri_impl::register_shortcut(app, active_binding),
        KeyboardImplementation::HandyKeys => handy_keys::register_shortcut(app, active_binding),
    }
}

/// Unregister a shortcut using the appropriate implementation.
/// The implementation-specific binding string is copied into
/// `current_binding` before dispatching to the backend.
pub fn unregister_shortcut(app: &AppHandle, binding: ShortcutBinding) -> Result<(), String> {
    let settings = get_settings(app);
    let mut active_binding = binding.clone();
    active_binding.current_binding = binding
        .binding_for(settings.keyboard_implementation)
        .to_string();
    match settings.keyboard_implementation {
        KeyboardImplementation::Tauri => tauri_impl::unregister_shortcut(app, active_binding),
        KeyboardImplementation::HandyKeys => handy_keys::unregister_shortcut(app, active_binding),
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
                tauri_binding: "escape".to_string(),
                handy_keys_binding: "escape".to_string(),
            })
    });
    b.set_binding_for(settings.keyboard_implementation, new_binding);
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

    let settings = settings::get_settings_safe(&app);

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
        let mut b_holder: Option<ShortcutBinding> = None;
        settings::patch_settings(&app, |s| {
            b_holder = Some(apply_cancel_binding_update(s, &id, binding.clone()));
        });
        let b = b_holder.expect("cancel binding updated");
        crate::secure_input::reconcile_fallback(&app);
        return Ok(BindingResponse {
            success: true,
            binding: Some(b),
            error: None,
        });
    }

    // Defensive: cancel should never reach the registration path
    debug_assert!(
        id != "cancel",
        "cancel binding should have been handled above"
    );

    // Unregister the existing binding
    if let Err(e) = unregister_shortcut(&app, binding_to_modify.clone()) {
        let error_msg = format!("Failed to unregister shortcut: {}", e);
        error!("change_binding error: {}", error_msg);
    }

    // Validate the new shortcut for the current keyboard implementation
    if let Err(e) = validate_shortcut_for_implementation(&binding, settings.keyboard_implementation)
    {
        warn!("change_binding validation error: {}", e);
        restore_registration(&app, &binding_to_modify);
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

    // Create an updated binding for the active keyboard implementation.
    // The per-implementation helper also mirrors the value to `current_binding`
    // so the frontend immediately sees the new binding.
    let mut updated_binding = binding_to_modify.clone();
    updated_binding.set_binding_for(settings.keyboard_implementation, binding);

    // Register the new binding
    if let Err(e) = register_shortcut(&app, updated_binding.clone()) {
        let error_msg = format!("Failed to register shortcut: {}", e);
        error!("change_binding error: {}", error_msg);
        restore_registration(&app, &binding_to_modify);
        return Ok(BindingResponse {
            success: false,
            binding: None,
            error: Some(error_msg),
        });
    }

    // Persist the updated binding atomically.
    settings::patch_settings(&app, |s| {
        s.bindings.insert(id.clone(), updated_binding.clone());
    });

    // Synchronize any active Secure Input shadows.
    crate::secure_input::reconcile_fallback(&app);

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
    let binding = settings::get_stored_binding(&settings::get_settings(&app), &id)?;
    change_binding(app, id, binding.default_binding)
}

/// Best-effort re-register of the previous binding after a failed change,
/// so a failure leaves the user's shortcut working exactly as before.
fn restore_registration(app: &AppHandle, binding: &ShortcutBinding) {
    if let Err(e) = register_shortcut(app, binding.clone()) {
        error!(
            "Failed to restore previous binding '{}' ({}): {}",
            binding.id, binding.current_binding, e
        );
    }
}

/// Unregister every binding while the user is recording a new shortcut in
/// the UI, so no existing shortcut can fire — or swallow the keystrokes —
/// mid-capture. The "cancel" binding is untouched: it is managed dynamically
/// by the recording lifecycle.
pub fn suspend_all_shortcuts(app: &AppHandle) {
    let settings = get_settings(app);
    for (id, mut binding) in settings.bindings {
        if id == "cancel" {
            continue;
        }
        binding.current_binding = binding
            .binding_for(settings.keyboard_implementation)
            .to_string();
        if let Err(e) = unregister_shortcut(app, binding) {
            debug!(
                "suspend_all_shortcuts: could not unregister '{}': {}",
                id, e
            );
        }
    }
}

/// Re-register every binding from settings after shortcut recording ends.
/// Registering an already-registered shortcut fails cleanly in both
/// implementations, so this is idempotent and safe on every exit path.
pub fn resume_all_shortcuts(app: &AppHandle) {
    let settings = get_settings(app);
    for (id, mut binding) in settings.bindings.clone() {
        if id == "cancel" {
            continue;
        }
        if id == "transcribe_with_post_process" && !settings.post_process_enabled {
            continue;
        }
        binding.current_binding = binding
            .binding_for(settings.keyboard_implementation)
            .to_string();
        if let Err(e) = register_shortcut(app, binding) {
            debug!("resume_all_shortcuts: could not register '{}': {}", id, e);
        }
    }
}

/// Temporarily unregister all bindings while the user is recording a
/// shortcut in the UI. This avoids firing actions while keys are recorded.
#[tauri::command]
#[specta::specta]
pub fn suspend_all_bindings(app: AppHandle) -> Result<(), String> {
    suspend_all_shortcuts(&app);
    Ok(())
}

/// Re-register all bindings after the user has finished recording.
#[tauri::command]
#[specta::specta]
pub fn resume_all_bindings(app: AppHandle) -> Result<(), String> {
    resume_all_shortcuts(&app);
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
    settings::patch_settings(&app, |s| {
        s.keyboard_implementation = new_impl;
    });

    // Carbon fallback registrations use the Tauri plugin. Remove them before
    // registering the full Tauri implementation to avoid duplicate conflicts.
    if new_impl == KeyboardImplementation::Tauri {
        crate::secure_input::reconcile_fallback(&app);
    }

    // Initialize new implementation if needed (HandyKeys needs state)
    if new_impl == KeyboardImplementation::HandyKeys {
        if initialize_handy_keys_with_rollback(&app)? {
            // Shortcuts already registered during init.
            crate::secure_input::reconcile_fallback(&app);
            return Ok(ImplementationChangeResult {
                success: true,
                reset_bindings: vec![],
            });
        }
    }

    // Register all shortcuts with new implementation, resetting invalid ones
    let reset_bindings = register_all_shortcuts_for_implementation(&app, new_impl);
    crate::secure_input::reconcile_fallback(&app);

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

        // Mirror the stored implementation-specific binding into `current_binding`
        // before dispatching, so we unregister the shortcut that was actually
        // registered under the old implementation.
        let mut active_binding = binding.clone();
        active_binding.current_binding = binding.binding_for(implementation).to_string();

        let result = match implementation {
            KeyboardImplementation::Tauri => tauri_impl::unregister_shortcut(app, active_binding),
            KeyboardImplementation::HandyKeys => {
                handy_keys::unregister_shortcut(app, active_binding)
            }
        };

        if let Err(e) = result {
            warn!(
                "Failed to unregister shortcut '{}' during switch: {}",
                id, e
            );
        }
    }
}

/// Register all shortcuts for a specific implementation, validating and resetting invalid ones.
/// Only the per-implementation field is reset; the other implementation's stored binding is
/// preserved so switching back later does not require re-recording.
fn register_all_shortcuts_for_implementation(
    app: &AppHandle,
    implementation: KeyboardImplementation,
) -> Vec<String> {
    let mut reset_bindings: Vec<(String, ShortcutBinding)> = Vec::new();
    let default_bindings = settings::get_default_settings().bindings;
    let current_settings = settings::get_settings_safe(app);

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

        let impl_binding = binding.binding_for(implementation).to_string();

        // Validate the shortcut for the target implementation
        if let Err(e) = validate_shortcut_for_implementation(&impl_binding, implementation) {
            info!(
                "Shortcut '{}' ({}) is invalid for {:?}: {}. Resetting to default.",
                id, impl_binding, implementation, e
            );

            // Reset only the per-implementation field; the other implementation's binding
            // is preserved so switching back later keeps the user's recorded shortcut.
            binding.set_binding_for(implementation, default_binding.current_binding.clone());
            reset_bindings.push((id.clone(), binding.clone()));
        }

        // Register with the appropriate implementation. Use a clone with `current_binding`
        // set to the implementation-specific value so the backend doesn't need to know
        // about the dual-storage design. Re-read from `binding` in case it was reset above.
        let mut active_binding = binding.clone();
        active_binding.current_binding = binding.binding_for(implementation).to_string();
        let result = match implementation {
            KeyboardImplementation::Tauri => tauri_impl::register_shortcut(app, active_binding),
            KeyboardImplementation::HandyKeys => handy_keys::register_shortcut(app, active_binding),
        };

        if let Err(e) = result {
            error!(
                "Failed to register shortcut '{}' for {:?}: {}",
                id, implementation, e
            );
        }
    }

    // Persist any reset bindings in a single atomic write.
    if !reset_bindings.is_empty() {
        settings::patch_settings(app, |s| {
            for (id, binding) in &reset_bindings {
                s.bindings.insert(id.clone(), binding.clone());
            }
        });
    }

    reset_bindings.into_iter().map(|(id, _)| id).collect()
}

/// Initialize HandyKeys if not already initialized, with rollback on failure
fn initialize_handy_keys_with_rollback(app: &AppHandle) -> Result<bool, String> {
    if app.try_state::<handy_keys::HandyKeysState>().is_some() {
        return Ok(false); // Already initialized, caller should continue
    }

    if let Err(e) = handy_keys::init_shortcuts(app) {
        error!("Failed to initialize HandyKeys: {}", e);
        // Rollback to Tauri
        settings::patch_settings(app, |s| {
            s.keyboard_implementation = KeyboardImplementation::Tauri;
        });
        crate::secure_input::reconcile_fallback(app);
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
pub fn change_shortcut_activation_setting(
    app: AppHandle,
    activation: ShortcutActivation,
) -> Result<(), String> {
    settings::patch_settings(&app, |s| {
        s.shortcut_activation = activation;
    });
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_hold_threshold_ms_setting(app: AppHandle, ms: u64) -> Result<(), String> {
    settings::patch_settings(&app, |s| {
        s.hold_threshold_ms = ms;
    });
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_audio_feedback_setting(app: AppHandle, enabled: bool) -> Result<(), String> {
    settings::patch_settings(&app, |s| {
        s.audio_feedback = enabled;
    });
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_audio_feedback_volume_setting(app: AppHandle, volume: f32) -> Result<(), String> {
    settings::patch_settings(&app, |s| {
        s.audio_feedback_volume = volume;
    });
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_sound_theme_setting(app: AppHandle, theme: String) -> Result<(), String> {
    let parsed = match theme.as_str() {
        "marimba" => SoundTheme::Marimba,
        "pop" => SoundTheme::Pop,
        "custom" => SoundTheme::Custom,
        other => {
            warn!("Invalid sound theme '{}', defaulting to marimba", other);
            SoundTheme::Marimba
        }
    };
    settings::patch_settings(&app, |s| {
        s.sound_theme = parsed;
    });
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_translate_to_english_setting(app: AppHandle, enabled: bool) -> Result<(), String> {
    settings::patch_settings(&app, |s| {
        s.translate_to_english = enabled;
    });
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_convert_us_to_british_setting(app: AppHandle, enabled: bool) -> Result<(), String> {
    settings::patch_settings(&app, |s| {
        s.convert_us_to_british = enabled;
    });
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_convert_number_words_to_digits_setting(
    app: AppHandle,
    enabled: bool,
) -> Result<(), String> {
    settings::patch_settings(&app, |s| {
        s.convert_number_words_to_digits = enabled;
    });
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_selected_language_setting(app: AppHandle, language: String) -> Result<(), String> {
    settings::patch_settings(&app, |s| {
        s.selected_language = language;
    });
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_overlay_position_setting(app: AppHandle, position: String) -> Result<(), String> {
    let parsed = match position.as_str() {
        "none" => OverlayPosition::None,
        "top" => OverlayPosition::Top,
        "bottom" => OverlayPosition::Bottom,
        other => {
            warn!("Invalid overlay position '{}', defaulting to bottom", other);
            OverlayPosition::Bottom
        }
    };
    settings::patch_settings(&app, |s| {
        s.overlay_position = parsed;
    });

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
    settings::patch_settings(&app, |s| {
        s.overlay_screen_target = parsed;
    });
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
    settings::patch_settings(&app, |s| {
        s.debug_mode = enabled;
    });

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
    settings::patch_settings(&app, |s| {
        s.start_hidden = enabled;
    });

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
    settings::patch_settings(&app, |s| {
        s.autostart_enabled = enabled;
    });

    // Apply the autostart setting immediately
    crate::autostart::apply_autostart(&app, enabled);

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
    settings::patch_settings(&app, |s| {
        s.update_checks_enabled = enabled;
    });

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
    settings::patch_settings(&app, |s| {
        s.custom_words = words;
    });
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_word_correction_threshold_setting(
    app: AppHandle,
    threshold: f64,
) -> Result<(), String> {
    settings::patch_settings(&app, |s| {
        s.word_correction_threshold = threshold;
    });
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn update_advanced_custom_words(app: AppHandle, words: Vec<CustomWord>) -> Result<(), String> {
    settings::patch_settings(&app, |s| {
        s.advanced_custom_words = words;
    });
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn update_custom_filler_words(app: AppHandle, words: Vec<String>) -> Result<(), String> {
    settings::patch_settings(&app, |s| {
        s.custom_filler_words = if words.is_empty() { None } else { Some(words) };
    });
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_use_advanced_custom_words_setting(
    app: AppHandle,
    enabled: bool,
) -> Result<(), String> {
    settings::patch_settings(&app, |s| {
        // Backward compatibility: map boolean to mode
        s.word_correction_mode = if enabled {
            settings::WordCorrectionMode::Pronunciation
        } else {
            settings::WordCorrectionMode::WordBias
        };
        s.use_advanced_custom_words = enabled;
    });
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_word_correction_mode(
    app: AppHandle,
    mode: settings::WordCorrectionMode,
) -> Result<(), String> {
    settings::patch_settings(&app, |s| {
        s.word_correction_mode = mode;
        // Keep use_advanced_custom_words in sync for backward compatibility
        s.use_advanced_custom_words = mode == settings::WordCorrectionMode::Pronunciation;
    });
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn update_word_replacements(
    app: AppHandle,
    replacements: Vec<settings::WordReplacement>,
) -> Result<(), String> {
    settings::patch_settings(&app, |s| {
        s.word_replacements = replacements;
    });
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_extra_recording_buffer_setting(app: AppHandle, ms: u64) -> Result<(), String> {
    settings::patch_settings(&app, |s| {
        s.extra_recording_buffer_ms = ms;
    });
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_pre_recording_buffer_setting(app: AppHandle, ms: u64) -> Result<(), String> {
    settings::patch_settings(&app, |s| {
        s.pre_recording_buffer_ms = ms;
    });

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
    settings::patch_settings(&app, |s| {
        s.paste_delay_ms = ms;
    });
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_paste_method_setting(app: AppHandle, method: String) -> Result<(), String> {
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
    settings::patch_settings(&app, |s| {
        s.paste_method = parsed;
    });
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
    settings::patch_settings(&app, |s| {
        s.typing_tool = parsed;
    });
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_external_script_path_setting(
    app: AppHandle,
    path: Option<String>,
) -> Result<(), String> {
    settings::patch_settings(&app, |s| {
        s.external_script_path = path;
    });
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_clipboard_handling_setting(app: AppHandle, handling: String) -> Result<(), String> {
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
    settings::patch_settings(&app, |s| {
        s.clipboard_handling = parsed;
    });
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_auto_submit_setting(app: AppHandle, enabled: bool) -> Result<(), String> {
    settings::patch_settings(&app, |s| {
        s.auto_submit = enabled;
    });
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_auto_submit_key_setting(app: AppHandle, key: String) -> Result<(), String> {
    let parsed = match key.as_str() {
        "enter" => AutoSubmitKey::Enter,
        "ctrl_enter" => AutoSubmitKey::CtrlEnter,
        "cmd_enter" => AutoSubmitKey::CmdEnter,
        other => {
            warn!("Invalid auto submit key '{}', defaulting to enter", other);
            AutoSubmitKey::Enter
        }
    };
    settings::patch_settings(&app, |s| {
        s.auto_submit_key = parsed;
    });
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_post_process_enabled_setting(app: AppHandle, enabled: bool) -> Result<(), String> {
    let settings = settings::get_settings_safe(&app);
    let binding = settings
        .bindings
        .get("transcribe_with_post_process")
        .cloned();

    settings::patch_settings(&app, |s| {
        s.post_process_enabled = enabled;
    });

    // Register or unregister the post-processing shortcut
    if let Some(binding) = binding {
        if enabled {
            let _ = register_shortcut(&app, binding);
        } else {
            let _ = unregister_shortcut(&app, binding);
        }
    }

    crate::secure_input::reconcile_fallback(&app);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_experimental_enabled_setting(app: AppHandle, enabled: bool) -> Result<(), String> {
    settings::patch_settings(&app, |s| {
        s.experimental_enabled = enabled;
    });
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_post_process_base_url_setting(
    app: AppHandle,
    provider_id: String,
    base_url: String,
) -> Result<(), String> {
    settings::patch_settings_try(&app, |s| {
        let label = s
            .post_process_provider(&provider_id)
            .map(|provider| provider.label.clone())
            .ok_or_else(|| format!("Provider '{}' not found", provider_id))?;

        let provider = s
            .post_process_provider_mut(&provider_id)
            .ok_or_else(|| format!("Provider '{}' not found after initial lookup", provider_id))?;

        if provider.id != "custom" {
            return Err(format!(
                "Provider '{}' does not allow editing the base URL",
                label
            ));
        }

        provider.base_url = base_url.clone();
        Ok(())
    })
}

#[tauri::command]
#[specta::specta]
pub fn change_post_process_api_format_setting(
    app: AppHandle,
    provider_id: String,
    api_format: settings::PostProcessApiFormat,
) -> Result<(), String> {
    settings::patch_settings_try(&app, |s| {
        let label = s
            .post_process_provider(&provider_id)
            .map(|provider| provider.label.clone())
            .ok_or_else(|| format!("Provider '{}' not found", provider_id))?;

        let provider = s
            .post_process_provider_mut(&provider_id)
            .ok_or_else(|| format!("Provider '{}' not found after initial lookup", provider_id))?;

        // Only the custom provider is user-editable; built-in providers are synced
        // from defaults at load time.
        if provider.id != "custom" {
            return Err(format!(
                "Provider '{}' does not allow editing the API format",
                label
            ));
        }

        provider.api_format = api_format;
        Ok(())
    })
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
    settings::patch_settings_try(&app, |s| {
        validate_provider_exists(s, &provider_id)?;
        s.post_process_api_keys
            .insert(provider_id.clone(), api_key.clone());
        Ok(())
    })
}

#[tauri::command]
#[specta::specta]
pub fn change_post_process_model_setting(
    app: AppHandle,
    provider_id: String,
    model: String,
) -> Result<(), String> {
    settings::patch_settings_try(&app, |s| {
        validate_provider_exists(s, &provider_id)?;
        s.post_process_models
            .insert(provider_id.clone(), model.clone());
        Ok(())
    })
}

#[tauri::command]
#[specta::specta]
pub fn set_post_process_provider(app: AppHandle, provider_id: String) -> Result<(), String> {
    settings::patch_settings_try(&app, |s| {
        validate_provider_exists(s, &provider_id)?;
        s.post_process_provider_id = provider_id.clone();
        Ok(())
    })
}

#[tauri::command]
#[specta::specta]
pub fn add_post_process_prompt(
    app: AppHandle,
    name: String,
    prompt: String,
) -> Result<LLMPrompt, String> {
    // Generate unique ID using timestamp and random component
    let id = format!("prompt_{}", chrono::Utc::now().timestamp_millis());

    let new_prompt = LLMPrompt {
        id: id.clone(),
        name,
        prompt,
    };

    settings::patch_settings(&app, |s| {
        s.post_process_prompts.push(new_prompt.clone());
    });

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
    settings::patch_settings_try(&app, |s| {
        if let Some(existing_prompt) = s.post_process_prompts.iter_mut().find(|p| p.id == id) {
            existing_prompt.name = name.clone();
            existing_prompt.prompt = prompt.clone();
            Ok(())
        } else {
            Err(format!("Prompt with id '{}' not found", id))
        }
    })
}

#[tauri::command]
#[specta::specta]
pub fn delete_post_process_prompt(app: AppHandle, id: String) -> Result<(), String> {
    settings::patch_settings_try(&app, |s| {
        // Don't allow deleting the last prompt
        if s.post_process_prompts.len() <= 1 {
            return Err("Cannot delete the last prompt".to_string());
        }

        // Find and remove the prompt
        let original_len = s.post_process_prompts.len();
        s.post_process_prompts.retain(|p| p.id != id);

        if s.post_process_prompts.len() == original_len {
            return Err(format!("Prompt with id '{}' not found", id));
        }

        // If the deleted prompt was selected, select the first one or None
        if s.post_process_selected_prompt_id.as_ref() == Some(&id) {
            s.post_process_selected_prompt_id =
                s.post_process_prompts.first().map(|p| p.id.clone());
        }

        Ok(())
    })
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
    settings::patch_settings_try(&app, |s| {
        // Verify the prompt exists
        if !s.post_process_prompts.iter().any(|p| p.id == id) {
            return Err(format!("Prompt with id '{}' not found", id));
        }

        s.post_process_selected_prompt_id = Some(id.clone());
        Ok(())
    })
}

#[tauri::command]
#[specta::specta]
pub fn change_mute_while_recording_setting(app: AppHandle, enabled: bool) -> Result<(), String> {
    settings::patch_settings(&app, |s| {
        s.mute_while_recording = enabled;
    });
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_append_trailing_space_setting(app: AppHandle, enabled: bool) -> Result<(), String> {
    settings::patch_settings(&app, |s| {
        s.append_trailing_space = enabled;
    });
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_lazy_stream_close_setting(app: AppHandle, enabled: bool) -> Result<(), String> {
    settings::patch_settings(&app, |s| {
        s.lazy_stream_close = enabled;
    });
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_filler_word_removal_enabled_setting(
    app: AppHandle,
    enabled: bool,
) -> Result<(), String> {
    settings::patch_settings(&app, |s| {
        s.filler_word_removal_enabled = enabled;
    });
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_chinese_script_setting(app: AppHandle, script: ChineseScript) -> Result<(), String> {
    settings::patch_settings(&app, |s| {
        s.chinese_script = script;
    });
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn change_app_language_setting(app: AppHandle, language: String) -> Result<(), String> {
    settings::patch_settings(&app, |s| {
        s.app_language = language.clone();
    });

    // Refresh the tray menu with the new language, but do it off the
    // synchronous command path. The tray rebuild touches backend locks
    // (transcription engine, available models) that can be held by streaming
    // or model threads; running it inline here blocks the main thread and
    // freezes the UI. Scheduling it on the async runtime lets the IPC call
    // return immediately, and the best-effort reads inside the tray code
    // prevent the actual rebuild from hanging the event loop.
    let app_for_tray = app.clone();
    let language_for_tray = language.clone();
    tauri::async_runtime::spawn(async move {
        let app_inside = app_for_tray.clone();
        let _ = app_for_tray.run_on_main_thread(move || {
            tray::update_tray_menu(
                &app_inside,
                &tray::TrayIconState::Idle,
                Some(&language_for_tray),
            );
        });
    });

    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_show_tray_icon_setting(app: AppHandle, enabled: bool) -> Result<(), String> {
    settings::patch_settings(&app, |s| {
        s.show_tray_icon = enabled;
    });

    // Apply change immediately
    tray::set_tray_visibility(&app, enabled);

    Ok(())
}

/// Request an unload of the transcription model so it reloads with the new
/// accelerator on next use.
fn unload_transcription_model_for_accelerator_change(app: &AppHandle) {
    let Some(tm) = app.try_state::<std::sync::Arc<parking_lot::Mutex<crate::managers::transcription::TranscriptionManager>>>() else {
        log::warn!("TranscriptionManager not initialized, skipping model unload after accelerator change");
        return;
    };
    if tm.lock().is_model_loaded() {
        // Request the unload without waiting for the transcription worker to
        // exit, so the settings command never blocks behind a transcription.
        // The next model use loads fresh with the new accelerator.
        tm.lock().request_unload();
    }
}

#[tauri::command]
#[specta::specta]
pub fn change_transcribe_accelerator_setting(
    app: AppHandle,
    accelerator: settings::TranscribeAcceleratorSetting,
) -> Result<(), String> {
    settings::patch_settings(&app, |s| {
        s.transcribe_accelerator = accelerator;
    });
    unload_transcription_model_for_accelerator_change(&app);
    // An explicit accelerator change is the user's way of trying the GPU
    // again: forget any earlier GPU failure so the next load may use (and
    // list) the GPU.
    if let Some(tm) = app.try_state::<std::sync::Arc<parking_lot::Mutex<crate::managers::transcription::TranscriptionManager>>>() {
        tm.lock()
            .retry_transcribe_gpu("the transcribe.cpp accelerator setting changed");
    }
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_transcribe_gpu_device(app: AppHandle, device: Option<String>) -> Result<(), String> {
    settings::patch_settings(&app, |s| {
        s.transcribe_gpu_device = device;
    });
    unload_transcription_model_for_accelerator_change(&app);
    // An explicit GPU device change is the user's way of trying the GPU
    // again: forget any earlier GPU failure so the next load may use (and
    // list) the GPU.
    if let Some(tm) = app.try_state::<std::sync::Arc<parking_lot::Mutex<crate::managers::transcription::TranscriptionManager>>>() {
        tm.lock()
            .retry_transcribe_gpu("the transcribe.cpp GPU device setting changed");
    }
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
    app: AppHandle,
) -> Result<crate::managers::transcription::AvailableAccelerators, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let tm = app.state::<std::sync::Arc<
            parking_lot::Mutex<crate::managers::transcription::TranscriptionManager>,
        >>();
        let tm = tm.lock();
        crate::managers::transcription::get_available_accelerators(&tm)
    })
    .await
    .map_err(|e| format!("get_available_accelerators task failed: {}", e))
}

/// Parse a string into a `VadSensitivity` enum value.
///
/// Returns `Ok(None)` if the input is `None`, allowing callers to treat a
/// missing value as "remove any override".
fn parse_vad_sensitivity(
    sensitivity: Option<String>,
) -> Result<Option<settings::VadSensitivity>, String> {
    match sensitivity {
        None => Ok(None),
        Some(s) => match s.as_str() {
            "very_relaxed" => Ok(Some(settings::VadSensitivity::VeryRelaxed)),
            "relaxed" => Ok(Some(settings::VadSensitivity::Relaxed)),
            "balanced" => Ok(Some(settings::VadSensitivity::Balanced)),
            "quick" => Ok(Some(settings::VadSensitivity::Quick)),
            "very_quick" => Ok(Some(settings::VadSensitivity::VeryQuick)),
            _ => Err(format!("Invalid VAD sensitivity: {}", s)),
        },
    }
}

#[tauri::command]
#[specta::specta]
pub fn change_vad_sensitivity_setting(app: AppHandle, sensitivity: String) -> Result<(), String> {
    let vad_sensitivity = parse_vad_sensitivity(Some(sensitivity.clone()))?
        .ok_or_else(|| "VAD sensitivity cannot be empty".to_string())?;

    info!("Changing VAD sensitivity to '{}'", sensitivity);

    settings::patch_settings(&app, |s| {
        s.vad_sensitivity = vad_sensitivity;
    });
    info!("VAD sensitivity '{}' saved to settings cache", sensitivity);

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
pub async fn change_vad_backend_setting(app: AppHandle, backend: VadBackend) -> Result<(), String> {
    if settings::get_settings(&app).vad_backend == backend {
        return Ok(());
    }

    // Construct/swap the detector and, when necessary, reopen cpal away from
    // the webview thread. Persist only after the runtime change succeeds so a
    // rejected in-progress switch or failed microphone reopen rolls back cleanly.
    let manager = app
        .state::<std::sync::Arc<crate::managers::audio::AudioRecordingManager>>()
        .inner()
        .clone();
    tokio::task::spawn_blocking(move || manager.update_vad_backend(backend))
        .await
        .map_err(|e| format!("audio task join failed: {e}"))?
        .map_err(|e| format!("Failed to update VAD backend: {e}"))?;

    settings::patch_settings(&app, |s| {
        s.vad_backend = backend;
    });
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn set_model_vad_sensitivity(
    app: AppHandle,
    model_id: String,
    sensitivity: Option<String>,
) -> Result<(), String> {
    let parsed = parse_vad_sensitivity(sensitivity)?;

    settings::patch_settings(&app, |s| match parsed {
        Some(v) => {
            info!(
                "Setting per-model VAD sensitivity for '{}' to '{:?}'",
                model_id, v
            );
            s.per_model_vad_sensitivity.insert(model_id.clone(), v);
        }
        None => {
            info!(
                "Removing per-model VAD sensitivity override for '{}'",
                model_id
            );
            s.per_model_vad_sensitivity.remove(&model_id);
        }
    });

    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_live_captions_enabled_setting(app: AppHandle, enabled: bool) -> Result<(), String> {
    let settings = settings::get_settings_safe(&app);
    let live_captions_model = settings.live_captions_model.clone();

    settings::patch_settings(&app, |s| {
        s.live_captions_enabled = enabled;
    });

    // Pre-warm the model when live captions is enabled AND a dedicated live
    // captions model is configured. Without this, the model loads lazily on
    // first hotkey press, causing a delay before live captions appear.
    // We intentionally target the dedicated live captions TM here, never the
    // main TM, so the two transcription paths do not steal each other's engine.
    if enabled && !live_captions_model.is_empty() {
        if let Some(live_captions_tm) = app.try_state::<LiveCaptionsTranscriptionManager>() {
            live_captions_tm
                .0
                .lock()
                .initiate_model_load_for(&live_captions_model);
            debug!(
                "Live captions enabled — pre-warming live captions model '{}'",
                live_captions_model
            );
        }
    }

    // When disabling live captions, cancel any active live captions stream
    // worker so it stops consuming audio immediately. This is especially
    // important when a dedicated live captions model is configured, because
    // the streaming router may still be feeding it frames.
    if !enabled && !live_captions_model.is_empty() {
        if let Some(live_captions_tm) = app.try_state::<LiveCaptionsTranscriptionManager>() {
            live_captions_tm.0.lock().cancel_stream();
            debug!("Live captions disabled — cancelling active live captions stream");
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
pub fn change_long_form_chunking_setting(app: AppHandle, enabled: bool) -> Result<(), String> {
    settings::patch_settings(&app, |s| {
        s.long_form_chunking = enabled;
    });
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_live_captions_model_setting(app: AppHandle, model_id: String) -> Result<(), String> {
    settings::patch_settings(&app, |s| {
        s.live_captions_model = model_id.clone();
    });

    // If the user selected a specific live captions model, pre-warm it so
    // it's ready for the first recording. An empty string means "use the
    // main transcription model", in which case the main model load already
    // handles pre-warming.
    if !model_id.is_empty() {
        if let Some(live_captions_tm) = app.try_state::<LiveCaptionsTranscriptionManager>() {
            live_captions_tm.0.lock().initiate_model_load_for(&model_id);
            debug!(
                "Live captions model set to '{}' — pre-warming live captions model",
                model_id
            );
        }
    }

    // Recreate the recorder so the stream router is attached (when a dedicated
    // live captions model is selected) or detached (when the model is cleared).
    // The recorder's create_audio_recorder() reads live_captions_model and
    // decides whether to wire the streaming callback to the live captions TM or
    // the main TM, so a recreation is required for the change to take effect.
    if let Some(rm) =
        app.try_state::<std::sync::Arc<crate::managers::audio::AudioRecordingManager>>()
    {
        if let Err(e) = rm.recreate_recorder() {
            warn!(
                "Failed to recreate recorder after live captions model change: {}",
                e
            );
            return Err(format!("Failed to apply live captions model change: {}", e));
        } else {
            debug!(
                "Recorder recreated for live captions model change (model_id='{}')",
                model_id
            );
        }
    } else {
        log::warn!(
            "AudioRecordingManager not initialized, skipping recorder recreation after live captions model change"
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
    settings::patch_settings(&app, |s| {
        s.overlay_scale = scale;
    });
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn change_noise_suppression_enabled_setting(
    app: AppHandle,
    enabled: bool,
) -> Result<(), String> {
    settings::patch_settings(&app, |s| {
        s.noise_suppression_enabled = enabled;
    });

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
    settings::patch_settings(&app, |s| {
        s.noise_suppression_level = noise_level;
    });

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

        let result =
            apply_cancel_binding_update(&mut settings, "cancel", "cmd+backspace".to_string());

        assert_eq!(result.current_binding, "cmd+backspace");
        assert!(settings.bindings.contains_key("cancel"));
        assert_eq!(settings.bindings["cancel"].current_binding, "cmd+backspace");
    }

    #[test]
    fn apply_cancel_binding_overrides_existing() {
        let mut settings = get_default_settings();
        assert_eq!(settings.bindings["cancel"].current_binding, "escape");

        let result =
            apply_cancel_binding_update(&mut settings, "cancel", "cmd+shift+x".to_string());

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

#[cfg(test)]
mod compound_key_tests {
    use handy_keys::Hotkey;
    use tauri_plugin_global_shortcut::Shortcut;

    #[test]
    fn compound_shortcut_keys_parse_on_both_backends() {
        for key in [
            "scrolllock",
            "capslock",
            "numlock",
            "pageup",
            "pagedown",
            "printscreen",
        ] {
            assert!(key.parse::<Shortcut>().is_ok(), "Tauri rejected {key}");
            assert!(key.parse::<Hotkey>().is_ok(), "HandyKeys rejected {key}");
        }
    }
}
