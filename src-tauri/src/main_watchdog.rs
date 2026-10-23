//! Main-thread heartbeat watchdog.
//!
//! The 2026-10-23 wedge (see `research/main-thread-hang-tray-lock-wedge-2026-10-23.md`)
//! was invisible to logs: the macOS main thread parked forever inside a paste
//! closure, every background `run_on_main_thread` caller blocked with it, and
//! nothing was ever logged because the parked thread could not log.
//!
//! This watchdog detects exactly that blind spot: a background thread sends a
//! no-op heartbeat to the main thread every 2s (`run_on_main_thread` from a
//! non-main thread only *enqueues* a message — it never blocks) and the main
//! thread stamps a monotonic clock when it executes it. If the stamp goes
//! stale for more than 5 seconds, the main loop is not draining and we say so
//! loudly in the log, rate-limited to one report per 30s.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tauri::AppHandle;

/// Monotonic milliseconds (measured from the first call) at which the main
/// thread last executed a heartbeat task. 0 = no heartbeat seen yet.
static LAST_MAIN_THREAD_TICK_MS: AtomicU64 = AtomicU64::new(0);

/// Monotonic milliseconds at which the last MAIN-THREAD-HANG warning was
/// emitted (rate limiting). 0 = never reported.
static LAST_HANG_REPORT_MS: AtomicU64 = AtomicU64::new(0);

/// How often the watchdog thread polls / sends a heartbeat.
const POLL_INTERVAL: Duration = Duration::from_secs(2);
/// A tick older than this means the main loop is not draining.
const STALE_THRESHOLD_MS: u64 = 5000;
/// Minimum gap between hang warnings, so a long wedge burns one log line per
/// 30s instead of flooding.
const REPORT_COOLDOWN_MS: u64 = 30_000;

/// Monotonic clock in milliseconds, immune to system clock changes (unlike
/// `SystemTime`, which can jump backwards and produce false hang reports).
fn mono_ms() -> u64 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// Spawns the watchdog thread. Call once from Tauri's `.setup(...)`.
pub fn start(app: AppHandle) {
    // Seed the stamp so a main thread that never drains from the very first
    // poll is still reported after the stale threshold.
    LAST_MAIN_THREAD_TICK_MS.store(mono_ms(), Ordering::Relaxed);

    let spawn_result = std::thread::Builder::new()
        .name("main-thread-watchdog".into())
        .spawn(move || loop {
            std::thread::sleep(POLL_INTERVAL);

            // Heartbeat: fire-and-forget by design. From a non-main thread
            // this only enqueues a task on the main loop's queue; it never
            // parks the watchdog even if the main thread is wedged.
            let _ = app.run_on_main_thread(|| {
                LAST_MAIN_THREAD_TICK_MS.store(mono_ms(), Ordering::Relaxed);
            });

            // Staleness check.
            let now = mono_ms();
            let last_tick = LAST_MAIN_THREAD_TICK_MS.load(Ordering::Relaxed);
            let stale_for_ms = now.saturating_sub(last_tick);
            if stale_for_ms > STALE_THRESHOLD_MS {
                let last_report = LAST_HANG_REPORT_MS.load(Ordering::Relaxed);
                if now.saturating_sub(last_report) > REPORT_COOLDOWN_MS {
                    LAST_HANG_REPORT_MS.store(now, Ordering::Relaxed);
                    log::warn!(
                        "MAIN-THREAD-HANG elapsed={}s last_tick={}s ago — main loop is not draining; \
                         run_on_main_thread callers (tray, overlay, paste) will block",
                        stale_for_ms / 1000,
                        stale_for_ms / 1000
                    );
                }
            }
        });

    if let Err(e) = spawn_result {
        // The watchdog is diagnostics-only; never take the app down over it.
        log::error!("Failed to spawn main-thread-watchdog: {}", e);
    }
}
