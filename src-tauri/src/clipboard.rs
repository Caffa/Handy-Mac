use crate::input::{self, EnigoState};
#[cfg(target_os = "linux")]
use crate::settings::TypingTool;
use crate::settings::{get_settings, AutoSubmitKey, ClipboardHandling, PasteMethod};
use enigo::{Direction, Enigo, Key, Keyboard};
use log::{error, info, warn};
use std::process::Command;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_clipboard_manager::ClipboardExt;

#[cfg(target_os = "linux")]
use crate::utils::{is_kde_wayland, is_wayland};

// ── macOS Accessibility API bindings for paste verification ─────────────
//
// These are raw FFI bindings to the macOS Accessibility API (HIServices).
// They allow us to check whether a Cmd+V paste actually landed in the
// target text field, implementing the "verify-then-commit" pattern for
// clipboard restore. Without this, we'd unconditionally restore the
// clipboard content even if the paste failed, destroying the transcription
// text (Bug 3).
#[cfg(target_os = "macos")]
mod macos_ax {
    use std::os::raw::c_int;

    // Opaque types from ApplicationServices.framework
    #[repr(C)]
    pub struct AXUIElement(pub *mut std::ffi::c_void);
    #[repr(C)]
    pub struct CFString(pub *mut std::ffi::c_void);
    #[repr(C)]
    pub struct CFType(pub *mut std::ffi::c_void);

    pub type AXUIElementRef = *const AXUIElement;
    pub type CFTypeRef = *const CFType;
    pub type CFStringRef = *const CFString;
    pub type AXError = c_int;
    pub type CFStringEncoding = u32;

    // AXError codes
    pub const KAX_ERROR_SUCCESS: i32 = 0;

    // AX attribute names
    pub const KAX_FOCUSED_UI_ELEMENT_ATTRIBUTE: *const i8 =
        b"AXFocusedUIElement\0".as_ptr() as *const i8;
    pub const KAX_VALUE_ATTRIBUTE: *const i8 = b"AXValue\0".as_ptr() as *const i8;

    // CoreFoundation encoding
    pub const K_CFSTRING_ENCODING_UTF8: u32 = 0x08000100;

    #[link(kind = "framework", name = "ApplicationServices")]
    extern "C" {
        pub fn AXUIElementCreateApplication(pid: i32) -> AXUIElementRef;
        pub fn AXUIElementCopyAttributeValue(
            element: AXUIElementRef,
            attribute: *const i8,
            value: *mut *mut std::ffi::c_void,
        ) -> AXError;
        pub fn CFGetTypeID(cf: CFTypeRef) -> usize;
        pub fn CFStringGetTypeID() -> usize;
        pub fn CFStringGetLength(theString: CFStringRef) -> isize;
        pub fn CFStringGetCString(
            theString: CFStringRef,
            buffer: *mut i8,
            bufferSize: isize,
            encoding: CFStringEncoding,
        ) -> bool;
        pub fn CFRelease(cf: CFTypeRef);
    }
}

#[cfg(target_os = "macos")]
use macos_ax::*;

/// Writes text to the system clipboard without pasting or restoring the previous content.
/// Used as a fallback when the paste keystroke fails, so the user can manually paste.
pub fn write_to_clipboard(text: &str, app_handle: &AppHandle) -> Result<(), String> {
    let clipboard = app_handle.clipboard();

    // On Wayland, prefer wl-copy for better compatibility
    #[cfg(target_os = "linux")]
    let write_result = if is_wayland() && is_wl_copy_available() {
        info!("Using wl-copy for clipboard write on Wayland (fallback)");
        write_clipboard_via_wl_copy(text)
    } else {
        clipboard
            .write_text(text)
            .map_err(|e| format!("Failed to write to clipboard: {}", e))
    };

    #[cfg(not(target_os = "linux"))]
    let write_result = clipboard
        .write_text(text)
        .map_err(|e| format!("Failed to write to clipboard: {}", e));

    write_result
}

/// Verifies that a paste operation landed in the target application.
///
/// Implements the "verify-then-commit" pattern (Bug 3 fix): we don't
/// restore the original clipboard content until we have some confidence
/// that the paste was consumed by the target app. If verification fails,
/// we keep the transcription text on the clipboard so the user can
/// manually Cmd+V again.
///
/// On macOS, uses the Accessibility API (AXValue) to check if the focused
/// text field's value contains the pasted text. Falls back to a conservative
/// heuristic on other platforms.
///
/// Returns true if the paste was verified (safe to restore clipboard),
/// false if verification failed or is unavailable (keep transcription text).
fn verify_paste_landed(_app_handle: &AppHandle, _pasted_text: &str) -> bool {
    #[cfg(target_os = "macos")]
    {
        verify_paste_landed_macos(_pasted_text)
    }

    #[cfg(not(target_os = "macos"))]
    {
        // On non-macOS platforms, we can't use AX to verify.
        // Conservatively return false to keep transcription text on clipboard
        // so the user can manually paste again if needed.
        info!("Paste verification: not available on this platform — keeping transcription text on clipboard");
        false
    }
}

#[cfg(target_os = "macos")]
fn verify_paste_landed_macos(pasted_text: &str) -> bool {
    use objc2::rc::autoreleasepool;
    use objc2_app_kit::NSWorkspace;

    autoreleasepool(|_| {
        let workspace = NSWorkspace::sharedWorkspace();
        let Some(app) = workspace.frontmostApplication() else {
            info!("Paste verification: no frontmost app — assuming paste failed");
            return false;
        };

        let pid = app.processIdentifier();

        // Use the macOS Accessibility API to check the focused element.
        // This works at the window-server level, independent of whether
        // our panel is click-through.
        let ax_app: AXUIElementRef = unsafe { AXUIElementCreateApplication(pid) };

        let mut focused_element_ptr: *mut std::ffi::c_void = std::ptr::null_mut();
        let result = unsafe {
            AXUIElementCopyAttributeValue(
                ax_app,
                KAX_FOCUSED_UI_ELEMENT_ATTRIBUTE as *const i8,
                &mut focused_element_ptr as *mut _,
            )
        };

        if result != KAX_ERROR_SUCCESS {
            info!(
                "Paste verification: could not get focused AX element (error={}) — assuming paste failed",
                result
            );
            // Release ax_app (Create Rule: caller must release).
            unsafe { macos_ax::CFRelease(ax_app as macos_ax::CFTypeRef) };
            return false;
        }

        let focused_element: AXUIElementRef = focused_element_ptr as AXUIElementRef;

        // Get the AXValue of the focused element
        let mut value_ptr: *mut std::ffi::c_void = std::ptr::null_mut();
        let value_result = unsafe {
            AXUIElementCopyAttributeValue(
                focused_element,
                KAX_VALUE_ATTRIBUTE as *const i8,
                &mut value_ptr as *mut _,
            )
        };

        if value_result != KAX_ERROR_SUCCESS {
            // Element has no AXValue — might be a non-text field (terminal, etc.)
            // In this case, we can't verify, so assume the paste landed since
            // many terminal apps don't expose AXValue.
            info!(
                "Paste verification: focused element has no AXValue (error={}) — assuming paste succeeded (terminal-like app)",
                value_result
            );
            // Release the focused element (Create-rule +1 reference)
            unsafe { macos_ax::CFRelease(focused_element_ptr as macos_ax::CFTypeRef) };
            unsafe { macos_ax::CFRelease(ax_app as macos_ax::CFTypeRef) };
            return true;
        }

        // Check if the AXValue contains the pasted text
        // Release the focused element (Create-rule +1 reference) now that
        // we no longer need it.
        unsafe { macos_ax::CFRelease(focused_element_ptr as macos_ax::CFTypeRef) };

        let type_id = unsafe { CFGetTypeID(value_ptr as CFTypeRef) };
        let string_type_id = unsafe { CFStringGetTypeID() };

        if type_id == string_type_id {
            let cf_string: CFStringRef = value_ptr as CFStringRef;
            let len = unsafe { CFStringGetLength(cf_string) };
            // Allocate buffer for UTF-8 string + null terminator
            // Each UTF-16 code unit can expand to up to 3 UTF-8 bytes
            let buffer_size = (len as usize) * 3 + 1;
            let mut buffer = vec![0u8; buffer_size];

            let success = unsafe {
                CFStringGetCString(
                    cf_string,
                    buffer.as_mut_ptr() as *mut i8,
                    buffer_size as isize,
                    K_CFSTRING_ENCODING_UTF8,
                )
            };

            if success {
                // Remove trailing null bytes
                let end = buffer.iter().position(|&b| b == 0).unwrap_or(buffer.len());
                let s = String::from_utf8_lossy(&buffer[..end]);
                let contains = s.contains(pasted_text);
                if contains {
                    info!("Paste verification: AXValue contains pasted text — paste confirmed");
                } else {
                    info!("Paste verification: AXValue does NOT contain pasted text — paste may have failed");
                }
                // Release the CFString object obtained from AXUIElementCopyAttributeValue
                // (Core Foundation Create Rule: caller must release).
                unsafe { macos_ax::CFRelease(value_ptr as macos_ax::CFTypeRef) };
                // Release ax_app (Create Rule: caller must release).
                unsafe { macos_ax::CFRelease(ax_app as macos_ax::CFTypeRef) };
                contains
            } else {
                info!("Paste verification: could not extract string from AXValue — assuming paste failed");
                unsafe { macos_ax::CFRelease(value_ptr as macos_ax::CFTypeRef) };
                unsafe { macos_ax::CFRelease(ax_app as macos_ax::CFTypeRef) };
                false
            }
        } else {
            info!(
                "Paste verification: AXValue is not a string (type={}) — assuming paste failed",
                type_id
            );
            unsafe { macos_ax::CFRelease(value_ptr as macos_ax::CFTypeRef) };
            unsafe { macos_ax::CFRelease(ax_app as macos_ax::CFTypeRef) };
            false
        }
    })
}

/// Pastes text using the clipboard: saves current content, writes text, sends paste keystroke, restores clipboard.
///
/// This function is split into two phases to avoid holding the Enigo lock during the
/// paste-delay sleep. Phase 1 (clipboard write + delay) requires no Enigo lock. Phase 2
/// (key-sending + verify-then-commit) requires the Enigo lock only briefly.
fn paste_via_clipboard(
    enigo: &mut Enigo,
    text: &str,
    app_handle: &AppHandle,
    paste_method: &PasteMethod,
    paste_delay_ms: u64,
) -> Result<(), String> {
    // Phase 1: Write text to clipboard + delay (no Enigo lock needed).
    let clipboard_content = prepare_clipboard_for_paste(text, app_handle, paste_delay_ms)?;

    // Phase 2: Send paste key combo + verify (Enigo lock held by caller).
    send_paste_keys_and_verify(enigo, text, app_handle, paste_method, &clipboard_content)
}

/// Phase 1 of clipboard paste: write text to clipboard and sleep for paste_delay_ms.
/// Returns the original clipboard content for later restoration.
/// Does NOT require the Enigo lock — safe to call before acquiring it.
fn prepare_clipboard_for_paste(
    text: &str,
    app_handle: &AppHandle,
    paste_delay_ms: u64,
) -> Result<String, String> {
    let clipboard = app_handle.clipboard();
    let clipboard_content = clipboard.read_text().unwrap_or_default();

    // Write text to clipboard first
    // On Wayland, prefer wl-copy for better compatibility (especially with umlauts)
    #[cfg(target_os = "linux")]
    let write_result = if is_wayland() && is_wl_copy_available() {
        info!("Using wl-copy for clipboard write on Wayland");
        write_clipboard_via_wl_copy(text)
    } else {
        clipboard
            .write_text(text)
            .map_err(|e| format!("Failed to write to clipboard: {}", e))
    };

    #[cfg(not(target_os = "linux"))]
    let write_result = clipboard
        .write_text(text)
        .map_err(|e| format!("Failed to write to clipboard: {}", e));

    write_result?;

    // Sleep to let the clipboard update propagate before sending the key combo.
    // This is done WITHOUT holding the Enigo lock, so other Enigo users
    // are not starved during the delay.
    std::thread::sleep(Duration::from_millis(paste_delay_ms));

    Ok(clipboard_content)
}

/// Phase 2 of clipboard paste: send the paste key combo, verify it landed,
/// and optionally restore the original clipboard content.
/// Requires the Enigo lock to be held by the caller for key-sending operations.
fn send_paste_keys_and_verify(
    enigo: &mut Enigo,
    text: &str,
    app_handle: &AppHandle,
    paste_method: &PasteMethod,
    clipboard_content: &str,
) -> Result<(), String> {
    // Send paste key combo
    #[cfg(target_os = "linux")]
    let key_combo_sent = try_send_key_combo_linux(paste_method)?;

    #[cfg(not(target_os = "linux"))]
    let key_combo_sent = false;

    // Fall back to enigo if no native tool handled it
    if !key_combo_sent {
        match paste_method {
            PasteMethod::CtrlV => input::send_paste_ctrl_v(enigo)?,
            PasteMethod::CtrlShiftV => input::send_paste_ctrl_shift_v(enigo)?,
            PasteMethod::ShiftInsert => input::send_paste_shift_insert(enigo)?,
            _ => return Err("Invalid paste method for clipboard paste".into()),
        }
    }

    // Verify-then-commit: wait for the paste to land before restoring clipboard.
    // This fixes Bug 3 where the clipboard was restored too eagerly,
    // destroying the transcription text before the target app had a chance
    // to read it. If the paste might have failed (e.g., focus was lost),
    // we keep the transcription text on the clipboard so the user can
    // manually paste it.
    //
    // Phase 1: Give the target app time to process Cmd+V.
    // 150ms is generous — most apps process Cmd+V within 1 frame (16ms).
    std::thread::sleep(std::time::Duration::from_millis(150));

    // Phase 2: Verify the paste landed (macOS only, using Accessibility API).
    // If verification is available and confirms the paste succeeded, we
    // can safely restore the original clipboard content.
    // If verification fails or is unavailable, we err on the side of
    // caution: keep the transcription text on the clipboard so the user
    // can manually Cmd+V again.
    let paste_verified = verify_paste_landed(app_handle, text);

    let clipboard = app_handle.clipboard();

    if paste_verified {
        // Paste verified — safe to restore original clipboard content
        info!("Paste verified — restoring original clipboard content");

        #[cfg(target_os = "linux")]
        if is_wayland() && is_wl_copy_available() {
            let _ = write_clipboard_via_wl_copy(clipboard_content);
        } else {
            let _ = clipboard.write_text(clipboard_content);
        }

        #[cfg(not(target_os = "linux"))]
        let _ = clipboard.write_text(clipboard_content);
    } else {
        // Paste NOT verified — keep transcription text on clipboard.
        // The user can manually Cmd+V to paste again. The transcription
        // text stays available on the clipboard until the user copies
        // something else or the next transcription overwrites it.
        //
        // This is the "verify-then-commit" pattern: we don't destroy the
        // transcription text (commit) until we verify the paste landed.
        info!("Paste not verified — keeping transcription text on clipboard for manual paste");
    }

    Ok(())
}

/// Attempts to send a key combination using Linux-native tools.
/// Returns `Ok(true)` if a native tool handled it, `Ok(false)` to fall back to enigo.
#[cfg(target_os = "linux")]
fn try_send_key_combo_linux(paste_method: &PasteMethod) -> Result<bool, String> {
    if is_wayland() {
        // Wayland: prefer wtype (but not on KDE), then dotool, then ydotool
        // Note: wtype doesn't work on KDE (no zwp_virtual_keyboard_manager_v1 support)
        if !is_kde_wayland() && is_wtype_available() {
            info!("Using wtype for key combo");
            send_key_combo_via_wtype(paste_method)?;
            return Ok(true);
        }
        if is_dotool_available() {
            info!("Using dotool for key combo");
            send_key_combo_via_dotool(paste_method)?;
            return Ok(true);
        }
        if is_ydotool_available() {
            info!("Using ydotool for key combo");
            send_key_combo_via_ydotool(paste_method)?;
            return Ok(true);
        }
    } else {
        // X11: prefer xdotool, then ydotool
        if is_xdotool_available() {
            info!("Using xdotool for key combo");
            send_key_combo_via_xdotool(paste_method)?;
            return Ok(true);
        }
        if is_ydotool_available() {
            info!("Using ydotool for key combo");
            send_key_combo_via_ydotool(paste_method)?;
            return Ok(true);
        }
    }

    Ok(false)
}

/// Attempts to type text directly using Linux-native tools.
/// Returns `Ok(true)` if a native tool handled it, `Ok(false)` to fall back to enigo.
#[cfg(target_os = "linux")]
fn try_direct_typing_linux(text: &str, preferred_tool: TypingTool) -> Result<bool, String> {
    // If user specified a tool, try only that one
    if preferred_tool != TypingTool::Auto {
        return match preferred_tool {
            TypingTool::Wtype if is_wtype_available() => {
                info!("Using user-specified wtype");
                type_text_via_wtype(text)?;
                Ok(true)
            }
            TypingTool::Kwtype if is_kwtype_available() => {
                info!("Using user-specified kwtype");
                type_text_via_kwtype(text)?;
                Ok(true)
            }
            TypingTool::Dotool if is_dotool_available() => {
                info!("Using user-specified dotool");
                type_text_via_dotool(text)?;
                Ok(true)
            }
            TypingTool::Ydotool if is_ydotool_available() => {
                info!("Using user-specified ydotool");
                type_text_via_ydotool(text)?;
                Ok(true)
            }
            TypingTool::Xdotool if is_xdotool_available() => {
                info!("Using user-specified xdotool");
                type_text_via_xdotool(text)?;
                Ok(true)
            }
            _ => Err(format!(
                "Typing tool {:?} is not available on this system",
                preferred_tool
            )),
        };
    }

    // Auto mode - existing fallback chain
    if is_wayland() {
        // KDE Wayland: prefer kwtype (uses KDE Fake Input protocol, supports umlauts)
        if is_kde_wayland() && is_kwtype_available() {
            info!("Using kwtype for direct text input on KDE Wayland");
            type_text_via_kwtype(text)?;
            return Ok(true);
        }
        // Wayland: prefer wtype, then dotool, then ydotool
        // Note: wtype doesn't work on KDE (no zwp_virtual_keyboard_manager_v1 support)
        if !is_kde_wayland() && is_wtype_available() {
            info!("Using wtype for direct text input");
            type_text_via_wtype(text)?;
            return Ok(true);
        }
        if is_dotool_available() {
            info!("Using dotool for direct text input");
            type_text_via_dotool(text)?;
            return Ok(true);
        }
        if is_ydotool_available() {
            info!("Using ydotool for direct text input");
            type_text_via_ydotool(text)?;
            return Ok(true);
        }
    } else {
        // X11: prefer xdotool, then ydotool
        if is_xdotool_available() {
            info!("Using xdotool for direct text input");
            type_text_via_xdotool(text)?;
            return Ok(true);
        }
        if is_ydotool_available() {
            info!("Using ydotool for direct text input");
            type_text_via_ydotool(text)?;
            return Ok(true);
        }
    }

    Ok(false)
}

/// Returns the list of available typing tools on this system.
/// Always includes "auto" as the first entry.
#[cfg(target_os = "linux")]
pub fn get_available_typing_tools() -> Vec<String> {
    let mut tools = vec!["auto".to_string()];
    if is_wtype_available() {
        tools.push("wtype".to_string());
    }
    if is_kwtype_available() {
        tools.push("kwtype".to_string());
    }
    if is_dotool_available() {
        tools.push("dotool".to_string());
    }
    if is_ydotool_available() {
        tools.push("ydotool".to_string());
    }
    if is_xdotool_available() {
        tools.push("xdotool".to_string());
    }
    tools
}

/// Check if wtype is available (Wayland text input tool)
#[cfg(target_os = "linux")]
fn is_wtype_available() -> bool {
    Command::new("which")
        .arg("wtype")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// Check if dotool is available (another Wayland text input tool)
#[cfg(target_os = "linux")]
fn is_dotool_available() -> bool {
    Command::new("which")
        .arg("dotool")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// Check if ydotool is available (uinput-based, works on both Wayland and X11)
#[cfg(target_os = "linux")]
fn is_ydotool_available() -> bool {
    Command::new("which")
        .arg("ydotool")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

#[cfg(target_os = "linux")]
fn is_xdotool_available() -> bool {
    Command::new("which")
        .arg("xdotool")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// Check if kwtype is available (KDE Wayland virtual keyboard input tool)
#[cfg(target_os = "linux")]
fn is_kwtype_available() -> bool {
    Command::new("which")
        .arg("kwtype")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// Check if wl-copy is available (Wayland clipboard tool)
#[cfg(target_os = "linux")]
fn is_wl_copy_available() -> bool {
    Command::new("which")
        .arg("wl-copy")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// Type text directly via wtype on Wayland.
#[cfg(target_os = "linux")]
fn type_text_via_wtype(text: &str) -> Result<(), String> {
    let output = Command::new("wtype")
        .arg("--") // Protect against text starting with -
        .arg(text)
        .output()
        .map_err(|e| format!("Failed to execute wtype: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("wtype failed: {}", stderr));
    }

    Ok(())
}

/// Type text directly via xdotool on X11.
#[cfg(target_os = "linux")]
fn type_text_via_xdotool(text: &str) -> Result<(), String> {
    let output = Command::new("xdotool")
        .arg("type")
        .arg("--clearmodifiers")
        .arg("--")
        .arg(text)
        .output()
        .map_err(|e| format!("Failed to execute xdotool: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("xdotool failed: {}", stderr));
    }

    Ok(())
}

/// Type text directly via dotool (works on both Wayland and X11 via uinput).
#[cfg(target_os = "linux")]
fn type_text_via_dotool(text: &str) -> Result<(), String> {
    use std::io::Write;
    use std::process::Stdio;

    let mut child = Command::new("dotool")
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to spawn dotool: {}", e))?;

    if let Some(mut stdin) = child.stdin.take() {
        // dotool uses "type <text>" command
        writeln!(stdin, "type {}", text)
            .map_err(|e| format!("Failed to write to dotool stdin: {}", e))?;
    }

    let output = child
        .wait_with_output()
        .map_err(|e| format!("Failed to wait for dotool: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("dotool failed: {}", stderr));
    }

    Ok(())
}

/// Type text directly via ydotool (uinput-based, requires ydotoold daemon).
#[cfg(target_os = "linux")]
fn type_text_via_ydotool(text: &str) -> Result<(), String> {
    let output = Command::new("ydotool")
        .arg("type")
        .arg("--")
        .arg(text)
        .output()
        .map_err(|e| format!("Failed to execute ydotool: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("ydotool failed: {}", stderr));
    }

    Ok(())
}

/// Type text directly via kwtype (KDE Wayland virtual keyboard, uses KDE Fake Input protocol).
#[cfg(target_os = "linux")]
fn type_text_via_kwtype(text: &str) -> Result<(), String> {
    let output = Command::new("kwtype")
        .arg("--")
        .arg(text)
        .output()
        .map_err(|e| format!("Failed to execute kwtype: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("kwtype failed: {}", stderr));
    }

    Ok(())
}

/// Write text to clipboard via wl-copy (Wayland clipboard tool).
/// Uses Stdio::null() to avoid blocking on repeated calls — wl-copy forks a
/// daemon that inherits piped fds, causing read_to_end to hang indefinitely.
#[cfg(target_os = "linux")]
fn write_clipboard_via_wl_copy(text: &str) -> Result<(), String> {
    use std::process::Stdio;
    let status = Command::new("wl-copy")
        .arg("--")
        .arg(text)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| format!("Failed to execute wl-copy: {}", e))?;

    if !status.success() {
        return Err("wl-copy failed".into());
    }

    Ok(())
}

/// Send a key combination (e.g., Ctrl+V) via wtype on Wayland.
#[cfg(target_os = "linux")]
fn send_key_combo_via_wtype(paste_method: &PasteMethod) -> Result<(), String> {
    let args: Vec<&str> = match paste_method {
        PasteMethod::CtrlV => vec!["-M", "ctrl", "-k", "v"],
        PasteMethod::ShiftInsert => vec!["-M", "shift", "-k", "Insert"],
        PasteMethod::CtrlShiftV => vec!["-M", "ctrl", "-M", "shift", "-k", "v"],
        _ => return Err("Unsupported paste method".into()),
    };

    let output = Command::new("wtype")
        .args(&args)
        .output()
        .map_err(|e| format!("Failed to execute wtype: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("wtype failed: {}", stderr));
    }

    Ok(())
}

/// Send a key combination (e.g., Ctrl+V) via dotool.
#[cfg(target_os = "linux")]
fn send_key_combo_via_dotool(paste_method: &PasteMethod) -> Result<(), String> {
    let command;
    match paste_method {
        PasteMethod::CtrlV => command = "echo key ctrl+v | dotool",
        PasteMethod::ShiftInsert => command = "echo key shift+insert | dotool",
        PasteMethod::CtrlShiftV => command = "echo key ctrl+shift+v | dotool",
        _ => return Err("Unsupported paste method".into()),
    }
    use std::process::Stdio;
    let status = Command::new("sh")
        .arg("-c")
        .arg(command)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| format!("Failed to execute dotool: {}", e))?;
    if !status.success() {
        return Err("dotool failed".into());
    }

    Ok(())
}

/// Send a key combination (e.g., Ctrl+V) via ydotool (requires ydotoold daemon).
#[cfg(target_os = "linux")]
fn send_key_combo_via_ydotool(paste_method: &PasteMethod) -> Result<(), String> {
    // ydotool uses Linux input event keycodes with format <keycode>:<pressed>
    // where pressed is 1 for down, 0 for up. Keycodes: ctrl=29, shift=42, v=47, insert=110
    let args: Vec<&str> = match paste_method {
        PasteMethod::CtrlV => vec!["key", "29:1", "47:1", "47:0", "29:0"],
        PasteMethod::ShiftInsert => vec!["key", "42:1", "110:1", "110:0", "42:0"],
        PasteMethod::CtrlShiftV => vec!["key", "29:1", "42:1", "47:1", "47:0", "42:0", "29:0"],
        _ => return Err("Unsupported paste method".into()),
    };

    let output = Command::new("ydotool")
        .args(&args)
        .output()
        .map_err(|e| format!("Failed to execute ydotool: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("ydotool failed: {}", stderr));
    }

    Ok(())
}

/// Send a key combination (e.g., Ctrl+V) via xdotool on X11.
#[cfg(target_os = "linux")]
fn send_key_combo_via_xdotool(paste_method: &PasteMethod) -> Result<(), String> {
    let key_combo = match paste_method {
        PasteMethod::CtrlV => "ctrl+v",
        PasteMethod::CtrlShiftV => "ctrl+shift+v",
        PasteMethod::ShiftInsert => "shift+Insert",
        _ => return Err("Unsupported paste method".into()),
    };

    let output = Command::new("xdotool")
        .arg("key")
        .arg("--clearmodifiers")
        .arg(key_combo)
        .output()
        .map_err(|e| format!("Failed to execute xdotool: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("xdotool failed: {}", stderr));
    }

    Ok(())
}

/// Pastes text by invoking an external script.
/// The script receives the text to paste as a single argument.
fn paste_via_external_script(text: &str, script_path: &str) -> Result<(), String> {
    info!("Pasting via external script: {}", script_path);

    let output = Command::new(script_path)
        .arg(text)
        .output()
        .map_err(|e| format!("Failed to execute external script '{}': {}", script_path, e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        return Err(format!(
            "External script '{}' failed with exit code {:?}. stderr: {}, stdout: {}",
            script_path,
            output.status.code(),
            stderr.trim(),
            stdout.trim()
        ));
    }

    Ok(())
}

/// Types text directly by simulating individual key presses.
fn paste_direct(
    enigo: &mut Enigo,
    text: &str,
    #[cfg(target_os = "linux")] typing_tool: TypingTool,
) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        if try_direct_typing_linux(text, typing_tool)? {
            return Ok(());
        }
        info!("Falling back to enigo for direct text input");
    }

    input::paste_text_direct(enigo, text)
}

fn send_return_key(enigo: &mut Enigo, key_type: AutoSubmitKey) -> Result<(), String> {
    match key_type {
        AutoSubmitKey::Enter => {
            enigo
                .key(Key::Return, Direction::Press)
                .map_err(|e| format!("Failed to press Return key: {}", e))?;
            enigo
                .key(Key::Return, Direction::Release)
                .map_err(|e| format!("Failed to release Return key: {}", e))?;
        }
        AutoSubmitKey::CtrlEnter => {
            enigo
                .key(Key::Control, Direction::Press)
                .map_err(|e| format!("Failed to press Control key: {}", e))?;
            enigo
                .key(Key::Return, Direction::Press)
                .map_err(|e| format!("Failed to press Return key: {}", e))?;
            enigo
                .key(Key::Return, Direction::Release)
                .map_err(|e| format!("Failed to release Return key: {}", e))?;
            enigo
                .key(Key::Control, Direction::Release)
                .map_err(|e| format!("Failed to release Control key: {}", e))?;
        }
        AutoSubmitKey::CmdEnter => {
            enigo
                .key(Key::Meta, Direction::Press)
                .map_err(|e| format!("Failed to press Meta/Cmd key: {}", e))?;
            enigo
                .key(Key::Return, Direction::Press)
                .map_err(|e| format!("Failed to press Return key: {}", e))?;
            enigo
                .key(Key::Return, Direction::Release)
                .map_err(|e| format!("Failed to release Return key: {}", e))?;
            enigo
                .key(Key::Meta, Direction::Release)
                .map_err(|e| format!("Failed to release Meta/Cmd key: {}", e))?;
        }
    }

    Ok(())
}

fn should_send_auto_submit(auto_submit: bool, paste_method: PasteMethod) -> bool {
    auto_submit && paste_method != PasteMethod::None
}

pub fn paste(text: String, app_handle: AppHandle) -> Result<(), String> {
    let settings = get_settings(&app_handle);
    let paste_method = settings.paste_method;
    let paste_delay_ms = settings.paste_delay_ms;

    // Check if the previously frontmost app is a desktop/file manager (e.g., Finder).
    // On macOS, Finder interprets Cmd+V as "paste files" or does nothing with text.
    // When the user is focused on Finder/Desktop, we fall back to clipboard-only
    // mode and show a toast notification instead of attempting a paste.
    if crate::focus::is_saved_app_desktop_like(&app_handle) {
        info!("Frontmost app is Finder/Desktop — falling back to clipboard-only mode");
        let text_for_clipboard = if settings.append_trailing_space {
            format!("{} ", text)
        } else {
            text.clone()
        };
        match write_to_clipboard(&text_for_clipboard, &app_handle) {
            Ok(()) => {
                info!("Text copied to clipboard (desktop fallback)");
                let _ = app_handle.emit("paste-error-clipboard-fallback", &text_for_clipboard);
                return Ok(());
            }
            Err(e) => {
                return Err(format!("Clipboard fallback failed for desktop app: {}", e));
            }
        }
    }

    // Restore the previously frontmost application before pasting.
    // On macOS, the overlay's orderFrontRegardless can steal focus
    // from the user's target app. Without restoring, Cmd+V goes to
    // Handy instead of the user's text editor/terminal/browser.
    crate::focus::restore_frontmost_app(&app_handle);

    // Append trailing space if setting is enabled
    let text = if settings.append_trailing_space {
        format!("{} ", text)
    } else {
        text
    };

    info!(
        "Paste started: method={:?}, delay={}ms, text_len={}",
        paste_method,
        paste_delay_ms,
        text.len()
    );

    // ── Clipboard-based methods: do clipboard write + delay BEFORE Enigo lock ──
    // The Enigo lock is only needed for key-sending, not for clipboard I/O or
    // sleep delays. Holding it across paste_delay_ms starves other Enigo users.
    let clipboard_saved_content: Option<String> = match paste_method {
        PasteMethod::CtrlV | PasteMethod::CtrlShiftV | PasteMethod::ShiftInsert => {
            info!("Pasting via clipboard method (preparing clipboard)");
            Some(prepare_clipboard_for_paste(
                &text,
                &app_handle,
                paste_delay_ms,
            )?)
        }
        _ => None,
    };

    // Get the managed Enigo instance and acquire lock ONLY for key-sending operations.
    let enigo_state = app_handle
        .try_state::<EnigoState>()
        .ok_or("Enigo state not initialized")?;

    info!("Acquiring Enigo lock...");
    let mut enigo = enigo_state.0.lock();
    info!("Enigo lock acquired");

    // Perform the paste operation
    let result = match paste_method {
        PasteMethod::None => {
            info!("PasteMethod::None selected - skipping paste action");
            Ok(())
        }
        PasteMethod::Direct => {
            info!("Pasting via Direct method");
            paste_direct(
                &mut enigo,
                &text,
                #[cfg(target_os = "linux")]
                settings.typing_tool,
            )
        }
        PasteMethod::CtrlV | PasteMethod::CtrlShiftV | PasteMethod::ShiftInsert => {
            info!("Pasting via clipboard method (sending keys)");
            // Clipboard was already prepared and delay was already slept —
            // just send the key combo and verify.
            send_paste_keys_and_verify(
                &mut enigo,
                &text,
                &app_handle,
                &paste_method,
                clipboard_saved_content.as_deref().unwrap_or(""),
            )
        }
        PasteMethod::ExternalScript => {
            info!("Pasting via external script");
            let script_path = settings
                .external_script_path
                .as_ref()
                .filter(|p| !p.is_empty())
                .ok_or("External script path is not configured")?;
            paste_via_external_script(&text, script_path)
        }
    };

    match &result {
        Ok(()) => {
            info!("Paste completed successfully");

            // If paste succeeded, still copy to clipboard if setting enabled
            if settings.clipboard_handling == ClipboardHandling::CopyToClipboard {
                info!("Copying text to clipboard");
                if let Err(e) = app_handle.clipboard().write_text(&text) {
                    warn!("Failed to copy to clipboard after paste: {}", e);
                }
            }
        }
        Err(e) => {
            warn!("Paste failed ({}), falling back to clipboard", e);

            // ALWAYS fall back to clipboard on paste failure
            match write_to_clipboard(&text, &app_handle) {
                Ok(()) => {
                    info!("Text copied to clipboard as fallback after paste failure");
                    // Notify frontend that we fell back to clipboard
                    let _ = app_handle.emit("paste-error-clipboard-fallback", &text);
                }
                Err(clipboard_err) => {
                    error!("Clipboard fallback also failed: {}", clipboard_err);
                    // Both paste and clipboard failed — return the combined error
                    return Err(format!(
                        "Paste failed and clipboard fallback failed: {} | {}",
                        e, clipboard_err
                    ));
                }
            }
        }
    }

    // Only send auto-submit if the paste operation itself succeeded.
    // When paste fails and clipboard fallback is used, the text is in
    // the clipboard but not in the target field — pressing Enter would
    // submit nothing or the wrong content.
    if result.is_ok() && should_send_auto_submit(settings.auto_submit, paste_method) {
        info!("Sending auto-submit key");
        std::thread::sleep(std::time::Duration::from_millis(50));
        if let Err(e) = send_return_key(&mut enigo, settings.auto_submit_key) {
            error!("Auto-submit failed: {}", e);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_submit_requires_setting_enabled() {
        assert!(!should_send_auto_submit(false, PasteMethod::CtrlV));
        assert!(!should_send_auto_submit(false, PasteMethod::Direct));
    }

    #[test]
    fn auto_submit_skips_none_paste_method() {
        assert!(!should_send_auto_submit(true, PasteMethod::None));
    }

    #[test]
    fn auto_submit_runs_for_active_paste_methods() {
        assert!(should_send_auto_submit(true, PasteMethod::CtrlV));
        assert!(should_send_auto_submit(true, PasteMethod::Direct));
        assert!(should_send_auto_submit(true, PasteMethod::CtrlShiftV));
        assert!(should_send_auto_submit(true, PasteMethod::ShiftInsert));
    }
}
