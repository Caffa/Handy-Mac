// Settings store operations: load, save, flush, debounced writer,
// sanitization, and migration helpers.

use log::{debug, error, info, trace, warn};
use std::collections::HashMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;
use std::sync::RwLock;
use tauri::{AppHandle, Manager, Runtime};
use tauri_plugin_store::StoreExt;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use super::defaults::get_default_settings;
use super::types::*;

/// In-memory cache of the current settings. This is the single source of truth
/// for all reads — `get_settings_safe` reads from here, not from the store.
/// `write_settings_safe` updates this cache immediately and schedules a debounced
/// disk flush. This eliminates the read-modify-write race where a quick second
/// settings change could read stale data from the store before the first
/// debounced write flushed.
pub struct SettingsCache {
    settings: RwLock<AppSettings>,
}

impl SettingsCache {
    pub fn new(settings: AppSettings) -> Self {
        Self {
            settings: RwLock::new(settings),
        }
    }

    pub fn get(&self) -> AppSettings {
        self.settings.read().unwrap().clone()
    }

    pub fn update(&self, settings: AppSettings) {
        *self.settings.write().unwrap() = settings;
        trace!("[settings] cache updated");
    }
}

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
}

/// Migrate legacy single `current_binding` values into per-implementation
/// storage. On first load of an older `settings_store.json`, both backends
/// inherit the existing binding so the user doesn't lose their shortcut.
/// Returns true if any binding was changed.
pub(crate) fn migrate_bindings(settings: &mut AppSettings) -> bool {
    let mut changed = false;
    for binding in settings.bindings.values_mut() {
        if binding.tauri_binding.is_empty() && !binding.current_binding.is_empty() {
            binding.tauri_binding = binding.current_binding.clone();
            changed = true;
        }
        if binding.handy_keys_binding.is_empty() && !binding.current_binding.is_empty() {
            binding.handy_keys_binding = binding.current_binding.clone();
            changed = true;
        }
    }
    changed
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
pub(crate) fn open_settings_store<R: Runtime>(
    app: &AppHandle<R>,
) -> Option<Arc<tauri_plugin_store::Store<R>>> {
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
                // Sync the API format for built-in providers so older settings that
                // pre-date PostProcessApiFormat adopt the correct native format.
                // The custom provider is user-editable, so its format is left alone.
                if existing.api_format != provider.api_format && existing.id != "custom" {
                    debug!(
                        "Updating api_format for provider '{}' from {:?} to {:?}",
                        provider.id, existing.api_format, provider.api_format
                    );
                    existing.api_format = provider.api_format;
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
pub fn load_or_create_app_settings_safe<R: Runtime>(app: &AppHandle<R>) -> AppSettings {
    safe_settings_operation("load_or_create_app_settings", || {
        load_or_create_app_settings(app)
    })
    .unwrap_or_else(|| {
        error!("Falling back to default settings after panic in load_or_create_app_settings");
        get_default_settings()
    })
}

/// Safe wrapper around [`get_settings`] that catches panics and falls back to defaults.
/// Reads from the in-memory cache when available, eliminating the read-modify-write
/// race with the debounced disk writer.
pub fn get_settings_safe<R: Runtime>(app: &AppHandle<R>) -> AppSettings {
    safe_settings_operation("get_settings_safe", || {
        if let Some(cache) = app.try_state::<Arc<SettingsCache>>() {
            trace!("[settings] get_safe (cache hit)");
            return cache.get();
        }
        get_settings(app)
    })
    .unwrap_or_else(|| {
        error!("Falling back to default settings after panic in get_settings_safe");
        get_default_settings()
    })
}

/// Safe wrapper around [`write_settings`] that catches panics.
/// Delegates to `write_settings` which updates the cache and schedules a
/// debounced disk write. The `_safe` variant wraps the call in a panic
/// catcher as an extra safety net for callers that prefer it.
pub fn write_settings_safe(app: &AppHandle, settings: AppSettings) {
    let _ = safe_settings_operation("write_settings_safe", || {
        write_settings(app, settings);
    });
}

/// Safe wrapper around [`write_settings_immediate`] that catches panics.
/// Delegates to `write_settings_immediate` which updates the cache and
/// writes to disk synchronously.
#[allow(dead_code)]
pub fn write_settings_immediate_safe(app: &AppHandle, settings: AppSettings) {
    let _ = safe_settings_operation("write_settings_immediate", || {
        write_settings_immediate(app, settings);
    });
}

/// Atomically patch the in-memory settings cache and flush the result to disk.
///
/// This helper is intended for small, isolated field updates (e.g. custom
/// words, selected model). It holds the `SettingsCache` write lock across the
/// entire patch, sanitization, and synchronous disk flush. This prevents two
/// common loss scenarios:
///
/// 1. Read-modify-write races where two writers read the same snapshot,
///    mutate different fields, and the second disk write clobbers the first.
///    Because the disk flush happens while the cache lock is held, the value
///    written to disk is always the latest merged cache value.
/// 2. Debounced disk writes that are lost because the app quits or crashes
///    before the flush fires.
///
/// Callers pass a closure that receives `&mut AppSettings` and can modify any
/// field; the patched value is then written to the cache and immediately saved.
pub fn patch_settings<R: Runtime, F>(app: &AppHandle<R>, f: F)
where
    F: FnOnce(&mut AppSettings),
{
    let _ = safe_settings_operation("patch_settings", || {
        if let Some(cache) = app.try_state::<Arc<SettingsCache>>() {
            let mut guard = cache.settings.write().unwrap();
            f(&mut guard);
            sanitize_floats(&mut guard);
            write_settings_to_disk(app, &guard);
            // guard drops here, releasing the write lock after disk flush.
        } else {
            let mut settings = get_settings(app);
            f(&mut settings);
            write_settings_immediate(app, settings);
        }
    });
}

// ── Core load/save functions ──

/// Rebuilds settings from a store value that failed to deserialize as a whole.
/// Every stored field that is individually valid is kept; only broken values
/// (e.g. an enum variant written by a newer or older version) fall back to
/// their default. This means one bad field can never reset the rest of the
/// user's configuration (#1619).
fn salvage_settings(stored: &serde_json::Value) -> AppSettings {
    let Some(stored_map) = stored.as_object() else {
        warn!("Stored settings are not a JSON object; falling back to defaults");
        return get_default_settings();
    };

    // Track which per-implementation binding keys were actually present in the
    // stored JSON. This is the signal used below to decide whether a legacy
    // binding (only `current_binding`) needs to be migrated into the newer
    // `tauri_binding` / `handy_keys_binding` fields. We record presence *before*
    // any merging happens, because a missing key should be treated differently
    // from an empty key.
    let mut binding_impl_keys_present: HashMap<String, (bool, bool)> = HashMap::new();
    if let Some(bindings) = stored_map.get("bindings").and_then(|v| v.as_object()) {
        for (id, binding) in bindings {
            if let Some(obj) = binding.as_object() {
                binding_impl_keys_present.insert(
                    id.clone(),
                    (
                        obj.contains_key("tauri_binding"),
                        obj.contains_key("handy_keys_binding"),
                    ),
                );
            }
        }
    }

    let mut merged = serde_json::to_value(get_default_settings())
        .expect("default settings serialize to a JSON object");

    for (key, value) in stored_map {
        let previous = merged
            .as_object_mut()
            .expect("merged settings stay an object")
            .insert(key.clone(), value.clone());
        if serde_json::from_value::<AppSettings>(merged.clone()).is_err() {
            // Log only the key: values may hold secrets (e.g. API keys).
            warn!("Dropping invalid settings field '{key}', keeping its default");
            let map = merged
                .as_object_mut()
                .expect("merged settings stay an object");
            match previous {
                Some(previous) => map.insert(key.clone(), previous),
                None => map.remove(key),
            };
        }
    }

    // After the deep/top-level merge, force any stored binding that did not
    // contain `tauri_binding` / `handy_keys_binding` to have an explicit empty
    // string for those fields. Without this, a deep merge would keep the
    // default non-empty binding, and `migrate_bindings` would skip the legacy
    // `current_binding` copy because the per-implementation field would no
    // longer be empty. We only do this when the stored value for the binding
    // was an object; non-object values are left for serde to handle.
    if let Some(merged_bindings) = merged
        .as_object_mut()
        .and_then(|m| m.get_mut("bindings"))
        .and_then(|b| b.as_object_mut())
    {
        for (id, (has_tauri, has_handy)) in &binding_impl_keys_present {
            if let Some(binding_obj) = merged_bindings.get_mut(id).and_then(|b| b.as_object_mut()) {
                if !has_tauri && !binding_obj.contains_key("tauri_binding") {
                    binding_obj.insert(
                        "tauri_binding".to_string(),
                        serde_json::Value::String(String::new()),
                    );
                }
                if !has_handy && !binding_obj.contains_key("handy_keys_binding") {
                    binding_obj.insert(
                        "handy_keys_binding".to_string(),
                        serde_json::Value::String(String::new()),
                    );
                }
            }
        }
    }

    serde_json::from_value(merged).unwrap_or_else(|e| {
        warn!("Failed to reassemble salvaged settings ({e}); falling back to defaults");
        get_default_settings()
    })
}

pub fn load_or_create_app_settings<R: Runtime>(app: &AppHandle<R>) -> AppSettings {
    // Initialize store
    let Some(store) = open_settings_store(app) else {
        error!("Cannot load settings: store initialization failed, returning defaults");
        return get_default_settings();
    };

    let mut settings = if let Some(settings_value) = store.get("settings") {
        match serde_json::from_value::<AppSettings>(settings_value.clone()) {
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

                // Migrate legacy single-binding storage to per-implementation fields
                if migrate_bindings(&mut settings) {
                    updated = true;
                }

                // One-time shortcut activation migration (only while the new
                // key is absent): the retired `push_to_talk` bool maps onto
                // the two legacy modes so upgrading users keep exactly the
                // behavior they had. Only fresh installs get the
                // hold-or-toggle default. (upstream c62a5fcd)
                if settings_value.get("shortcut_activation").is_none() {
                    if let Some(push_to_talk) =
                        settings_value.get("push_to_talk").and_then(|v| v.as_bool())
                    {
                        settings.shortcut_activation = if push_to_talk {
                            ShortcutActivation::PushToTalk
                        } else {
                            ShortcutActivation::Toggle
                        };
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
                        match store.save() {
                            Ok(()) => info!("[settings] flushed to disk"),
                            Err(e) => error!("[settings] disk write FAILED: {e}"),
                        }
                    }
                }

                settings
            }
            Err(e) => {
                warn!("Failed to parse stored settings ({e}); salvaging valid fields");
                let mut salvaged = salvage_settings(&settings_value);
                // migrate_bindings must run before the salvaged value is saved
                // back to disk; otherwise the per-implementation binding fields
                // that salvage_settings deliberately emptied never get filled
                // from the legacy `current_binding` value at real startup.
                migrate_bindings(&mut salvaged);
                if let Some(value) = settings_to_value(&salvaged) {
                    store.set("settings", value);
                    match store.save() {
                        Ok(()) => info!("[settings] flushed to disk"),
                        Err(e) => error!("[settings] disk write FAILED: {e}"),
                    }
                }
                salvaged
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

pub fn get_settings<R: Runtime>(app: &AppHandle<R>) -> AppSettings {
    // Prefer the in-memory cache — this is the single source of truth at runtime.
    // Only fall back to the plugin-store path during early startup before
    // SettingsCache is managed (e.g. during load_or_create_app_settings).
    if let Some(cache) = app.try_state::<Arc<SettingsCache>>() {
        trace!("[settings] get (cache hit)");
        return cache.get();
    }

    warn!("[settings] get: cache not managed yet, falling back to plugin-store");

    let Some(store) = open_settings_store(app) else {
        error!("Cannot get settings: store initialization failed, returning defaults");
        return get_default_settings();
    };

    let mut settings = if let Some(settings_value) = store.get("settings") {
        match serde_json::from_value::<AppSettings>(settings_value.clone()) {
            Ok(settings) => settings,
            Err(e) => {
                warn!("Failed to parse settings: {}, salvaging valid fields", e);
                let mut salvaged = salvage_settings(&settings_value);
                // Same migration as load_or_create_app_settings: legacy bindings
                // present in the partial store value must be written back to the
                // per-implementation fields before saving.
                migrate_bindings(&mut salvaged);
                if let Some(value) = settings_to_value(&salvaged) {
                    store.set("settings", value);
                    match store.save() {
                        Ok(()) => info!("[settings] flushed to disk"),
                        Err(e) => error!("[settings] disk write FAILED: {e}"),
                    }
                }
                salvaged
            }
        }
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

    // Migrate legacy single-binding storage to per-implementation fields
    if migrate_bindings(&mut settings) {
        needs_save = true;
    }

    if needs_save {
        sanitize_floats(&mut settings);
        if let Some(value) = settings_to_value(&settings) {
            store.set("settings", value);
            match store.save() {
                Ok(()) => info!("[settings] flushed to disk"),
                Err(e) => error!("[settings] disk write FAILED: {e}"),
            }
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
/// Also updates the in-memory cache immediately so that subsequent reads
/// (via `get_settings` or `get_settings_safe`) see the new value, eliminating
/// the read-modify-write race with the debounced disk writer.
pub fn write_settings<R: Runtime>(app: &AppHandle<R>, settings: AppSettings) {
    let _ = safe_settings_operation("write_settings", || {
        // Diagnostics: log any change to keyboard_implementation so future
        // unexpected reverts can be traced to the code path that wrote them.
        if let Some(cache) = app.try_state::<Arc<SettingsCache>>() {
            let previous = cache.get().keyboard_implementation;
            if previous != settings.keyboard_implementation {
                warn!(
                    "[settings] keyboard_implementation changing from {:?} to {:?}",
                    previous, settings.keyboard_implementation
                );
            }
        }

        // Update the in-memory cache first — mirror what write_settings_safe does.
        if let Some(cache) = app.try_state::<Arc<SettingsCache>>() {
            cache.update(settings.clone());
        }

        debug!(
            "[settings] write: active_model_id={:?} live_captions={} selected_language={} post_process_enabled={} overlay_position={:?} keyboard_implementation={:?}",
            settings.selected_model,
            settings.live_captions_enabled,
            settings.selected_language,
            settings.post_process_enabled,
            settings.overlay_position,
            settings.keyboard_implementation,
        );

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

/// Write `settings` to the persistent store without touching the
/// in-memory `SettingsCache`. This is the low-level disk-only path used by
/// callers that already hold the cache write lock and have already sanitized
/// the value.
fn write_settings_to_disk<R: Runtime>(app: &AppHandle<R>, settings: &AppSettings) {
    let Some(store) = open_settings_store(app) else {
        error!("Cannot write settings: store initialization failed, settings not saved");
        return;
    };

    let Some(value) = settings_to_value(settings) else {
        error!("Cannot write settings: serialization failed, settings not saved");
        return;
    };

    store.set("settings", value);
    match store.save() {
        Ok(()) => info!("[settings] flushed to disk"),
        Err(e) => error!("[settings] disk write FAILED: {e}"),
    }
}

/// Write settings to disk immediately, bypassing the debounce timer.
/// Also updates the in-memory cache if available, to keep reads consistent.
pub fn write_settings_immediate<R: Runtime>(app: &AppHandle<R>, mut settings: AppSettings) {
    let _ = safe_settings_operation("write_settings_immediate", || {
        // Diagnostics: log any change to keyboard_implementation so future
        // unexpected reverts can be traced to the code path that wrote them.
        if let Some(cache) = app.try_state::<Arc<SettingsCache>>() {
            let previous = cache.get().keyboard_implementation;
            if previous != settings.keyboard_implementation {
                warn!(
                    "[settings] keyboard_implementation changing from {:?} to {:?}",
                    previous, settings.keyboard_implementation
                );
            }
        }

        // Update the in-memory cache so reads reflect the immediate write.
        if let Some(cache) = app.try_state::<Arc<SettingsCache>>() {
            cache.update(settings.clone());
        }

        sanitize_floats(&mut settings);
        write_settings_to_disk(app, &settings);
    });
}

/// Flush any pending debounced settings to disk synchronously.
///
/// Called from Tauri's `RunEvent::ExitRequested` run callback, which executes
/// on the main event-loop thread with **no active Tokio runtime context**.
/// That means any use of `tokio::runtime::Handle::current()`, `block_in_place`,
/// or `block_on` would panic (or deadlock if the runtime is already tearing
/// down). The panic is swallowed by `safe_settings_operation`, so the pending
/// write would be silently dropped — which is exactly how custom words were
/// lost on quit.
///
/// Instead, this function relies on the fact that `write_settings` and
/// `write_settings_immediate` always update the in-memory `SettingsCache`
/// synchronously before scheduling a disk write. On exit we simply grab the
/// current cache value and perform a synchronous `write_settings_immediate`.
/// If the cache is not available there is nothing authoritative to flush, so
/// we log a warning and return without panicking or blocking.
pub fn flush_settings<R: Runtime>(app: &AppHandle<R>) {
    let _ = safe_settings_operation("flush_settings", || {
        let Some(cache) = app.try_state::<Arc<SettingsCache>>() else {
            warn!(
                "[settings] exit flush: SettingsCache not available, cannot flush pending changes"
            );
            return;
        };

        let settings = cache.get();
        write_settings_immediate(app, settings);
        info!("[settings] exit flush complete");
    });
}

pub fn get_bindings<R: Runtime>(app: &AppHandle<R>) -> HashMap<String, ShortcutBinding> {
    let settings = get_settings_safe(app);
    settings.bindings
}

/// Look up a stored binding by id. Returns an error for unknown ids so the
/// reset path rejects them instead of silently fabricating a binding
/// (upstream a6eed754).
pub fn get_stored_binding(settings: &AppSettings, id: &str) -> Result<ShortcutBinding, String> {
    settings
        .bindings
        .get(id)
        .cloned()
        .ok_or_else(|| format!("Binding with id '{}' not found", id))
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

    /// Check whether there is a pending write that hasn't been flushed yet.
    /// Used by `flush_settings` to skip the expensive blocking path when
    /// there's nothing to write.
    pub async fn has_pending(&self) -> bool {
        self.pending.lock().await.is_some()
    }

    /// Schedule a settings write. If a write is already pending the new value
    /// replaces it and the debounce timer is restarted.
    pub async fn write<R: Runtime>(&self, app: AppHandle<R>, settings: AppSettings) {
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
    pub async fn flush<R: Runtime>(&self, app: &AppHandle<R>) {
        {
            let mut timer = self.timer.lock().await;
            if let Some(handle) = timer.take() {
                handle.abort();
            }
        }
        self.flush_inner(app).await;
    }

    /// Internal flush: write the pending settings (if any) to the store.
    async fn flush_inner<R: Runtime>(&self, app: &AppHandle<R>) {
        let maybe_settings = {
            let mut pending = self.pending.lock().await;
            pending.take()
        };

        if let Some(settings) = maybe_settings {
            debug!("[settings] flushing debounced settings to disk");
            write_settings_immediate(app, settings);
        }
    }
}

// ── Tests ──

#[cfg(test)]
#[allow(unused_imports)]
mod tests {
    use super::get_stored_binding;
    use crate::settings::defaults::get_default_settings;

    #[test]
    fn stored_binding_returns_the_requested_binding() {
        let settings = get_default_settings();

        let result = get_stored_binding(&settings, "transcribe");

        assert_eq!(result.unwrap().id, "transcribe");
    }

    #[test]
    fn unknown_stored_binding_returns_an_error() {
        let settings = get_default_settings();

        let result = get_stored_binding(&settings, "unknown");

        assert_eq!(result.unwrap_err(), "Binding with id 'unknown' not found");
    }

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

    #[test]
    fn salvage_preserves_binding_fields_added_after_user_saved() {
        // Simulates a settings file that was saved before per-implementation
        // bindings existed: it has `current_binding` but not `tauri_binding` or
        // `handy_keys_binding`. A separate *broken* field forces the salvage path.
        let mut stored = serde_json::to_value(get_default_settings()).unwrap();
        stored
            .as_object_mut()
            .unwrap()
            .insert("broken_future_field".into(), serde_json::json!("nope"));
        stored
            .as_object_mut()
            .unwrap()
            .get_mut("bindings")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .get_mut("transcribe")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert(
                "current_binding".into(),
                serde_json::json!("ctrl_right+command_right+y"),
            );
        stored
            .as_object_mut()
            .unwrap()
            .get_mut("bindings")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .get_mut("transcribe")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove("tauri_binding");
        stored
            .as_object_mut()
            .unwrap()
            .get_mut("bindings")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .get_mut("transcribe")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove("handy_keys_binding");

        let mut salvaged = salvage_settings(&stored);
        assert!(migrate_bindings(&mut salvaged));

        let binding = salvaged
            .bindings
            .get("transcribe")
            .expect("transcribe binding");
        assert_eq!(binding.current_binding, "ctrl_right+command_right+y");
        assert_eq!(binding.tauri_binding, "ctrl_right+command_right+y");
        assert_eq!(binding.handy_keys_binding, "ctrl_right+command_right+y");
        assert_eq!(
            binding.binding_for(KeyboardImplementation::Tauri),
            "ctrl_right+command_right+y"
        );
        assert_eq!(
            binding.binding_for(KeyboardImplementation::HandyKeys),
            "ctrl_right+command_right+y"
        );
    }

    #[test]
    fn salvage_migrates_legacy_binding_after_broken_field_forces_salvage() {
        // Legacy stored settings with only `current_binding` for a shortcut,
        // plus a broken unrelated top-level field that forces `salvage_settings`.
        let stored = serde_json::json!({
            "bindings": {
                "transcribe": {
                    "id": "transcribe",
                    "name": "Transcribe",
                    "description": "Converts your speech into text.",
                    "default_binding": "option+space",
                    "current_binding": "ctrl_right+command_right+y"
                }
            },
            "broken_future_field": "nope"
        });

        let mut salvaged = salvage_settings(&stored);
        assert!(migrate_bindings(&mut salvaged));

        let binding = salvaged
            .bindings
            .get("transcribe")
            .expect("transcribe binding");
        assert_eq!(binding.current_binding, "ctrl_right+command_right+y");
        assert_eq!(binding.tauri_binding, "ctrl_right+command_right+y");
        assert_eq!(binding.handy_keys_binding, "ctrl_right+command_right+y");
        assert_eq!(
            binding.binding_for(KeyboardImplementation::Tauri),
            "ctrl_right+command_right+y"
        );
        assert_eq!(
            binding.binding_for(KeyboardImplementation::HandyKeys),
            "ctrl_right+command_right+y"
        );
    }

    /// Verifies that `salvage_settings` preserves `custom_words` (and related
    /// per-field settings) when an unrelated top-level field is broken. This
    /// is the load path used whenever a stored settings file fails full
    /// deserialization. If salvage dropped custom_words, the user's words would
    /// silently revert to defaults on the next launch — matching the reported
    /// symptom.
    #[test]
    fn salvage_settings_preserves_custom_words_across_broken_field() {
        let mut stored = serde_json::to_value(get_default_settings()).unwrap();
        let map = stored.as_object_mut().unwrap();
        // Add user custom words as they would appear on disk.
        map.insert(
            "custom_words".into(),
            serde_json::json!(["Zettelkasten", "MacBook"]),
        );
        map.insert(
            "advanced_custom_words".into(),
            serde_json::json!([{ "word": "Kubernetes", "pronunciations": [] }]),
        );
        map.insert(
            "word_replacements".into(),
            serde_json::json!([{ "mistranslation": "teh", "correction": "the" }]),
        );
        map.insert(
            "custom_filler_words".into(),
            serde_json::json!(["um", "uh"]),
        );
        // An unrelated invalid field forces the salvage path.
        map.insert("broken_future_field".into(), serde_json::json!("nope"));

        let salvaged = salvage_settings(&stored);

        assert_eq!(
            salvaged.custom_words,
            vec!["Zettelkasten".to_string(), "MacBook".to_string()]
        );
        assert_eq!(salvaged.advanced_custom_words.len(), 1);
        assert_eq!(salvaged.advanced_custom_words[0].word, "Kubernetes");
        assert_eq!(salvaged.word_replacements.len(), 1);
        assert_eq!(
            salvaged.custom_filler_words,
            Some(vec!["um".to_string(), "uh".to_string()])
        );
    }

    /// Verifies that `flush_settings` can write the in-memory cache to disk
    /// from a thread with no Tokio runtime context. This mirrors Tauri's
    /// `RunEvent::ExitRequested` callback, which runs on the main event-loop
    /// thread without an active runtime. The previous async implementation
    /// panicked in this situation and the panic was swallowed, so custom words
    /// were lost on quit.
    #[test]
    fn flush_settings_writes_cache_to_disk_without_runtime() {
        let app = tauri::test::mock_builder()
            .plugin(tauri_plugin_store::Builder::new().build())
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap()
            .handle()
            .clone();

        app.manage(Arc::new(SettingsWriter::new()));

        let mut settings = get_default_settings();
        settings.custom_words = vec!["Zettelkasten".to_string(), "MacBook".to_string()];
        app.manage(Arc::new(SettingsCache::new(settings)));

        // Run the flush from a plain std::thread with no Tokio runtime — exactly
        // the context in which Tauri invokes `RunEvent::ExitRequested`.
        let flushed = std::thread::spawn({
            let app = app.clone();
            move || std::panic::catch_unwind(AssertUnwindSafe(|| flush_settings(&app)))
        })
        .join()
        .expect("flush thread should not panic");

        assert!(
            flushed.is_ok(),
            "flush_settings must not panic outside a Tokio runtime context"
        );

        let store = open_settings_store(&app).expect("store opens in mock app");
        let stored_value = store
            .get("settings")
            .expect("settings should be written to disk");
        let stored: AppSettings =
            serde_json::from_value(stored_value).expect("stored settings should deserialize");

        assert_eq!(
            stored.custom_words,
            vec!["Zettelkasten".to_string(), "MacBook".to_string()]
        );
    }

    /// Regression test for the exact user-reported scenario: custom words are
    /// added in the UI, `write_settings` updates the cache and schedules a
    /// debounced disk flush, then the app is quit before the debounce fires.
    /// `flush_settings` must persist the cached words even when called from a
    /// thread with no Tokio runtime.
    #[test]
    fn flush_settings_preserves_debounced_custom_words_on_exit() {
        let rt = tokio::runtime::Runtime::new().expect("test runtime");

        let app = tauri::test::mock_builder()
            .plugin(tauri_plugin_store::Builder::new().build())
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap()
            .handle()
            .clone();

        app.manage(Arc::new(SettingsWriter::new()));
        app.manage(Arc::new(SettingsCache::new(get_default_settings())));

        rt.block_on(async {
            let mut settings = get_settings_safe(&app);
            settings.custom_words = vec!["Kubernetes".to_string(), "Terraform".to_string()];
            // This updates the cache immediately and schedules a debounced write
            // that we intentionally do not let fire.
            write_settings(&app, settings);
        });

        // Ensure the debounce task has not had time to run.
        std::thread::sleep(std::time::Duration::from_millis(50));

        let flushed = std::thread::spawn({
            let app = app.clone();
            move || std::panic::catch_unwind(AssertUnwindSafe(|| flush_settings(&app)))
        })
        .join()
        .expect("flush thread should not panic");

        assert!(
            flushed.is_ok(),
            "flush_settings must not panic outside a Tokio runtime context"
        );

        let store = open_settings_store(&app).expect("store opens in mock app");
        let stored_value = store
            .get("settings")
            .expect("settings should be written to disk");
        let stored: AppSettings =
            serde_json::from_value(stored_value).expect("stored settings should deserialize");

        assert_eq!(
            stored.custom_words,
            vec!["Kubernetes".to_string(), "Terraform".to_string()]
        );
    }

    #[test]
    fn load_or_create_app_settings_migrates_legacy_binding_in_salvage_path() {
        // End-to-end simulation of the runtime salvage path: create a mock
        // app, seed the store with a legacy settings JSON that fails full
        // deserialization (because of a broken field), then call
        // `load_or_create_app_settings` directly. The function must both
        // salvage and migrate the binding, and persist the migrated value
        // so that subsequent loads no longer need the salvage path.
        let app = tauri::test::mock_builder()
            .plugin(tauri_plugin_store::Builder::new().build())
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap()
            .handle()
            .clone();

        let stored = serde_json::json!({
            "bindings": {
                "transcribe": {
                    "id": "transcribe",
                    "name": "Transcribe",
                    "description": "Converts your speech into text.",
                    "default_binding": "option+space",
                    "current_binding": "ctrl_right+command_right+y"
                }
            },
            "broken_future_field": "nope"
        });

        // Seed the store with legacy, partially-broken JSON.
        let store = open_settings_store(&app).expect("store opens in mock app");
        store.set("settings", stored);
        store.save().expect("store saves");

        // This is the real startup path being fixed.
        let settings = load_or_create_app_settings(&app);

        let binding = settings
            .bindings
            .get("transcribe")
            .expect("transcribe binding");
        assert_eq!(binding.current_binding, "ctrl_right+command_right+y");
        assert_eq!(binding.tauri_binding, "ctrl_right+command_right+y");
        assert_eq!(binding.handy_keys_binding, "ctrl_right+command_right+y");

        // Verify the migrated value was written back to disk.
        let reloaded_value = store
            .get("settings")
            .expect("settings still in store after load");
        let reloaded: AppSettings = serde_json::from_value(reloaded_value)
            .expect("reloaded settings deserialize cleanly after migration");
        let reloaded_binding = reloaded
            .bindings
            .get("transcribe")
            .expect("transcribe binding after reload");
        assert_eq!(reloaded_binding.tauri_binding, "ctrl_right+command_right+y");
        assert_eq!(
            reloaded_binding.handy_keys_binding,
            "ctrl_right+command_right+y"
        );
    }

    /// Verifies that `patch_settings` applies concurrent field updates
    /// atomically and persists the merged result to disk. Without atomic
    /// patching, two read-modify-write writers that touch different fields can
    /// clobber each other: whichever writes last keeps only its own field.
    /// This test reproduces that exact race scenario with two threads.
    #[test]
    fn patch_settings_atomic_concurrent_fields() {
        let app = tauri::test::mock_builder()
            .plugin(tauri_plugin_store::Builder::new().build())
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap()
            .handle()
            .clone();

        app.manage(Arc::new(SettingsCache::new(get_default_settings())));

        let app_words = app.clone();
        let handle_words = std::thread::spawn(move || {
            patch_settings(&app_words, |s| {
                s.custom_words = vec!["Zettelkasten".to_string()];
            });
        });

        let app_model = app.clone();
        let handle_model = std::thread::spawn(move || {
            patch_settings(&app_model, |s| {
                s.selected_model = "test-model".to_string();
            });
        });

        handle_words
            .join()
            .expect("custom_words thread should finish");
        handle_model
            .join()
            .expect("selected_model thread should finish");

        // Both fields must survive in the in-memory cache.
        let cached = app
            .try_state::<Arc<SettingsCache>>()
            .expect("SettingsCache should be managed")
            .get();
        assert_eq!(cached.custom_words, vec!["Zettelkasten".to_string()]);
        assert_eq!(cached.selected_model, "test-model".to_string());

        // And both fields must have been written to disk.
        let store = open_settings_store(&app).expect("store opens in mock app");
        let stored_value = store
            .get("settings")
            .expect("settings should be written to disk");
        let stored: AppSettings =
            serde_json::from_value(stored_value).expect("stored settings should deserialize");
        assert_eq!(stored.custom_words, vec!["Zettelkasten".to_string()]);
        assert_eq!(stored.selected_model, "test-model".to_string());
    }
}
