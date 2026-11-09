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
use crate::audio_toolkit::is_bluetooth_device_name;
#[cfg(target_os = "macos")]
use crate::logging;
#[cfg(target_os = "macos")]
use log::{debug, error, info, warn};
#[cfg(target_os = "macos")]
use parking_lot::Mutex;
#[cfg(target_os = "macos")]
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(target_os = "macos")]
use std::sync::Arc;
#[cfg(target_os = "macos")]
use std::time::{Duration, Instant};
#[cfg(target_os = "macos")]
use tauri::Manager;

#[cfg(target_os = "macos")]
const WAKE_DEVICE_WAIT_SECS: u64 = 10;
#[cfg(target_os = "macos")]
const WAKE_DEVICE_POLL_MS: u64 = 1000;
#[cfg(target_os = "macos")]
const WAKE_DEDUPE_SECS: u64 = 30;
#[cfg(target_os = "macos")]
const WAKE_ABSENT_RETRY_POLL_SECS: u64 = 10;
#[cfg(target_os = "macos")]
const WAKE_ABSENT_RETRY_MAX_SECS: u64 = 180;
#[cfg(target_os = "macos")]
const WAKE_DEFERRED_IDLE_POLL_MS: u64 = 2000;
#[cfg(target_os = "macos")]
const WAKE_DEFERRED_IDLE_MAX_SECS: u64 = 120;
#[cfg(target_os = "macos")]
const WAKE_BYPASS_TTL_SECS: u64 = 3600;
/// Duplicate-suppression window for screens-wake events. macOS fires
/// `ScreensDidWake` on every display wake (including screensaver fades), so
/// this is wider than the system-wake window.
#[cfg(target_os = "macos")]
const SCREENS_WAKE_DEDUPE_SECS: u64 = 120;

/// Last-seen wake epoch for duplicate-wake suppression. A value of 0 means no
/// wake has been recorded yet.
#[cfg(target_os = "macos")]
static LAST_WAKE_EPOCH: AtomicU64 = AtomicU64::new(0);

/// Last-seen epoch of a screens-wake event (display wake). Separate from
/// `LAST_WAKE_EPOCH` because displays wake far more often than the system.
#[cfg(target_os = "macos")]
static LAST_SCREENS_WAKE_EPOCH: AtomicU64 = AtomicU64::new(0);

/// Decision for handling a screens-wake (display wake) event.
#[cfg(target_os = "macos")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScreensWakeDecision {
    /// Inside the dedupe window — ignore.
    Duplicate,
    /// Device present (plain screensaver fade) or watchdog gated off — log
    /// only, do not arm.
    LogOnly,
    /// Device genuinely absent from the USB tree (dock power loss) — arm the
    /// wake bypass and spawn the recovery thread.
    ArmRecovery,
}

/// Pure decision logic for the screens-wake path: arm the post-wake bypass
/// only when the watchdog settings gate passes AND the configured device is
/// genuinely absent from the USB tree.
#[cfg(target_os = "macos")]
fn screens_wake_decision(
    duplicate: bool,
    watchdog_enabled: bool,
    cycle_on_wake: bool,
    device_configured: bool,
    device_absent: bool,
) -> ScreensWakeDecision {
    if duplicate {
        return ScreensWakeDecision::Duplicate;
    }
    let bypass_gated = watchdog_enabled && cycle_on_wake && device_configured;
    if !bypass_gated || !device_absent {
        return ScreensWakeDecision::LogOnly;
    }
    ScreensWakeDecision::ArmRecovery
}

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

            // Same shape as `didWake:`, for display-only wakes. Fires on
            // machines that never system-sleep (SleepDisabled=1) where
            // `DidWake` never does.
            extern "C-unwind" fn screens_did_wake_impl(
                this: *mut AnyObject,
                _cmd: Sel,
                _notification: *mut AnyObject,
            ) {
                let this_ref = unsafe { &*this };
                let ivar = AnyClass::instance_variable(this_ref.class(), c"appHandle")
                    .expect("HandyWakeObserver missing appHandle ivar");
                let app_ptr: *const c_void = unsafe { *ivar.load::<*const c_void>(this_ref) };

                if app_ptr.is_null() {
                    warn!("NSWorkspaceScreensDidWakeNotification fired with null appHandle ivar");
                    return;
                }

                // SAFETY: see did_wake_impl.
                let app_ptr = app_ptr as *const Arc<tauri::AppHandle>;
                let app_handle: &Arc<tauri::AppHandle> = unsafe { &*app_ptr };

                info!("Received native NSWorkspaceScreensDidWakeNotification; checking device presence");
                on_screens_wake(app_handle);
            }

            unsafe {
                builder.add_method(
                    sel!(didWake:),
                    did_wake_impl as extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject),
                );
                builder.add_method(
                    sel!(screensDidWake:),
                    screens_did_wake_impl
                        as extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject),
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

    // Second registration on the same observer for display-only wakes.
    let screens_name = unsafe { objc2_app_kit::NSWorkspaceScreensDidWakeNotification };
    unsafe {
        center.addObserver_selector_name_object(
            &*observer,
            sel!(screensDidWake:),
            Some(screens_name),
            None,
        );
    }

    debug!("Registered NSWorkspaceDidWake + ScreensDidWake observers");
}

/// Returns true when the coordinator reports Idle or no coordinator is present.
/// The None case is treated as idle because there is no active pipeline to guard.
#[cfg(target_os = "macos")]
fn is_idle(app_state: Option<&crate::transcription_coordinator::AppState>) -> bool {
    matches!(
        app_state,
        Some(crate::transcription_coordinator::AppState::Idle { .. }) | None
    )
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

    // USB watchdog recovery settings (read once). The bypass is only armed
    // when the watchdog is enabled, wake cycling is enabled, AND a device is
    // configured. Use this same gate for both the primary wake path and the
    // duplicate-wake TTL refresh so we never arm a bypass when cycling is off.
    let settings = crate::settings::get_settings_safe(app_handle);
    let device_name = settings.usb_watchdog_device_name.clone();
    let should_arm_bypass = settings.usb_watchdog_enabled
        && settings.usb_watchdog_cycle_on_wake
        && !device_name.is_empty();

    // Duplicate-wake suppression. NSWorkspaceDidWakeNotification and the
    // wall-clock polling detector can both fire for the same wake event, and
    // multiple rapid notifications are common. We still refresh the bypass
    // TTL on duplicates so the first-failure window does not expire early,
    // but only when the same settings gate that arms it on a fresh wake passes.
    let now = epoch_secs();
    let prev = LAST_WAKE_EPOCH.swap(now, Ordering::SeqCst);
    if prev != 0 && now.saturating_sub(prev) < WAKE_DEDUPE_SECS {
        info!(
            "on_system_wake: duplicate wake detected (last was {}s ago), skipping recovery thread",
            now.saturating_sub(prev)
        );
        logging::emit(logging::AppEvent::WakeRecovery {
            stage: "duplicate_wake".to_string(),
            reason: format!("{}s since last wake", now.saturating_sub(prev)),
            device_seen_secs: None,
        });
        if should_arm_bypass {
            if let Some(rm) =
                app_handle.try_state::<Arc<crate::managers::audio::AudioRecordingManager>>()
            {
                rm.usb_watchdog.arm_wake_bypass(WAKE_BYPASS_TTL_SECS);
            }
        }
        return;
    }

    // USB watchdog recovery
    if !settings.usb_watchdog_enabled {
        info!("Wake recovery skipped: USB watchdog is disabled (usb_watchdog_enabled=false)");
        info!(
            "on_system_wake: complete, elapsed={:?}",
            wake_start.elapsed()
        );
        return;
    }

    if !settings.usb_watchdog_cycle_on_wake {
        info!(
            "Wake recovery skipped: cycle-on-wake is disabled (usb_watchdog_cycle_on_wake=false)"
        );
        info!(
            "on_system_wake: complete, elapsed={:?}",
            wake_start.elapsed()
        );
        return;
    }

    if device_name.is_empty() {
        warn!("Wake recovery skipped: USB watchdog device name not configured (empty setting)");
        info!(
            "on_system_wake: complete, elapsed={:?}",
            wake_start.elapsed()
        );
        return;
    }

    // Arm the first-failure bypass before spawning the recovery thread so a
    // mic-open failure that fires immediately after wake can skip cooldown.
    if let Some(rm) = app_handle.try_state::<Arc<crate::managers::audio::AudioRecordingManager>>() {
        rm.usb_watchdog.arm_wake_bypass(WAKE_BYPASS_TTL_SECS);
    }

    logging::emit(logging::AppEvent::WakeRecovery {
        stage: "wake_detected".to_string(),
        reason: "new macOS wake event".to_string(),
        device_seen_secs: None,
    });
    info!(
        "Triggering post-wake USB recovery for device '{}' (defer if busy, retry if absent)",
        device_name
    );

    info!(
        "on_system_wake: complete (USB recovery spawned), elapsed={:?}",
        wake_start.elapsed()
    );

    spawn_wake_recovery_thread(app_handle, device_name);
}

/// Spawn the shared wake-recovery thread: wait for USB re-enumeration, wait
/// for idle, then cycle and restart. Used by both the system-wake path and
/// the screens-wake path.
#[cfg(target_os = "macos")]
fn spawn_wake_recovery_thread(app_handle: &Arc<tauri::AppHandle>, device_name: String) {
    // A Bluetooth watchdog device is invisible to uhubctl, so the USB tree
    // polling would always end in "device absent after wake retry window".
    // Bail out before spawning: zero subprocesses, zero events.
    if is_bluetooth_device_name(&device_name) {
        debug!(
            "Wake recovery: device '{}' is Bluetooth; skipping USB recovery thread",
            device_name
        );
        return;
    }

    let ah = app_handle.clone();
    std::thread::spawn(move || {
        let thread_start = Instant::now();
        let device_name_for_thread = device_name.clone();
        // Phase 1: settle, then poll for device re-enumeration.
        std::thread::sleep(Duration::from_secs(2));

        let mut device_seen = false;
        let mut device_seen_secs: Option<u64> = None;
        for waited_secs in 0..WAKE_DEVICE_WAIT_SECS {
            if crate::usb_watchdog::device_present_in_usb_tree(&device_name_for_thread) {
                device_seen = true;
                device_seen_secs = Some(waited_secs);
                logging::emit(logging::AppEvent::WakeRecovery {
                    stage: "device_wait".to_string(),
                    reason: "device re-appeared in USB tree".to_string(),
                    device_seen_secs,
                });
                info!(
                    "Wake recovery: device '{}' re-appeared in USB tree after {}s",
                    device_name_for_thread, waited_secs
                );
                break;
            }
            std::thread::sleep(Duration::from_millis(WAKE_DEVICE_POLL_MS));
        }

        // Phase 2: if absent, retry with longer polling until the retry window.
        while !device_seen && thread_start.elapsed().as_secs() < WAKE_ABSENT_RETRY_MAX_SECS {
            std::thread::sleep(Duration::from_secs(WAKE_ABSENT_RETRY_POLL_SECS));
            let elapsed = thread_start.elapsed().as_secs();
            if crate::usb_watchdog::device_present_in_usb_tree(&device_name_for_thread) {
                device_seen = true;
                device_seen_secs = Some(elapsed);
                logging::emit(logging::AppEvent::WakeRecovery {
                    stage: "device_wait".to_string(),
                    reason: "device re-appeared in USB tree after retry".to_string(),
                    device_seen_secs,
                });
                info!(
                    "Wake recovery: device '{}' re-appeared in USB tree after {}s (retry)",
                    device_name_for_thread, elapsed
                );
                break;
            }
        }

        if !device_seen {
            let reason = "device absent after wake retry window";
            warn!(
                "Wake recovery: device '{}' did not re-appear within {}s; skipping USB power cycle",
                device_name_for_thread, WAKE_ABSENT_RETRY_MAX_SECS
            );
            let app_handle_for_event = (*ah).clone();
            crate::usb_watchdog::emit_cycle_event_with_handle(
                &Some(app_handle_for_event),
                "usb-power-cycle-failed",
                reason,
            );
            logging::emit(logging::AppEvent::WakeRecovery {
                stage: "cycle_skipped".to_string(),
                reason: reason.to_string(),
                device_seen_secs: None,
            });
            return;
        }

        // Phase 3: wait until the app is idle. Do not power-cycle underneath an
        // active recording or processing transcription.
        let mut busy_emitted = false;
        let deferred_start = Instant::now();
        loop {
            let app_state = ah
                .try_state::<crate::TranscriptionCoordinator>()
                .map(|c| c.get_state());
            let busy = !is_idle(app_state.as_ref());

            if busy {
                if !busy_emitted {
                    logging::emit(logging::AppEvent::WakeRecovery {
                        stage: "deferred_busy".to_string(),
                        reason: "app is not idle".to_string(),
                        device_seen_secs,
                    });
                    busy_emitted = true;
                }
                if deferred_start.elapsed().as_secs() >= WAKE_DEFERRED_IDLE_MAX_SECS {
                    let reason = "app remained busy beyond deferred idle window";
                    warn!("Wake recovery: {}", reason);
                    let app_handle_for_event = (*ah).clone();
                    crate::usb_watchdog::emit_cycle_event_with_handle(
                        &Some(app_handle_for_event),
                        "usb-power-cycle-failed",
                        "wake cycle deferred: app busy",
                    );
                    logging::emit(logging::AppEvent::WakeRecovery {
                        stage: "deferred_gave_up".to_string(),
                        reason: reason.to_string(),
                        device_seen_secs,
                    });
                    return;
                }
                std::thread::sleep(Duration::from_millis(WAKE_DEFERRED_IDLE_POLL_MS));
                continue;
            }

            break;
        }

        // Phase 4: run the cycle.
        let recording_manager =
            ah.try_state::<Arc<crate::managers::audio::AudioRecordingManager>>();

        match recording_manager {
            Some(rm) => {
                let wd = &rm.usb_watchdog;
                logging::emit(logging::AppEvent::WakeRecovery {
                    stage: "cycle_started".to_string(),
                    reason: "app idle, starting post-wake USB power cycle".to_string(),
                    device_seen_secs,
                });
                info!("Wake recovery: starting post-wake USB power cycle sequence");

                let cycled = wd.wake_recovery_cycle_blocking();
                if cycled {
                    logging::emit(logging::AppEvent::WakeRecovery {
                        stage: "cycle_result".to_string(),
                        reason: "success".to_string(),
                        device_seen_secs,
                    });
                    info!("Wake recovery: post-wake USB power cycle completed");
                    if let Err(e) = rm.restart_microphone_if_needed() {
                        error!(
                            "Wake recovery: failed to restart microphone after cycle: {}",
                            e
                        );
                    }
                } else {
                    let reason = "cycling guard or cooldown active";
                    logging::emit(logging::AppEvent::WakeRecovery {
                        stage: "cycle_skipped".to_string(),
                        reason: reason.to_string(),
                        device_seen_secs,
                    });
                    warn!(
                        "Wake recovery: post-wake USB power cycle was skipped ({})",
                        reason
                    );
                }
            }
            None => {
                error!("Wake recovery: audio recording manager state not found");
            }
        }
    });
}

/// Handle a display-only wake (`NSWorkspaceScreensDidWakeNotification`).
///
/// Machines that never system-sleep (SleepDisabled=1) never fire
/// `NSWorkspaceDidWake` or trip the wall-clock detector — only the displays
/// sleep. On display wake a dock can power-lose and re-enumerate the whole
/// USB tree ~2.3s later, leaving the standing mic stream a zombie. Arm the
/// wake bypass ONLY when the configured watchdog device is genuinely absent
/// from the USB tree: a plain screensaver fade leaves the device present and
/// must not disturb anything.
#[cfg(target_os = "macos")]
fn on_screens_wake(app_handle: &Arc<tauri::AppHandle>) {
    let now = epoch_secs();
    let prev = LAST_SCREENS_WAKE_EPOCH.swap(now, Ordering::SeqCst);
    // Dedupe short-circuits BEFORE the uhubctl probe: macOS fires
    // ScreensDidWake on every display wake, and a duplicate must not pay for
    // a USB tree subprocess.
    if prev != 0 && now.saturating_sub(prev) < SCREENS_WAKE_DEDUPE_SECS {
        debug!(
            "on_screens_wake: duplicate screens-wake (last was {}s ago), skipping",
            now.saturating_sub(prev)
        );
        return;
    }

    let settings = crate::settings::get_settings_safe(app_handle);
    let device_configured = !settings.usb_watchdog_device_name.is_empty();

    // Only run the uhubctl probe when the settings gate passes, so a plain
    // screensaver fade never pays for a USB tree query. A Bluetooth
    // watchdog device never gates: BT devices are invisible to uhubctl
    // ("device not found" on every wake) and must not power-cycle USB.
    let bypass_gated = settings.usb_watchdog_enabled
        && settings.usb_watchdog_cycle_on_wake
        && device_configured
        && !is_bluetooth_device_name(&settings.usb_watchdog_device_name);
    let device_absent = bypass_gated
        && !crate::usb_watchdog::device_present_in_usb_tree(&settings.usb_watchdog_device_name);

    // Duplicate cannot occur here (short-circuited above); the arm stays for
    // exhaustiveness and is unit-tested directly via screens_wake_decision.
    match screens_wake_decision(
        false,
        settings.usb_watchdog_enabled,
        settings.usb_watchdog_cycle_on_wake,
        device_configured,
        device_absent,
    ) {
        ScreensWakeDecision::Duplicate => {}
        ScreensWakeDecision::LogOnly => {
            debug!(
                "on_screens_wake: screens woke but device present or watchdog gated off; no arm"
            );
        }
        ScreensWakeDecision::ArmRecovery => {
            info!(
                "on_screens_wake: device '{}' absent after display wake — arming bypass and spawning recovery",
                settings.usb_watchdog_device_name
            );
            if let Some(rm) =
                app_handle.try_state::<Arc<crate::managers::audio::AudioRecordingManager>>()
            {
                rm.usb_watchdog.arm_wake_bypass(WAKE_BYPASS_TTL_SECS);
            }
            logging::emit(logging::AppEvent::WakeRecovery {
                stage: "screens_wake_device_absent".to_string(),
                reason: format!(
                    "screens woke, device '{}' absent from USB tree",
                    settings.usb_watchdog_device_name
                ),
                device_seen_secs: None,
            });
            spawn_wake_recovery_thread(app_handle, settings.usb_watchdog_device_name.clone());
        }
    }
}

/// No-op on non-macOS platforms.
#[cfg(not(target_os = "macos"))]
pub fn start_sleep_wake_listener(_app_handle: tauri::AppHandle) {
    // Sleep/wake detection is macOS-only
}

#[cfg(target_os = "macos")]
fn epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn is_idle_over_all_app_state_variants() {
        use crate::transcription_coordinator::AppState;

        assert!(is_idle(None));
        assert!(is_idle(Some(&AppState::Idle { session_id: 0 })));
        assert!(!is_idle(Some(&AppState::Recording {
            binding_id: "transcribe".to_string(),
            session_id: 1,
        })));
        assert!(!is_idle(Some(&AppState::Processing {
            binding_id: Some("transcribe".to_string()),
            session_id: 2,
        })));
        assert!(!is_idle(Some(&AppState::UsbCycling {
            stage: "recovering".to_string(),
            session_id: 3,
        })));
        assert!(!is_idle(Some(&AppState::Confirming {
            text: "hello".to_string(),
            binding_id: Some("router".to_string()),
            session_id: 4,
        })));
    }

    #[test]
    fn screens_wake_decision_table() {
        // Duplicate within the dedupe window: ignore regardless of gates.
        assert_eq!(
            screens_wake_decision(true, true, true, true, true),
            ScreensWakeDecision::Duplicate
        );

        // Watchdog settings gate off: log only, even with the device absent.
        assert_eq!(
            screens_wake_decision(false, false, true, true, true),
            ScreensWakeDecision::LogOnly
        );
        assert_eq!(
            screens_wake_decision(false, true, false, true, true),
            ScreensWakeDecision::LogOnly
        );
        assert_eq!(
            screens_wake_decision(false, true, true, false, false),
            ScreensWakeDecision::LogOnly
        );

        // Plain screensaver fade: device present → log only.
        assert_eq!(
            screens_wake_decision(false, true, true, true, false),
            ScreensWakeDecision::LogOnly
        );

        // Dock power loss: device genuinely absent → arm recovery.
        assert_eq!(
            screens_wake_decision(false, true, true, true, true),
            ScreensWakeDecision::ArmRecovery
        );
    }
}
