//! macOS keyboard listener using CGEventTap

use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use objc2_core_foundation::{CFMachPort, CFRetained, CFRunLoop, CFRunLoopSource};
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventFlags, CGEventMask, CGEventSource, CGEventSourceStateID,
    CGEventTapCallBack, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
    CGEventTapProxy, CGEventType,
};

use crate::error::{Error, Result};
use crate::platform::state::{BlockingHotkeys, ListenerState};
use crate::types::{Key, KeyEvent, Modifiers};
use log::{debug, error, warn};

use super::keycode::{flags_have_alpha_shift, flags_have_fn, keycode_to_key, keycode_to_modifier};
use super::permissions::check_accessibility;

/// Number of consecutive zero-callback heartbeats before we consider the tap
/// silently dead and recreate it. Five seconds is long enough that a user
/// pausing to read or think does not tear the tap down, while still recovering
/// quickly from the "enabled=true but callbacks_since_last=0" failure mode
/// observed after long runtimes or sleep/wake.
const TAP_DEATH_THRESHOLD_SECS: u32 = 5;

/// Internal listener state returned to KeyboardListener
pub(crate) struct MacOSListenerState {
    pub event_receiver: Receiver<KeyEvent>,
    pub thread_handle: Option<JoinHandle<()>>,
    pub running: Arc<AtomicBool>,
    pub blocking_hotkeys: Option<BlockingHotkeys>,
}

/// Spawn a macOS keyboard listener using CGEventTap
pub(crate) fn spawn(blocking_hotkeys: Option<BlockingHotkeys>) -> Result<MacOSListenerState> {
    if !check_accessibility() {
        return Err(Error::AccessibilityNotGranted);
    }

    let (tx, rx) = mpsc::channel();
    let state = Arc::new(Mutex::new(ListenerState::new(tx, blocking_hotkeys.clone())));
    let running = Arc::new(AtomicBool::new(true));

    // Channel to communicate event tap creation success/failure
    let (init_tx, init_rx) = mpsc::channel::<std::result::Result<(), String>>();

    let thread_state = Arc::clone(&state);
    let thread_running = Arc::clone(&running);

    let handle = thread::spawn(move || {
        run_event_tap(thread_state, thread_running, init_tx);
    });

    // Wait for the event tap to be created
    match init_rx.recv() {
        Ok(Ok(())) => {
            // Event tap created successfully
        }
        Ok(Err(msg)) => {
            return Err(Error::EventTapCreationFailed(msg));
        }
        Err(_) => {
            return Err(Error::EventTapCreationFailed(
                "Event tap thread terminated unexpectedly".to_string(),
            ));
        }
    }

    Ok(MacOSListenerState {
        event_receiver: rx,
        thread_handle: Some(handle),
        running,
        blocking_hotkeys,
    })
}

/// Reconcile internally tracked modifiers against the actual CGEventFlags from the OS.
///
/// This corrects drift caused by missed events (e.g., tap disabled by timeout, system
/// interruptions like Mission Control or screen lock). Should only be called for
/// non-FlagsChanged events, where flags reflect the current state with no change pending.
fn reconcile_modifiers(current: &mut Modifiers, flags: CGEventFlags) {
    // If OS says a modifier group is NOT held, clear our tracked bits.
    // This fixes "stuck modifier" from missed release events.
    if !flags.contains(CGEventFlags::MaskControl) {
        current.remove(Modifiers::CTRL_LEFT | Modifiers::CTRL_RIGHT);
    }
    if !flags.contains(CGEventFlags::MaskShift) {
        current.remove(Modifiers::SHIFT_LEFT | Modifiers::SHIFT_RIGHT);
    }
    if !flags.contains(CGEventFlags::MaskCommand) {
        current.remove(Modifiers::CMD_LEFT | Modifiers::CMD_RIGHT);
    }
    if !flags.contains(CGEventFlags::MaskAlternate) {
        current.remove(Modifiers::OPT_LEFT | Modifiers::OPT_RIGHT);
    }

    // If OS says a modifier group IS held but we have no bits for it,
    // we missed a press event. Default to left side as fallback.
    if flags.contains(CGEventFlags::MaskControl) && !current.intersects(Modifiers::CTRL) {
        current.insert(Modifiers::CTRL_LEFT);
    }
    if flags.contains(CGEventFlags::MaskShift) && !current.intersects(Modifiers::SHIFT) {
        current.insert(Modifiers::SHIFT_LEFT);
    }
    if flags.contains(CGEventFlags::MaskCommand) && !current.intersects(Modifiers::CMD) {
        current.insert(Modifiers::CMD_LEFT);
    }
    if flags.contains(CGEventFlags::MaskAlternate) && !current.intersects(Modifiers::OPT) {
        current.insert(Modifiers::OPT_LEFT);
    }
}

/// Diagnostic counter: tracks how many events the CGEventTap has delivered.
/// Incremented in `event_tap_callback` (hot path) but the log is throttled
/// to every 1000 calls. Kept at module scope so the run-loop heartbeat can
/// read it for delta logging.
static CALLBACK_COUNT: AtomicU64 = AtomicU64::new(0);

/// Bundles a CGEventTap mach port with its run-loop source so callers can
/// enable, add to the run loop, and later disable/remove/invalidate them as a unit.
struct TapAndSource {
    tap: CFRetained<CFMachPort>,
    source: CFRetained<CFRunLoopSource>,
}

impl TapAndSource {
    /// Enable the tap and add its source to the current run loop.
    ///
    /// # Safety
    /// `state_ptr` must be the same valid pointer that was passed into
    /// `tap_create`, because CGEventTap may still invoke the callback while the
    /// source is attached. The caller retains ownership of the Arc.
    unsafe fn add_to_run_loop(&self) {
        let run_loop = CFRunLoop::current();
        if let Some(rl) = run_loop {
            rl.add_source(Some(&self.source), unsafe {
                objc2_core_foundation::kCFRunLoopCommonModes
            });
            CGEvent::tap_enable(&self.tap, true);
        } else {
            // We already validated the run loop exists before calling this, so
            // this path is defensive-only. If it somehow fails, we cannot use
            // the tap; the caller is responsible for cleanup via
            // remove_from_run_loop plus the single Arc::from_raw at thread exit.
            error!("Failed to get current run loop when adding event tap");
        }
    }

    /// Disable the tap, remove its source from the run loop, and invalidate
    /// the mach port. Safe to call multiple times.
    fn remove_from_run_loop(&self) {
        CGEvent::tap_enable(&self.tap, false);
        if let Some(rl) = CFRunLoop::current() {
            rl.remove_source(Some(&self.source), unsafe {
                objc2_core_foundation::kCFRunLoopCommonModes
            });
        }
        CFMachPort::invalidate(&self.tap);
    }
}

/// The callback function for the event tap
///
/// Returns NULL to block the event, or the event pointer to pass it through.
unsafe extern "C-unwind" fn event_tap_callback(
    _proxy: CGEventTapProxy,
    event_type: CGEventType,
    event: NonNull<CGEvent>,
    user_info: *mut c_void,
) -> *mut CGEvent {
    // Increment the diagnostic counter; throttle the hot-path log.
    let count = CALLBACK_COUNT.fetch_add(1, Ordering::Relaxed);
    if count % 1000 == 0 && count > 0 {
        debug!("CGEventTap callback count: {}", count);
    }

    // Safety: user_info is our state pointer
    let state = &*(user_info as *const Mutex<ListenerState>);

    let cg_event = event.as_ref();
    let flags = CGEvent::flags(Some(cg_event));

    let mut should_block = false;

    if let Ok(mut state) = state.lock() {
        // Reconcile tracked modifiers against OS flags for non-FlagsChanged events.
        // On FlagsChanged, flags reflect the state *after* the current change, so
        // reconciling would fight with the toggle logic.
        if event_type != CGEventType::FlagsChanged {
            reconcile_modifiers(&mut state.current_modifiers, flags);
        }

        // Build side-specific modifiers from internally tracked state + FN from flags
        let modifiers = if flags_have_fn(flags) {
            state.current_modifiers | Modifiers::FN
        } else {
            state.current_modifiers & !Modifiers::FN
        };

        match event_type {
            CGEventType::KeyDown => {
                let keycode = CGEvent::integer_value_field(
                    Some(cg_event),
                    CGEventField::KeyboardEventKeycode,
                ) as u16;

                let key = keycode_to_key(keycode);

                // Skip special function key events (e.g., F3 triggering Mission Control).
                // These have MaskSecondaryFn set but use special keycodes (like 0xA0)
                // that we don't recognize. Without this check, they'd be reported as
                // "Fn pressed" with no key.
                if key.is_none() && flags_have_fn(flags) {
                    return event.as_ptr();
                }

                // Check if this should be blocked
                should_block = state.should_block(modifiers, key);

                let _ = state.event_sender.send(KeyEvent {
                    modifiers,
                    key,
                    is_key_down: true,
                    changed_modifier: None,
                });
            }
            CGEventType::KeyUp => {
                let keycode = CGEvent::integer_value_field(
                    Some(cg_event),
                    CGEventField::KeyboardEventKeycode,
                ) as u16;

                let key = keycode_to_key(keycode);

                // Skip special function key events (same as KeyDown)
                if key.is_none() && flags_have_fn(flags) {
                    return event.as_ptr();
                }

                // Block key up if we blocked key down (to be consistent)
                should_block = state.should_block(modifiers, key);

                let _ = state.event_sender.send(KeyEvent {
                    modifiers,
                    key,
                    is_key_down: false,
                    changed_modifier: None,
                });
            }
            CGEventType::FlagsChanged => {
                let keycode = CGEvent::integer_value_field(
                    Some(cg_event),
                    CGEventField::KeyboardEventKeycode,
                ) as u16;

                let changed_modifier = keycode_to_modifier(keycode);

                // Check if this is a lock key (e.g., Caps Lock) which comes through
                // as FlagsChanged but isn't a traditional modifier
                let lock_key = keycode_to_key(keycode);

                // Handle lock keys specially - they come through FlagsChanged
                // but don't change our tracked modifier state
                if let Some(key) = lock_key {
                    let is_key_down = flags_have_alpha_shift(flags);

                    should_block = state.should_block(modifiers, Some(key));

                    let _ = state.event_sender.send(KeyEvent {
                        modifiers,
                        key: Some(key),
                        is_key_down,
                        changed_modifier: None,
                    });
                } else if let Some(modifier_bit) = changed_modifier {
                    // Regular modifier key — use keycode to toggle the specific bit
                    let was_set = state.current_modifiers.contains(modifier_bit);
                    let is_key_down = !was_set;

                    if is_key_down {
                        state.current_modifiers |= modifier_bit;
                    } else {
                        state.current_modifiers &= !modifier_bit;
                    }

                    // Re-derive modifiers after update (include FN from flags)
                    let new_modifiers = if flags_have_fn(flags) {
                        state.current_modifiers | Modifiers::FN
                    } else {
                        state.current_modifiers & !Modifiers::FN
                    };

                    // Check if this modifier-only combo should be blocked
                    if is_key_down {
                        should_block = state.should_block(new_modifiers, None);
                    }

                    let _ = state.event_sender.send(KeyEvent {
                        modifiers: new_modifiers,
                        key: None,
                        is_key_down,
                        changed_modifier: changed_modifier,
                    });
                } else if keycode == 0x3F {
                    // FN key itself — tracked via flags, not keycode state
                    let had_fn = modifiers.contains(Modifiers::FN);
                    let has_fn = flags_have_fn(flags);
                    if had_fn != has_fn {
                        let new_modifiers = if has_fn {
                            state.current_modifiers | Modifiers::FN
                        } else {
                            state.current_modifiers & !Modifiers::FN
                        };

                        if has_fn {
                            should_block = state.should_block(new_modifiers, None);
                        }

                        let _ = state.event_sender.send(KeyEvent {
                            modifiers: new_modifiers,
                            key: None,
                            is_key_down: has_fn,
                            changed_modifier: Some(Modifiers::FN),
                        });
                    }
                }
            }
            // Mouse button events
            // Only report left/right clicks when modifiers are held (to avoid noise)
            CGEventType::LeftMouseDown if !modifiers.is_empty() => {
                let _ = state.event_sender.send(KeyEvent {
                    modifiers,
                    key: Some(Key::MouseLeft),
                    is_key_down: true,
                    changed_modifier: None,
                });
            }
            CGEventType::LeftMouseUp if !modifiers.is_empty() => {
                let _ = state.event_sender.send(KeyEvent {
                    modifiers,
                    key: Some(Key::MouseLeft),
                    is_key_down: false,
                    changed_modifier: None,
                });
            }
            CGEventType::RightMouseDown if !modifiers.is_empty() => {
                let _ = state.event_sender.send(KeyEvent {
                    modifiers,
                    key: Some(Key::MouseRight),
                    is_key_down: true,
                    changed_modifier: None,
                });
            }
            CGEventType::RightMouseUp if !modifiers.is_empty() => {
                let _ = state.event_sender.send(KeyEvent {
                    modifiers,
                    key: Some(Key::MouseRight),
                    is_key_down: false,
                    changed_modifier: None,
                });
            }
            // Pass through unmodified left/right clicks
            CGEventType::LeftMouseDown
            | CGEventType::LeftMouseUp
            | CGEventType::RightMouseDown
            | CGEventType::RightMouseUp => {}
            CGEventType::OtherMouseDown => {
                let button_number = CGEvent::integer_value_field(
                    Some(cg_event),
                    CGEventField::MouseEventButtonNumber,
                );
                let key = match button_number {
                    2 => Some(Key::MouseMiddle),
                    3 => Some(Key::MouseX1),
                    4 => Some(Key::MouseX2),
                    _ => None, // Unknown button
                };
                if let Some(key) = key {
                    let _ = state.event_sender.send(KeyEvent {
                        modifiers,
                        key: Some(key),
                        is_key_down: true,
                        changed_modifier: None,
                    });
                }
            }
            CGEventType::OtherMouseUp => {
                let button_number = CGEvent::integer_value_field(
                    Some(cg_event),
                    CGEventField::MouseEventButtonNumber,
                );
                let key = match button_number {
                    2 => Some(Key::MouseMiddle),
                    3 => Some(Key::MouseX1),
                    4 => Some(Key::MouseX2),
                    _ => None,
                };
                if let Some(key) = key {
                    let _ = state.event_sender.send(KeyEvent {
                        modifiers,
                        key: Some(key),
                        is_key_down: false,
                        changed_modifier: None,
                    });
                }
            }
            CGEventType::TapDisabledByTimeout | CGEventType::TapDisabledByUserInput => {
                // macOS disabled the tap (callback latency or user input).
                // The run loop thread will re-enable it; reconcile modifiers
                // now since we may have missed events while disabled.
                warn!("CGEventTap disabled by {:?}", event_type);
                reconcile_modifiers(
                    &mut state.current_modifiers,
                    CGEventSource::flags_state(CGEventSourceStateID::CombinedSessionState),
                );
            }
            _ => {}
        }
    }

    if should_block {
        // Block the event from reaching other applications
        std::ptr::null_mut()
    } else {
        // Pass the event through unchanged
        event.as_ptr()
    }
}

/// Create a new CGEventTap and matching CFRunLoopSource using the provided
/// state pointer. On success, the tap is ready to be added to the current run
/// loop. On failure, an error is reported through `init_tx` and `None` is
/// returned. The caller retains ownership of the `state` `Arc`; this function
/// borrows the pointer only for the duration of `tap_create`.
///
/// # Safety
/// `state_ptr` must point to a valid `Arc<Mutex<ListenerState>>` that remains
/// alive for the lifetime of the returned tap/source.
unsafe fn create_tap_and_source(
    state_ptr: *mut c_void,
    init_tx: &Sender<std::result::Result<(), String>>,
) -> Option<TapAndSource> {
    // Event types we want to monitor
    let event_mask: CGEventMask = (1 << CGEventType::KeyDown.0)
        | (1 << CGEventType::KeyUp.0)
        | (1 << CGEventType::FlagsChanged.0)
        // Mouse buttons
        | (1 << CGEventType::LeftMouseDown.0)
        | (1 << CGEventType::LeftMouseUp.0)
        | (1 << CGEventType::RightMouseDown.0)
        | (1 << CGEventType::RightMouseUp.0)
        | (1 << CGEventType::OtherMouseDown.0)
        | (1 << CGEventType::OtherMouseUp.0);

    let callback: CGEventTapCallBack = Some(event_tap_callback);

    // Use Default mode (not ListenOnly) to enable optional event blocking
    let tap: Option<CFRetained<CFMachPort>> = CGEvent::tap_create(
        CGEventTapLocation::SessionEventTap,
        CGEventTapPlacement::HeadInsertEventTap,
        CGEventTapOptions::Default,
        event_mask,
        callback,
        state_ptr,
    );

    let tap = match tap {
        Some(t) => t,
        None => {
            // Do NOT consume the state pointer here. The caller (run_event_tap)
            // owns the Arc and is responsible for dropping it exactly once at
            // thread exit. Dropping it here would lead to a double-free when
            // run_event_tap later calls Arc::from_raw on the same pointer.
            let _ = init_tx.send(Err(
                "Failed to create event tap. Your terminal app may need accessibility permission in System Settings > Privacy & Security > Accessibility".to_string()
            ));
            return None;
        }
    };

    // Create run loop source
    let source: Option<CFRetained<CFRunLoopSource>> =
        CFMachPort::new_run_loop_source(None, Some(&tap), 0);

    let source = match source {
        Some(s) => s,
        None => {
            CFMachPort::invalidate(&tap);
            let _ = init_tx.send(Err("Failed to create run loop source".to_string()));
            return None;
        }
    };

    Some(TapAndSource { tap, source })
}

/// Run the event tap in a dedicated thread
fn run_event_tap(
    state: Arc<Mutex<ListenerState>>,
    running: Arc<AtomicBool>,
    init_tx: Sender<std::result::Result<(), String>>,
) {
    // Store state in a raw pointer for the callback. We intentionally never
    // call Arc::from_raw on this pointer during normal operation; the same
    // `state` Arc is reused across tap recreations so that listener state
    // (modifiers, blocking hotkeys, callback counts) survives.
    let state_ptr: *mut c_void = Arc::into_raw(Arc::clone(&state)) as *mut c_void;

    let Some(tap_source) = (unsafe { create_tap_and_source(state_ptr, &init_tx) }) else {
        return;
    };

    // add_to_run_loop() attaches the tap/source to this thread's run loop. If
    // it fails, remove_from_run_loop() in the cleanup path is a no-op and the
    // single Arc::from_raw at thread exit balances Arc::into_raw above.
    unsafe {
        tap_source.add_to_run_loop();
    }

    // Signal successful initialization
    let _ = init_tx.send(Ok(()));

    // Run the loop
    let mut heartbeat_start = Instant::now();
    let mut last_callback_count: u64 = 0;
    let mut zero_callback_seconds: u32 = 0;
    let mut consecutive_reenable_failures: u32 = 0;
    let mut active_tap_source = tap_source;
    while running.load(std::sync::atomic::Ordering::SeqCst) {
        // Run for a short interval, then check if we should stop
        CFRunLoop::run_in_mode(
            unsafe { objc2_core_foundation::kCFRunLoopDefaultMode },
            0.1, // 100ms timeout
            true,
        );

        // Re-enable tap if macOS disabled it due to callback latency
        let enabled_before = CGEvent::tap_is_enabled(&active_tap_source.tap);
        if !enabled_before {
            CGEvent::tap_enable(&active_tap_source.tap, true);
        }
        let enabled_after = CGEvent::tap_is_enabled(&active_tap_source.tap);

        if enabled_after {
            consecutive_reenable_failures = 0;
        } else if enabled_before {
            // tap_enable was called this iteration and it is still disabled.
            consecutive_reenable_failures += 1;
            if consecutive_reenable_failures > 5 {
                error!(
                    "CGEventTap appears invalidated; re-enable is no-op (consecutive_failures={})",
                    consecutive_reenable_failures
                );
                // Keep looping — do not break — the tap will be recreated below
                // when the zero-callback threshold is reached.
            }
        }

        // 1-second heartbeat for diagnosing tap death after sleep/wake.
        if heartbeat_start.elapsed() >= Duration::from_secs(1) {
            let current_count = CALLBACK_COUNT.load(Ordering::Relaxed);
            let delta = current_count - last_callback_count;
            debug!(
                "CGEventTap heartbeat: enabled={}, callbacks_since_last={}",
                enabled_after, delta
            );

            // Track how many seconds in a row the tap delivered zero callbacks.
            // We only recreate after TAP_DEATH_THRESHOLD_SECS to avoid tearing
            // down the tap when the user is simply idle. This detects the "silent
            // death" case where tap_is_enabled still returns true but the tap
            // never calls us back (observed after long runtimes / sleep-wake).
            if delta == 0 {
                zero_callback_seconds += 1;
            } else {
                zero_callback_seconds = 0;
            }

            // Recreate the tap if it has been "enabled" but silent for long enough.
            // tap_enable alone cannot recover from this state; only a fresh tap
            // and run-loop source restores callbacks. We reuse the same `state_ptr`
            // (and the underlying `Arc`) so listener state is preserved.
            if zero_callback_seconds >= TAP_DEATH_THRESHOLD_SECS {
                warn!(
                    "CGEventTap appears silently dead ({}s with zero callbacks despite enabled=true); recreating",
                    zero_callback_seconds
                );

                // Remove the old tap/source from the run loop and invalidate it
                // before creating a replacement, so macOS releases the old event
                // tap slot.
                active_tap_source.remove_from_run_loop();

                if let Some(new_tap_source) = unsafe { create_tap_and_source(state_ptr, &init_tx) }
                {
                    unsafe {
                        new_tap_source.add_to_run_loop();
                    }
                    active_tap_source = new_tap_source;
                } else {
                    // Recreation failed. Without a working tap we cannot deliver
                    // hotkeys, so exit the run loop. The caller (run_event_tap)
                    // still owns the state Arc and will drop it once at cleanup.
                    error!("CGEventTap recreation failed; exiting event tap thread");
                    break;
                }

                zero_callback_seconds = 0;
                last_callback_count = CALLBACK_COUNT.load(Ordering::Relaxed);
                consecutive_reenable_failures = 0;
            } else {
                last_callback_count = current_count;
            }

            heartbeat_start = Instant::now();
        }
    }

    // Cleanup
    active_tap_source.remove_from_run_loop();
    unsafe {
        // Balance the Arc::into_raw above; after this the `state` Arc is dropped.
        let _ = Arc::from_raw(state_ptr as *const Mutex<ListenerState>);
    }
}
