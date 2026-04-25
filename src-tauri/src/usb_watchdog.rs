//! USB hub power-cycle watchdog for recovering dead USB audio devices
//!
//! When Handy fails to open the microphone stream (device not found, zombie
//! device, etc.), this module can automatically power-cycle a USB hub port
//! via `uhubctl` and then retry the stream open.
//!
//! Ported from the Hammerspoon Rode watchdog script at:
//!   ~/.hammerspoon/init.lua

use log::{debug, error, info, warn};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Default uhubctl binary path
const UHUBCTL_PATH: &str = "/usr/local/bin/uhubctl";

/// Alternate path common on Apple Silicon Macs (Homebrew arm64)
const UHUBCTL_PATH_ARM: &str = "/opt/homebrew/bin/uhubctl";

/// How long to wait after cycling power before considering the device "up"
const POWER_CYCLE_SETTLE_SECS: u64 = 12;

/// Minimum seconds between two automatic power cycles (cooldown)
const RESET_COOLDOWN_SECS: u64 = 30;

/// Internal state for the watchdog (shared behind Arc)
pub struct UsbWatchdog {
    /// Whether the watchdog is enabled
    enabled: AtomicBool,
    /// USB hub location ID, e.g. "8-3"
    hub_id: Mutex<String>,
    /// Port number on the hub, e.g. "2"
    port: Mutex<String>,
    /// A cycle is currently in progress
    cycling: AtomicBool,
    /// Epoch seconds of last completed cycle (for cooldown)
    last_cycle_epoch: AtomicU64,
    /// Number of consecutive mic-open failures since last successful open
    consecutive_failures: AtomicU64,
    /// After how many consecutive failures to trigger a cycle (default 2)
    fail_threshold: AtomicU64,
}

impl UsbWatchdog {
    pub fn new(enabled: bool, hub_id: &str, port: &str) -> Self {
        Self {
            enabled: AtomicBool::new(enabled),
            hub_id: Mutex::new(hub_id.to_string()),
            port: Mutex::new(port.to_string()),
            cycling: AtomicBool::new(false),
            last_cycle_epoch: AtomicU64::new(0),
            consecutive_failures: AtomicU64::new(0),
            fail_threshold: AtomicU64::new(2),
        }
    }

    /// Update configuration at runtime
    pub fn update_config(&self, enabled: bool, hub_id: String, port: String) {
        self.enabled.store(enabled, Ordering::SeqCst);
        *self.hub_id.lock().unwrap() = hub_id;
        *self.port.lock().unwrap() = port;
        debug!("USB watchdog config updated: enabled={}", enabled);
    }

    #[allow(dead_code)]
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::SeqCst)
    }

    /// Called when the mic stream fails to open. Returns `true` if a
    /// power cycle was initiated (caller should wait and retry).
    pub fn on_mic_open_failed(&self) -> bool {
        if !self.enabled.load(Ordering::SeqCst) {
            debug!("USB watchdog disabled, skipping auto-cycle");
            return false;
        }

        if self.cycling.load(Ordering::SeqCst) {
            debug!("USB cycle already in progress, skipping");
            return false;
        }

        let failures = self.consecutive_failures.fetch_add(1, Ordering::SeqCst) + 1;
        let threshold = self.fail_threshold.load(Ordering::SeqCst);
        debug!(
            "USB watchdog: mic open failure #{} (threshold: {})",
            failures, threshold
        );

        if failures < threshold {
            debug!("Failure count below threshold, not cycling yet");
            return false;
        }

        self.power_cycle()
    }

    /// Called when the mic stream opens successfully (resets failure counter)
    pub fn on_mic_open_succeeded(&self) {
        let prev = self.consecutive_failures.swap(0, Ordering::SeqCst);
        if prev > 0 {
            debug!("USB watchdog: mic opened successfully, reset failures (was {})", prev);
        }
    }

    /// Attempt a USB hub port power cycle. Returns `true` if the cycle
    /// command was launched.
    pub fn power_cycle(&self) -> bool {
        // Check cooldown
        let now_epoch = epoch_secs();
        let last = self.last_cycle_epoch.load(Ordering::SeqCst);
        if now_epoch > last && (now_epoch - last) < RESET_COOLDOWN_SECS {
            let remaining = RESET_COOLDOWN_SECS - (now_epoch - last);
            debug!(
                "USB watchdog: cooldown active, {}s remaining",
                remaining
            );
            return false;
        }

        if self.cycling.swap(true, Ordering::SeqCst) {
            debug!("USB watchdog: cycle already in progress");
            return false;
        }

        let hub_id = self.hub_id.lock().unwrap().clone();
        let port = self.port.lock().unwrap().clone();

        if hub_id.is_empty() || port.is_empty() {
            warn!("USB watchdog: hub_id or port not configured, skipping");
            self.cycling.store(false, Ordering::SeqCst);
            return false;
        }

        self.last_cycle_epoch.store(now_epoch, Ordering::SeqCst);
        self.consecutive_failures.store(0, Ordering::SeqCst);

        info!(
            "USB watchdog: power cycling hub {} port {}",
            hub_id, port
        );

        // Spawn the uhubctl command and wait for it + settle time
        let hub_id_clone = hub_id.clone();
        let port_clone = port.clone();
        std::thread::spawn(move || {
            let start = Instant::now();
            match run_uhubctl_cycle(&hub_id_clone, &port_clone) {
                Ok(()) => {
                    info!(
                        "USB watchdog: uhubctl cycle completed in {:?}",
                        start.elapsed()
                    );
                    // Wait for device to re-enumerate
                    std::thread::sleep(Duration::from_secs(POWER_CYCLE_SETTLE_SECS));
                    info!("USB watchdog: settle period complete, device should be available");
                }
                Err(e) => {
                    error!("USB watchdog: uhubctl failed: {}", e);
                }
            }
        });

        self.cycling.store(false, Ordering::SeqCst);
        true
    }

    /// Manually trigger a power cycle (e.g. from settings UI or hotkey).
    /// Ignores cooldown.
    pub fn force_power_cycle(&self) -> bool {
        if self.cycling.swap(true, Ordering::SeqCst) {
            debug!("USB watchdog: cycle already in progress");
            return false;
        }

        let hub_id = self.hub_id.lock().unwrap().clone();
        let port = self.port.lock().unwrap().clone();

        if hub_id.is_empty() || port.is_empty() {
            warn!("USB watchdog: hub_id or port not configured");
            self.cycling.store(false, Ordering::SeqCst);
            return false;
        }

        self.last_cycle_epoch.store(epoch_secs(), Ordering::SeqCst);
        self.consecutive_failures.store(0, Ordering::SeqCst);

        info!(
            "USB watchdog: FORCE power cycling hub {} port {}",
            hub_id, port
        );

        let hub_id_clone = hub_id.clone();
        let port_clone = port.clone();
        std::thread::spawn(move || {
            let start = Instant::now();
            match run_uhubctl_cycle(&hub_id_clone, &port_clone) {
                Ok(()) => {
                    info!(
                        "USB watchdog: forced uhubctl cycle completed in {:?}",
                        start.elapsed()
                    );
                    std::thread::sleep(Duration::from_secs(POWER_CYCLE_SETTLE_SECS));
                    info!("USB watchdog: settle period complete after forced cycle");
                }
                Err(e) => {
                    error!("USB watchdog: forced uhubctl failed: {}", e);
                }
            }
        });

        self.cycling.store(false, Ordering::SeqCst);
        true
    }
}

/// Resolve the uhubctl binary path
fn uhubctl_bin() -> Option<std::path::PathBuf> {
    // Check standard paths first
    for path in [UHUBCTL_PATH, UHUBCTL_PATH_ARM] {
        if std::path::Path::new(path).exists() {
            return Some(std::path::PathBuf::from(path));
        }
    }
    // Fall back to PATH lookup
    which_uhubctl()
}

fn which_uhubctl() -> Option<std::path::PathBuf> {
    std::process::Command::new("which")
        .arg("uhubctl")
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                String::from_utf8(o.stdout).ok()
            } else {
                None
            }
        })
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .map(std::path::PathBuf::from)
}

/// Run `uhubctl -l <hub> -p <port> -a cycle -d 3`
fn run_uhubctl_cycle(hub_id: &str, port: &str) -> Result<(), String> {
    let bin = uhubctl_bin().ok_or_else(|| "uhubctl not found on system".to_string())?;

    let output = std::process::Command::new(&bin)
        .args([
            "-l",
            hub_id,
            "-p",
            port,
            "-a",
            "cycle",
            "-d",
            "3",
        ])
        .env("PATH", "/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin")
        .output()
        .map_err(|e| format!("Failed to execute uhubctl: {}", e))?;

    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        debug!("uhubctl stdout: {}", stdout.trim());
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        Err(format!(
            "uhubctl exited with {}: stderr={}, stdout={}",
            output.status,
            stderr.trim(),
            stdout.trim()
        ))
    }
}

/// Check if uhubctl is available on the system
pub fn is_uhubctl_available() -> bool {
    uhubctl_bin().is_some()
}

/// Attempt to install uhubctl via Homebrew on macOS.
/// Returns true if uhubctl is available after the attempt (either already
/// installed or newly installed).
///
/// This is meant to be called once at app startup. It runs `brew install`
/// and logs the result. If Homebrew is not installed or the install fails,
/// it simply returns false — the watchdog will be disabled until the user
/// installs uhubctl manually.
pub fn ensure_uhubctl_installed() -> bool {
    if is_uhubctl_available() {
        info!("uhubctl found — USB watchdog ready");
        return true;
    }

    info!("uhubctl not found, attempting to install via Homebrew…");

    // Check if brew is available
    let brew_check = std::process::Command::new("which")
        .arg("brew")
        .output();

    match brew_check {
        Ok(output) if output.status.success() => {
            info!("Homebrew found, installing uhubctl…");

            match std::process::Command::new("brew")
                .args(["install", "uhubctl"])
                .env("PATH", "/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin")
                .output()
            {
                Ok(output) => {
                    if output.status.success() {
                        info!("uhubctl installed successfully via Homebrew");
                        if is_uhubctl_available() {
                            info!("uhubctl confirmed available — USB watchdog ready");
                            true
                        } else {
                            warn!("uhubctl installed but not found at expected paths");
                            is_uhubctl_available()
                        }
                    } else {
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        warn!("brew install uhubctl failed: {}", stderr.trim());
                        false
                    }
                }
                Err(e) => {
                    warn!("Failed to run brew install uhubctl: {}", e);
                    false
                }
            }
        }
        _ => {
            info!("Homebrew not found — USB watchdog requires uhubctl. Install with: brew install uhubctl");
            false
        }
    }
}

fn epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}