//! Handy-keys based keyboard shortcut implementation
//!
//! This module provides an alternative to Tauri's global-shortcut plugin
//! using the handy-keys library for more control over keyboard events.
//!
//! ## Architecture
//!
//! The implementation uses a dedicated manager thread that owns the `HotkeyManager`:
//!
//! ```text
//! ┌─────────────────┐     commands      ┌──────────────────────┐
//! │   Main Thread   │ ───────────────▶ │   Manager Thread     │
//! │                 │   (via channel)   │                      │
//! │ - register()    │                   │ - owns HotkeyManager │
//! │ - unregister()  │                   │ - polls for events   │
//! └─────────────────┘                   │ - dispatches actions │
//!                                       └──────────────────────┘
//! ```
//!
//! This design ensures thread-safety since `HotkeyManager` is only accessed
//! from a single thread. Commands (register/unregister) are sent via an mpsc
//! channel and responses are synchronously awaited.
//!
//! ## Recording Mode
//!
//! For UI key capture, a separate `KeyboardListener` is created on-demand and
//! polled from a dedicated recording thread. Events are emitted to the frontend
//! via Tauri's event system.

use handy_keys::{Hotkey, HotkeyId, HotkeyManager, HotkeyState, KeyboardListener};
use log::{debug, error, info, warn};
use parking_lot::Mutex;
use serde::Serialize;
use specta::Type;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, Runtime};

use crate::settings::{self, get_settings, KeyboardImplementation, ShortcutBinding};

use super::handler::handle_shortcut_event;

/// Commands that can be sent to the hotkey manager thread
enum ManagerCommand {
    Register {
        binding_id: String,
        hotkey_string: String,
        response: Sender<Result<(), String>>,
    },
    Unregister {
        binding_id: String,
        response: Sender<Result<(), String>>,
    },
    ResetPressedHotkeys {
        response: Sender<Result<(), String>>,
    },
    Shutdown,
}

/// State for the handy-keys shortcut manager
pub struct HandyKeysState {
    /// Channel to send commands to the manager thread (wrapped in Mutex for Sync)
    command_sender: Mutex<Sender<ManagerCommand>>,
    /// Handle to the manager thread (wrapped in Mutex for Sync, allows proper join on drop)
    thread_handle: Mutex<Option<JoinHandle<()>>>,
    /// Recording listener for UI key capture (only active during recording)
    recording_listener: Mutex<Option<KeyboardListener>>,
    /// Flag indicating if we're in recording mode
    is_recording: AtomicBool,
    /// The binding ID being recorded (if any)
    recording_binding_id: Mutex<Option<String>>,
    /// Flag to stop recording loop
    recording_running: Arc<AtomicBool>,
}

/// Key event sent to frontend during recording mode
#[derive(Debug, Clone, Serialize, Type)]
pub struct FrontendKeyEvent {
    /// Currently pressed modifier keys
    pub modifiers: Vec<String>,
    /// The key that was pressed (if any)
    pub key: Option<String>,
    /// Whether this is a key down event
    pub is_key_down: bool,
    /// The full hotkey string (e.g., "option+space")
    pub hotkey_string: String,
}

impl HandyKeysState {
    /// Create a new HandyKeysState
    pub fn new(app: AppHandle) -> Result<Self, String> {
        let (cmd_tx, cmd_rx) = mpsc::channel::<ManagerCommand>();

        // Start the manager thread
        let app_clone = app.clone();
        let thread_handle = thread::spawn(move || {
            Self::manager_thread(cmd_rx, app_clone);
        });

        Ok(Self {
            command_sender: Mutex::new(cmd_tx),
            thread_handle: Mutex::new(Some(thread_handle)),
            recording_listener: Mutex::new(None),
            is_recording: AtomicBool::new(false),
            recording_binding_id: Mutex::new(None),
            recording_running: Arc::new(AtomicBool::new(false)),
        })
    }

    /// The main manager thread - owns the HotkeyManager and processes commands
    fn manager_thread(cmd_rx: Receiver<ManagerCommand>, app: AppHandle) {
        info!("handy-keys manager thread started");

        // Create the HotkeyManager in this thread
        let manager = match HotkeyManager::new_with_blocking() {
            Ok(m) => m,
            Err(e) => {
                error!("Failed to create HotkeyManager: {}", e);
                return;
            }
        };

        // Maps binding IDs to HotkeyIds and hotkey strings
        let mut binding_to_hotkey: HashMap<String, HotkeyId> = HashMap::new();
        let mut hotkey_to_binding: HashMap<HotkeyId, (String, String)> = HashMap::new(); // (binding_id, hotkey_string)

        loop {
            // Check for hotkey events (non-blocking)
            while let Some(event) = manager.try_recv() {
                if let Some((binding_id, hotkey_string)) = hotkey_to_binding.get(&event.id) {
                    debug!(
                        "handy-keys event: binding={}, hotkey={}, state={:?}",
                        binding_id, hotkey_string, event.state
                    );
                    let is_pressed = event.state == HotkeyState::Pressed;
                    let t = Instant::now();
                    handle_shortcut_event(&app, binding_id, hotkey_string, is_pressed);
                    let dt = t.elapsed();
                    if dt > Duration::from_millis(100) {
                        warn!(
                            "handle_shortcut_event took {:?} (binding_id={}, hotkey_string={}, is_pressed={})",
                            dt, binding_id, hotkey_string, is_pressed
                        );
                    }
                } else {
                    debug!(
                        "handy-keys event with unknown hotkey id {:?}; active bindings={}",
                        event.id,
                        hotkey_to_binding.len()
                    );
                }
            }

            // Check for commands (non-blocking with timeout)
            match cmd_rx.recv_timeout(std::time::Duration::from_millis(10)) {
                Ok(cmd) => match cmd {
                    ManagerCommand::Register {
                        binding_id,
                        hotkey_string,
                        response,
                    } => {
                        let result = Self::do_register(
                            &manager,
                            &mut binding_to_hotkey,
                            &mut hotkey_to_binding,
                            &binding_id,
                            &hotkey_string,
                        );
                        let _ = response.send(result);
                    }
                    ManagerCommand::Unregister {
                        binding_id,
                        response,
                    } => {
                        let result = Self::do_unregister(
                            &manager,
                            &mut binding_to_hotkey,
                            &mut hotkey_to_binding,
                            &binding_id,
                        );
                        let _ = response.send(result);
                    }
                    ManagerCommand::ResetPressedHotkeys { response } => {
                        let result = manager
                            .reset_pressed_hotkeys()
                            .map_err(|e| format!("Failed to reset pressed hotkeys: {}", e));
                        let _ = response.send(result);
                    }
                    ManagerCommand::Shutdown => {
                        info!("handy-keys manager thread shutting down");
                        break;
                    }
                },
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    // No command, continue
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    info!("Command channel disconnected, shutting down");
                    break;
                }
            }
        }

        info!("handy-keys manager thread stopped");
    }

    /// Register a hotkey
    fn do_register(
        manager: &HotkeyManager,
        binding_to_hotkey: &mut HashMap<String, HotkeyId>,
        hotkey_to_binding: &mut HashMap<HotkeyId, (String, String)>,
        binding_id: &str,
        hotkey_string: &str,
    ) -> Result<(), String> {
        let hotkey: Hotkey = hotkey_string
            .parse()
            .map_err(|e| format!("Failed to parse hotkey '{}': {}", hotkey_string, e))?;

        let id = manager
            .register(hotkey)
            .map_err(|e| format!("Failed to register hotkey: {}", e))?;

        binding_to_hotkey.insert(binding_id.to_string(), id);
        hotkey_to_binding.insert(id, (binding_id.to_string(), hotkey_string.to_string()));

        debug!(
            "Registered handy-keys shortcut: {} -> {:?}",
            binding_id, hotkey
        );
        Ok(())
    }

    /// Unregister a hotkey
    fn do_unregister(
        manager: &HotkeyManager,
        binding_to_hotkey: &mut HashMap<String, HotkeyId>,
        hotkey_to_binding: &mut HashMap<HotkeyId, (String, String)>,
        binding_id: &str,
    ) -> Result<(), String> {
        if let Some(id) = binding_to_hotkey.remove(binding_id) {
            manager
                .unregister(id)
                .map_err(|e| format!("Failed to unregister hotkey: {}", e))?;
            hotkey_to_binding.remove(&id);
            debug!("Unregistered handy-keys shortcut: {}", binding_id);
        }
        Ok(())
    }

    /// Register a shortcut binding
    pub fn register(&self, binding: &ShortcutBinding) -> Result<(), String> {
        let (tx, rx) = mpsc::channel();
        self.command_sender
            .lock()
            .send(ManagerCommand::Register {
                binding_id: binding.id.clone(),
                hotkey_string: binding.current_binding.clone(),
                response: tx,
            })
            .map_err(|_| "Failed to send register command")?;

        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(resp) => resp,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                error!("Manager command timed out (5s); manager thread may be wedged");
                Err("Manager register command timed out".to_string())
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                error!("Manager command channel disconnected");
                Err("Manager command channel disconnected".to_string())
            }
        }
    }

    /// Unregister a shortcut binding
    pub fn unregister(&self, binding: &ShortcutBinding) -> Result<(), String> {
        let (tx, rx) = mpsc::channel();
        self.command_sender
            .lock()
            .send(ManagerCommand::Unregister {
                binding_id: binding.id.clone(),
                response: tx,
            })
            .map_err(|_| "Failed to send unregister command")?;

        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(resp) => resp,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                error!("Manager command timed out (5s); manager thread may be wedged");
                Err("Manager unregister command timed out".to_string())
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                error!("Manager command channel disconnected");
                Err("Manager command channel disconnected".to_string())
            }
        }
    }

    /// Reset the internal pressed-hotkey set inside the manager thread.
    ///
    /// This clears any hotkey IDs that remain stuck after missed key-release
    /// events (e.g. after macOS sleep/wake). It should be called after
    /// re-registering shortcuts so the new HotkeyIds do not collide with stale
    /// state, although clearing the set directly also removes the stale entries.
    pub fn reset_pressed_hotkeys(&self) -> Result<(), String> {
        let (tx, rx) = mpsc::channel();
        self.command_sender
            .lock()
            .send(ManagerCommand::ResetPressedHotkeys { response: tx })
            .map_err(|_| "Failed to send reset pressed hotkeys command")?;

        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(resp) => resp,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                error!("Manager command timed out (5s); manager thread may be wedged");
                Err("Manager reset pressed hotkeys command timed out".to_string())
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                error!("Manager command channel disconnected");
                Err("Manager command channel disconnected".to_string())
            }
        }
    }

    /// Start recording mode for a specific binding
    pub fn start_recording(&self, app: &AppHandle, binding_id: String) -> Result<(), String> {
        if self.is_recording.load(Ordering::SeqCst) {
            // The flag is set, but the recording may be stale if the loop
            // exited without `stop_recording()` being called (e.g. the state
            // was dropped, the app restarted, or a panic leaked). Recover if
            // there is no active listener or the running flag is false.
            let has_listener = self.recording_listener.lock().is_some();
            let is_running = self.recording_running.load(Ordering::SeqCst);
            if has_listener && is_running {
                return Err("Already recording".into());
            }
            warn!(
                "[RECOVERY] handy-keys is_recording=true but listener={} running={}; resetting stale state",
                has_listener, is_running
            );
            let _ = self.stop_recording();
        }

        // Create a new keyboard listener for recording
        let listener = KeyboardListener::new()
            .map_err(|e| format!("Failed to create keyboard listener: {}", e))?;

        {
            let mut recording = self.recording_listener.lock();
            *recording = Some(listener);
        }
        {
            let mut binding = self.recording_binding_id.lock();
            *binding = Some(binding_id);
        }

        self.is_recording.store(true, Ordering::SeqCst);
        self.recording_running.store(true, Ordering::SeqCst);

        // Start a thread to emit key events to the frontend
        let app_clone = app.clone();
        let recording_running = Arc::clone(&self.recording_running);
        thread::spawn(move || {
            Self::recording_loop(app_clone, recording_running);
        });

        debug!("Started handy-keys recording mode");
        Ok(())
    }

    /// Recording loop - emits key events to frontend during recording
    fn recording_loop(app: AppHandle, running: Arc<AtomicBool>) {
        while running.load(Ordering::SeqCst) {
            let event = {
                let state = match app.try_state::<HandyKeysState>() {
                    Some(s) => s,
                    None => break,
                };
                let listener = state.recording_listener.lock();
                listener.as_ref().and_then(|l| l.try_recv())
            };

            if let Some(key_event) = event {
                // Convert to frontend-friendly format
                let frontend_event = FrontendKeyEvent {
                    modifiers: modifiers_to_strings(key_event.modifiers),
                    key: key_event.key.map(|k| k.to_string().to_lowercase()),
                    is_key_down: key_event.is_key_down,
                    hotkey_string: key_event
                        .as_hotkey()
                        .map(|h| h.to_handy_string())
                        .unwrap_or_default(),
                };

                // Emit to frontend
                if let Err(e) = app.emit("handy-keys-event", &frontend_event) {
                    error!("Failed to emit key event: {}", e);
                }
            } else {
                thread::sleep(std::time::Duration::from_millis(10));
            }
        }

        debug!("Recording loop ended");
    }

    /// Stop recording mode
    pub fn stop_recording(&self) -> Result<(), String> {
        self.is_recording.store(false, Ordering::SeqCst);
        self.recording_running.store(false, Ordering::SeqCst);

        {
            let mut recording = self.recording_listener.lock();
            *recording = None;
        }
        {
            let mut binding = self.recording_binding_id.lock();
            *binding = None;
        }

        debug!("Stopped handy-keys recording mode");
        Ok(())
    }
}

impl Drop for HandyKeysState {
    fn drop(&mut self) {
        // Signal recording to stop
        self.recording_running.store(false, Ordering::SeqCst);
        self.is_recording.store(false, Ordering::SeqCst);

        // Send shutdown command
        {
            let sender = self.command_sender.lock();
            let _ = sender.send(ManagerCommand::Shutdown);
        }

        // Wait for the manager thread to finish
        {
            let mut handle = self.thread_handle.lock();
            if let Some(h) = handle.take() {
                let _ = h.join();
            }
        }
    }
}

/// Convert handy-keys Modifiers to a list of strings
fn modifiers_to_strings(modifiers: handy_keys::Modifiers) -> Vec<String> {
    let mut result = Vec::new();

    if modifiers.contains(handy_keys::Modifiers::CTRL) {
        result.push("ctrl".to_string());
    }
    if modifiers.contains(handy_keys::Modifiers::OPT) {
        #[cfg(target_os = "macos")]
        result.push("option".to_string());
        #[cfg(not(target_os = "macos"))]
        result.push("alt".to_string());
    }
    if modifiers.contains(handy_keys::Modifiers::SHIFT) {
        result.push("shift".to_string());
    }
    if modifiers.contains(handy_keys::Modifiers::CMD) {
        #[cfg(target_os = "macos")]
        result.push("command".to_string());
        #[cfg(not(target_os = "macos"))]
        result.push("super".to_string());
    }
    if modifiers.contains(handy_keys::Modifiers::FN) {
        result.push("fn".to_string());
    }

    result
}

/// Validate a shortcut string for the HandyKeys implementation.
/// HandyKeys is more permissive: allows modifier-only combos and the fn key.
pub fn validate_shortcut(raw: &str) -> Result<(), String> {
    if raw.trim().is_empty() {
        return Err("Shortcut cannot be empty".into());
    }
    // HandyKeys accepts modifier-only, key-only, and modifier+key combos
    // Just verify the string is parseable
    raw.parse::<Hotkey>()
        .map(|_| ())
        .map_err(|e| format!("Invalid shortcut for HandyKeys: {}", e))
}

/// Initialize handy-keys shortcuts
pub fn init_shortcuts(app: &AppHandle) -> Result<(), String> {
    let state = HandyKeysState::new(app.clone())?;

    let default_bindings = settings::get_default_settings().bindings;
    let user_settings = settings::load_or_create_app_settings_safe(app);

    // Register all bindings except cancel (which is dynamic).
    // Mirror the stored implementation-specific binding into `current_binding`
    // so the rest of this backend can keep reading `current_binding`.
    for (id, default_binding) in default_bindings {
        if id == "cancel" {
            continue;
        }
        // Skip post-processing shortcut when the feature is disabled
        if id == "transcribe_with_post_process" && !user_settings.post_process_enabled {
            continue;
        }

        let mut binding = user_settings
            .bindings
            .get(&id)
            .cloned()
            .unwrap_or(default_binding);
        binding.current_binding = binding
            .binding_for(KeyboardImplementation::HandyKeys)
            .to_string();

        if let Err(e) = state.register(&binding) {
            error!(
                "Failed to register handy-keys shortcut {} during init: {}",
                id, e
            );
        }
    }

    app.manage(state);
    info!("handy-keys shortcuts initialized");
    Ok(())
}

/// Register a shortcut
pub fn register_shortcut(app: &AppHandle, binding: ShortcutBinding) -> Result<(), String> {
    let state = app
        .try_state::<HandyKeysState>()
        .ok_or("HandyKeysState not initialized")?;
    state.register(&binding)
}

/// Unregister a shortcut
pub fn unregister_shortcut(app: &AppHandle, binding: ShortcutBinding) -> Result<(), String> {
    let state = app
        .try_state::<HandyKeysState>()
        .ok_or("HandyKeysState not initialized")?;
    state.unregister(&binding)
}

/// Start key recording mode
#[tauri::command]
#[specta::specta]
pub fn start_handy_keys_recording(app: AppHandle, binding_id: String) -> Result<(), String> {
    let settings = get_settings(&app);
    if settings.keyboard_implementation != settings::KeyboardImplementation::HandyKeys {
        return Err("handy-keys is not the active keyboard implementation".into());
    }

    // While Secure Input is active the tap receives no KeyDown/KeyUp, so the
    // recorder would silently capture just the modifier and overwrite the
    // binding with it (upstream #1578). Refuse instead; the frontend maps this
    // marker to a localized explanation, and the noted impact makes the
    // warning banner appear with the full story.
    if crate::secure_input::is_enabled_now() {
        crate::secure_input::note_recorder_blocked(&app);
        return Err("secure-input-active".into());
    }

    let state = app
        .try_state::<HandyKeysState>()
        .ok_or("HandyKeysState not initialized")?;

    // Suspend every registered shortcut so a combo that overlaps an existing
    // binding can't fire it (or have its keys swallowed) mid-capture.
    super::suspend_all_shortcuts(&app);

    let result = state.start_recording(&app, binding_id);
    if result.is_err() {
        super::resume_all_shortcuts(&app);
    }
    result
}

/// Stop key recording mode
#[tauri::command]
#[specta::specta]
pub fn stop_handy_keys_recording(app: AppHandle) -> Result<(), String> {
    let settings = get_settings(&app);
    if settings.keyboard_implementation != settings::KeyboardImplementation::HandyKeys {
        return Err("handy-keys is not the active keyboard implementation".into());
    }

    let state = app
        .try_state::<HandyKeysState>()
        .ok_or("HandyKeysState not initialized")?;

    // Restore shortcuts from settings regardless of how recording ended.
    // A commit has already registered the new binding via change_binding;
    // re-registering it here fails cleanly and is ignored.
    let result = state.stop_recording();
    super::resume_all_shortcuts(&app);
    result
}

/// Reset the internal pressed-hotkey set across all active handy-keys shortcuts.
///
/// Call this whenever a transcription session ends (normally, via cancel, or
/// after a processing timeout) so a missed key-release event does not leave
/// the recording hotkey "stuck" and silently swallow the next press.
pub fn reset_all_pressed_hotkeys<R: Runtime>(app: &AppHandle<R>) {
    let settings = get_settings(app);
    if settings.keyboard_implementation != KeyboardImplementation::HandyKeys {
        debug!(
            "Skipping handy-keys pressed-hotkey reset: active implementation is {:?}",
            settings.keyboard_implementation
        );
        return;
    }

    let state = match app.try_state::<HandyKeysState>() {
        Some(s) => s,
        None => {
            debug!("HandyKeysState not available, cannot reset pressed-hotkey state");
            return;
        }
    };

    if let Err(e) = state.reset_pressed_hotkeys() {
        warn!("Failed to reset handy-keys pressed-hotkey state: {}", e);
    } else {
        info!("Reset handy-keys pressed-hotkey state");
    }
}

/// Reset hotkey state after macOS wake-from-sleep.
///
/// After wake, key release events are missed by the OS, so `HotkeyManager`'s
/// internal `pressed_hotkeys` set retains stale hotkey IDs. This causes the
/// next press to be silently swallowed. By unregistering and re-registering
/// each shortcut, we get fresh `HotkeyId` values that aren't in the stale set,
/// restoring normal shortcut behavior.
pub fn reset_hotkey_state_after_wake(app: &AppHandle) {
    let wake_start = Instant::now();
    info!("reset_hotkey_state_after_wake: start");

    let settings = get_settings(app);
    if settings.keyboard_implementation != KeyboardImplementation::HandyKeys {
        // Tauri implementation uses a different mechanism; nothing to reset.
        info!(
            "reset_hotkey_state_after_wake: skipped, active keyboard implementation is {:?}",
            settings.keyboard_implementation
        );
        return;
    }

    let state = match app.try_state::<HandyKeysState>() {
        Some(s) => s,
        None => {
            warn!("HandyKeysState not available during hotkey reset after wake");
            return;
        }
    };

    let default_bindings = settings::get_default_settings().bindings;
    let user_settings = settings::load_or_create_app_settings_safe(app);

    info!("Resetting handy-keys pressed-hotkey state after wake-from-sleep (unregister + re-register all shortcuts)");

    for (id, default_binding) in default_bindings {
        // Cancel shortcut is dynamically registered/unregistered during recording;
        // skip it here to avoid interference with the recording state machine.
        if id == "cancel" {
            continue;
        }
        // Skip post-processing shortcut when the feature is disabled.
        if id == "transcribe_with_post_process" && !user_settings.post_process_enabled {
            continue;
        }

        let mut binding = user_settings
            .bindings
            .get(&id)
            .cloned()
            .unwrap_or(default_binding);

        // Mirror the stored HandyKeys binding into `current_binding` so the
        // manager unregister/register calls use the correct hotkey string.
        binding.current_binding = binding
            .binding_for(KeyboardImplementation::HandyKeys)
            .to_string();

        if let Err(e) = state.unregister(&binding) {
            warn!(
                "Failed to unregister hotkey '{}' during wake reset: {}",
                id, e
            );
        }
        if let Err(e) = state.register(&binding) {
            error!(
                "Failed to re-register hotkey '{}' during wake reset: {}",
                id, e
            );
        } else {
            debug!("Re-registered hotkey '{}' with fresh ID after wake", id);
        }
    }

    info!("Hotkey state reset complete after wake-from-sleep");

    if let Err(e) = state.reset_pressed_hotkeys() {
        warn!(
            "Failed to reset handy-keys pressed-hotkey state after wake-from-sleep: {}",
            e
        );
    } else {
        info!("Cleared handy-keys pressed-hotkey state after wake-from-sleep");
    }

    info!(
        "reset_hotkey_state_after_wake: complete, elapsed={:?}",
        wake_start.elapsed()
    );
}
