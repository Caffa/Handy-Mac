use enigo::{Enigo, Key, Keyboard, Mouse, Settings};
use log::{error, info};
use parking_lot::Mutex;
use tauri::{AppHandle, Manager};

/// Wrapper for Enigo to store in Tauri's managed state.
/// Enigo is wrapped in a Mutex since it requires mutable access.
pub struct EnigoState(pub Mutex<Enigo>);

impl EnigoState {
    pub fn new() -> Result<Self, String> {
        info!("Initializing Enigo...");
        match Enigo::new(&Settings::default()) {
            Ok(enigo) => {
                info!("Enigo initialized successfully");
                Ok(Self(Mutex::new(enigo)))
            }
            Err(e) => {
                error!("Failed to initialize Enigo: {}", e);
                Err(format!("Failed to initialize Enigo: {}", e))
            }
        }
    }
}

/// Get the current mouse cursor position using the managed Enigo instance.
/// Returns None if the state is not available or if getting the location fails.
pub fn get_cursor_position(app_handle: &AppHandle) -> Option<(i32, i32)> {
    let enigo_state = app_handle.try_state::<EnigoState>()?;
    let enigo = enigo_state.0.lock();
    match enigo.location() {
        Ok(loc) => Some(loc),
        Err(e) => {
            error!("Failed to get cursor position: {}", e);
            None
        }
    }
}

/// Sends a Ctrl+V or Cmd+V paste command using platform-specific virtual key codes.
/// This ensures the paste works regardless of keyboard layout (e.g., Russian, AZERTY, DVORAK).
/// Note: On Wayland, this may not work - callers should check for Wayland and use alternative methods.
pub fn send_paste_ctrl_v(enigo: &mut Enigo) -> Result<(), String> {
    info!("Sending Ctrl+V/Cmd+V paste command");
    // Platform-specific key definitions
    #[cfg(target_os = "macos")]
    let (modifier_key, v_key_code) = (Key::Meta, Key::Other(9));
    #[cfg(target_os = "windows")]
    let (modifier_key, v_key_code) = (Key::Control, Key::Other(0x56)); // VK_V
    #[cfg(target_os = "linux")]
    let (modifier_key, v_key_code) = (Key::Control, Key::Unicode('v'));

    // Press modifier + V
    if let Err(e) = enigo.key(modifier_key, enigo::Direction::Press) {
        error!("Failed to press modifier key: {}", e);
        return Err(format!("Failed to press modifier key: {}", e));
    }
    if let Err(e) = enigo.key(v_key_code, enigo::Direction::Click) {
        error!("Failed to click V key: {}", e);
        let _ = enigo.key(modifier_key, enigo::Direction::Release);
        return Err(format!("Failed to click V key: {}", e));
    }

    std::thread::sleep(std::time::Duration::from_millis(100));

    if let Err(e) = enigo.key(modifier_key, enigo::Direction::Release) {
        error!("Failed to release modifier key: {}", e);
        return Err(format!("Failed to release modifier key: {}", e));
    }

    info!("Ctrl+V/Cmd+V paste command sent successfully");
    Ok(())
}

/// Sends a Ctrl+Shift+V paste command.
/// This is commonly used in terminal applications on Linux to paste without formatting.
/// Note: On Wayland, this may not work - callers should check for Wayland and use alternative methods.
pub fn send_paste_ctrl_shift_v(enigo: &mut Enigo) -> Result<(), String> {
    info!("Sending Ctrl+Shift+V paste command");
    // Platform-specific key definitions
    #[cfg(target_os = "macos")]
    let (modifier_key, v_key_code) = (Key::Meta, Key::Other(9)); // Cmd+Shift+V on macOS
    #[cfg(target_os = "windows")]
    let (modifier_key, v_key_code) = (Key::Control, Key::Other(0x56)); // VK_V
    #[cfg(target_os = "linux")]
    let (modifier_key, v_key_code) = (Key::Control, Key::Unicode('v'));

    // Press Ctrl/Cmd + Shift + V
    if let Err(e) = enigo.key(modifier_key, enigo::Direction::Press) {
        error!("Failed to press modifier key: {}", e);
        return Err(format!("Failed to press modifier key: {}", e));
    }
    if let Err(e) = enigo.key(Key::Shift, enigo::Direction::Press) {
        error!("Failed to press Shift key: {}", e);
        let _ = enigo.key(modifier_key, enigo::Direction::Release);
        return Err(format!("Failed to press Shift key: {}", e));
    }
    if let Err(e) = enigo.key(v_key_code, enigo::Direction::Click) {
        error!("Failed to click V key: {}", e);
        let _ = enigo.key(Key::Shift, enigo::Direction::Release);
        let _ = enigo.key(modifier_key, enigo::Direction::Release);
        return Err(format!("Failed to click V key: {}", e));
    }

    std::thread::sleep(std::time::Duration::from_millis(100));

    if let Err(e) = enigo.key(Key::Shift, enigo::Direction::Release) {
        error!("Failed to release Shift key: {}", e);
    }
    if let Err(e) = enigo.key(modifier_key, enigo::Direction::Release) {
        error!("Failed to release modifier key: {}", e);
        return Err(format!("Failed to release modifier key: {}", e));
    }

    info!("Ctrl+Shift+V paste command sent successfully");
    Ok(())
}

/// Sends a Shift+Insert paste command (Windows and Linux only).
/// This is more universal for terminal applications and legacy software.
/// Note: On Wayland, this may not work - callers should check for Wayland and use alternative methods.
pub fn send_paste_shift_insert(enigo: &mut Enigo) -> Result<(), String> {
    info!("Sending Shift+Insert paste command");
    #[cfg(target_os = "windows")]
    let insert_key_code = Key::Other(0x2D); // VK_INSERT
    #[cfg(not(target_os = "windows"))]
    let insert_key_code = Key::Other(0x76); // XK_Insert (keycode 118 / 0x76, also used as fallback)

    // Press Shift + Insert
    if let Err(e) = enigo.key(Key::Shift, enigo::Direction::Press) {
        error!("Failed to press Shift key: {}", e);
        return Err(format!("Failed to press Shift key: {}", e));
    }
    if let Err(e) = enigo.key(insert_key_code, enigo::Direction::Click) {
        error!("Failed to click Insert key: {}", e);
        let _ = enigo.key(Key::Shift, enigo::Direction::Release);
        return Err(format!("Failed to click Insert key: {}", e));
    }

    std::thread::sleep(std::time::Duration::from_millis(100));

    if let Err(e) = enigo.key(Key::Shift, enigo::Direction::Release) {
        error!("Failed to release Shift key: {}", e);
        return Err(format!("Failed to release Shift key: {}", e));
    }

    info!("Shift+Insert paste command sent successfully");
    Ok(())
}

/// Pastes text directly using the enigo text method.
/// This tries to use system input methods if possible, otherwise simulates keystrokes one by one.
pub fn paste_text_direct(enigo: &mut Enigo, text: &str) -> Result<(), String> {
    info!("Pasting text directly, length={}", text.len());
    match enigo.text(text) {
        Ok(()) => {
            info!("Text pasted directly successfully");
            Ok(())
        }
        Err(e) => {
            error!("Failed to send text directly: {}", e);
            Err(format!("Failed to send text directly: {}", e))
        }
    }
}
