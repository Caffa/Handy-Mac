//! macOS sleep/wake detection via wall-clock vs monotonic-time polling
//!
//! Detects when macOS wakes from sleep and triggers USB power-cycle
//! recovery for the configured USB audio device. This is needed because
//! USB audio devices (especially the RØDE VideoMic NTG) often become
//! unresponsive after the Mac wakes from sleep — the CoreAudio input
//! unit is suspended and never resumes, and the device can enter a
//! zombie state where it appears connected but produces no audio.
//!
//! Uses a simple timer-based approach: polls every 3 seconds comparing
//! wall clock time vs monotonic time. After sleep, the wall clock jumps
//! forward significantly more than monotonic time, which we detect
//! as a "wake from sleep" event. This avoids complex Objective-C runtime
//! FFI and works without any external crate dependencies.

#[cfg(target_os = "macos")]
use log::{debug, error, info, warn};
#[cfg(target_os = "macos")]
use std::sync::Arc;
#[cfg(target_os = "macos")]
use tauri::Manager;

/// Start listening for macOS wake-from-sleep events.
///
/// Spawns a background thread that polls every 3 seconds, comparing
/// wall clock time (`SystemTime`) against monotonic time (`Instant`).
/// When the wall clock jumps ahead of monotonic time by more than 10
/// seconds, we know the system just woke from sleep.
///
/// The thread runs for the lifetime of the app; it is daemon-like
/// and will be cleaned up when the process exits.
#[cfg(target_os = "macos")]
pub fn start_sleep_wake_listener(app_handle: tauri::AppHandle) {
    use std::sync::atomic::{AtomicBool, Ordering};

    static LISTENER_STARTED: AtomicBool = AtomicBool::new(false);

    if LISTENER_STARTED.swap(true, Ordering::SeqCst) {
        debug!("Sleep/wake listener already started, skipping");
        return;
    }

    info!("Starting timer-based sleep/wake detector (polling every 3s)");

    let app_handle = Arc::new(app_handle);

    std::thread::spawn(move || {
        use std::time::{Instant, SystemTime};

        // Track the difference between wall clock and monotonic time.
        // After sleep, wall clock jumps forward but monotonic time
        // approximately matches our 3s poll interval. So if wall clock
        // suddenly jumped by e.g. 300s but monotonic only advanced 3s,
        // the system was sleeping for ~297s.
        let mut last_wall = SystemTime::now();
        let mut last_mono = Instant::now();

        loop {
            std::thread::sleep(std::time::Duration::from_secs(3));

            let now_wall = SystemTime::now();
            let now_mono = Instant::now();

            let mono_elapsed = now_mono.duration_since(last_mono);
            let wall_elapsed = now_wall
                .duration_since(last_wall)
                .unwrap_or(std::time::Duration::from_secs(0));

            // After sleep:
            //   mono_elapsed ≈ 3s (our poll interval)
            //   wall_elapsed ≈ 3s + sleep_duration
            //
            // We require wall > mono + 10s to avoid false positives
            // from normal scheduling jitter (which is typically <1s).
            let wall_minus_mono = wall_elapsed
                .as_secs()
                .saturating_sub(mono_elapsed.as_secs());

            if wall_minus_mono > 10 {
                // Verify this isn't just a monotonic clock reset:
                // our poll interval should still be roughly 3s.
                let poll_interval = mono_elapsed.as_secs();
                if poll_interval >= 2 && poll_interval <= 15 {
                    info!(
                        "Detected macOS wake from sleep (wall clock {}s ahead of monotonic, poll interval {}s)",
                        wall_minus_mono, poll_interval
                    );
                    on_system_wake(&app_handle);
                }
            }

            last_wall = now_wall;
            last_mono = now_mono;
        }
    });
}

/// Handle macOS wake-from-sleep event.
///
/// Checks the cycle-on-wake setting and triggers a USB power cycle
/// if enabled. Gives macOS a brief moment to re-enumerate USB
/// devices before cycling.
#[cfg(target_os = "macos")]
fn on_system_wake(app_handle: &Arc<tauri::AppHandle>) {
    info!("macOS woke from sleep — checking USB watchdog cycle-on-wake setting");

    let settings = crate::settings::get_settings(app_handle);
    let cycle_on_wake = settings.usb_watchdog_enabled && settings.usb_watchdog_cycle_on_wake;

    if !cycle_on_wake {
        debug!("USB watchdog cycle-on-wake is disabled, skipping");
        return;
    }

    let device_name = settings.usb_watchdog_device_name.clone();
    if device_name.is_empty() {
        warn!("Wake notification: USB watchdog device name not configured, skipping cycle");
        return;
    }

    info!(
        "Triggering USB power cycle on wake for device '{}'",
        device_name
    );

    // Trigger the forced power cycle asynchronously.
    // Give macOS 2 seconds to re-enumerate USB after wake before cycling
    // power — immediately after wake the USB tree may not be stable yet.
    let ah = app_handle.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(2));

        let watchdog = ah.try_state::<Arc<crate::usb_watchdog::UsbWatchdog>>();

        match watchdog {
            Some(wd) => {
                info!("Starting post-wake USB power cycle");
                if wd.force_power_cycle() {
                    info!("Post-wake USB power cycle initiated successfully");

                    // After a successful forced power cycle, restart the
                    // microphone stream if it should be active.
                    if let Some(rm) =
                        ah.try_state::<Arc<crate::managers::audio::AudioRecordingManager>>()
                    {
                        if let Err(e) = rm.restart_microphone_if_needed() {
                            error!("Failed to restart microphone after wake USB cycle: {}", e);
                        }
                    }
                } else {
                    warn!(
                        "Post-wake USB power cycle was skipped (already cycling or device not found)"
                    );
                }
            }
            None => {
                warn!("USB watchdog not available in app state on wake");
            }
        }
    });
}

/// No-op on non-macOS platforms.
#[cfg(not(target_os = "macos"))]
pub fn start_sleep_wake_listener(_app_handle: tauri::AppHandle) {
    // Sleep/wake detection is macOS-only; no-op on other platforms
}
