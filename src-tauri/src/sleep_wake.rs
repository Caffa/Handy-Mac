//! macOS sleep/wake detection via wall-clock monitoring
//!
//! Detects when macOS wakes from sleep by monitoring how long a thread-sleep
//! actually takes in wall-clock time. If a 3-second sleep takes more than
//! 10 seconds of wall-clock time, the system was likely suspended.
//!
//! This is the most reliable cross-platform-friendly way to detect wake
//! without fragile Objective-C runtime FFI.
//!
//! On macOS we also register an `NSWorkspaceDidWakeNotification` observer on
//! the default `NSNotificationCenter`. This fires much earlier than the
//! wall-clock detector (typically within milliseconds of wake), closing the
//! race window where the user can press the stop hotkey before we have reset
//! HandyKeys state. The 3-second polling detector remains as a fallback in
//! case the native notification is delayed or unavailable.

#[cfg(target_os = "macos")]
use log::{debug, error, info, warn};
#[cfg(target_os = "macos")]
use parking_lot::Mutex;
#[cfg(target_os = "macos")]
use std::sync::Arc;
#[cfg(target_os = "macos")]
use std::time::Instant;
#[cfg(target_os = "macos")]
use tauri::Manager;

#[cfg(target_os = "macos")]
const WAKE_DEVICE_WAIT_SECS: u64 = 10;
#[cfg(target_os = "macos")]
const WAKE_DEVICE_POLL_MS: u64 = 1000;

/// Start listening for macOS wake-from-sleep events.
///
/// On macOS this installs both an `NSWorkspaceDidWakeNotification` observer
/// and the existing 3-second polling detector. On other platforms it is a
/// no-op.
#[cfg(target_os = "macos")]
pub fn start_sleep_wake_listener(app_handle: tauri::AppHandle) {
    use std::sync::atomic::{AtomicBool, Ordering};

    static LISTENER_STARTED: AtomicBool = AtomicBool::new(false);

    if LISTENER_STARTED.swap(true, Ordering::SeqCst) {
        debug!("Sleep/wake listener already started, skipping");
        return;
    }

    info!("Starting robust sleep/wake detector (native notification + 3s polling fallback)");

    let app_handle = Arc::new(app_handle);

    // Install the native NSWorkspaceDidWakeNotification observer before
    // starting the polling thread. The observer callback is invoked on the
    // same notification-center thread; it only logs and calls on_system_wake,
    // both of which are short and non-blocking.
    start_native_wake_observer(&app_handle);

    std::thread::spawn(move || {
        use std::time::{Duration, Instant, SystemTime};

        let sleep_duration = Duration::from_secs(3);
        let wake_threshold = Duration::from_secs(10);

        loop {
            let before_wall = SystemTime::now();
            let before_mono = Instant::now();

            std::thread::sleep(sleep_duration);

            let after_wall = SystemTime::now();
            let after_mono = Instant::now();

            let wall_elapsed = after_wall
                .duration_since(before_wall)
                .unwrap_or(sleep_duration);
            let mono_elapsed = after_mono.duration_since(before_mono);

            // If the 3s sleep took more than 10s of wall-clock time,
            // the system was almost certainly sleeping.
            if wall_elapsed > wake_threshold {
                info!(
                    "Detected macOS wake from sleep (polling fallback): 3s sleep took {}s wall-clock time (monotonic: {}s)",
                    wall_elapsed.as_secs(),
                    mono_elapsed.as_secs()
                );
                on_system_wake(&app_handle);
            } else {
                // Heartbeat to logs at DEBUG level
                debug!(
                    "Sleep/wake heartbeat: 3s sleep took {}s wall-clock (mono: {}s)",
                    wall_elapsed.as_secs(),
                    mono_elapsed.as_secs()
                );
            }
        }
    });
}

/// Register an observer for `NSWorkspaceDidWakeNotification` on the default
/// notification center.
///
/// The observer fires immediately when macOS wakes, long before the 3-second
/// polling detector notices the wall-clock jump. We intentionally do NOT try
/// to remove the observer at shutdown: Foundation observers added with the
/// simple `addObserver:selector:name:object:` API must be removed with a
/// matching `removeObserver:` call, but Tauri does not expose a symmetric
/// shutdown hook in the library crate. The process is exiting anyway, so the
/// observer leak is harmless and far less risky than keeping a raw Objective-C
/// object alive across the process boundary without a reliable cleanup path.
#[cfg(target_os = "macos")]
fn start_native_wake_observer(app_handle: &Arc<tauri::AppHandle>) {
    use objc2::runtime::{AnyClass, AnyObject, ClassBuilder, Sel};
    use objc2::{msg_send, sel, ClassType};
    use std::ffi::{c_void, CStr};
    use std::sync::Arc;

    // Create a tiny NSObject subclass to act as the notification observer.
    // It stores a raw pointer to the Arc<AppHandle> in an ivar and forwards
    // `didWake:` to our Rust wake handler. Using a dedicated class is safer
    // than relying on KVC hacks on plain NSObject instances.
    let observer_class: &'static AnyClass = {
        let name = CStr::from_bytes_with_nul(b"HandyWakeObserver\0").unwrap();
        // If the class already exists (e.g. a previous process image in tests),
        // reuse it. Otherwise build and register it.
        if let Some(existing) = AnyClass::get(name) {
            existing
        } else {
            let mut builder = ClassBuilder::new(name, objc2::runtime::NSObject::class())
                .expect("Failed to create HandyWakeObserver class");

            // Store the app_handle pointer as an ivar so the callback can
            // retrieve it without relying on fragile context plumbing.
            builder.add_ivar::<*const c_void>(c"appHandle");

            // Method signature for `-(void)didWake:(NSNotification*)notification`
            // Encoding is inferred from the Rust function type by `add_method`.
            // The body is `unsafe` internally, but the function itself must not
            // be `unsafe` to satisfy objc2's MethodImplementation trait.
            // We use `*mut AnyObject` for `this` to avoid higher-ranked lifetime
            // issues that arise with `&mut AnyObject` in function item types.
            extern "C-unwind" fn did_wake_impl(
                this: *mut AnyObject,
                _cmd: Sel,
                _notification: *mut AnyObject,
            ) {
                // Read the ivar we added above.
                let this_ref = unsafe { &*this };
                let ivar = AnyClass::instance_variable(this_ref.class(), c"appHandle")
                    .expect("HandyWakeObserver missing appHandle ivar");
                let app_ptr: *const c_void = unsafe { *ivar.load::<*const c_void>(this_ref) };

                if app_ptr.is_null() {
                    warn!("NSWorkspaceDidWakeNotification fired with null appHandle ivar");
                    return;
                }

                // SAFETY: The pointer was obtained from Arc::into_raw on an
                // Arc<tauri::AppHandle>, and the observer is guaranteed to be
                // single-use per process (protected by LISTENER_STARTED). The
                // Arc is never freed by this callback; ownership stays with
                // the observer registration for the lifetime of the process.
                let app_ptr = app_ptr as *const Arc<tauri::AppHandle>;
                let app_handle: &Arc<tauri::AppHandle> = unsafe { &*app_ptr };

                info!("Received native NSWorkspaceDidWakeNotification; running wake handlers");
                on_system_wake(app_handle);
            }

            unsafe {
                builder.add_method(
                    sel!(didWake:),
                    did_wake_impl as extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject),
                );
            }

            builder.register()
        }
    };

    // Leak an Arc<AppHandle> clone into the observer's ivar. This mirrors the
    // intentional lifetime of the observer: the pointer remains valid until the
    // process exits, so there is no need (and no safe place) to reclaim it.
    let app_ptr: *const c_void = Arc::into_raw(app_handle.clone()) as *const c_void;

    // Allocate and init an observer instance. The notification center
    // will retain it for the lifetime of the registration.
    let observer: *mut AnyObject = unsafe { msg_send![observer_class, alloc] };
    let observer: *mut AnyObject = unsafe { msg_send![observer, init] };

    // Set the ivar.
    let ivar = AnyClass::instance_variable(observer_class, c"appHandle")
        .expect("HandyWakeObserver missing appHandle ivar");
    unsafe {
        *ivar.load_mut::<*const c_void>(&mut *observer) = app_ptr;
    }

    let workspace = objc2_app_kit::NSWorkspace::sharedWorkspace();
    let center = workspace.notificationCenter();
    let name = unsafe { objc2_app_kit::NSWorkspaceDidWakeNotification };
    unsafe {
        center.addObserver_selector_name_object(&*observer, sel!(didWake:), Some(name), None);
    }

    debug!("Registered NSWorkspaceDidWakeNotification observer");
}

/// Handle macOS wake-from-sleep event.
#[cfg(target_os = "macos")]
fn on_system_wake(app_handle: &Arc<tauri::AppHandle>) {
    let wake_start = Instant::now();
    info!("on_system_wake: start");

    // Reset hotkey state first — this is independent of USB watchdog settings
    // and should happen immediately so shortcuts work right after wake.
    crate::shortcut::handy_keys::reset_hotkey_state_after_wake(app_handle);

    // Sleep/wake is a known trigger for GPU/Metal context loss that can wedge
    // a previously-healthy transcribe-cpp session. Instead of unconditionally
    // reloading (which makes every wake as slow as a cold model load), only
    // force a reload if the engine is not currently loaded. A loaded engine
    // that is actually wedged will be caught and reloaded by the failure-counted
    // engine recovery path on its first failed transcription.
    if let Some(tm) =
        app_handle.try_state::<Arc<Mutex<crate::managers::transcription::TranscriptionManager>>>()
    {
        if let Some(guard) = tm.try_lock_for(std::time::Duration::from_secs(2)) {
            if !guard.is_model_loaded() {
                guard.reload_model_on_next_use();
                info!("on_system_wake: main model not loaded; marked for reload on next use");
            } else {
                info!("on_system_wake: main model already loaded; skipping preemptive wake reload");
            }
        } else {
            warn!("on_system_wake: could not acquire main TM lock; marking for reload to be safe");
            tm.lock().reload_model_on_next_use();
        }
    }
    if let Some(live_tm) = app_handle.try_state::<crate::LiveCaptionsTranscriptionManager>() {
        let guard = live_tm.0.lock();
        if !guard.is_model_loaded() {
            guard.reload_model_on_next_use();
            info!("on_system_wake: live-captions model not loaded; marked for reload on next use");
        } else {
            info!("on_system_wake: live-captions model already loaded; skipping preemptive wake reload");
        }
    }

    // USB watchdog recovery
    let settings = crate::settings::get_settings_safe(app_handle);
    let cycle_on_wake = settings.usb_watchdog_enabled && settings.usb_watchdog_cycle_on_wake;

    if !cycle_on_wake {
        // Log clearly why recovery is skipped
        if !settings.usb_watchdog_enabled {
            info!("Wake recovery skipped: USB watchdog is disabled (usb_watchdog_enabled=false)");
        } else {
            info!("Wake recovery skipped: cycle-on-wake is disabled (usb_watchdog_cycle_on_wake=false)");
        }
        info!(
            "on_system_wake: complete, elapsed={:?}",
            wake_start.elapsed()
        );
        return;
    }

    let device_name = settings.usb_watchdog_device_name.clone();
    if device_name.is_empty() {
        warn!("Wake recovery skipped: USB watchdog device name not configured (empty setting)");
        info!(
            "on_system_wake: complete, elapsed={:?}",
            wake_start.elapsed()
        );
        return;
    }

    info!(
        "Triggering post-wake USB recovery for device '{}' (wait for re-enumeration, then cycle unconditionally if idle)",
        device_name
    );

    info!(
        "on_system_wake: complete (USB recovery spawned), elapsed={:?}",
        wake_start.elapsed()
    );

    let ah = app_handle.clone();
    std::thread::spawn(move || {
        // Give macOS USB services a moment to resume before polling.
        std::thread::sleep(std::time::Duration::from_secs(2));

        // Wait for the device to re-appear in the USB tree. Post-wake
        // re-enumeration can lag, and force_power_cycle's resolve_device gives
        // up on the first miss, so we poll before deciding the device is absent.
        let mut device_seen = false;
        for waited_secs in 0..WAKE_DEVICE_WAIT_SECS {
            if crate::usb_watchdog::device_present_in_usb_tree(&device_name) {
                device_seen = true;
                info!(
                    "Wake recovery: device '{}' re-appeared in USB tree after {}s",
                    device_name, waited_secs
                );
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(WAKE_DEVICE_POLL_MS));
        }

        // Guard: a recording may have started while we were waiting. Do not
        // power-cycle the USB hub underneath an active transcription.
        let app_state = ah
            .try_state::<crate::TranscriptionCoordinator>()
            .map(|c| c.get_state());
        let busy = !matches!(
            app_state,
            Some(crate::transcription_coordinator::AppState::Idle { .. }) | None
        );
        if busy {
            warn!(
                "Wake recovery: skipping USB power cycle, app is busy: {:?}",
                app_state
            );
            return;
        }

        let recording_manager =
            ah.try_state::<Arc<crate::managers::audio::AudioRecordingManager>>();

        match recording_manager {
            Some(rm) => {
                let wd = &rm.usb_watchdog;
                if device_seen {
                    warn!("Wake recovery: triggering post-wake USB power cycle");
                    info!("Wake recovery: starting post-wake USB power cycle sequence");
                    // force_power_cycle already handles restart_microphone_if_needed
                    if !wd.force_power_cycle() {
                        warn!("Wake recovery: post-wake USB power cycle was skipped (already cycling or cooldown active)");
                    }
                } else {
                    warn!(
                        "Wake recovery: device '{}' did not re-appear within {}s; skipping USB power cycle",
                        device_name, WAKE_DEVICE_WAIT_SECS
                    );
                    let app_handle_for_event = (*ah).clone();
                    crate::usb_watchdog::emit_cycle_event_with_handle(
                        &Some(app_handle_for_event),
                        "usb-power-cycle-failed",
                        "device absent after wake wait",
                    );
                }
            }
            None => {
                error!("Wake recovery: audio recording manager state not found");
            }
        }
    });
}

/// No-op on non-macOS platforms.
#[cfg(not(target_os = "macos"))]
pub fn start_sleep_wake_listener(_app_handle: tauri::AppHandle) {
    // Sleep/wake detection is macOS-only
}
