// Settings store operations: load, save, flush, debounced writer,
// sanitization, and migration helpers.

use log::{debug, error, warn};
use std::collections::HashMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;
use tauri::{AppHandle, Manager};
use tauri_plugin_store::StoreExt;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use super::defaults::get_default_settings;
use super::types::*;

/// Validate that float fields are not NaN before serialization.
pub(crate) fn sanitize_floats(settings: &mut AppSettings) {
    if settings.audio_feedback_volume.is_nan() {
        error!("audio_feedback_volume is NaN, resetting to default");
        settings.audio_feedback_volume = default_audio_feedback_volume();
    }
    if settings.word_correction_threshold.is_nan() {
        error!("word_correction_threshold is NaN, resetting to default");
        settings.word_correction_threshold = default_word_correction_threshold();
    }
    if settings.overlay_scale.is_nan() {
        error!("overlay_scale is NaN, resetting to default");
        settings.overlay_scale = default_overlay_scale();
    }
    if settings.hybrid_threshold_secs.is_nan() {
        error!("hybrid_threshold_secs is NaN, resetting to default");
        settings.hybrid_threshold_secs = default_hybrid_threshold_secs();
    }
}

/// Helper: serialize settings to a serde_json::Value, logging errors instead of panicking.
pub(crate) fn settings_to_value(settings: &AppSettings) -> Option<serde_json::Value> {
    match serde_json::to_value(settings) {
        Ok(v) => Some(v),
        Err(e) => {
            error!("Failed to serialize settings to JSON: {}", e);
            None
        }
    }
}

/// Helper: open the settings store, logging errors instead of panicking.
pub(crate) fn open_settings_store(
    app: &AppHandle,
) -> Option<Arc<tauri_plugin_store::Store<tauri::Wry>>> {
    match app.store(crate::portable::store_path(SETTINGS_STORE_PATH)) {
        Ok(store) => Some(store),
        Err(e) => {
            error!("Failed to initialize settings store: {}", e);
            None
        }
    }
}

/// Execute a settings operation safely, catching any panics before they can
/// propagate to WebKit's URL scheme handler (which calls `abort()` on panic).
pub(crate) fn safe_settings_operation<F, T>(label: &str, op: F) -> Option<T>
where
    F: FnOnce() -> T,
{
    match catch_unwind(AssertUnwindSafe(op)) {
        Ok(result) => Some(result),
        Err(panic_info) => {
            error!(
                "Panic in settings operation ({}) — caught to prevent WebKit abort: {:?}",
                label, panic_info
            );
            None
        }
    }
}

/// Ensure post-process providers have default entries. Returns true if settings changed.
pub(crate) fn ensure_post_process_defaults(settings: &mut AppSettings) -> bool {
    let mut changed = false;
    for provider in default_post_process_providers() {
        match settings
            .post_process_providers
            .iter_mut()
            .find(|p| p.id == provider.id)
        {
            Some(existing) => {
                if existing.supports_structured_output != provider.supports_structured_output {
                    debug!(
                        "Updating supports_structured_output for provider '{}' from {} to {}",
                        provider.id,
                        existing.supports_structured_output,
                        provider.supports_structured_output
                    );
                    existing.supports_structured_output = provider.supports_structured_output;
                    changed = true;
                }
            }
            None => {
                settings.post_process_providers.push(provider.clone());
                changed = true;
            }
        }

        if !settings.post_process_api_keys.contains_key(&provider.id) {
            settings
                .post_process_api_keys
                .insert(provider.id.clone(), String::new());
            changed = true;
        }

        let default_model = default_model_for_provider(&provider.id);
        match settings.post_process_models.get_mut(&provider.id) {
            Some(existing) => {
                if existing.is_empty() && !default_model.is_empty() {
                    *existing = default_model.clone();
                    changed = true;
                }
            }
            None => {
                settings
                    .post_process_models
                    .insert(provider.id.clone(), default_model);
                changed = true;
            }
        }
    }

    changed
}

// ── Safe wrappers ──

/// Safe wrapper around [`load_or_create_app_settings`] that catches panics
/// and falls back to defaults.
pub fn load_or_create_app_settings_safe(app: &AppHandle) -> AppSettings {
    safe_settings_operation("load_or_create_app_settings", || {
        load_or_create_app_settings(app)
    })
    .unwrap_or_else(|| {
        error!("Falling back to default settings after panic in load_or_create_app_settings");
        get_default_settings()
    })
}

/// Safe wrapper around [`get_settings`] that catches panics and falls back to defaults.
pub fn get_settings_safe(app: &AppHandle) -> AppSettings {
    safe_settings_operation("get_settings", || get_settings(app)).unwrap_or_else(|| {
        error!("Falling back to default settings after panic in get_settings");
        get_default_settings()
    })
}

/// Safe wrapper around [`write_settings`] that catches panics.
pub fn write_settings_safe(app: &AppHandle, settings: AppSettings) {
    let _ = safe_settings_operation("write_settings", || {
        write_settings(app, settings);
    });
}

/// Safe wrapper around [`write_settings_immediate`] that catches panics.
#[allow(dead_code)]
pub fn write_settings_immediate_safe(app: &AppHandle, settings: AppSettings) {
    let _ = safe_settings_operation("write_settings_immediate", || {
        write_settings_immediate(app, settings);
    });
}

// ── Core load/save functions ──

pub fn load_or_create_app_settings(app: &AppHandle) -> AppSettings {
    // Initialize store
    let Some(store) = open_settings_store(app) else {
        error!("Cannot load settings: store initialization failed, returning defaults");
        return get_default_settings();
    };

    let mut settings = if let Some(settings_value) = store.get("settings") {
        match serde_json::from_value::<AppSettings>(settings_value) {
            Ok(mut settings) => {
                debug!("Found existing settings: {:?}", settings);
                let default_settings = get_default_settings();
                let mut updated = false;

                // Merge default bindings into existing settings
                for (key, value) in default_settings.bindings {
                    if !settings.bindings.contains_key(&key) {
                        debug!("Adding missing binding: {}", key);
                        settings.bindings.insert(key, value);
                        updated = true;
                    }
                }

                // Migrate new settings fields
                if settings.router_script_path.is_none()
                    && default_settings.router_script_path.is_some()
                {
                    debug!("Migrating router_script_path from default");
                    settings.router_script_path = default_settings.router_script_path.clone();
                    updated = true;
                }
                if settings.router_env_file.is_none() && default_settings.router_env_file.is_some()
                {
                    debug!("Migrating router_env_file from default");
                    settings.router_env_file = default_settings.router_env_file.clone();
                    updated = true;
                }

                // Migrate usb_watchdog_cycle_on_wake
                if settings.usb_watchdog_enabled && !settings.usb_watchdog_cycle_on_wake {
                    debug!("Migrating usb_watchdog_cycle_on_wake to true for enabled watchdog");
                    settings.usb_watchdog_cycle_on_wake = true;
                    updated = true;
                }

                // Migrate use_advanced_custom_words to word_correction_mode
                if settings.use_advanced_custom_words
                    && settings.word_correction_mode == WordCorrectionMode::WordBias
                {
                    debug!("Migrating use_advanced_custom_words=true to word_correction_mode=Pronunciation");
                    settings.word_correction_mode = WordCorrectionMode::Pronunciation;
                    updated = true;
                }

                if updated {
                    debug!("Settings updated with new bindings");
                    sanitize_floats(&mut settings);
                    if let Some(value) = settings_to_value(&settings) {
                        store.set("settings", value);
                        let _ = store.save();
                    }
                }

                settings
            }
            Err(e) => {
                warn!("Failed to parse settings: {}", e);
                let default_settings = get_default_settings();
                if let Some(value) = settings_to_value(&default_settings) {
                    store.set("settings", value);
                }
                default_settings
            }
        }
    } else {
        let default_settings = get_default_settings();
        if let Some(value) = settings_to_value(&default_settings) {
            store.set("settings", value);
        }
        default_settings
    };

    if ensure_post_process_defaults(&mut settings) {
        sanitize_floats(&mut settings);
        if let Some(value) = settings_to_value(&settings) {
            store.set("settings", value);
        }
    }

    settings
}

pub fn get_settings(app: &AppHandle) -> AppSettings {
    let Some(store) = open_settings_store(app) else {
        error!("Cannot get settings: store initialization failed, returning defaults");
        return get_default_settings();
    };

    let mut settings = if let Some(settings_value) = store.get("settings") {
        serde_json::from_value::<AppSettings>(settings_value).unwrap_or_else(|e| {
            warn!("Failed to parse settings: {}, returning defaults", e);
            let default_settings = get_default_settings();
            if let Some(value) = settings_to_value(&default_settings) {
                store.set("settings", value);
            }
            default_settings
        })
    } else {
        let default_settings = get_default_settings();
        if let Some(value) = settings_to_value(&default_settings) {
            store.set("settings", value);
        }
        default_settings
    };

    // Migrate new settings fields that may be None in existing configs
    let default_settings = get_default_settings();
    let mut needs_save = false;

    if settings.router_script_path.is_none() && default_settings.router_script_path.is_some() {
        debug!("Migrating router_script_path from default");
        settings.router_script_path = default_settings.router_script_path.clone();
        needs_save = true;
    }
    if settings.router_env_file.is_none() && default_settings.router_env_file.is_some() {
        debug!("Migrating router_env_file from default");
        settings.router_env_file = default_settings.router_env_file.clone();
        needs_save = true;
    }

    if settings.usb_watchdog_enabled && !settings.usb_watchdog_cycle_on_wake {
        debug!("Migrating usb_watchdog_cycle_on_wake to true for enabled watchdog");
        settings.usb_watchdog_cycle_on_wake = true;
        needs_save = true;
    }

    // Merge missing bindings too
    for (key, value) in default_settings.bindings {
        if !settings.bindings.contains_key(&key) {
            debug!("Adding missing binding: {}", key);
            settings.bindings.insert(key, value);
            needs_save = true;
        }
    }

    if needs_save {
        sanitize_floats(&mut settings);
        if let Some(value) = settings_to_value(&settings) {
            store.set("settings", value);
            let _ = store.save();
        }
    }

    if ensure_post_process_defaults(&mut settings) {
        sanitize_floats(&mut settings);
        if let Some(value) = settings_to_value(&settings) {
            store.set("settings", value);
        }
    }

    settings
}

/// Write settings to disk using the debounced writer.
pub fn write_settings(app: &AppHandle, settings: AppSettings) {
    let _ = safe_settings_operation("write_settings", || {
        if let Some(writer) = app.try_state::<Arc<SettingsWriter>>() {
            let app_clone = app.clone();
            let writer = writer.inner().clone();
            tokio::spawn(async move {
                writer.write(app_clone, settings).await;
            });
        } else {
            write_settings_immediate(app, settings);
        }
    });
}

/// Write settings to disk immediately, bypassing the debounce timer.
pub fn write_settings_immediate(app: &AppHandle, mut settings: AppSettings) {
    let _ = safe_settings_operation("write_settings_immediate", || {
        let Some(store) = open_settings_store(app) else {
            error!("Cannot write settings: store initialization failed, settings not saved");
            return;
        };

        sanitize_floats(&mut settings);

        let Some(value) = settings_to_value(&settings) else {
            error!("Cannot write settings: serialization failed, settings not saved");
            return;
        };

        store.set("settings", value);
        let _ = store.save();
    });
}

/// Flush any pending debounced settings to disk.
pub fn flush_settings(app: &AppHandle) {
    let _ = safe_settings_operation("flush_settings", || {
        if let Some(writer) = app.try_state::<Arc<SettingsWriter>>() {
            let writer = writer.inner().clone();
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    writer.flush(app).await;
                })
            });
        }
    });
}

pub fn get_bindings(app: &AppHandle) -> HashMap<String, ShortcutBinding> {
    let settings = get_settings_safe(app);
    settings.bindings
}

pub fn get_stored_binding(app: &AppHandle, id: &str) -> ShortcutBinding {
    let bindings = get_bindings(app);

    if let Some(binding) = bindings.get(id) {
        return binding.clone();
    }

    warn!(
        "Binding '{}' not found in current settings, falling back to defaults",
        id
    );
    let default_settings = get_default_settings();

    if let Some(default_binding) = default_settings.bindings.get(id) {
        return default_binding.clone();
    }

    warn!(
        "Binding '{}' not found in defaults either, creating fallback binding",
        id
    );
    ShortcutBinding {
        id: id.to_string(),
        name: id.to_string(),
        description: format!("{} shortcut", id),
        default_binding: String::new(),
        current_binding: String::new(),
    }
}

pub fn get_history_limit(app: &AppHandle) -> usize {
    let settings = get_settings_safe(app);
    settings.history_limit
}

pub fn get_recording_retention_period(app: &AppHandle) -> RecordingRetentionPeriod {
    let settings = get_settings_safe(app);
    settings.recording_retention_period
}

// ── Debounced settings writer ──

/// Default debounce interval in milliseconds.
pub const SETTINGS_DEBOUNCE_MS: u64 = 500;

/// State for the debounced settings writer.
pub struct SettingsWriter {
    pending: Mutex<Option<AppSettings>>,
    timer: Mutex<Option<JoinHandle<()>>>,
    debounce_ms: u64,
}

impl SettingsWriter {
    /// Create a new writer with the default debounce interval.
    pub fn new() -> Self {
        Self {
            pending: Mutex::new(None),
            timer: Mutex::new(None),
            debounce_ms: SETTINGS_DEBOUNCE_MS,
        }
    }

    /// Create a writer with a custom debounce interval (useful in tests).
    #[allow(dead_code)]
    pub fn with_debounce_ms(ms: u64) -> Self {
        Self {
            pending: Mutex::new(None),
            timer: Mutex::new(None),
            debounce_ms: ms,
        }
    }

    /// Schedule a settings write. If a write is already pending the new value
    /// replaces it and the debounce timer is restarted.
    pub async fn write(&self, app: AppHandle, settings: AppSettings) {
        {
            let mut pending = self.pending.lock().await;
            *pending = Some(settings);
        }

        {
            let mut timer = self.timer.lock().await;
            if let Some(handle) = timer.take() {
                handle.abort();
            }
        }

        let debounce_ms = self.debounce_ms;
        let new_handle = tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_millis(debounce_ms)).await;

            let Some(writer) = app.try_state::<Arc<SettingsWriter>>() else {
                warn!("SettingsWriter not available, skipping debounced flush");
                return;
            };
            writer.flush_inner(&app).await;
        });

        {
            let mut timer = self.timer.lock().await;
            *timer = Some(new_handle);
        }
    }

    /// Flush any pending settings to disk immediately.
    pub async fn flush(&self, app: &AppHandle) {
        {
            let mut timer = self.timer.lock().await;
            if let Some(handle) = timer.take() {
                handle.abort();
            }
        }
        self.flush_inner(app).await;
    }

    /// Internal flush: write the pending settings (if any) to the store.
    async fn flush_inner(&self, app: &AppHandle) {
        let maybe_settings = {
            let mut pending = self.pending.lock().await;
            pending.take()
        };

        if let Some(settings) = maybe_settings {
            debug!("Flushing debounced settings to disk");
            write_settings_immediate(app, settings);
        }
    }
}

// ── Tests ──

#[cfg(test)]
#[allow(unused_imports)]
mod tests {
    use super::*;

    #[test]
    fn default_settings_disable_auto_submit() {
        let settings = get_default_settings();
        assert!(!settings.auto_submit);
        assert_eq!(settings.auto_submit_key, AutoSubmitKey::Enter);
    }

    #[test]
    fn debug_output_redacts_api_keys() {
        let mut settings = get_default_settings();
        settings
            .post_process_api_keys
            .insert("openai".to_string(), "sk-proj-secret-key-12345".to_string());
        settings.post_process_api_keys.insert(
            "anthropic".to_string(),
            "sk-ant-secret-key-67890".to_string(),
        );
        settings
            .post_process_api_keys
            .insert("empty_provider".to_string(), "".to_string());

        let debug_output = format!("{:?}", settings);

        assert!(!debug_output.contains("sk-proj-secret-key-12345"));
        assert!(!debug_output.contains("sk-ant-secret-key-67890"));
        assert!(debug_output.contains("[REDACTED]"));
    }

    #[test]
    fn secret_map_debug_redacts_values() {
        let map = SecretMap(HashMap::from([("key".into(), "secret".into())]));
        let out = format!("{:?}", map);
        assert!(!out.contains("secret"));
        assert!(out.contains("[REDACTED]"));
    }
}
