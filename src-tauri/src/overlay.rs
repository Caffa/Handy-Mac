use crate::input;
use crate::settings;
use crate::settings::{OverlayPosition, OverlayScreenTarget};
use crate::transcription_coordinator::{emit_app_state, AppState};
use log::{debug, info};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize};

use crate::transcription_coordinator::TranscriptionCoordinator;

/// Session counter for overlay hide-guard. Incremented when a new recording
/// starts so any pending hide from a previous session is invalidated.
static OVERLAY_SESSION: AtomicU64 = AtomicU64::new(0);

/// Bump the overlay session counter. Called when a new recording starts.
/// Any pending hide operation from a previous session will see the session
/// has changed and will skip hiding the window.
pub fn bump_overlay_session() -> u64 {
    OVERLAY_SESSION.fetch_add(1, Ordering::SeqCst)
}

/// Global flag controlling whether the overlay can become the key window
/// (accept keyboard input). Set to true when the user is editing transcription
/// text in the overlay, and restored to false when editing ends.
/// On macOS, the RecordingOverlayPanel class has `can_become_key_window: false`
/// which prevents it from accepting keyboard input. When this flag is true,
/// the swizzled `canBecomeKeyWindow` method returns true, allowing the panel
/// to become key and accept keyboard focus for text editing.
#[cfg(target_os = "macos")]
static OVERLAY_CAN_BECOME_KEY: AtomicBool = AtomicBool::new(false);

/// Global flag for cursor tracking on macOS.
/// When the cursor enters the overlay (via NSTrackingArea), this flag is set
/// to true and `ignoresMouseEvents` is disabled, allowing mouse events to
/// reach the webview for interactive elements (cancel button, edit textarea).
/// When the cursor exits the overlay, the flag is set to false and
/// `ignoresMouseEvents` is re-enabled, making the panel click-through.
///
/// BUGFIX (Fix 5): This replaces the React onMouseEnter/onMouseLeave approach,
/// which has a chicken-and-egg problem: the panel is click-through, so mouse
/// events can't reach the webview to trigger the handlers. NSTrackingArea with
/// ActiveAlways fires cursor-tracking events even for click-through panels.
#[cfg(target_os = "macos")]
static OVERLAY_CURSOR_IN_PANEL: AtomicBool = AtomicBool::new(false);

// Cached "overlay is enabled" flag, kept in sync with the overlay_position
// setting. Avoids reading the Tauri store on every audio callback (~24 Hz
// during recording). Defaults to false so the audio path doesn't emit until
// lib.rs::setup populates the cache from initial settings.
static OVERLAY_ENABLED: AtomicBool = AtomicBool::new(false);

// Throttle mic-level emission to ~30 FPS to mitigate the WebKitWebProcess
// memory leak (tauri-apps/wry#1489). The raw audio callback fires far faster
// than the UI needs; capping the rate cuts per-frame eval_script/IPC volume.
static LAST_MIC_LEVEL_EMIT: AtomicU64 = AtomicU64::new(0);
const EMIT_THROTTLE_MS: u64 = 33; // ~30 FPS

/// Swizzle the `canBecomeKeyWindow` method on the RecordingOverlayPanel class
/// to check the `OVERLAY_CAN_BECOME_KEY` global flag instead of always returning false.
/// This must be called once after the RecordingOverlayPanel class is registered
/// with the Objective-C runtime (i.e., after `create_recording_overlay` creates the panel).
///
/// # Safety
/// This uses the Objective-C runtime to replace a method implementation. It is safe
/// because:
/// - The RecordingOverlayPanel class exists (created by `tauri_panel!` macro)
/// - The replacement function has the correct signature for `canBecomeKeyWindow`
/// - The `OVERLAY_CAN_BECOME_KEY` flag is accessed with atomic ordering
#[cfg(target_os = "macos")]
fn swizzle_can_become_key_window() {
    use objc2::runtime::AnyClass;
    use std::ffi::CStr;

    let class_name = CStr::from_bytes_with_nul(b"RecordingOverlayPanel\0").unwrap();
    let Some(class) = AnyClass::get(class_name) else {
        log::error!("swizzle_can_become_key_window: RecordingOverlayPanel class not found");
        return;
    };

    // Safety: We're replacing the `canBecomeKeyWindow` instance method on
    // RecordingOverlayPanel. This is an instance method that returns BOOL (bool).
    // Our replacement reads a global AtomicBool, which is thread-safe.
    unsafe {
        /// Replacement canBecomeKeyWindow implementation that reads the global flag.
        /// This is called by the Objective-C runtime when the window system checks
        /// whether this panel can become the key window.
        ///
        /// # Safety
        /// The Objective-C runtime passes `self` and `_cmd` as the first two
        /// arguments, matching the standard messaging convention.
        unsafe extern "C-unwind" fn overlay_can_become_key(
            _this: *mut objc2::runtime::AnyObject,
            _cmd: objc2::runtime::Sel,
        ) -> bool {
            OVERLAY_CAN_BECOME_KEY.load(Ordering::SeqCst)
        }

        let sel = objc2::sel!(canBecomeKeyWindow);
        // Type encoding for canBecomeKeyWindow method:
        // Return type: BOOL (encoded as "B" on macOS ARM64 where BOOL = bool)
        // The self and _cmd parameters are implicit in ObjC encoding.
        let types = CStr::from_bytes_with_nul(b"B\0").unwrap();
        let imp: objc2::runtime::Imp = std::mem::transmute::<
            unsafe extern "C-unwind" fn(
                *mut objc2::runtime::AnyObject,
                objc2::runtime::Sel,
            ) -> bool,
            objc2::runtime::Imp,
        >(overlay_can_become_key);
        objc2::ffi::class_replaceMethod(
            class as *const AnyClass as *mut AnyClass,
            sel,
            imp,
            types.as_ptr(),
        );
    }

    log::info!("swizzle_can_become_key_window: Successfully swizzled canBecomeKeyWindow on RecordingOverlayPanel");
}

/// Swizzle `mouseEntered:` and `mouseExited:` on RecordingOverlayPanel to handle
/// cursor tracking for click-through overlays.
///
/// BUGFIX (Fix 5): The cancel button on the overlay relies on React `onMouseEnter`
/// to toggle click-through off, but `onMouseEnter` can't fire while the NSPanel is
/// click-through (`ignoresMouseEvents = true`). This is a chicken-and-egg problem.
///
/// The solution uses macOS's NSTrackingArea with `NSTrackingActiveAlways`, which
/// fires `mouseEntered:` and `mouseExited:` events even for click-through panels.
/// When the cursor enters the panel, we temporarily disable `ignoresMouseEvents`
/// so interactive elements can receive events. When the cursor exits, we re-enable
/// `ignoresMouseEvents` for click-through.
///
/// This is the standard pattern used by macOS HUD/floating-overlay apps.
#[cfg(target_os = "macos")]
fn swizzle_mouse_tracking() {
    use objc2::runtime::AnyClass;
    use std::ffi::CStr;

    let class_name = CStr::from_bytes_with_nul(b"RecordingOverlayPanel\0").unwrap();
    let Some(class) = AnyClass::get(class_name) else {
        log::error!("swizzle_mouse_tracking: RecordingOverlayPanel class not found");
        return;
    };

    unsafe {
        // Swizzle mouseEntered: — called when the cursor enters the tracking area.
        // When the cursor enters the panel, we disable ignoresMouseEvents so the
        // webview can receive mouse events for interactive elements.
        unsafe extern "C-unwind" fn overlay_mouse_entered(
            this: &objc2::runtime::AnyObject,
            _cmd: objc2::runtime::Sel,
            _event: &objc2::runtime::AnyObject,
        ) {
            OVERLAY_CURSOR_IN_PANEL.store(true, Ordering::SeqCst);

            // Disable ignoresMouseEvents so interactive elements (buttons,
            // textareas) can receive mouse events.
            let _: () = objc2::msg_send![this, setIgnoresMouseEvents: false];

            log::debug!("Overlay mouse entered: disabled ignoresMouseEvents");
        }

        let mouse_entered_sel = objc2::sel!(mouseEntered:);
        let types = CStr::from_bytes_with_nul(b"v@:@\0").unwrap();
        let imp: objc2::runtime::Imp = std::mem::transmute::<
            unsafe extern "C-unwind" fn(
                &objc2::runtime::AnyObject,
                objc2::runtime::Sel,
                &objc2::runtime::AnyObject,
            ),
            objc2::runtime::Imp,
        >(overlay_mouse_entered);
        objc2::ffi::class_replaceMethod(
            class as *const AnyClass as *mut AnyClass,
            mouse_entered_sel,
            imp,
            types.as_ptr(),
        );

        // Swizzle mouseExited: — called when the cursor exits the tracking area.
        // When the cursor exits the panel, we re-enable ignoresMouseEvents for
        // click-through behavior.
        unsafe extern "C-unwind" fn overlay_mouse_exited(
            this: &objc2::runtime::AnyObject,
            _cmd: objc2::runtime::Sel,
            _event: &objc2::runtime::AnyObject,
        ) {
            OVERLAY_CURSOR_IN_PANEL.store(false, Ordering::SeqCst);

            // Only re-enable click-through if the overlay doesn't need keyboard focus.
            // If the user is editing text (can_become_key), we keep mouse events enabled
            // so they can continue interacting with the text area.
            if !OVERLAY_CAN_BECOME_KEY.load(Ordering::SeqCst) {
                let _: () = objc2::msg_send![this, setIgnoresMouseEvents: true];
            }

            log::debug!("Overlay mouse exited: re-enabled ignoresMouseEvents");
        }

        let mouse_exited_sel = objc2::sel!(mouseExited:);
        let types = CStr::from_bytes_with_nul(b"v@:@\0").unwrap();
        let imp: objc2::runtime::Imp = std::mem::transmute::<
            unsafe extern "C-unwind" fn(
                &objc2::runtime::AnyObject,
                objc2::runtime::Sel,
                &objc2::runtime::AnyObject,
            ),
            objc2::runtime::Imp,
        >(overlay_mouse_exited);
        objc2::ffi::class_replaceMethod(
            class as *const AnyClass as *mut AnyClass,
            mouse_exited_sel,
            imp,
            types.as_ptr(),
        );
    }

    log::info!("swizzle_mouse_tracking: Successfully swizzled mouseEntered:/mouseExited: on RecordingOverlayPanel");
}

/// Add an NSTrackingArea to the overlay panel so that mouseEntered:/mouseExited:
/// are called even when the panel is click-through (ignoresMouseEvents = true).
///
/// BUGFIX (Fix 5): Without this tracking area, the swizzled mouseEntered:/mouseExited:
/// methods would never be called, and we'd have no way to detect when the cursor
/// enters/exits the overlay while it's click-through.
///
/// The NSTrackingActiveAlways option ensures tracking works regardless of the
/// panel's active state, which is critical for a floating overlay that should
/// work even when another app is frontmost.
#[cfg(target_os = "macos")]
fn add_cursor_tracking_area(app_handle: &AppHandle) {
    use objc2_app_kit::NSTrackingAreaOptions;

    let Some(overlay_window) = app_handle.get_webview_window("recording_overlay") else {
        log::debug!("add_cursor_tracking_area: overlay window not found");
        return;
    };

    let Ok(panel) = overlay_window.to_panel::<RecordingOverlayPanel>() else {
        log::debug!("add_cursor_tracking_area: could not convert to panel");
        return;
    };

    // Get the content view of the panel. This is the NSView that fills the
    // panel and is where we add the tracking area.
    let content_view = panel.content_view();

    // Get the window (panel) from the content view. The window IS the
    // RecordingOverlayPanel, which is where we swizzled mouseEntered:/mouseExited:.
    // We set the window as the tracking area's owner so that mouse tracking events
    // are delivered to the panel (where our swizzled handlers are), not to the
    // content view.
    //
    // BUGFIX: Previously, the content_view was set as the owner, which meant
    // mouseEntered:/mouseExited: were delivered to the content view (NSView).
    // But we swizzled these methods on RecordingOverlayPanel (NSPanel), so the
    // events never reached our handlers. Setting the panel as the owner ensures
    // the events go to the right place.
    let window: Option<objc2::rc::Retained<objc2_app_kit::NSWindow>> =
        unsafe { objc2::msg_send![&content_view, window] };
    let Some(window) = window else {
        log::error!("add_cursor_tracking_area: content view has no window");
        return;
    };

    // Create NSTrackingArea options:
    // MouseEnteredAndExited: fire when cursor enters/exits the tracking rect
    // ActiveAlways: fire even when the app is inactive or the panel is click-through
    // InVisibleRect: automatically update the tracking rect when the view resizes
    let options = NSTrackingAreaOptions::MouseEnteredAndExited
        | NSTrackingAreaOptions::ActiveAlways
        | NSTrackingAreaOptions::InVisibleRect;

    // Create the tracking area covering the entire content view.
    // The window (panel) is the owner — mouseEntered:/mouseExited: messages will be
    // delivered to the panel, where our swizzled handlers will toggle ignoresMouseEvents.
    //
    // NOTE: The owner is NOT retained by the tracking area on macOS 10.10+.
    // The window outlives the tracking area (the tracking area is added to the
    // content view, which is owned by the window), so this is safe.
    //
    // We use raw msg_send! because NSTrackingArea::alloc() requires a
    // MainThreadMarker which is awkward to obtain in this context.
    // Since this code runs on the main thread (via run_on_main_thread), this is safe.
    let bounds: objc2_foundation::NSRect = unsafe { objc2::msg_send![&content_view, bounds] };
    let tracking_area: objc2::rc::Retained<objc2_app_kit::NSTrackingArea> = unsafe {
        let alloc: *mut objc2_app_kit::NSTrackingArea =
            objc2::msg_send![objc2_app_kit::NSTrackingArea::class(), alloc];
        let area: *mut objc2_app_kit::NSTrackingArea = objc2::msg_send![
            alloc,
            initWithRect: bounds,
            options: options.bits(),
            owner: &*window as *const _,
            userInfo: objc2::ffi::nil
        ];
        objc2::rc::Retained::from_raw(area).expect("NSTrackingArea init failed")
    };

    // Add the tracking area to the content view.
    // SAFETY: addTrackingArea retains the tracking area for the view's lifetime.
    unsafe {
        let _: () = objc2::msg_send![&content_view, addTrackingArea: &*tracking_area];
    }

    log::info!("add_cursor_tracking_area: added NSTrackingArea to overlay panel");
}

/// Tauri command to toggle the overlay window's ability to become the key window
/// (accept keyboard input). On macOS, this swizzles the RecordingOverlayPanel's
/// `canBecomeKeyWindow` method to check a global flag.
///
/// When `can_become_key` is true, the overlay can accept keyboard focus,
/// enabling text editing in the transcription preview. When false, the overlay
/// returns to its default behavior of not accepting keyboard input.
///
/// On non-macOS platforms, this is a no-op (keyboard input works by default).
#[tauri::command]
#[specta::specta]
pub fn set_overlay_can_become_key(app: AppHandle, can_become_key: bool) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        OVERLAY_CAN_BECOME_KEY.store(can_become_key, Ordering::SeqCst);

        if let Some(overlay_window) = app.get_webview_window("recording_overlay") {
            let window = overlay_window.clone();
            let _ = overlay_window.run_on_main_thread(move || {
                if let Ok(panel) = window.to_panel::<RecordingOverlayPanel>() {
                    if can_become_key {
                        // Make the panel the key window so it receives keyboard input.
                        // This is necessary because the overlay is normally non-activating
                        // and cannot become key by default.
                        panel.make_key_and_order_front();
                    } else {
                        // Resign key window status when editing ends, restoring
                        // the overlay's normal click-through behavior.
                        panel.resign_key_window();
                    }
                }
            });
        }

        Ok(())
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, can_become_key);
        Ok(())
    }
}

/// Tauri command to toggle whether the overlay panel accepts mouse events.
///
/// When `enabled` is true, the overlay panel accepts mouse events (for interactive
/// elements like buttons and textareas). When `enabled` is false, all mouse events
/// (clicks, scrolls, hovers) pass through to apps below the overlay.
///
/// This implements the standard macOS pattern for click-through overlays:
/// - Default: ignores_mouse_events = true (click-through)
/// - Mouse enters interactive element: ignores_mouse_events = false (accept events)
/// - Mouse leaves interactive element: ignores_mouse_events = true (click-through)
///
/// BUGFIX (Fix 5): On macOS, the previous approach relied on React onMouseEnter/
/// onMouseLeave to toggle click-through. This has a chicken-and-egg problem: the
/// panel is click-through, so mouse events can't reach the webview to trigger
/// onMouseEnter. The NSTrackingArea approach works at the OS level and fires
/// cursorUpdate: events even for click-through panels.
///
/// On non-macOS platforms, this is a no-op (CSS pointer-events handles this).
#[tauri::command]
#[specta::specta]
pub fn set_overlay_mouse_passthrough(app: AppHandle, enabled: bool) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        if let Some(overlay_window) = app.get_webview_window("recording_overlay") {
            let window = overlay_window.clone();
            let _ = overlay_window.run_on_main_thread(move || {
                if let Ok(panel) = window.to_panel::<RecordingOverlayPanel>() {
                    // When enabled=true, the panel should accept mouse events
                    // (ignores_mouse_events = false).
                    // When enabled=false, mouse events pass through
                    // (ignores_mouse_events = true).
                    panel.set_ignores_mouse_events(!enabled);
                }
            });
        }
        Ok(())
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, enabled);
        Ok(())
    }
}

#[cfg(not(target_os = "macos"))]
use tauri::WebviewWindowBuilder;

#[cfg(target_os = "macos")]
use tauri::WebviewUrl;

#[cfg(target_os = "macos")]
use tauri_nspanel::{tauri_panel, CollectionBehavior, PanelBuilder, PanelLevel, WebviewWindowExt};

#[cfg(target_os = "linux")]
use gtk_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

#[cfg(target_os = "linux")]
use std::env;

#[cfg(target_os = "macos")]
tauri_panel! {
    panel!(RecordingOverlayPanel {
        config: {
            can_become_key_window: false,
            is_floating_panel: true
        }
    })
}

/// Native window width for transcription preview — needs to accommodate
/// the live captions box which can be up to 600px (or 1200px at 2x scale).
/// Also accommodates the preview text which is ~3x wider than the visualizer pill (516px).
const OVERLAY_WINDOW_WIDTH: f64 = 800.0;
/// Base window width (used for scaled calculations).
const OVERLAY_WINDOW_WIDTH_BASE: f64 = 800.0;
/// Minimum window height for the recording pill (just the pill, no preview).
const OVERLAY_WINDOW_MIN_HEIGHT: f64 = 100.0;
/// Visible pill height used for position calculations.
const OVERLAY_PILL_HEIGHT: f64 = 50.0;
/// Maximum percentage of screen height to use for the overlay window.
const OVERLAY_MAX_SCREEN_RATIO: f64 = 0.85;
/// Window height for live captions mode (taller to show multi-line text)
const OVERLAY_LIVE_CAPTIONS_HEIGHT: f64 = 280.0;

#[cfg(target_os = "macos")]
const OVERLAY_TOP_OFFSET: f64 = 46.0;
#[cfg(any(target_os = "windows", target_os = "linux"))]
const OVERLAY_TOP_OFFSET: f64 = 4.0;

#[cfg(target_os = "macos")]
const OVERLAY_BOTTOM_OFFSET: f64 = 15.0;

#[cfg(any(target_os = "windows", target_os = "linux"))]
const OVERLAY_BOTTOM_OFFSET: f64 = 40.0;

#[cfg(target_os = "linux")]
fn update_gtk_layer_shell_anchors(overlay_window: &tauri::webview::WebviewWindow) {
    let window_clone = overlay_window.clone();
    let _ = overlay_window.run_on_main_thread(move || {
        // Try to get the GTK window from the Tauri webview
        if let Ok(gtk_window) = window_clone.gtk_window() {
            let settings = settings::get_settings_safe(window_clone.app_handle());
            match settings.overlay_position {
                OverlayPosition::Top => {
                    gtk_window.set_anchor(Edge::Top, true);
                    gtk_window.set_anchor(Edge::Bottom, false);
                }
                OverlayPosition::Bottom | OverlayPosition::None => {
                    gtk_window.set_anchor(Edge::Bottom, true);
                    gtk_window.set_anchor(Edge::Top, false);
                }
            }
        }
    });
}

/// Returns true when the environment variable is set to a truthy value
/// (e.g. "1", "true", "yes", "on").
/// "0", "false", "no", "off" and empty string are treated as falsy (case-insensitive).
/// Returns false when the variable is not set.
#[cfg(target_os = "linux")]
fn env_flag_enabled(name: &str) -> bool {
    match env::var(name) {
        Ok(v) => !matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "" | "0" | "false" | "no" | "off"
        ),
        Err(_) => false,
    }
}

/// Initializes GTK layer shell for Linux overlay window
/// Returns true if layer shell was successfully initialized, false otherwise
#[cfg(target_os = "linux")]
fn init_gtk_layer_shell(overlay_window: &tauri::webview::WebviewWindow) -> bool {
    if env_flag_enabled("HANDY_NO_GTK_LAYER_SHELL") {
        debug!("Skipping GTK layer shell init (HANDY_NO_GTK_LAYER_SHELL is enabled)");
        return false;
    }

    if !gtk_layer_shell::is_supported() {
        return false;
    }

    // Try to get the GTK window from the Tauri webview
    if let Ok(gtk_window) = overlay_window.gtk_window() {
        // Initialize layer shell
        gtk_window.init_layer_shell();
        gtk_window.set_layer(Layer::Overlay);
        gtk_window.set_keyboard_mode(KeyboardMode::None);
        gtk_window.set_exclusive_zone(0);

        update_gtk_layer_shell_anchors(overlay_window);

        return true;
    }
    false
}

/// Forces a window to be topmost using Win32 API (Windows only)
/// This is more reliable than Tauri's set_always_on_top which can be overridden
#[cfg(target_os = "windows")]
fn force_overlay_topmost(overlay_window: &tauri::webview::WebviewWindow) {
    use windows::Win32::UI::WindowsAndMessaging::{
        SetWindowPos, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW,
    };

    // Clone because run_on_main_thread takes 'static
    let overlay_clone = overlay_window.clone();

    // Make sure the Win32 call happens on the UI thread
    let _ = overlay_clone.clone().run_on_main_thread(move || {
        if let Ok(hwnd) = overlay_clone.hwnd() {
            unsafe {
                // Force Z-order: make this window topmost without changing size/pos or stealing focus
                let _ = SetWindowPos(
                    hwnd,
                    Some(HWND_TOPMOST),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
                );
            }
        }
    });
}

fn get_monitor_with_cursor(app_handle: &AppHandle) -> Option<tauri::Monitor> {
    if let Some(mouse_location) = input::get_cursor_position(app_handle) {
        if let Ok(monitors) = app_handle.available_monitors() {
            for monitor in monitors {
                // Tauri's monitor position/size are physical pixels, but enigo
                // may return logical coordinates (confirmed on macOS via
                // NSEvent::mouseLocation; on Windows, GetCursorPos behavior
                // depends on the process DPI-awareness context). Dividing by
                // scale_factor normalizes to logical, which is safe regardless:
                // if enigo returns logical it matches directly, and if it returns
                // physical on a scale=1 monitor the division is a no-op.
                let scale = monitor.scale_factor();
                let pos = PhysicalPosition::new(
                    (monitor.position().x as f64 / scale) as i32,
                    (monitor.position().y as f64 / scale) as i32,
                );
                let size = PhysicalSize::new(
                    (monitor.size().width as f64 / scale) as u32,
                    (monitor.size().height as f64 / scale) as u32,
                );
                if is_mouse_within_monitor(mouse_location, &pos, &size) {
                    return Some(monitor);
                }
            }
        }
    }

    app_handle.primary_monitor().ok().flatten()
}

/// Returns the monitor on which the overlay should appear, based on the
/// `overlay_screen_target` setting:
/// - `Cursor` (default): same monitor as the mouse cursor.
/// - `SideScreen`: the first monitor that is NOT the cursor's monitor.
///   Falls back to the cursor monitor (then primary) if no other monitor exists.
fn get_target_overlay_monitor(app_handle: &AppHandle) -> Option<tauri::Monitor> {
    let screen_target = settings::get_settings_safe(app_handle).overlay_screen_target;

    match screen_target {
        OverlayScreenTarget::Cursor => {
            let monitor = get_monitor_with_cursor(app_handle);
            debug!(
                "get_target_overlay_monitor: Cursor target, using monitor at ({:?})",
                monitor.as_ref().map(|m| m.position())
            );
            monitor
        }
        OverlayScreenTarget::SideScreen => {
            let cursor_monitor = get_monitor_with_cursor(app_handle);

            if let Some(ref cm) = cursor_monitor {
                // Find the first monitor whose position differs from the cursor monitor
                if let Ok(monitors) = app_handle.available_monitors() {
                    for monitor in &monitors {
                        if monitor.position() != cm.position() {
                            debug!(
                                "get_target_overlay_monitor: SideScreen target, cursor at ({}, {}), using side monitor at ({}, {})",
                                cm.position().x, cm.position().y,
                                monitor.position().x, monitor.position().y
                            );
                            return Some(monitor.clone());
                        }
                    }
                }
                debug!(
                    "get_target_overlay_monitor: SideScreen target but no other monitor found, falling back to cursor monitor"
                );
                cursor_monitor
            } else {
                debug!(
                    "get_target_overlay_monitor: SideScreen target but cursor monitor unknown, falling back to primary"
                );
                app_handle.primary_monitor().ok().flatten()
            }
        }
    }
}

/// Calculate the overlay window height based on monitor height.
/// Uses a percentage of screen height to accommodate variable-length
/// transcription text without clipping, with a minimum for the pill.
fn calculate_overlay_window_height(monitor_height: f64) -> f64 {
    (monitor_height * OVERLAY_MAX_SCREEN_RATIO).max(OVERLAY_WINDOW_MIN_HEIGHT)
}

fn is_mouse_within_monitor(
    mouse_pos: (i32, i32),
    monitor_pos: &PhysicalPosition<i32>,
    monitor_size: &PhysicalSize<u32>,
) -> bool {
    let (mouse_x, mouse_y) = mouse_pos;
    let PhysicalPosition {
        x: monitor_x,
        y: monitor_y,
    } = *monitor_pos;
    let PhysicalSize {
        width: monitor_width,
        height: monitor_height,
    } = *monitor_size;

    mouse_x >= monitor_x
        && mouse_x < (monitor_x + monitor_width as i32)
        && mouse_y >= monitor_y
        && mouse_y < (monitor_y + monitor_height as i32)
}

/// Returns overlay position and window height in logical coordinates (points on macOS).
///
/// On macOS, the Bottom anchor uses the work area (visibleFrame) so the overlay
/// tracks the Dock — above it when shown, at the screen edge when hidden. This
/// relies on tauri 2.11's work_area.position.y fix (#14655), the same bug that
/// led PR #969 to abandon work_area for full monitor bounds. Top and the other
/// platforms keep full monitor bounds plus the fixed offsets (work_area is
/// unreliable on Wayland; Windows' offset clears the taskbar).
///
/// We must use LogicalPosition (not PhysicalPosition) because Tauri/tao
/// converts PhysicalPosition using the scale factor of the monitor the window
/// is *currently* on, which is wrong when moving cross-monitor.
fn calculate_overlay_position(app_handle: &AppHandle) -> Option<(f64, f64, f64)> {
    let monitor = get_target_overlay_monitor(app_handle)?;
    let scale = monitor.scale_factor();
    let monitor_x = monitor.position().x as f64 / scale;
    let monitor_y = monitor.position().y as f64 / scale;
    let monitor_width = monitor.size().width as f64 / scale;
    let monitor_height = monitor.size().height as f64 / scale;

    // Dynamic window height based on screen size (85% of screen height)
    let window_height = calculate_overlay_window_height(monitor_height);

    let settings = settings::get_settings_safe(app_handle);

    debug!(
        "calculate_overlay_position: overlay_position={:?}, monitor=({},{}) {}x{}",
        settings.overlay_position, monitor_x, monitor_y, monitor_width, monitor_height
    );

    // Center the window which is wider than the pill
    let x = monitor_x + (monitor_width - OVERLAY_WINDOW_WIDTH) / 2.0;
    let y = match settings.overlay_position {
        OverlayPosition::Top => {
            let pos_y = monitor_y + OVERLAY_TOP_OFFSET;
            debug!("calculate_overlay_position: Top position, y={}", pos_y);
            pos_y
        }
        OverlayPosition::Bottom | OverlayPosition::None => {
            // Use pill height for positioning so the visible content sits at
            // the same screen position regardless of the taller transparent window.
            let window_extra = window_height - OVERLAY_PILL_HEIGHT;

            // On macOS, use the work area (visibleFrame) so the overlay
            // tracks the Dock — above it when shown, at the screen edge
            // when hidden. work_area shares monitor.position's global
            // coordinate space, so no monitor offset is added.
            #[cfg(target_os = "macos")]
            let bottom = {
                let wa = monitor.work_area();
                (wa.position.y as f64 + wa.size.height as f64) / scale
            };
            #[cfg(not(target_os = "macos"))]
            let bottom = monitor_y + monitor_height;

            let pos_y = bottom - OVERLAY_PILL_HEIGHT - OVERLAY_BOTTOM_OFFSET - window_extra / 2.0;
            debug!(
                "calculate_overlay_position: Bottom/None position, y={}",
                pos_y
            );
            pos_y
        }
    };

    debug!("calculate_overlay_position: final position ({}, {})", x, y);
    Some((x, y, window_height))
}

/// Creates the recording overlay window and keeps it hidden by default
#[cfg(not(target_os = "macos"))]
pub fn create_recording_overlay(app_handle: &AppHandle) {
    // Get initial monitor to determine window height
    let (initial_height, has_monitor) = match get_target_overlay_monitor(app_handle) {
        Some(monitor) => {
            let scale = monitor.scale_factor();
            let monitor_height = monitor.size().height as f64 / scale;
            (calculate_overlay_window_height(monitor_height), true)
        }
        None => {
            // On Linux Wayland, monitor detection may fail; use a reasonable default
            (calculate_overlay_window_height(1080.0), false)
        }
    };

    // On non-Linux, require a monitor
    #[cfg(not(target_os = "linux"))]
    if !has_monitor {
        debug!("Failed to determine overlay position, not creating overlay window");
        return;
    }

    // Position starts unset — update_overlay_position() sets the correct
    // LogicalPosition before the overlay is shown.
    let mut builder = WebviewWindowBuilder::new(
        app_handle,
        "recording_overlay",
        tauri::WebviewUrl::App("src/overlay/index.html".into()),
    )
    .title("Recording")
    .resizable(false)
    .inner_size(OVERLAY_WINDOW_WIDTH, initial_height)
    .shadow(false)
    .maximizable(false)
    .minimizable(false)
    .closable(false)
    .accept_first_mouse(true)
    .decorations(false)
    .always_on_top(true)
    .skip_taskbar(true)
    .transparent(true)
    .focused(false)
    .visible(false);

    if let Some(data_dir) = crate::portable::data_dir() {
        builder = builder.data_directory(data_dir.join("webview"));
    }

    #[allow(unused_variables)]
    match builder.build() {
        Ok(window) => {
            #[cfg(target_os = "linux")]
            {
                // Try to initialize GTK layer shell, ignore errors if compositor doesn't support it
                if init_gtk_layer_shell(&window) {
                    debug!("GTK layer shell initialized for overlay window");
                } else {
                    debug!("GTK layer shell not available, falling back to regular window");
                }
            }

            debug!("Recording overlay window created successfully (hidden)");
        }
        Err(e) => {
            debug!("Failed to create recording overlay window: {}", e);
        }
    }
}

/// Creates the recording overlay panel and keeps it hidden by default (macOS)
#[cfg(target_os = "macos")]
pub fn create_recording_overlay(app_handle: &AppHandle) {
    if let Some((x, y, window_height)) = calculate_overlay_position(app_handle) {
        // PanelBuilder creates a Tauri window then converts it to NSPanel.
        // The window remains registered, so get_webview_window() still works.
        match PanelBuilder::<_, RecordingOverlayPanel>::new(app_handle, "recording_overlay")
            .url(WebviewUrl::App("src/overlay/index.html".into()))
            .title("Recording")
            .position(tauri::Position::Logical(tauri::LogicalPosition { x, y }))
            .level(PanelLevel::Status)
            .size(tauri::Size::Logical(tauri::LogicalSize {
                width: OVERLAY_WINDOW_WIDTH,
                height: window_height,
            }))
            .has_shadow(false)
            .transparent(true)
            .no_activate(true)
            .corner_radius(0.0)
            .with_window(|w| w.decorations(false).transparent(true))
            .collection_behavior(
                CollectionBehavior::new()
                    .can_join_all_spaces()
                    .full_screen_auxiliary(),
            )
            .build()
        {
            Ok(panel) => {
                let _ = panel.hide();

                // Swizzle canBecomeKeyWindow on the RecordingOverlayPanel class
                // so it checks OVERLAY_CAN_BECOME_KEY instead of always returning false.
                // This must happen after the class is registered by PanelBuilder::build()
                // which triggers define_class! registration.
                swizzle_can_become_key_window();

                // BUGFIX (Fix 5): Swizzle mouseEntered:/mouseExited: on the
                // RecordingOverlayPanel to handle cursor tracking for click-through.
                // When the cursor enters the panel, we temporarily disable
                // ignoresMouseEvents so interactive elements (cancel button,
                // edit textarea) can receive mouse events. When the cursor
                // exits, we re-enable click-through.
                //
                // NSTrackingArea with ActiveAlways fires these events even for
                // click-through panels, solving the chicken-and-egg problem
                // where onMouseEnter can't fire because the panel is click-through.
                swizzle_mouse_tracking();

                // Add an NSTrackingArea to the panel's content view so that
                // mouseEntered:/mouseExited: are called even when the panel
                // is click-through.
                add_cursor_tracking_area(app_handle);
            }
            Err(e) => {
                log::error!("Failed to create recording overlay panel: {}", e);
            }
        }
    }
}

/// Overlay action mode — determines which visual theme the frontend uses.
/// The payload emitted to the frontend is formatted as `"{state}:{action}"`
/// (e.g. `"recording:transcribe"`, `"transcribing:router"`), allowing the
/// overlay to vary icon/colours/labels based on the originating action.
pub enum OverlayMode {
    Transcribe,
    #[allow(dead_code)]
    TranscribeWithPostProcess,
    Router,
}

impl std::fmt::Display for OverlayMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OverlayMode::Transcribe => write!(f, "transcribe"),
            OverlayMode::TranscribeWithPostProcess => write!(f, "post_process"),
            OverlayMode::Router => write!(f, "router"),
        }
    }
}

fn format_overlay_payload(state: &str, mode: &OverlayMode) -> String {
    format!("{}:{}", state, mode)
}

pub(crate) fn show_overlay_state(app_handle: &AppHandle, state: &str, mode: &OverlayMode) {
    // Check if overlay should be shown based on position setting
    let settings = settings::get_settings_safe(app_handle);
    if settings.overlay_position == OverlayPosition::None {
        return;
    }

    // Save the currently frontmost application BEFORE showing the overlay.
    // On macOS, orderFrontRegardless (called by tauri-nspanel's show())
    // activates the Handy app, stealing focus from the user's target app.
    crate::focus::save_frontmost_app(app_handle);

    // Position the window with dynamic height based on state and mode.
    // Dynamic height ensures the window only covers what's needed, keeping
    // transparent areas click-through at the OS level. Position jumps are
    // prevented by combining set_position + set_size in a single atomic
    // run_on_main_thread closure.
    position_overlay_fixed(app_handle, state, mode);

    if let Some(overlay_window) = app_handle.get_webview_window("recording_overlay") {
        // NOTE: The 5ms sleep that was here has been replaced with a synchronous
        // wait inside position_overlay_on_monitor() using a oneshot channel.
        // position_overlay_on_monitor now blocks until the main thread confirms
        // the position has been applied, so the window is correctly positioned
        // before show() is called below. See position_overlay_on_monitor's
        // #[cfg(target_os = "macos")] block for the recv_timeout logic.

        let _ = overlay_window.show();

        // On Windows, aggressively re-assert "topmost" in the native Z-order after showing
        #[cfg(target_os = "windows")]
        force_overlay_topmost(&overlay_window);

        // On macOS, always set the entire overlay to click-through at the OS level.
        // This allows clicks and scrolls on transparent areas to pass through to apps
        // below. Interactive elements (cancel button, edit textarea) temporarily
        // disable click-through via set_overlay_mouse_passthrough when the mouse
        // enters them, and re-enable it when the mouse leaves.
        #[cfg(target_os = "macos")]
        {
            let window = overlay_window.clone();
            let _ = overlay_window.run_on_main_thread(move || {
                if let Ok(panel) = window.to_panel::<RecordingOverlayPanel>() {
                    panel.set_ignores_mouse_events(true);
                }
            });
        }

        let payload = format_overlay_payload(state, mode);
        let _ = overlay_window.emit("show-overlay", payload);

        // NOTE: We intentionally do NOT emit an app-state event here.
        // The TranscriptionCoordinator is the single source of truth for
        // AppState. Emitting from show_overlay_state would overwrite the
        // coordinator's state with incomplete data (e.g., missing binding_id).
        // The coordinator emits app-state for all transitions: Recording,
        // Processing, Confirming, UsbCycling, and Idle.
    }
}

/// Position the overlay window with dynamic height based on state and mode.
///
/// Uses dynamic window height so the window only covers what's needed — this
/// ensures transparent areas below the pill are click-through at the OS level
/// (on macOS, CSS pointer-events: none alone is insufficient; the NSPanel
/// still captures scroll/hover events on transparent regions).
///
/// Position jumps are prevented by combining set_position + set_size in a
/// single atomic run_on_main_thread closure, rather than using a fixed max-height
/// window approach (which broke click-through by making the window too tall).
///
/// Height varies by state and mode:
/// - Regular recording/transcribing: minimal height (100px, pill only)
/// - Recording with live captions: 280px for multi-line text
/// - Router confirming/processing: full height for transcription preview
/// Helper function to position overlay window on a specific monitor.
/// Used by update_overlay_position for both cursor-based and fallback positioning.
///
/// Calculates dynamic window height based on state and mode:
/// - Regular recording/transcribing: minimal height (pill only, 100px)
/// - Recording with live captions: 280px to show multi-line text
/// - Router confirming/processing: full height to show transcription preview
///
/// On macOS, set_position + set_size are combined in a single
/// run_on_main_thread closure to prevent position jumps caused by
/// async interleaving of separate main-thread calls.
fn position_overlay_on_monitor(
    overlay_window: &tauri::webview::WebviewWindow,
    monitor: &tauri::Monitor,
    app_handle: &AppHandle,
    state: &str,
    mode: &OverlayMode,
) {
    let scale = monitor.scale_factor();
    let monitor_x = monitor.position().x as f64 / scale;
    let monitor_y = monitor.position().y as f64 / scale;
    let monitor_width = monitor.size().width as f64 / scale;
    let monitor_height = monitor.size().height as f64 / scale;

    let overlay_scale = settings::get_settings_safe(app_handle).overlay_scale;

    // Dynamic window height based on state and mode:
    // - Router confirming/processing: full height for transcription preview
    // - All other non-router states: always live-captions height (280px)
    //   This ensures the window never resizes between state transitions AND
    //   the pill position stays identical whether live captions is on or off.
    //   The pill is anchored at the top; extra space below is unused when
    //   live captions is off. This prevents macOS position jumps from async
    //   set_position/set_size calls during recording → transcribing → processing.
    let actual_height = match mode {
        OverlayMode::Router if state == "confirming" || state == "processing" => {
            calculate_overlay_window_height(monitor_height) * overlay_scale
        }
        OverlayMode::Router | OverlayMode::Transcribe | OverlayMode::TranscribeWithPostProcess => {
            // Always use live-captions height so the window never resizes
            // and the pill stays at the same screen position regardless of
            // the live_captions_enabled setting. Live captions renders below
            // the pill in the extra window space; when off, the space is
            // simply empty. This extends the "fixed canvas" approach from
            // FIX_PLAN_VISUALIZER.md to also cover live-captions on/off.
            OVERLAY_LIVE_CAPTIONS_HEIGHT * overlay_scale
        }
    };

    let actual_width = OVERLAY_WINDOW_WIDTH_BASE * overlay_scale;

    // Center the window horizontally
    let x = monitor_x + (monitor_width - actual_width) / 2.0;

    // Calculate Y position so the pillbox stays fixed on screen.
    // The pillbox is at the top of the window (CSS: margin: 4px auto auto),
    // height ~50px. The window may be taller (for live captions, router mode, etc),
    // with extra space below the pillbox.
    // We position the window so the pillbox stays at a fixed screen position
    // regardless of window height.
    let settings = settings::get_settings_safe(app_handle);
    let y = match settings.overlay_position {
        OverlayPosition::Top => monitor_y + OVERLAY_TOP_OFFSET,
        OverlayPosition::Bottom | OverlayPosition::None => {
            let pill_height = OVERLAY_PILL_HEIGHT * overlay_scale;
            let pill_margin = 4.0 * overlay_scale;
            monitor_y + monitor_height - OVERLAY_BOTTOM_OFFSET - pill_height - pill_margin
        }
    };

    // On macOS, combine set_position + set_size in a SINGLE run_on_main_thread
    // closure to prevent position jumps caused by async interleaving of separate
    // main-thread calls. This is the actual fix for the position jump bug — the
    // Y calculation is independent of window height, so the jump was caused by
    // the position and size updates being applied asynchronously in separate calls.
    #[cfg(target_os = "macos")]
    {
        let window = overlay_window.clone();
        let (position_tx, position_rx) = std::sync::mpsc::channel();
        let _ = overlay_window.run_on_main_thread(move || {
            let _ = window.set_position(tauri::Position::Logical(tauri::LogicalPosition { x, y }));
            let _ = window.set_size(tauri::Size::Logical(tauri::LogicalSize {
                width: actual_width,
                height: actual_height,
            }));
            let _ = position_tx.send(());
        });
        // Wait for the main thread to apply the position before showing the window.
        // Without this, the window can appear at the wrong position (center of screen)
        // because show() is called before the async run_on_main_thread completes.
        let _ = position_rx.recv_timeout(std::time::Duration::from_millis(200));
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ =
            overlay_window.set_position(tauri::Position::Logical(tauri::LogicalPosition { x, y }));
        let _ = overlay_window.set_size(tauri::Size::Logical(tauri::LogicalSize {
            width: actual_width,
            height: actual_height,
        }));
    }
}

fn position_overlay_fixed(app_handle: &AppHandle, state: &str, mode: &OverlayMode) {
    if let Some(overlay_window) = app_handle.get_webview_window("recording_overlay") {
        #[cfg(target_os = "linux")]
        {
            update_gtk_layer_shell_anchors(&overlay_window);
        }

        // BUGFIX (2026-07-01): Visualizer Positioning Bug — Center Screen After Router
        // PROBLEM: When Handy finishes routing and the user immediately starts a new
        // transcription, the visualizer appears in the CENTER of the screen instead of
        // at the configured position (top or bottom). This happens because
        // get_monitor_with_cursor() can fail transiently during the hide/show cycle,
        // leaving the window unpositioned.
        //
        // ROOT CAUSE: get_monitor_with_cursor() has multiple failure points:
        // 1. input::get_cursor_position() returns None (cursor unavailable)
        // 2. available_monitors() fails
        // 3. No monitor contains the cursor
        // 4. primary_monitor() fallback also fails
        //
        // FIX: Add fallback to primary_monitor() when get_monitor_with_cursor() fails.
        // Also add logging to track positioning failures.
        //
        // See learning-log.md "Visualizer Positioning Bug — Center Screen After Router"
        // for full documentation.

        // Try to get the target overlay monitor (respects overlay_screen_target setting)
        if let Some(monitor) = get_target_overlay_monitor(app_handle) {
            debug!(
                "position_overlay_fixed: Using target monitor at ({}, {})",
                monitor.position().x,
                monitor.position().y
            );
            position_overlay_on_monitor(&overlay_window, &monitor, app_handle, state, mode);
        } else {
            // FALLBACK: Use primary monitor when target-based detection fails
            debug!("position_overlay_fixed: get_target_overlay_monitor returned None, falling back to primary monitor");

            if let Some(primary) = app_handle.primary_monitor().ok().flatten() {
                debug!(
                    "position_overlay_fixed: Using primary monitor at ({}, {})",
                    primary.position().x,
                    primary.position().y
                );
                position_overlay_on_monitor(&overlay_window, &primary, app_handle, state, mode);
            } else {
                log::error!("position_overlay_fixed: CRITICAL - No monitor available for positioning! Window will appear at default position.");
            }
        }

        // On macOS, make the window click-through during router "processing" state.
        // This allows clicks to pass through to the app below even on transparent areas.
        // Note: This is NOT in position_overlay_fixed() because it's state-dependent.
        // The initial position call should not assume any click-through state.
    }
}

/// Shows the recording overlay window with a specific mode.
pub fn show_recording_overlay_with_mode(app_handle: &AppHandle, mode: OverlayMode) {
    show_overlay_state(app_handle, "recording", &mode);
}

/// Shows the transcribing overlay window with a specific mode.
pub fn show_transcribing_overlay_with_mode(app_handle: &AppHandle, mode: OverlayMode) {
    show_overlay_state(app_handle, "transcribing", &mode);
}

/// Shows the processing overlay window with a specific mode.
pub fn show_processing_overlay_with_mode(app_handle: &AppHandle, mode: OverlayMode) {
    show_overlay_state(app_handle, "processing", &mode);
}

/// Shows the recording overlay window with default (transcribe) mode
pub fn show_recording_overlay(app_handle: &AppHandle) {
    show_overlay_state(app_handle, "recording", &OverlayMode::Transcribe);
}

/// Shows the transcribing overlay window with default (transcribe) mode
pub fn show_transcribing_overlay(app_handle: &AppHandle) {
    show_overlay_state(app_handle, "transcribing", &OverlayMode::Transcribe);
}

/// Shows the processing overlay window with default (transcribe) mode
pub fn show_processing_overlay(app_handle: &AppHandle) {
    show_overlay_state(app_handle, "processing", &OverlayMode::Transcribe);
}

/// Updates the overlay window position and size based on current settings and mode.
/// Router mode uses a taller window to accommodate the transcription preview,
/// while regular transcription uses minimal height to avoid blocking click-through.
/// During processing (filing), the window stays tall to show the text preview,
/// but CSS makes it click-through and visually dimmed.
/// The overlay_scale setting (1.0 or 2.0) scales the window dimensions.
///
/// On macOS, set_position and set_size are combined in a single run_on_main_thread
/// closure to prevent position jumps caused by async interleaving.
pub fn update_overlay_position(app_handle: &AppHandle, state: &str, mode: &OverlayMode) {
    position_overlay_fixed(app_handle, state, mode);
}

/// Hides the recording overlay window with fade-out animation.
/// Emits `force: false` so the frontend respects the state check (won't hide
/// if a new recording is already active).
///
/// BUGFIX (2026-07-01): Race condition — Visualizer closing during new transcription.
/// The old implementation checked `is_active_use()` in actions.rs (too early)
/// then scheduled `window.hide()` via `run_on_main_thread`. If a new recording
/// started between the check and the actual hide, the new visualizer was
/// incorrectly closed.
///
/// FIX: Two-layer guard:
/// 1. Session ID: Capture OVERLAY_SESSION at call time, check it in the
///    closure. If a new recording started (bumped the session), the IDs
///    won't match, so we skip the hide.
/// 2. is_active_use(): Check inside the closure at the latest possible
///    moment, right before window.hide(). If still active, skip the hide.
///
/// Both checks must pass to hide the window.
///
/// NOTE: The `hide-overlay` event and `AppState::Idle` are still emitted
/// unconditionally because the frontend needs to know about the state
/// transition. The frontend's hide-overlay handler has its own guard
/// (checking if recording/transcription is active) and will ignore the
/// event if a new recording is in progress. Similarly, `AppState::Idle`
/// will be superseded by the next `AppState::Recording` when the new
/// recording's state is emitted.
pub fn hide_recording_overlay(app_handle: &AppHandle) {
    info!("hide_recording_overlay: called");
    // Always hide the overlay regardless of settings - if setting was changed while recording,
    // we still want to hide it properly
    if let Some(overlay_window) = app_handle.get_webview_window("recording_overlay") {
        // Emit event to trigger fade-out animation
        // force: false means the frontend will check state before hiding
        let _ = overlay_window.emit("hide-overlay", serde_json::json!({ "force": false }));

        // NOTE: We intentionally do NOT emit AppState::Idle here.
        // The coordinator is the sole authority for state transitions, and it
        // already emits Idle on ProcessingFinished. Emitting Idle from here
        // causes Bug 2: when the router's delayed hide fires while a second
        // transcription is active, the spurious Idle emission overrides the
        // frontend's Recording state, collapsing the visualizer/volume bar.
        // The session guard below prevents the OS-level hide; the frontend
        // hide-overlay event (emitted above) is also gated by its own state
        // checks, so no explicit Idle emission is needed.

        // Capture session ID at call time — if a new recording starts between
        // now and the closure executing, the session will have been bumped.
        let session_at_call = OVERLAY_SESSION.load(Ordering::SeqCst);

        // Schedule hide on main thread after delay for animation.
        // The closure checks BOTH the session ID and is_active_use() to
        // guard against the race condition where a new recording starts
        // between the caller's is_active_use() check and the actual hide.
        let app_handle_clone = app_handle.clone();
        let window_clone = overlay_window.clone();
        let _ = overlay_window.run_on_main_thread(move || {
            // GUARD 1: Session changed — a new recording started, keep overlay
            let session_now = OVERLAY_SESSION.load(Ordering::SeqCst);
            if session_now != session_at_call {
                log::info!(
                    "hide_recording_overlay: session changed ({} -> {}), keeping overlay",
                    session_at_call,
                    session_now
                );
                return;
            }

            // GUARD 2: Active use check at the latest possible moment
            let is_active = app_handle_clone
                .try_state::<Arc<TranscriptionCoordinator>>()
                .map_or(false, |coord| coord.is_active_use());

            if !is_active {
                // On macOS, reset ignores_mouse_events to false (accept events)
                // before hiding, so the next show starts with a clean state.
                // show_overlay_state will set ignores_mouse_events(true) again
                // when the overlay reappears.
                #[cfg(target_os = "macos")]
                {
                    if let Ok(panel) = window_clone.to_panel::<RecordingOverlayPanel>() {
                        panel.set_ignores_mouse_events(false);
                    }
                }
                let _ = window_clone.hide();
            } else {
                log::info!("hide_recording_overlay: keeping overlay — new recording active");
            }
        });
    }
}

/// Force hide the recording overlay, bypassing state checks.
/// Used for cancel operation where the overlay must close regardless of state.
///
/// FIXED: Removed thread spawn to prevent orphaned threads on crash.
pub fn force_hide_recording_overlay(app_handle: &AppHandle) {
    if let Some(overlay_window) = app_handle.get_webview_window("recording_overlay") {
        // Emit force: true to bypass the frontend state check for cancel
        let _ = overlay_window.emit("hide-overlay", serde_json::json!({ "force": true }));

        // Also emit app-state: Idle for the new frontend state hook.
        // This ensures the frontend resets to Idle even if it misses the hide-overlay event.
        emit_app_state(app_handle, &AppState::Idle);

        // Hide immediately on main thread - no thread spawn, safer on crash
        let window_clone = overlay_window.clone();
        let _ = overlay_window.run_on_main_thread(move || {
            // On macOS, reset ignores_mouse_events to false before hiding,
            // so the next show starts with a clean state.
            #[cfg(target_os = "macos")]
            {
                if let Ok(panel) = window_clone.to_panel::<RecordingOverlayPanel>() {
                    panel.set_ignores_mouse_events(false);
                }
            }
            let _ = window_clone.hide();
        });
    }
}

/// Update the cached overlay-enabled flag. Called from `lib.rs` at
/// startup after settings load, and from `change_overlay_position_setting`
/// whenever the user changes the overlay position.
pub fn update_overlay_enabled_cache(enabled: bool) {
    OVERLAY_ENABLED.store(enabled, Ordering::Relaxed);
}

pub fn emit_levels(app_handle: &AppHandle, levels: &Vec<f32>) {
    // Skip emission when the overlay is disabled. The recording_overlay
    // window is created at boot regardless of overlay_position, so
    // without this guard a hidden overlay's WebKit subprocess still
    // processes every event, driving unbounded memory growth (#1279).
    if !OVERLAY_ENABLED.load(Ordering::Relaxed) {
        return;
    }

    // Throttle to ~30 FPS. Even with the overlay enabled, the raw audio
    // callback fires far faster than the UI needs; capping emission rate
    // cuts the per-frame `eval_script`/IPC volume that drives the wry
    // memory growth in issue #1279 (upstream tauri-apps/wry#1489).
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let last = LAST_MIC_LEVEL_EMIT.load(Ordering::Relaxed);
    if now.saturating_sub(last) < EMIT_THROTTLE_MS {
        return;
    }
    LAST_MIC_LEVEL_EMIT.store(now, Ordering::Relaxed);

    // Target only the overlay window. In Tauri 2 both `AppHandle::emit`
    // and `WebviewWindow::emit` broadcast to all webviews; Tauri's
    // listener filter then skips webviews with no registered listener
    // for the event. `emit_to` produces a single eval_script call per
    // callback, cutting per-callback WebKit dispatch work in half.
    let _ = app_handle.emit_to("recording_overlay", "mic-level", levels);
}

// ---------------------------------------------------------------------------
// Test seams and test module for overlay session-guard logic.
//
// The session-guard in `hide_recording_overlay` prevents a stale hide from
// closing the overlay when a new recording has started.  The core logic is:
//
//   1. Capture OVERLAY_SESSION at call time.
//   2. In the closure, compare the captured value against the current value.
//   3. If they differ, a new recording started → skip the hide.
//
// Because `hide_recording_overlay` needs a full `AppHandle` (which can't be
// created in a unit test), we extract the session-guard decision into a pure
// function `should_hide_with_session` and test that.
//
// Test seams added:
// - `reset_overlay_session()` — resets the atomic counter to 0 for isolation.
// - `should_hide_with_session(session_at_call, is_active)` — pure function
//   implementing the same guard logic as `hide_recording_overlay`'s closure.
// ---------------------------------------------------------------------------

/// Test seam: Reset OVERLAY_SESSION to 0 for test isolation.
/// Production code never resets the counter (it only increments), so this is
/// strictly a `#[cfg(test)]` entry point.
#[cfg(test)]
fn reset_overlay_session() {
    OVERLAY_SESSION.store(0, Ordering::SeqCst);
}

/// Test seam: Pure decision function that mirrors the session-guard logic in
/// `hide_recording_overlay`'s `run_on_main_thread` closure.
///
/// Returns `true` if the hide should proceed (no session change, no active use),
/// or `false` if the hide should be suppressed (session bumped = new recording,
/// or still active).
///
/// This is the same logic as lines 1184–1213 of `hide_recording_overlay`, but
/// extracted into a testable pure function without AppHandle/NSWindow deps.
#[cfg(test)]
fn should_hide_with_session(session_at_call: u64, is_active: bool) -> bool {
    // GUARD 1: Session changed — a new recording started, keep overlay visible
    let session_now = OVERLAY_SESSION.load(Ordering::SeqCst);
    if session_now != session_at_call {
        return false;
    }

    // GUARD 2: Active use check at the latest possible moment
    if is_active {
        return false;
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;
    use std::sync::Arc;
    use std::thread;

    /// Serialize all overlay tests so they don't race on the global
    /// OVERLAY_SESSION counter. Each test acquires this lock as its
    /// first action, runs reset_overlay_session(), and holds the lock
    /// for the entire test body.
    static TEST_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

    // -----------------------------------------------------------------------
    // Test 1: Session counter increments after bump_overlay_session
    // Guards: RECURRING_BUGS_CHECKLIST Bug #1 (session counter)
    // -----------------------------------------------------------------------
    #[test]
    fn session_counter_increments_on_bump() {
        let _guard = TEST_LOCK.lock();
        reset_overlay_session();
        let before = OVERLAY_SESSION.load(Ordering::SeqCst);
        let returned = bump_overlay_session();
        let after = OVERLAY_SESSION.load(Ordering::SeqCst);

        // bump_overlay_session returns the *previous* value (fetch_add semantics)
        assert_eq!(
            returned, before,
            "bump_overlay_session should return the value before increment"
        );
        assert_eq!(
            after,
            before + 1,
            "OVERLAY_SESSION should increment by 1 after bump"
        );
    }

    // -----------------------------------------------------------------------
    // Test 2: Stale hide is suppressed (the core race fix)
    // Guards: RECURRING_BUGS_CHECKLIST Bug #1, learning-log 2026-06-15 and
    //         2026-06-17 (router filing race condition)
    //
    // Simulates the race: capture session, bump (new recording starts), then
    // attempt hide with the stale session. The hide must be suppressed.
    // -----------------------------------------------------------------------
    #[test]
    fn stale_hide_suppressed_when_session_changed() {
        let _guard = TEST_LOCK.lock();
        reset_overlay_session();

        // Simulate: show overlay for recording #1 (session = 0)
        let session_at_call = OVERLAY_SESSION.load(Ordering::SeqCst);

        // Simulate: new recording starts — session bumps to 1
        bump_overlay_session();

        // Simulate: stale hide arrives with session_at_call = 0
        let should_hide = should_hide_with_session(session_at_call, false);

        assert!(
            !should_hide,
            "Stale hide must be suppressed when session has changed"
        );
    }

    // -----------------------------------------------------------------------
    // Test 3: Non-stale hide succeeds (session unchanged, not active)
    // Guards: RECURRING_BUGS_CHECKLIST Bug #1 (happy path)
    //
    // When session hasn't changed and no recording is active, hide should
    // proceed normally.
    // -----------------------------------------------------------------------
    #[test]
    fn non_stale_hide_succeeds_when_session_unchanged() {
        let _guard = TEST_LOCK.lock();
        reset_overlay_session();

        // Simulate: show overlay for recording (session = 0)
        let session_at_call = OVERLAY_SESSION.load(Ordering::SeqCst);

        // No new recording — session stays at 0
        // No active recording — is_active = false
        let should_hide = should_hide_with_session(session_at_call, false);

        assert!(
            should_hide,
            "Non-stale hide should succeed when session unchanged and not active"
        );
    }

    // -----------------------------------------------------------------------
    // Test 4: New recording keeps visualizer visible (exact checklist scenario)
    // Guards: RECURRING_BUGS_CHECKLIST Bug #1, learning-log 2026-07-11
    //         (router post-filing bugs)
    //
    // Scenario: Start recording with routing → while routing is filing, start
    // a NEW recording → verify the NEW visualizer STAYS visible.
    //
    // Reproduces the session-counter logic: show overlay (session=N), start
    // new recording (session=N+1), stale hide arrives referencing session=N
    // → assert overlay is still visible (hide suppressed).
    // -----------------------------------------------------------------------
    #[test]
    fn new_recording_preserves_overlay_after_stale_hide() {
        let _guard = TEST_LOCK.lock();
        reset_overlay_session();

        // Step 1: First recording starts — overlay shown, session bumps to 1
        let session_first = bump_overlay_session(); // returns 0, session now 1
        assert_eq!(session_first, 0);

        // Step 2: While routing is filing, a stale hide arrives from the first
        // recording's completion. It captured session = 0 (before the bump).
        let stale_hide_session = session_first; // 0

        // Step 3: New recording starts — session bumps to 2
        let session_second = bump_overlay_session(); // returns 1, session now 2
        assert_eq!(session_second, 1);

        // Step 4: Stale hide from first recording arrives with session_at_call = 0
        let should_hide = should_hide_with_session(stale_hide_session, false);
        assert!(
            !should_hide,
            "Stale hide from first recording must not dismiss second recording's overlay"
        );

        // Step 5: The new recording's own hide (with correct session) should work
        let current_session = OVERLAY_SESSION.load(Ordering::SeqCst);
        let should_hide_current = should_hide_with_session(current_session, false);
        assert!(
            should_hide_current,
            "Current recording's hide should succeed when recording finishes"
        );
    }

    // -----------------------------------------------------------------------
    // Test 5: Active-use guard also suppresses hide
    // Guards: learning-log 2026-06-15 (is_active_use guard)
    //
    // Even if the session matches, if is_active_use() returns true (a recording
    // is still in progress), the hide must be suppressed.
    // -----------------------------------------------------------------------
    #[test]
    fn active_use_guard_suppresses_hide() {
        let _guard = TEST_LOCK.lock();
        reset_overlay_session();
        let session_at_call = OVERLAY_SESSION.load(Ordering::SeqCst);

        // Session hasn't changed, but recording is still active
        let should_hide = should_hide_with_session(session_at_call, true);

        assert!(
            !should_hide,
            "Hide must be suppressed when recording is still active (is_active_use = true)"
        );
    }

    // -----------------------------------------------------------------------
    // Test 6: Both guards combine — session changed AND active
    // Even if only one guard fires, the result should be suppressed.
    // Tests that both guards work together.
    // -----------------------------------------------------------------------
    #[test]
    fn both_guards_suppress_hide() {
        let _guard = TEST_LOCK.lock();
        reset_overlay_session();

        // Session changes AND recording is active — both guards fire
        let session_at_call = OVERLAY_SESSION.load(Ordering::SeqCst);
        bump_overlay_session(); // session changed

        assert!(
            !should_hide_with_session(session_at_call, true),
            "Both guards: session changed + active → suppress"
        );
        assert!(
            !should_hide_with_session(session_at_call, false),
            "One guard: session changed → suppress"
        );

        // Reset, now test session unchanged but active
        reset_overlay_session();
        let session_at_call = OVERLAY_SESSION.load(Ordering::SeqCst);
        assert!(
            !should_hide_with_session(session_at_call, true),
            "One guard: active → suppress"
        );
        assert!(
            should_hide_with_session(session_at_call, false),
            "No guards: session same + not active → proceed"
        );
    }

    // -----------------------------------------------------------------------
    // Test 7: Concurrent show/hide interleaving
    // Guards: RECURRING_BUGS_CHECKLIST Bug #1, learning-log 2026-06-17
    //
    // Spawns threads that concurrently bump (show) and check (hide) the
    // session counter. Verifies no panic, no deadlock, and final consistency.
    // -----------------------------------------------------------------------
    #[test]
    fn concurrent_bump_and_check_no_panic_or_deadlock() {
        let _guard = TEST_LOCK.lock();
        reset_overlay_session();

        const ITERATIONS: u64 = 1000;
        let bump_handles: Vec<_> = (0..2)
            .map(|_| {
                thread::spawn(|| {
                    for _ in 0..ITERATIONS {
                        bump_overlay_session();
                    }
                })
            })
            .collect();

        let check_counter = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let check_handles: Vec<_> = (0..2)
            .map(|_| {
                let counter = Arc::clone(&check_counter);
                thread::spawn(move || {
                    for _ in 0..ITERATIONS {
                        let session = OVERLAY_SESSION.load(Ordering::SeqCst);
                        // The session value should always be >= 0 and non-decreasing
                        // (we can't check monotonicity perfectly due to races, but
                        // we can check it never overflows or wraps)
                        assert!(
                            session < u64::MAX / 2,
                            "Session counter should not overflow"
                        );
                        counter.fetch_add(1, Ordering::SeqCst);
                    }
                })
            })
            .collect();

        for h in bump_handles {
            h.join().expect("Bump thread should not panic");
        }
        for h in check_handles {
            h.join().expect("Check thread should not panic");
        }

        // Final session should be >= 2 * ITERATIONS (two bump threads)
        let final_session = OVERLAY_SESSION.load(Ordering::SeqCst);
        assert!(
            final_session >= 2 * ITERATIONS,
            "Final session ({}) should be >= {} (2 threads × {} iterations)",
            final_session,
            2 * ITERATIONS,
            ITERATIONS
        );

        // All check iterations should have completed (2 check threads × ITERATIONS each)
        assert_eq!(
            check_counter.load(Ordering::SeqCst),
            2 * ITERATIONS,
            "All check iterations should complete"
        );
    }

    // -----------------------------------------------------------------------
    // Test 8: Multiple bumps — session is strictly monotonic
    // Verifies that sequential bumps produce strictly increasing session IDs.
    // -----------------------------------------------------------------------
    #[test]
    fn session_is_strictly_monotonic_across_bumps() {
        let _guard = TEST_LOCK.lock();
        reset_overlay_session();

        let mut prev = OVERLAY_SESSION.load(Ordering::SeqCst);
        for i in 1..=100 {
            let returned = bump_overlay_session();
            assert_eq!(
                returned, prev,
                "bump_overlay_session should return the previous value"
            );
            let current = OVERLAY_SESSION.load(Ordering::SeqCst);
            assert_eq!(
                current,
                prev + 1,
                "After bump #{}, session should be {}",
                i,
                prev + 1
            );
            prev = current;
        }
    }

    // -----------------------------------------------------------------------
    // Test 9: Reset overlay session for test isolation
    // Verifies that reset_overlay_session correctly resets the counter,
    // ensuring test isolation between test runs.
    // -----------------------------------------------------------------------
    #[test]
    fn reset_overlay_session_works() {
        let _guard = TEST_LOCK.lock();
        OVERLAY_SESSION.store(42, Ordering::SeqCst);
        assert_eq!(
            OVERLAY_SESSION.load(Ordering::SeqCst),
            42,
            "Precondition: session should be 42"
        );

        reset_overlay_session();
        assert_eq!(
            OVERLAY_SESSION.load(Ordering::SeqCst),
            0,
            "After reset, session should be 0"
        );
    }

    // -----------------------------------------------------------------------
    // Test 10: Initial state — session 0, no bump, no active
    // Verifies the very first hide succeeds before any recording has started.
    // Guards: edge case for fresh-app launch.
    // -----------------------------------------------------------------------
    #[test]
    fn initial_state_hide_succeeds() {
        let _guard = TEST_LOCK.lock();
        reset_overlay_session();

        // Before any recording, session = 0, captured = 0 → hide succeeds
        let session_at_call = OVERLAY_SESSION.load(Ordering::SeqCst);
        assert_eq!(session_at_call, 0);
        assert!(
            should_hide_with_session(session_at_call, false),
            "Hide should succeed in initial state (session=0, not active)"
        );
    }

    // -----------------------------------------------------------------------
    // Test 11: Triple recording — third recording's hide succeeds after two bumps
    // Scenario: Start rec1 (session 1), then rec2 (session 2), then rec3 (session 3).
    // Stale hides from rec1/rec2 are suppressed; rec3's own hide succeeds.
    // Guards: Regression test for multi-recording sequences.
    // -----------------------------------------------------------------------
    #[test]
    fn triple_recording_stale_hides_suppressed() {
        let _guard = TEST_LOCK.lock();
        reset_overlay_session();

        // Rec 1 starts
        let s1 = bump_overlay_session(); // returns 0, session now 1

        // Rec 2 starts
        let s2 = bump_overlay_session(); // returns 1, session now 2

        // Rec 3 starts
        let s3 = bump_overlay_session(); // returns 2, session now 3

        // Stale hide from rec1 (session=0) should be suppressed
        assert!(
            !should_hide_with_session(s1, false),
            "Stale hide from rec1 should be suppressed"
        );
        // Stale hide from rec2 (session=1) should be suppressed
        assert!(
            !should_hide_with_session(s2, false),
            "Stale hide from rec2 should be suppressed"
        );
        // Rec3's own hide (session=2) should be suppressed — session is now 3
        assert!(
            !should_hide_with_session(s3, false),
            "Stale hide from rec3 should be suppressed (session advanced to 3)"
        );
        // Current session hide should succeed
        let current = OVERLAY_SESSION.load(Ordering::SeqCst);
        assert_eq!(current, 3);
        assert!(
            should_hide_with_session(current, false),
            "Current session hide should succeed"
        );
    }

    // -----------------------------------------------------------------------
    // Test 12: Router flow simulation — recording → processing → idle
    // Guards: Regression test for the router filing race condition.
    //
    // Simulates the exact router lifecycle:
    // 1. User triggers recording (session bumped to 1)
    // 2. Recording stops → Processing starts → coordinator goes Idle
    // 3. hide_recording_overlay is called → captures session_at_call = 1
    //    should_hide_with_session(1, false) → current session is 1 → hide succeeds
    // 4. User starts a new recording (session bumped to 2)
    // 5. A stale hide arrives with session_at_call=1 → session is now 2 → suppressed
    // -----------------------------------------------------------------------
    #[test]
    fn router_flow_stale_hide_suppressed() {
        let _guard = TEST_LOCK.lock();
        reset_overlay_session();

        // Step 1: User triggers recording via router binding
        bump_overlay_session(); // returns 0, session now 1

        // Step 2: Recording finishes, router subprocess starts,
        // coordinator goes Idle (session stays at 1)

        // Step 3: hide_recording_overlay is called for the router recording.
        // In real code, session_at_call is captured at hide time (not show time).
        let session_at_call_step3 = OVERLAY_SESSION.load(Ordering::SeqCst);
        assert_eq!(session_at_call_step3, 1);
        let should_hide = should_hide_with_session(session_at_call_step3, false);
        assert!(
            should_hide,
            "Router's own hide should succeed (session matches, not active)"
        );

        // Step 4: User starts a new recording (session bumped to 2)
        bump_overlay_session(); // returns 1, session now 2
        let session_now = OVERLAY_SESSION.load(Ordering::SeqCst);
        assert_eq!(session_now, 2);

        // Step 5: A stale hide from the router arrives (captured session=1)
        // This is suppressed because session has advanced to 2.
        let should_hide_stale = should_hide_with_session(session_at_call_step3, false);
        assert!(
            !should_hide_stale,
            "Stale router hide must be suppressed after new recording starts"
        );
    }

    // -----------------------------------------------------------------------
    // Test 13: Hide suppressed when is_active=true regardless of session
    // Even with matching session, active recording must prevent hide.
    // Guards: Edge case for stop-command race.
    // -----------------------------------------------------------------------
    #[test]
    fn active_recording_blocks_hide_even_with_matching_session() {
        let _guard = TEST_LOCK.lock();
        reset_overlay_session();

        let session_at_call = OVERLAY_SESSION.load(Ordering::SeqCst);

        // Session matches, but is_active=true → hide suppressed
        assert!(
            !should_hide_with_session(session_at_call, true),
            "Active recording must block hide even when session matches"
        );

        // Now deactivate, hide should succeed
        assert!(
            should_hide_with_session(session_at_call, false),
            "After deactivation, hide should succeed with matching session"
        );
    }
}
