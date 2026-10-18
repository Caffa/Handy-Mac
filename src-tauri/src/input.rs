use enigo::{Enigo, Key, Keyboard, Mouse, Settings};
use log::{error, info};
use parking_lot::Mutex;
use tauri::{AppHandle, Manager};

#[cfg(target_os = "macos")]
mod macos {
    use super::Key;
    use log::{debug, warn};
    use std::ffi::c_void;

    type TisInputSourceRef = *const c_void;
    type CfDataRef = *const c_void;
    type CfStringRef = *const c_void;

    // kVK_ANSI_V. This is the behavior Handy used before layout-aware
    // resolution and remains the safest fallback if macOS cannot expose the
    // active layout.
    const ANSI_V_KEYCODE: u16 = 9;
    const KEYCODE_COUNT: u16 = 128;
    const UC_KEY_ACTION_DISPLAY: u16 = 3;
    const UC_KEY_TRANSLATE_NO_DEAD_KEYS_MASK: u32 = 1;
    // Carbon's cmdKey is bit 8. UCKeyTranslate expects Carbon modifiers shifted
    // right by 8, so Command is represented by bit 0 here.
    const COMMAND_MODIFIER_STATE: u32 = 1;

    #[link(name = "Carbon", kind = "framework")]
    unsafe extern "C" {
        fn TISCopyCurrentKeyboardLayoutInputSource() -> TisInputSourceRef;
        fn TISGetInputSourceProperty(
            input_source: TisInputSourceRef,
            property_key: CfStringRef,
        ) -> CfDataRef;
        static kTISPropertyUnicodeKeyLayoutData: CfStringRef;
        fn UCKeyTranslate(
            key_layout: *const u8,
            virtual_key_code: u16,
            key_action: u16,
            modifier_key_state: u32,
            keyboard_type: u32,
            key_translate_options: u32,
            dead_key_state: *mut u32,
            max_string_length: usize,
            actual_string_length: *mut usize,
            unicode_string: *mut u16,
        ) -> i32;
        fn LMGetKbdType() -> u8;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFDataGetBytePtr(data: CfDataRef) -> *const u8;
        fn CFRelease(value: *const c_void);
    }

    struct InputSource(TisInputSourceRef);

    impl Drop for InputSource {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: TISCopyCurrentKeyboardLayoutInputSource returned this
                // retained reference, so this balances that ownership.
                unsafe { CFRelease(self.0) };
            }
        }
    }

    fn find_keycode(mut matches: impl FnMut(u16) -> bool) -> Option<u16> {
        (0..KEYCODE_COUNT).find(|&keycode| matches(keycode))
    }

    /// Resolves the physical key that macOS interprets as `v` while Command is
    /// held. Including Command is important: non-Latin layouts commonly map
    /// Cmd shortcuts to their ANSI equivalents, while standard Dvorak does not.
    ///
    /// TIS APIs must run on the main thread. Handy's paste path already enters
    /// through `AppHandle::run_on_main_thread` before reaching this function.
    fn resolve_command_v_keycode() -> Result<u16, String> {
        // SAFETY: This function is called on the macOS main thread. The returned
        // source follows the Create Rule and is released by InputSource::drop.
        let source = InputSource(unsafe { TISCopyCurrentKeyboardLayoutInputSource() });
        if source.0.is_null() {
            return Err("macOS returned no current keyboard layout input source".into());
        }

        // SAFETY: The source remains retained for the duration of the scan and
        // the property constant is provided by Carbon.
        let layout_data =
            unsafe { TISGetInputSourceProperty(source.0, kTISPropertyUnicodeKeyLayoutData) };
        if layout_data.is_null() {
            return Err("current macOS keyboard layout has no Unicode layout data".into());
        }

        // SAFETY: layout_data is a CFData owned by the retained input source and
        // remains valid until source is dropped after the scan.
        let layout = unsafe { CFDataGetBytePtr(layout_data) };
        if layout.is_null() {
            return Err("current macOS keyboard layout data is empty".into());
        }

        // SAFETY: LMGetKbdType has no arguments and returns the current physical
        // keyboard type used by UCKeyTranslate.
        let keyboard_type = unsafe { LMGetKbdType() } as u32;
        let keycode = find_keycode(|keycode| {
            let mut dead_key_state = 0;
            let mut chars = [0_u16; 4];
            let mut length = 0_usize;

            // SAFETY: layout points to valid UCKeyboardLayout bytes while source
            // is retained. All output pointers reference initialized local
            // storage of the declared sizes.
            let status = unsafe {
                UCKeyTranslate(
                    layout,
                    keycode,
                    UC_KEY_ACTION_DISPLAY,
                    COMMAND_MODIFIER_STATE,
                    keyboard_type,
                    UC_KEY_TRANSLATE_NO_DEAD_KEYS_MASK,
                    &mut dead_key_state,
                    chars.len(),
                    &mut length,
                    chars.as_mut_ptr(),
                )
            };

            status == 0 && length == 1 && chars[0] == u16::from(b'v')
        })
        .ok_or_else(|| "could not map Cmd+V in the current macOS keyboard layout".to_string())?;

        Ok(keycode)
    }

    pub(super) fn command_v_key() -> Key {
        match resolve_command_v_keycode() {
            Ok(keycode) => {
                debug!("Resolved Cmd+V for the active macOS layout to keycode {keycode}");
                Key::Other(u32::from(keycode))
            }
            Err(error) => {
                warn!(
                    "Could not resolve Cmd+V for the active macOS layout ({error}); using ANSI V keycode {ANSI_V_KEYCODE}"
                );
                Key::Other(u32::from(ANSI_V_KEYCODE))
            }
        }
    }
}

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
    let (modifier_key, v_key_code) = (Key::Meta, macos::command_v_key());
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
    let (modifier_key, v_key_code) = (Key::Meta, macos::command_v_key()); // Cmd+Shift+V on macOS
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
