use crate::input;
use crate::settings;
use crate::settings::OverlayPosition;
use crate::transcription_coordinator::{emit_app_state, AppState};
use log::debug;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
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
        // BUGFIX (2026-07-01): On macOS, give the main thread time to process the
        // position update before showing the window. The position is set via
        // run_on_main_thread() which is asynchronous. Without this delay, the window
        // can be shown before the position update completes, causing it to appear
        // at the wrong position (center of screen instead of top/bottom).
        // This is part of the fix for "Visualizer Positioning Bug — Center Screen After Router".
        #[cfg(target_os = "macos")]
        {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }

        let _ = overlay_window.show();

        // On Windows, aggressively re-assert "topmost" in the native Z-order after showing
        #[cfg(target_os = "windows")]
        force_overlay_topmost(&overlay_window);

        // On macOS, update click-through state based on current overlay state.
        // During router "processing" state, the entire window should be click-through
        // (OS-level) so users can click on apps below the transparent overlay.
        // For all other states, only the CSS pointer-events: none areas are click-through.
        #[cfg(target_os = "macos")]
        {
            let should_ignore_mouse_events = matches!(mode, OverlayMode::Router) && state == "processing";
            let window = overlay_window.clone();
            let _ = overlay_window.run_on_main_thread(move || {
                if let Ok(panel) = window.to_panel::<RecordingOverlayPanel>() {
                    panel.set_ignores_mouse_events(should_ignore_mouse_events);
                }
            });
        }

        let payload = format_overlay_payload(state, mode);
        let _ = overlay_window.emit("show-overlay", payload);

        // Also emit app-state for the new frontend state hook (Phase 1 backward compat).
        // This supplements the existing show-overlay event with a structured AppState.
        let app_state = match state {
            "recording" => AppState::Recording {
                binding_id: String::new(), // Will be updated by coordinator
            },
            "transcribing" | "processing" => AppState::Processing,
            "usb-cycling" => AppState::UsbCycling {
                stage: String::new(),
            },
            "confirming" => AppState::Confirming {
                text: String::new(),
            },
            _ => AppState::Idle,
        };
        emit_app_state(app_handle, &app_state);
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
    // - Recording with live captions: 280px for multi-line text
    // - All other states (pill-only): 100px minimal height
    let actual_height = match mode {
        OverlayMode::Router if state == "confirming" || state == "processing" => {
            calculate_overlay_window_height(monitor_height) * overlay_scale
        },
        OverlayMode::Router | OverlayMode::Transcribe | OverlayMode::TranscribeWithPostProcess => {
            if state == "recording" && settings::get_settings_safe(app_handle).live_captions_enabled {
                OVERLAY_LIVE_CAPTIONS_HEIGHT * overlay_scale
            } else {
                OVERLAY_WINDOW_MIN_HEIGHT * overlay_scale
            }
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
        OverlayPosition::Top => {
            monitor_y + OVERLAY_TOP_OFFSET
        }
        OverlayPosition::Bottom | OverlayPosition::None => {
            let pill_height = OVERLAY_PILL_HEIGHT * overlay_scale;
            let pill_margin = 4.0 * overlay_scale;
            monitor_y + monitor_height
                - OVERLAY_BOTTOM_OFFSET
                - pill_height
                - pill_margin
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
        let _ = overlay_window.run_on_main_thread(move || {
            let _ = window.set_position(tauri::Position::Logical(
                tauri::LogicalPosition { x, y }
            ));
            let _ = window.set_size(tauri::Size::Logical(
                tauri::LogicalSize { width: actual_width, height: actual_height }
            ));
        });
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = overlay_window
            .set_position(tauri::Position::Logical(tauri::LogicalPosition { x, y }));
        let _ = overlay_window.set_size(tauri::Size::Logical(
            tauri::LogicalSize { width: actual_width, height: actual_height }
        ));
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

        // Try to get monitor with cursor first
        if let Some(monitor) = get_monitor_with_cursor(app_handle) {
            debug!("position_overlay_fixed: Using monitor with cursor at ({}, {})",
                monitor.position().x, monitor.position().y);
            position_overlay_on_monitor(&overlay_window, &monitor, app_handle, state, mode);
        } else {
            // FALLBACK: Use primary monitor when cursor-based detection fails
            debug!("position_overlay_fixed: get_monitor_with_cursor returned None, falling back to primary monitor");
            
            if let Some(primary) = app_handle.primary_monitor().ok().flatten() {
                debug!("position_overlay_fixed: Using primary monitor at ({}, {})",
                    primary.position().x, primary.position().y);
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
    // Always hide the overlay regardless of settings - if setting was changed while recording,
    // we still want to hide it properly
    if let Some(overlay_window) = app_handle.get_webview_window("recording_overlay") {
        // Emit event to trigger fade-out animation
        // force: false means the frontend will check state before hiding
        let _ = overlay_window.emit("hide-overlay", serde_json::json!({ "force": false }));

        // Also emit app-state: Idle for the new frontend state hook.
        // This supplements the existing hide-overlay event. Note: the coordinator
        // also emits Idle on ProcessingFinished, so this may be a duplicate emission,
        // but duplicate Idle emissions are harmless and ensure the frontend always
        // receives the state transition even if one event is lost.
        emit_app_state(app_handle, &AppState::Idle);

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
                    session_at_call, session_now
                );
                return;
            }

            // GUARD 2: Active use check at the latest possible moment
            let is_active = app_handle_clone
                .try_state::<Arc<TranscriptionCoordinator>>()
                .map_or(false, |coord| coord.is_active_use());

            if !is_active {
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
