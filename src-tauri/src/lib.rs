mod actions;

mod apple_intelligence;
mod audio_feedback;
pub mod audio_toolkit;
mod autostart;
pub mod catalog;
pub mod chinese_script;
pub mod cli;
mod clipboard;
mod commands;
mod emergency_save;
pub mod engine_supervisor;
pub mod error_events;
pub mod errors;
mod focus;
mod health;
mod helpers;
mod input;
mod llm_client;
mod logging;

mod managers;
mod overlay;
pub mod portable;
mod secure_input;
mod session;
pub mod settings;
pub mod shortcut;
mod signal_handle;
mod sleep_wake;
mod streaming_cancel_flag;
mod transcription_coordinator;
mod tray;
mod tray_i18n;
mod usb_watchdog;
mod utils;

pub use cli::CliArgs;
#[cfg(debug_assertions)]
use specta_typescript::{BigIntExportBehavior, Typescript};
use tauri_specta::{collect_commands, collect_events, Builder};

use env_filter::Builder as EnvFilterBuilder;
use managers::audio::AudioRecordingManager;
use managers::history::HistoryManager;
use managers::model::ModelManager;
use managers::retry_worker::RetryWorker;
use managers::transcription::TranscriptionManager;
use managers::transcription_retry::TranscriptionRetryQueue;
use parking_lot::Mutex;

#[cfg(unix)]
use signal_hook::consts::{SIGUSR1, SIGUSR2};
#[cfg(unix)]
use signal_hook::iterator::Signals;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use tauri::image::Image;

/// Wrapper type for the live captions [`TranscriptionManager`] instance.
///
/// Tauri's managed state is keyed by type; if both the main and live captions
/// transcription managers were stored as `Arc<Mutex<TranscriptionManager>>`,
/// `try_state::<Arc<Mutex<TranscriptionManager>>>()` would return whichever
/// instance was registered first (the main one). That caused every live
/// captions code path to operate on the main TM, stealing its engine and
/// breaking both live captions and final transcription.
///
/// This distinct newtype lets the live captions TM be registered and looked up
/// separately while still exposing the same inner manager API via `.0`.
pub struct LiveCaptionsTranscriptionManager(pub Arc<Mutex<TranscriptionManager>>);

/// The transcription engine supervisors (main + live captions manager), for
/// lock-free cancellation of engine work. See the cancel path in `utils.rs`.
pub struct TranscriptionEngineHandles {
    pub main: crate::engine_supervisor::EngineSupervisor,
    pub live: crate::engine_supervisor::EngineSupervisor,
}

/// Wrapper type for the live captions streaming cancel flag.
///
/// The main transcription path and the live captions streaming path must be
/// able to cancel independently: stopping a recording must cancel the main
/// TM's final batch work, but it must NOT cancel the live captions stream
/// (which finalizes separately). Tauri managed state is keyed by type, so
/// without a distinct wrapper `try_state::<StreamingCancelFlag>()` would
/// always return the main flag. This newtype lets the audio recorder pick the
/// correct cancel flag based on whether a dedicated live captions model is
/// configured.
pub struct LiveCaptionsStreamingCancelFlag(pub crate::streaming_cancel_flag::StreamingCancelFlag);
pub use transcription_coordinator::emit_app_state;
pub use transcription_coordinator::AppState;
pub use transcription_coordinator::CancelSignal;
pub use transcription_coordinator::TranscriptionCoordinator;

use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Listener, Manager};
use tauri_plugin_autostart::MacosLauncher;
use tauri_plugin_log::{Builder as LogBuilder, RotationStrategy, Target, TargetKind};

use crate::settings::get_settings_safe;
use crate::settings::SettingsCache;
use crate::settings::SettingsWriter;
use crate::streaming_cancel_flag::StreamingCancelFlag;

// Global atomic to store the file log level filter
// We use u8 to store the log::LevelFilter as a number
pub static FILE_LOG_LEVEL: AtomicU8 = AtomicU8::new(log::LevelFilter::Debug as u8);

fn level_filter_from_u8(value: u8) -> log::LevelFilter {
    match value {
        0 => log::LevelFilter::Off,
        1 => log::LevelFilter::Error,
        2 => log::LevelFilter::Warn,
        3 => log::LevelFilter::Info,
        4 => log::LevelFilter::Debug,
        5 => log::LevelFilter::Trace,
        _ => log::LevelFilter::Trace,
    }
}

fn build_console_filter() -> env_filter::Filter {
    let mut builder = EnvFilterBuilder::new();

    match std::env::var("RUST_LOG") {
        Ok(spec) if !spec.trim().is_empty() => {
            if let Err(err) = builder.try_parse(&spec) {
                log::warn!(
                    "Ignoring invalid RUST_LOG value '{}': {}. Falling back to info-level console logging",
                    spec,
                    err
                );
                builder.filter_level(log::LevelFilter::Info);
            }
        }
        _ => {
            builder.filter_level(log::LevelFilter::Info);
        }
    }

    builder.build()
}

/// Choose the macOS activation policy the process *launches* with.
///
/// Must run between `build()` and `run()`: that is the only point where
/// `App::set_activation_policy` sets tao's initial policy, which
/// `applicationDidFinishLaunching` then applies directly. Calling the
/// `AppHandle` variant from `setup` (which Tauri runs on `RunEvent::Ready`,
/// i.e. after launch) is instead a runtime Regular -> Accessory demotion of an
/// already-activated foreground app — the transition Apple documents as
/// unreliable, and what left a Dock icon behind for start-hidden launches on
/// macOS 26+ (#1787). Launching as Accessory avoids the transition entirely;
/// showing the window later promotes to Regular, which is the supported direction.
///
/// Mirrors the show-window decision in `setup`: the app launches without a
/// Dock icon only when it will start hidden (setting or `--start-hidden`) AND a
/// tray icon is available (setting and not `--no-tray`). With no tray the Dock
/// icon stays as the only way back into the app.
#[cfg(target_os = "macos")]
fn apply_startup_activation_policy(app: &mut tauri::App) {
    let cli_args = app.state::<CliArgs>().inner().clone();
    let settings = settings::get_settings(app.handle());

    let should_hide = settings.start_hidden || cli_args.start_hidden;
    let tray_available = settings.show_tray_icon && !cli_args.no_tray;

    if should_hide && tray_available {
        log::info!("Starting hidden with tray available: launching as Accessory (no Dock icon)");
        app.set_activation_policy(tauri::ActivationPolicy::Accessory);
    }
}

fn show_main_window(app: &AppHandle) {
    if let Some(main_window) = app.get_webview_window("main") {
        if let Err(e) = main_window.unminimize() {
            log::error!("Failed to unminimize webview window: {}", e);
        }
        if let Err(e) = main_window.show() {
            log::error!("Failed to show webview window: {}", e);
        }
        if let Err(e) = main_window.set_focus() {
            log::error!("Failed to focus webview window: {}", e);
        }
        #[cfg(target_os = "macos")]
        {
            if let Err(e) = app.set_activation_policy(tauri::ActivationPolicy::Regular) {
                log::error!("Failed to set activation policy to Regular: {}", e);
            }
        }
        return;
    }

    let webview_labels = app.webview_windows().keys().cloned().collect::<Vec<_>>();
    log::error!(
        "Main window not found. Webview labels: {:?}",
        webview_labels
    );
}

#[allow(unused_variables)]
fn should_force_show_permissions_window(app: &AppHandle) -> bool {
    #[cfg(target_os = "windows")]
    {
        let model_manager = app.state::<Arc<ModelManager>>();
        let has_downloaded_models = model_manager
            .get_available_models()
            .iter()
            .any(|model| model.is_downloaded);

        if !has_downloaded_models {
            return false;
        }

        let status = commands::audio::get_windows_microphone_permission_status();
        if status.supported && status.overall_access == commands::audio::PermissionAccess::Denied {
            log::info!(
                "Windows microphone permissions are denied; forcing main window visible for onboarding"
            );
            return true;
        }
    }

    false
}

fn initialize_core_logic(app_handle: &AppHandle) {
    // Note: transcribe.cpp native code no longer initializes in the app
    // process — it runs in the transcription worker (see engine_supervisor),
    // which registers backends and lists devices on demand.

    // Note: Enigo (keyboard/mouse simulation) is NOT initialized here.
    // The frontend is responsible for calling the `initialize_enigo` command
    // after onboarding completes. This avoids triggering permission dialogs
    // on macOS before the user is ready.

    // Initialise the structured JSONL event logger
    logging::init(app_handle);
    // Install the global panic hook so crashes are captured in both
    // handy.log and handy-events.jsonl before the process terminates.
    logging::install_panic_hook();

    // Initialize the managers (wrapped in Arc<Mutex<T>> for consistent state access)
    let history_manager =
        Arc::new(HistoryManager::new(app_handle).expect("Failed to initialize history manager"));

    // Initialize emergency backup system to prevent recording loss
    // This must happen BEFORE AudioRecordingManager starts recording
    let backup_dir = history_manager.recordings_dir();
    emergency_save::init_emergency_backup(&backup_dir);

    // Check for and recover any orphaned recordings from previous crashes
    let recovered = emergency_save::EmergencyBackup::recover_orphaned_recordings(&backup_dir);
    if !recovered.is_empty() {
        log::warn!(
            "Recovered {} orphaned recording(s) from previous session. They are in: {:?}",
            recovered.len(),
            backup_dir
        );
        // TODO: Emit event to frontend to notify user about recovered recordings
    }

    // NOTE: AudioRecordingManager uses internal locks for all its state (state, recorder, mode)
    // so we don't need an outer Mutex wrapper. This allows methods to run concurrently
    // without blocking each other for extended periods (e.g., USB recovery takes 10+ seconds).
    let recording_manager = Arc::new(
        AudioRecordingManager::new(app_handle).expect("Failed to initialize recording manager"),
    );
    let model_manager =
        Arc::new(ModelManager::new(app_handle).expect("Failed to initialize model manager"));
    let transcription_manager = Arc::new(Mutex::new(
        TranscriptionManager::new(app_handle, model_manager.clone())
            .expect("Failed to initialize transcription manager"),
    ));
    let live_captions_transcription_manager = Arc::new(Mutex::new(
        TranscriptionManager::new(app_handle, model_manager.clone())
            .expect("Failed to initialize live captions transcription manager"),
    ));
    let retry_queue = Arc::new(Mutex::new(
        TranscriptionRetryQueue::new(app_handle.clone())
            .expect("Failed to initialize transcription retry queue"),
    ));
    let retry_worker = Arc::new(Mutex::new(RetryWorker::new().with_interval(10)));

    // Add managers to Tauri's managed state
    app_handle.manage(recording_manager.clone());
    app_handle.manage(recording_manager.usb_watchdog.clone());
    app_handle.manage(model_manager.clone());
    // Store the streaming cancel flag separately so it can be accessed without
    // acquiring the TranscriptionManager mutex. This prevents blocking when:
    // 1. Streaming callback holds TM lock during transcription (seconds)
    // 2. Stop handler tries to call cancel_streaming() which needs TM lock
    // By storing the StreamingCancelFlag separately, stop can cancel without waiting.
    // We intentionally use the SAME flag as the TranscriptionManager so that:
    //   - `transcribe()` can abort on cancellation,
    //   - `transcribe_with_hook()` can clear it after acquiring the `is_transcribing` slot.
    let streaming_cancel_flag = {
        let tm = transcription_manager.lock();
        StreamingCancelFlag::from(tm.streaming_cancel_flag())
    };
    app_handle.manage(streaming_cancel_flag);
    // The live captions TM has its own streaming cancel flag so that cancelling
    // the live captions stream does not cancel the main model's final batch
    // transcription. It is wrapped in a distinct managed type so it can be
    // looked up separately from the main StreamingCancelFlag.
    let live_captions_streaming_cancel_flag = {
        let tm = live_captions_transcription_manager.lock();
        StreamingCancelFlag::from(tm.streaming_cancel_flag())
    };
    app_handle.manage(LiveCaptionsStreamingCancelFlag(
        live_captions_streaming_cancel_flag,
    ));
    app_handle.manage(transcription_manager.clone());
    app_handle.manage(LiveCaptionsTranscriptionManager(
        live_captions_transcription_manager.clone(),
    ));
    // Engine supervisor handles, stored so the cancel path can stop engine
    // work (a batch run or stream work in the transcription worker) without
    // acquiring the TranscriptionManager mutex — which the running
    // transcription itself may hold for its whole duration.
    app_handle.manage(TranscriptionEngineHandles {
        main: transcription_manager.lock().engine_supervisor(),
        live: live_captions_transcription_manager
            .lock()
            .engine_supervisor(),
    });
    app_handle.manage(history_manager.clone());
    app_handle.manage(retry_queue.clone());
    app_handle.manage(retry_worker.clone());
    app_handle.manage(Arc::new(session::SessionTracker::new()));
    app_handle.manage(focus::SavedFrontmostApp::new());
    app_handle.manage(Arc::new(SettingsWriter::new()));
    app_handle.manage(Arc::new(Mutex::new(())));

    // Initialize the in-memory settings cache with settings loaded from disk.
    // This must happen AFTER the store plugin is initialized and AFTER
    // SettingsWriter is managed, so that get_settings_safe can fall back
    // to disk reads if the cache is not yet available.
    // The cache is the single source of truth for all reads — eliminating the
    // read-modify-write race with the debounced disk writer.
    let initial_settings = settings::load_or_create_app_settings_safe(app_handle);
    app_handle.manage(Arc::new(SettingsCache::new(initial_settings)));

    // Note: Shortcuts are NOT initialized here.
    // The frontend is responsible for calling the `initialize_shortcuts` command
    // after permissions are confirmed (on macOS) or after onboarding completes.
    // This matches the pattern used for Enigo initialization.

    #[cfg(unix)]
    let signals = Signals::new(&[SIGUSR1, SIGUSR2]).unwrap();
    // Set up signal handlers for toggling transcription
    #[cfg(unix)]
    signal_handle::setup_signal_handler(app_handle.clone(), signals);

    // The macOS activation policy for a start-hidden launch is applied before
    // the event loop runs (see `apply_startup_activation_policy`), not here:
    // by the time `setup` runs the app has already launched as a Regular
    // (Dock) app, and demoting it at runtime is unreliable (#1787).

    // Get the current theme to set the appropriate initial icon
    let initial_theme = tray::get_current_theme(app_handle);

    // Choose the appropriate initial icon based on theme
    let initial_icon_path = tray::get_icon_path(initial_theme, tray::TrayIconState::Idle, false);

    // Track the current tray state so the Secure Input monitor can re-render
    // the icon (warning badge) without a recording-state transition.
    app_handle.manage(tray::CurrentTrayIconState::new(tray::TrayIconState::Idle));

    let tray = TrayIconBuilder::new()
        .icon(
            Image::from_path(
                app_handle
                    .path()
                    .resolve(initial_icon_path, tauri::path::BaseDirectory::Resource)
                    .unwrap(),
            )
            .unwrap(),
        )
        .tooltip(tray::tray_tooltip())
        .show_menu_on_left_click(true)
        .icon_as_template(true)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "settings" => {
                show_main_window(app);
            }
            "secure_input_warning" => {
                // Full explanation lives in the settings-window banner
                show_main_window(app);
            }
            "check_updates" => {
                let settings = settings::get_settings_safe(app);
                if settings.update_checks_enabled {
                    show_main_window(app);
                    let _ = app.emit("check-for-updates", ());
                }
            }
            "copy_last_transcript" => {
                tray::copy_last_transcript(app);
            }
            "unload_model" => {
                let Some(transcription_manager) =
                    app.try_state::<Arc<Mutex<TranscriptionManager>>>()
                else {
                    log::warn!("TranscriptionManager not available, skipping model unload");
                    return;
                };
                let tm = transcription_manager.lock();
                if !tm.is_model_loaded() {
                    log::warn!("No model is currently loaded.");
                    return;
                }
                // Request the unload without waiting for the transcription
                // worker to exit, so the tray handler never blocks behind a
                // transcription in progress. The model reads as unloaded at
                // once; the worker shuts down in the background.
                tm.request_unload();
                log::info!("Model unloaded via tray.");
                // Also unload the live captions model so VRAM is released
                // when the user explicitly requests unload.
                if let Some(live_tm) = app.try_state::<LiveCaptionsTranscriptionManager>() {
                    let live = live_tm.0.lock();
                    if live.is_model_loaded() {
                        live.request_unload();
                        log::info!("Live captions model unloaded via tray.");
                    }
                }
            }
            "cancel" => {
                use crate::utils::cancel_current_operation;

                // Offload the blocking cancel path to a worker thread so the
                // tray handler (main thread) never freezes. cancel_current_operation
                // can wait on locks and shortcut unregister round-trips.
                let app = app.clone();
                tauri::async_runtime::spawn_blocking(move || {
                    cancel_current_operation(&app);
                });
            }
            "quit" => {
                app.exit(0);
            }
            id if id.starts_with("model_select:") => {
                let model_id = id.strip_prefix("model_select:").unwrap().to_string();
                let current_model = settings::get_settings_safe(app).selected_model;
                if model_id == current_model {
                    return;
                }
                let app_clone = app.clone();
                std::thread::spawn(move || {
                    match commands::models::switch_active_model(&app_clone, &model_id) {
                        Ok(()) => {
                            log::info!("Model switched to {} via tray.", model_id);
                        }
                        Err(e) => {
                            log::error!("Failed to switch model via tray: {}", e);
                        }
                    }
                    tray::update_tray_menu(&app_clone, &tray::TrayIconState::Idle, None);
                });
            }
            _ => {}
        })
        .build(app_handle)
        .unwrap();
    app_handle.manage(tray);

    // Initialize tray menu with idle state
    utils::update_tray_menu(app_handle, &utils::TrayIconState::Idle, None);

    // Apply show_tray_icon setting
    let settings = settings::get_settings_safe(app_handle);
    if !settings.show_tray_icon {
        tray::set_tray_visibility(app_handle, false);
    }

    // Refresh tray menu when model state changes. Use the non-blocking
    // `try_update_tray_menu` variant because model-state-changed events can
    // arrive in bursts (download progress, scan completion, load/unload), and
    // waiting on an in-progress rebuild can deadlock with backend locks held by
    // streaming or model threads. A skipped refresh is harmless — the next
    // event or tray interaction will rebuild the menu.
    let app_handle_for_listener = app_handle.clone();
    app_handle.listen("model-state-changed", move |_| {
        tray::try_update_tray_menu(&app_handle_for_listener, &tray::TrayIconState::Idle, None);
    });

    // Apply the autostart preference (SMAppService login item on macOS 13+,
    // tauri-plugin-autostart elsewhere)
    let settings = settings::get_settings_safe(&app_handle);
    autostart::apply_autostart(app_handle, settings.autostart_enabled);

    // Create the recording overlay window (hidden by default)
    utils::create_recording_overlay(app_handle);

    // Position the overlay window based on current settings
    // (on fresh install this uses defaults, on existing install it uses saved position)
    crate::overlay::update_overlay_position(
        app_handle,
        "recording",
        &crate::overlay::OverlayMode::Transcribe,
    );
}

#[tauri::command]
#[specta::specta]
fn trigger_update_check(app: AppHandle) -> Result<(), String> {
    let settings = settings::get_settings_safe(&app);
    if !settings.update_checks_enabled {
        return Ok(());
    }
    app.emit("check-for-updates", ())
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
#[specta::specta]
fn show_main_window_command(app: AppHandle) -> Result<(), String> {
    show_main_window(&app);
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
/// Handles query-only flags (--is-active-use, --is-recording) by polling for result files.
/// Polls for result file with timeout, then exits with the correct code.
fn handle_query_flag(result_file: &str, flag_name: &str) -> ! {
    // Poll for result file with timeout
    let start = std::time::Instant::now();
    let timeout = std::time::Duration::from_secs(5);

    loop {
        if let Ok(content) = std::fs::read_to_string(result_file) {
            let lines: Vec<&str> = content.lines().collect();
            if lines.len() >= 2 {
                // Line 0: status string, Line 1: exit code
                println!("{}", lines[0]);
                if let Ok(code) = lines[1].parse::<i32>() {
                    // Clean up temp file
                    let _ = std::fs::remove_file(result_file);
                    // Use libc::_exit to bypass atexit handlers and avoid recursion
                    #[cfg(unix)]
                    unsafe {
                        libc::_exit(code);
                    }
                    #[cfg(not(unix))]
                    std::process::exit(code);
                }
            }
        }

        // Check timeout
        if start.elapsed() > timeout {
            break;
        }

        // Wait a bit before retrying
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    // Timeout or no result file = no running instance
    eprintln!("error: Handy is not running");
    #[cfg(unix)]
    unsafe {
        libc::_exit(2);
    }
    #[cfg(not(unix))]
    std::process::exit(2);
}

/// Check if this is a query-only CLI flag (--is-active-use or --is-recording).
fn is_query_only_flag(cli_args: &CliArgs) -> bool {
    cli_args.is_active_use || cli_args.is_recording
}

/// Set up a signal handler to poll for result files when the process exits.
/// This uses libc's atexit to ensure the handler runs even with std::process::exit.
#[cfg(target_os = "macos")]
fn setup_query_flag_handler(is_active_use: bool, is_recording: bool) {
    // Store flags for the atexit handler
    static QUERY_FLAGS: std::sync::OnceLock<(bool, bool)> = std::sync::OnceLock::new();
    QUERY_FLAGS.set((is_active_use, is_recording)).ok();

    // Register atexit handler
    extern "C" {
        fn atexit(cb: extern "C" fn()) -> i32;
    }

    extern "C" fn query_flag_atexit() {
        if let Some((is_active_use, is_recording)) = QUERY_FLAGS.get() {
            if *is_active_use {
                handle_query_flag("/tmp/handy-is-active-use.result", "is-active-use");
            }
            if *is_recording {
                handle_query_flag("/tmp/handy-is-recording.result", "is-recording");
            }
        }
    }

    unsafe {
        atexit(query_flag_atexit);
    }
}

#[cfg(not(target_os = "macos"))]
fn setup_query_flag_handler(_is_active_use: bool, _is_recording: bool) {
    // On non-macOS platforms, we rely on Drop handlers
    // This may not work if std::process::exit is called before Drop
}

pub fn run(cli_args: CliArgs) {
    // Avoid ggml-metal residency-set teardown assertions when a native engine
    // outlives the Tauri shutdown sequence (#1902). This must happen before
    // transcribe-cpp initializes its Metal device. Advanced users can restore
    // upstream residency behavior with HANDY_METAL_RESIDENCY=1.
    #[cfg(target_os = "macos")]
    if std::env::var("HANDY_METAL_RESIDENCY").as_deref() == Ok("1") {
        // ggml treats GGML_METAL_NO_RESIDENCY as presence-based, so remove an
        // inherited value as well when explicitly opting back in.
        std::env::remove_var("GGML_METAL_NO_RESIDENCY");
    } else {
        std::env::set_var("GGML_METAL_NO_RESIDENCY", "1");
    }

    // Detect portable mode before anything else
    portable::init();

    // For query-only flags, set up an atexit handler to poll for result files.
    // The single-instance plugin will call std::process::exit(0) after forwarding args,
    // which triggers our atexit handler.
    if is_query_only_flag(&cli_args) {
        setup_query_flag_handler(cli_args.is_active_use, cli_args.is_recording);
    }

    // Parse console logging directives from RUST_LOG, falling back to info-level logging
    // when the variable is unset
    let console_filter = build_console_filter();

    let specta_builder = Builder::<tauri::Wry>::new()
        .commands(collect_commands![
            shortcut::change_binding,
            shortcut::check_shortcut_conflicts,
            shortcut::reset_binding,
            shortcut::change_shortcut_activation_setting,
            shortcut::change_hold_threshold_ms_setting,
            shortcut::change_audio_feedback_setting,
            shortcut::change_audio_feedback_volume_setting,
            shortcut::change_sound_theme_setting,
            shortcut::change_start_hidden_setting,
            shortcut::change_autostart_setting,
            shortcut::change_translate_to_english_setting,
            shortcut::change_convert_us_to_british_setting,
            shortcut::change_convert_number_words_to_digits_setting,
            shortcut::change_selected_language_setting,
            shortcut::change_overlay_position_setting,
            shortcut::change_overlay_screen_target_setting,
            shortcut::change_debug_mode_setting,
            shortcut::change_word_correction_threshold_setting,
            shortcut::change_extra_recording_buffer_setting,
            shortcut::change_pre_recording_buffer_setting,
            shortcut::change_paste_delay_ms_setting,
            shortcut::change_paste_method_setting,
            shortcut::get_available_typing_tools,
            shortcut::change_typing_tool_setting,
            shortcut::change_external_script_path_setting,
            shortcut::change_clipboard_handling_setting,
            shortcut::change_auto_submit_setting,
            shortcut::change_auto_submit_key_setting,
            shortcut::change_post_process_enabled_setting,
            shortcut::change_experimental_enabled_setting,
            shortcut::change_post_process_base_url_setting,
            shortcut::change_post_process_api_format_setting,
            shortcut::change_post_process_api_key_setting,
            shortcut::change_post_process_model_setting,
            shortcut::set_post_process_provider,
            shortcut::fetch_post_process_models,
            shortcut::add_post_process_prompt,
            shortcut::update_post_process_prompt,
            shortcut::delete_post_process_prompt,
            shortcut::set_post_process_selected_prompt,
            shortcut::update_custom_words,
            shortcut::update_advanced_custom_words,
            shortcut::update_custom_filler_words,
            shortcut::update_word_replacements,
            shortcut::change_use_advanced_custom_words_setting,
            shortcut::change_word_correction_mode,
            shortcut::suspend_all_bindings,
            shortcut::resume_all_bindings,
            shortcut::change_mute_while_recording_setting,
            shortcut::change_append_trailing_space_setting,
            shortcut::change_lazy_stream_close_setting,
            shortcut::change_filler_word_removal_enabled_setting,
            shortcut::change_chinese_script_setting,
            shortcut::change_app_language_setting,
            shortcut::change_update_checks_setting,
            shortcut::change_keyboard_implementation_setting,
            shortcut::get_keyboard_implementation,
            shortcut::change_show_tray_icon_setting,
            shortcut::change_transcribe_accelerator_setting,
            shortcut::change_transcribe_gpu_device,
            shortcut::get_available_accelerators,
            shortcut::change_vad_sensitivity_setting,
            shortcut::change_vad_backend_setting,
            shortcut::set_model_vad_sensitivity,
            shortcut::change_live_captions_enabled_setting,
            shortcut::change_live_captions_model_setting,
            shortcut::change_overlay_scale_setting,
            shortcut::change_noise_suppression_enabled_setting,
            shortcut::change_noise_suppression_level_setting,
            shortcut::handy_keys::start_handy_keys_recording,
            shortcut::handy_keys::stop_handy_keys_recording,
            secure_input::get_secure_input_status,
            secure_input::run_keyboard_diagnostic,
            trigger_update_check,
            show_main_window_command,
            commands::cancel_operation,
            commands::extend_recording,
            commands::is_portable,
            commands::get_app_dir_path,
            commands::get_app_settings,
            commands::get_default_settings,
            commands::get_log_dir_path,
            commands::set_log_level,
            commands::open_recordings_folder,
            commands::open_log_dir,
            commands::open_app_data_dir,
            commands::check_apple_intelligence_available,
            commands::initialize_enigo,
            commands::initialize_shortcuts,
            commands::models::get_available_models,
            commands::models::get_model_info,
            commands::models::get_retired_models,
            commands::models::delete_retired_model,
            commands::models::download_model,
            commands::models::delete_model,
            commands::models::cancel_download,
            commands::models::set_active_model,
            commands::models::get_current_model,
            commands::models::get_transcription_model_status,
            commands::models::is_model_loading,
            commands::models::has_any_models_available,
            commands::models::has_any_models_or_downloads,
            commands::models::can_benchmark_models,
            commands::models::get_benchmark_clip_count,
            commands::models::benchmark_models,
            commands::audio::update_microphone_mode,
            commands::audio::get_microphone_mode,
            commands::audio::get_windows_microphone_permission_status,
            commands::audio::open_microphone_privacy_settings,
            commands::audio::get_available_microphones,
            commands::audio::set_selected_microphone,
            commands::audio::get_selected_microphone,
            commands::audio::get_available_output_devices,
            commands::audio::set_selected_output_device,
            commands::audio::get_selected_output_device,
            commands::audio::play_test_sound,
            commands::audio::check_custom_sounds,
            commands::audio::set_clamshell_microphone,
            commands::audio::get_clamshell_microphone,
            commands::audio::is_recording,
            commands::audio::get_microphone_channels,
            commands::audio::set_selected_channel,
            commands::audio::is_usb_watchdog_available,
            commands::audio::list_usb_devices,
            commands::audio::change_usb_watchdog_enabled_setting,
            commands::audio::change_usb_watchdog_device_name_setting,
            commands::audio::change_usb_watchdog_cycle_on_wake_setting,
            commands::audio::trigger_usb_power_cycle,
            commands::audio::start_pronunciation_recording,
            commands::audio::cancel_pronunciation_recording,
            commands::audio::stop_and_schedule_pronunciation,
            commands::audio::stop_and_transcribe_pronunciation_all_models,
            commands::transcription::set_model_unload_timeout,
            commands::transcription::get_model_load_status,
            commands::transcription::unload_model_manually,
            commands::transcription::set_repetition_suppression_level,
            commands::history::get_history_entries,
            commands::history::toggle_history_entry_saved,
            commands::history::get_audio_file_path,
            commands::history::delete_history_entry,
            commands::history::retry_history_entry_transcription,
            commands::history::update_history_limit,
            commands::history::update_recording_retention_period,
            commands::history::update_history_entry_tags,
            commands::history::update_history_entry_metadata,
            commands::history::export_history_json,
            commands::history::export_history_csv,
            commands::history::get_usage_stats,
            commands::transcription_retry::get_retry_queue,
            commands::transcription_retry::retry_transcription,
            commands::transcription_retry::remove_from_retry_queue,
            commands::transcription_retry::clear_retry_queue,
            commands::transcription_retry::get_retry_queue_count,
            commands::experiments::create_experiment_group,
            commands::experiments::get_experiment_group,
            commands::experiments::update_experiment_group,
            commands::experiments::add_transcription_variant,
            commands::experiments::get_variants_for_experiment,
            commands::experiments::update_transcription_variant,
            commands::experiments::get_complete_experiments,
            commands::experiments::generate_variants,
            session::get_session_history,
            health::get_health_report,
            health::get_log_entries,
            helpers::clamshell::is_laptop,
            commands::confirm_routing,
            commands::open_path,
            overlay::set_overlay_can_become_key,
            overlay::set_overlay_mouse_passthrough,
            overlay::set_overlay_force_click_through,
        ])
        .events(collect_events![
            managers::history::HistoryUpdate,
            managers::audio::DeviceListChanged,
        ]);

    #[cfg(debug_assertions)] // <- Only export on non-release builds
    specta_builder
        .export(
            Typescript::default().bigint(BigIntExportBehavior::Number),
            "../src/bindings.ts",
        )
        .expect("Failed to export typescript bindings");

    let invoke_handler = specta_builder.invoke_handler();

    #[allow(unused_mut)]
    let mut builder = tauri::Builder::default()
        .device_event_filter(tauri::DeviceEventFilter::Always)
        .plugin(tauri_plugin_dialog::init())
        .plugin(
            LogBuilder::new()
                .level(log::LevelFilter::Trace) // Set to most verbose level globally
                .max_file_size(500_000)
                .rotation_strategy(RotationStrategy::KeepOne)
                .clear_targets()
                .targets([
                    // Console output respects RUST_LOG environment variable
                    Target::new(TargetKind::Stdout).filter({
                        let console_filter = console_filter.clone();
                        move |metadata| console_filter.enabled(metadata)
                    }),
                    // File logs respect the user's settings (stored in FILE_LOG_LEVEL atomic)
                    Target::new(if let Some(data_dir) = portable::data_dir() {
                        TargetKind::Folder {
                            path: data_dir.join("logs"),
                            file_name: Some("handy".into()),
                        }
                    } else {
                        TargetKind::LogDir {
                            file_name: Some("handy".into()),
                        }
                    })
                    .filter(|metadata| {
                        let file_level = FILE_LOG_LEVEL.load(Ordering::Relaxed);
                        metadata.level() <= level_filter_from_u8(file_level)
                    }),
                ])
                .build(),
        );

    #[cfg(target_os = "macos")]
    {
        builder = builder.plugin(tauri_nspanel::init());
    }

    #[allow(unused_mut)]
    let mut app = builder
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            if args.iter().any(|a| a == "--toggle-transcription") {
                signal_handle::send_transcription_input(app, "transcribe", "CLI");
            } else if args.iter().any(|a| a == "--toggle-post-process") {
                signal_handle::send_transcription_input(app, "transcribe_with_post_process", "CLI");
            } else if args.iter().any(|a| a == "--cancel") {
                let app = app.clone();
                tauri::async_runtime::spawn_blocking(move || {
                    crate::utils::cancel_current_operation(&app);
                });
            } else if args.iter().any(|a| a == "--is-active-use") {
                // Query active use state: recording, transcribing, or processing.
                // Writes result to a temp file for the CLI instance to read.
                //
                // "Active use" means Handy is in use and should not be quit:
                // - Recording (user is speaking, audio being captured)
                // - Processing (transcription, post-processing, router filing in progress)
                // - Pronunciation recording (special mode for model training)
                //
                // Always-on mode with mic stream open but NOT actively recording
                // is NOT considered active use and won't block quit.
                //
                // We check BOTH the coordinator state AND AudioRecordingManager because:
                // - Coordinator tracks Recording/Processing stages for transcription pipeline
                // - AudioRecordingManager tracks pronunciation recordings which bypass coordinator
                let (is_coord_active, is_audio_recording) = {
                    let coord = app.try_state::<TranscriptionCoordinator>();
                    let audio = app.try_state::<Arc<AudioRecordingManager>>();
                    (
                        coord.as_ref().map_or(false, |c| c.is_active_use()),
                        audio.as_ref().map_or(false, |a| a.is_recording()),
                    )
                };
                let is_active = is_coord_active || is_audio_recording;

                // Also check audio state for debugging
                let is_open = app
                    .try_state::<Arc<AudioRecordingManager>>()
                    .map_or(false, |a| a.is_stream_open());
                let is_always_on = app
                    .try_state::<Arc<AudioRecordingManager>>()
                    .map_or(false, |a| a.is_always_on());

                // Print detailed status for debugging
                eprintln!("Handy active use status:");
                eprintln!("  Active use: {}", if is_active { "YES" } else { "no" });
                eprintln!(
                    "  Coordinator stage: {}",
                    if is_coord_active { "active" } else { "idle" }
                );
                eprintln!(
                    "  Recording session: {}",
                    if is_audio_recording { "ACTIVE" } else { "none" }
                );
                eprintln!("  Mic stream: {}", if is_open { "open" } else { "closed" });
                eprintln!(
                    "  Always-on mode: {}",
                    if is_always_on { "yes" } else { "no" }
                );

                // Write result to temp file for CLI instance to read
                // The CLI instance will read this file and exit with the correct code
                let result_file = "/tmp/handy-is-active-use.result";
                if let Ok(mut file) = std::fs::File::create(result_file) {
                    use std::io::Write;
                    let _ = writeln!(file, "{}", if is_active { "active-use" } else { "idle" });
                    let _ = writeln!(file, "{}", if is_active { "0" } else { "1" });
                }

                // Do NOT call std::process::exit() here — this callback runs
                // inside the RUNNING instance. Exiting would kill Handy.
            } else if args.iter().any(|a| a == "--is-recording") {
                // Query recording state and write to temp file for CLI instance.
                // The CLI caller will read the file and exit with the correct code.
                //
                // NOTE: This flag checks ONLY audio recording state. For scripts that need
                // to wait for Handy to be fully idle (including processing/transcription),
                // use --is-active-use instead.
                if let Some(audio_manager) = app.try_state::<Arc<AudioRecordingManager>>() {
                    let is_recording = audio_manager.is_recording();
                    let is_open = audio_manager.is_stream_open();
                    let is_always_on = audio_manager.is_always_on();

                    // Print detailed status for debugging
                    eprintln!("Handy audio status:");
                    eprintln!(
                        "  Recording session: {}",
                        if is_recording { "ACTIVE" } else { "none" }
                    );
                    eprintln!("  Mic stream: {}", if is_open { "open" } else { "closed" });
                    eprintln!(
                        "  Always-on mode: {}",
                        if is_always_on { "yes" } else { "no" }
                    );

                    // Write result to temp file for CLI instance to read
                    let result_file = "/tmp/handy-is-recording.result";
                    if let Ok(mut file) = std::fs::File::create(result_file) {
                        use std::io::Write;
                        let _ = writeln!(
                            file,
                            "{}",
                            if is_recording {
                                "recording"
                            } else {
                                "not-recording"
                            }
                        );
                        let _ = writeln!(file, "{}", if is_recording { "0" } else { "1" });
                    }

                    // Do NOT call std::process::exit() here — this callback runs
                    // inside the RUNNING instance. Exiting would kill Handy.
                } else {
                    eprintln!("error: AudioRecordingManager not initialized");
                    // Write error to temp file
                    let result_file = "/tmp/handy-is-recording.result";
                    if let Ok(mut file) = std::fs::File::create(result_file) {
                        use std::io::Write;
                        let _ = writeln!(file, "error");
                        let _ = writeln!(file, "2");
                    }
                }
            } else {
                show_main_window(app);
            }
        }))
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_os::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_macos_permissions::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_store::Builder::default().build())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            Some(vec![]),
        ))
        .manage(cli_args.clone())
        .setup(move |app| {
            // Query-only flags (--is-active-use, --is-recording) are handled at the start of run()
            // before Tauri initializes, so we don't need to handle them here.
            // This setup block only runs if we're starting the main app.

            specta_builder.mount_events(app);

            // Create main window programmatically so we can set data_directory
            // for portable mode (redirects WebView2 cache to portable Data dir)
            let mut win_builder =
                tauri::WebviewWindowBuilder::new(app, "main", tauri::WebviewUrl::App("/".into()))
                    .title("Handy")
                    .inner_size(680.0, 570.0)
                    .min_inner_size(680.0, 570.0)
                    .resizable(true)
                    .maximizable(true)
                    .visible(false);

            if let Some(data_dir) = portable::data_dir() {
                win_builder = win_builder.data_directory(data_dir.join("webview"));
            }

            win_builder.build()?;

            let mut settings = get_settings_safe(&app.handle());

            // CLI --debug flag overrides debug_mode and log level (runtime-only, not persisted)
            if cli_args.debug {
                settings.debug_mode = true;
                settings.log_level = settings::LogLevel::Trace;
            }

            let tauri_log_level: tauri_plugin_log::LogLevel = settings.log_level.into();
            let file_log_level: log::Level = tauri_log_level.into();
            // Store the file log level in the atomic for the filter to use
            FILE_LOG_LEVEL.store(file_log_level.to_level_filter() as u8, Ordering::Relaxed);
            let app_handle = app.handle().clone();
            // Create the non-blocking cancel signal before the coordinator.
            // The coordinator thread polls this flag on every loop iteration
            // to detect cancel requests without blocking on the TM mutex.
            let cancel_signal = CancelSignal::new();
            let cancel_flag = cancel_signal.flag();
            app.manage(cancel_signal);
            app.manage(TranscriptionCoordinator::new(
                app_handle.clone(),
                cancel_flag,
            ));

            initialize_core_logic(&app_handle);

            // Secure Input monitor (macOS): detects stuck secure input that
            // silently blocks keyed shortcuts, warns the user, and activates
            // the Carbon fallback. See secure_input.rs and upstream #1578.
            secure_input::init(&app_handle);

            // Populate the overlay-enabled cache from initial settings so the
            // audio path (overlay::emit_levels, called ~24 Hz during recording)
            // can do a single atomic load instead of reading the Tauri store.
            // Kept in sync by shortcut::change_overlay_position_setting.
            {
                let settings = settings::get_settings(&app_handle);
                crate::overlay::update_overlay_enabled_cache(
                    settings.overlay_position != settings::OverlayPosition::None,
                );
            }

            // Pre-warm accelerator/device enumeration on a background thread.
            // The first device listing opens the GPU, which on macOS loads
            // ggml's Metal library and compiles it when the system shader
            // cache does not have it yet, so it stays off the startup path —
            // and thanks to the transcription worker, out of the app process
            // entirely. get_available_accelerators then enumerates ORT
            // execution providers and transcribe-cpp compute devices; without
            // this the cost is paid synchronously the first time the user
            // opens the Advanced settings page (which calls the
            // get_available_accelerators command), causing a UI freeze. The
            // device list is kept by the transcription engine and refreshed by
            // every worker it starts that may use the GPU (CPU-only workers
            // can't see it).
            let devices_app_handle = app_handle.clone();
            std::thread::spawn(move || {
                // Detach the supervisor handle under a brief lock, then drop
                // the manager guard before probing: the probes are served by
                // the transcription worker and can block for as long as the
                // cold-start hello timeout (e.g. the first Metal shader
                // compile). Holding the manager mutex through them would make
                // the stop→transcribe path time out with TranscriptionBusy.
                let engine = {
                    let tm = devices_app_handle.state::<Arc<Mutex<TranscriptionManager>>>();
                    // Bound to a local so the manager guard drops before `tm`
                    // (a trailing temporary would drop after it).
                    let supervisor = tm.lock().engine_supervisor();
                    supervisor
                };
                crate::managers::transcription::report_compute_devices_on(&engine);
                let _ = crate::managers::transcription::get_available_accelerators_on(&engine);
            });

            // Pre-warm the ASR model on a background thread so it's ready when
            // the user first presses the hotkey. Without this, the model is loaded
            // lazily on the first hotkey press after the idle timeout, which blocks
            // transcription until loading completes — losing the first part of speech.
            // The model pre-load is idempotent: if already loaded or loading, it returns
            // immediately, so it's safe to call every startup.
            let app_handle_for_model_load = app_handle.clone();
            std::thread::spawn(move || {
                if let Some(transcription_manager) =
                    app_handle_for_model_load.try_state::<Arc<Mutex<TranscriptionManager>>>()
                {
                    let _ = transcription_manager.lock().initiate_model_load();
                }
            });

            // Start the retry worker to process failed transcriptions in the background.
            // Checks every 60 seconds for pending retries.
            if let Some(retry_worker) = app_handle.try_state::<Arc<Mutex<RetryWorker>>>() {
                retry_worker.lock().start(app_handle.clone());
            }

            // Install uhubctl if missing (macOS only, via Homebrew) so the
            // USB watchdog can recover dead USB audio devices automatically.
            #[cfg(target_os = "macos")]
            {
                std::thread::spawn(|| {
                    crate::usb_watchdog::ensure_uhubctl_installed();
                });
                // Start sleep/wake listener to auto-cycle USB on wake from sleep.
                sleep_wake::start_sleep_wake_listener(app_handle.clone());
            }
            // Hide tray icon if --no-tray was passed
            if cli_args.no_tray {
                tray::set_tray_visibility(&app_handle, false);
            }

            // Emit structured AppStarted event with version and platform info
            {
                let version = app_handle.package_info().version.to_string();
                let platform = if cfg!(target_os = "macos") {
                    "macos".to_string()
                } else if cfg!(target_os = "windows") {
                    "windows".to_string()
                } else if cfg!(target_os = "linux") {
                    "linux".to_string()
                } else {
                    "unknown".to_string()
                };
                logging::emit(logging::AppEvent::AppStarted { version, platform });
            }

            // Show main window only if not starting hidden.
            // CLI --start-hidden flag overrides the setting.
            // But if permission onboarding is required, always show the window.
            let should_hide = settings.start_hidden || cli_args.start_hidden;
            let should_force_show = should_force_show_permissions_window(&app_handle);

            // If start_hidden but tray is disabled, we must show the window
            // anyway. Without a tray icon, the dock is the only way back in.
            let tray_available = settings.show_tray_icon && !cli_args.no_tray;
            if should_force_show || !should_hide || !tray_available {
                show_main_window(&app_handle);
            }

            Ok(())
        })
        .on_window_event(|window, event| match event {
            tauri::WindowEvent::CloseRequested { api, .. } => {
                api.prevent_close();
                let _res = window.hide();

                #[cfg(target_os = "macos")]
                {
                    let settings = get_settings_safe(&window.app_handle());
                    let tray_visible =
                        settings.show_tray_icon && !window.app_handle().state::<CliArgs>().no_tray;
                    if tray_visible {
                        // Tray is available: hide the dock icon, app lives in the tray
                        let res = window
                            .app_handle()
                            .set_activation_policy(tauri::ActivationPolicy::Accessory);
                        if let Err(e) = res {
                            log::error!("Failed to set activation policy: {}", e);
                        }
                    }
                    // No tray: keep the dock icon visible so the user can reopen
                }
            }
            tauri::WindowEvent::ThemeChanged(theme) => {
                log::info!("Theme changed to: {:?}", theme);
                // Update tray icon to match new theme, maintaining idle state
                utils::set_tray_state(&window.app_handle(), utils::TrayIconState::Idle);
            }
            _ => {}
        })
        .invoke_handler(invoke_handler)
        .build(tauri::generate_context!())
        .expect("error while building tauri application");

    #[cfg(target_os = "macos")]
    apply_startup_activation_policy(&mut app);

    app.run(|app, event| {
        #[cfg(target_os = "macos")]
        if let tauri::RunEvent::Reopen { .. } = &event {
            show_main_window(app);
        }
        if let tauri::RunEvent::ExitRequested { .. } = &event {
            // Flush any pending debounced settings writes so that no
            // settings are lost when the app quits.
            crate::settings::flush_settings(app);
        }
        let _ = (app, event); // suppress unused warnings on non-macOS
    });
}
