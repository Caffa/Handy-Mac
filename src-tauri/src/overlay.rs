use crate::input;
use crate::settings;
use crate::settings::OverlayPosition;
use log::debug;
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize};

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

const OVERLAY_WIDTH: f64 = 172.0;
/// Native window width for transcription preview — needs to accommodate
/// the preview text which is ~3x wider than the visualizer pill (516px).
const OVERLAY_WINDOW_WIDTH: f64 = 540.0;
/// Visible pill width (centered within the wider window).
const OVERLAY_PILL_WIDTH: f64 = 172.0;
/// Minimum window height for the recording pill (just the pill, no preview).
const OVERLAY_WINDOW_MIN_HEIGHT: f64 = 100.0;
/// Visible pill height used for position calculations.
const OVERLAY_PILL_HEIGHT: f64 = 36.0;
/// Maximum percentage of screen height to use for the overlay window.
const OVERLAY_MAX_SCREEN_RATIO: f64 = 0.85;

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
            let settings = settings::get_settings(window_clone.app_handle());
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
/// Uses monitor position/size directly rather than work_area(), which can
/// return incorrect coordinates on macOS for monitors with negative positions.
/// The per-platform OVERLAY_TOP_OFFSET / OVERLAY_BOTTOM_OFFSET constants
/// already account for system chrome (menu bar, taskbar).
///
/// We must use LogicalPosition (not PhysicalPosition) because Tauri/tao
/// converts PhysicalPosition using the scale factor of the monitor the window
/// is *currently* on, which is wrong when moving cross-monitor.
fn calculate_overlay_position(app_handle: &AppHandle) -> Option<(f64, f64, f64)> {
    let monitor = get_monitor_with_cursor(app_handle)?;
    let scale = monitor.scale_factor();
    let monitor_x = monitor.position().x as f64 / scale;
    let monitor_y = monitor.position().y as f64 / scale;
    let monitor_width = monitor.size().width as f64 / scale;
    let monitor_height = monitor.size().height as f64 / scale;

    // Dynamic window height based on screen size (85% of screen height)
    let window_height = calculate_overlay_window_height(monitor_height);

    let settings = settings::get_settings(app_handle);
    
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
            let pos_y = monitor_y + monitor_height
                - OVERLAY_PILL_HEIGHT
                - OVERLAY_BOTTOM_OFFSET
                - window_extra / 2.0;
            debug!("calculate_overlay_position: Bottom/None position, y={}", pos_y);
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
    let (initial_height, has_monitor) = match get_monitor_with_cursor(app_handle) {
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
    let settings = settings::get_settings(app_handle);
    if settings.overlay_position == OverlayPosition::None {
        return;
    }

    // Save the currently frontmost application BEFORE showing the overlay.
    // On macOS, orderFrontRegardless (called by tauri-nspanel's show())
    // activates the Handy app, stealing focus from the user's target app.
    crate::focus::save_frontmost_app(app_handle);

    update_overlay_position(app_handle, state, mode);

    if let Some(overlay_window) = app_handle.get_webview_window("recording_overlay") {
        let _ = overlay_window.show();

        // On Windows, aggressively re-assert "topmost" in the native Z-order after showing
        #[cfg(target_os = "windows")]
        force_overlay_topmost(&overlay_window);

        let payload = format_overlay_payload(state, mode);
        let _ = overlay_window.emit("show-overlay", payload);
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
pub fn update_overlay_position(app_handle: &AppHandle, state: &str, mode: &OverlayMode) {
    if let Some(overlay_window) = app_handle.get_webview_window("recording_overlay") {
        #[cfg(target_os = "linux")]
        {
            update_gtk_layer_shell_anchors(&overlay_window);
        }

        if let Some((x, y, window_height)) = calculate_overlay_position(app_handle) {
            let _ = overlay_window
                .set_position(tauri::Position::Logical(tauri::LogicalPosition { x, y }));
            
            // Use minimal height for regular transcription to allow click-through.
            // Router mode needs full height during confirming (text preview) and
            // processing (showing "Filing..." with visible but dimmed preview).
            // During processing, the window is click-through at the OS level.
            // During recording, we only show the visualizer pill (minimal height).
            let actual_height = match mode {
                OverlayMode::Router if state == "confirming" || state == "processing" => window_height,
                OverlayMode::Router | OverlayMode::Transcribe | OverlayMode::TranscribeWithPostProcess => {
                    OVERLAY_WINDOW_MIN_HEIGHT
                }
            };
            
            let _ = overlay_window.set_size(tauri::Size::Logical(tauri::LogicalSize {
                width: OVERLAY_WINDOW_WIDTH,
                height: actual_height,
            }));
        }

        // On macOS, make the window click-through during router "processing" state.
        // This allows clicks to pass through to the app below even on transparent areas.
        #[cfg(target_os = "macos")]
        {
            let should_ignore_mouse_events = matches!(mode, OverlayMode::Router) && state == "processing";
            if let Ok(panel) = overlay_window.to_panel::<RecordingOverlayPanel>() {
                panel.set_ignores_mouse_events(should_ignore_mouse_events);
            }
        }
    }
}

/// Hides the recording overlay window with fade-out animation
pub fn hide_recording_overlay(app_handle: &AppHandle) {
    // Always hide the overlay regardless of settings - if setting was changed while recording,
    // we still want to hide it properly
    if let Some(overlay_window) = app_handle.get_webview_window("recording_overlay") {
        // Emit event to trigger fade-out animation
        let _ = overlay_window.emit("hide-overlay", ());
        // Hide the window after a short delay to allow animation to complete
        let window_clone = overlay_window.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(300));
            let _ = window_clone.hide();
        });
    }
}

pub fn emit_levels(app_handle: &AppHandle, levels: &Vec<f32>) {
    // emit levels to main app
    let _ = app_handle.emit("mic-level", levels);

    // also emit to the recording overlay if it's open
    if let Some(overlay_window) = app_handle.get_webview_window("recording_overlay") {
        let _ = overlay_window.emit("mic-level", levels);
    }
}
