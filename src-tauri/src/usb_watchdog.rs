//! USB hub power-cycle watchdog for recovering dead USB audio devices
//!
//! When Handy fails to open the microphone stream (device not found, zombie
//! device, etc.), this module can automatically power-cycle the USB hub port
//! via `uhubctl` and then retry the stream open.
//!
//! The user selects a device by *name* (e.g. "RØDE Microphones RØDE VideoMic NTG").
//! At cycle time, we re-run `uhubctl` to resolve the device name to a
//! specific hub location and port number, then cycle that port.
//!
//! Ported from the Hammerspoon Rode watchdog script at:
//!   ~/.hammerspoon/init.lua

use log::{debug, error, info, warn};
use serde::Serialize;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long to wait after cycling power before considering the device "up"
const POWER_CYCLE_SETTLE_SECS: u64 = 12;

/// Minimum seconds between two automatic power cycles (cooldown)
const RESET_COOLDOWN_SECS: u64 = 30;

/// A USB device discovered by `uhubctl`.
#[derive(Debug, Clone, Serialize, specta::Type)]
pub struct UsbDevice {
    /// Human-readable device name (e.g. "RØDE Microphones RØDE VideoMic NTG 762210B9")
    pub name: String,
    /// Hub location ID (e.g. "8-3")
    pub hub: String,
    /// Port number on the hub (e.g. "1")
    pub port: String,
}

/// Internal state for the watchdog (shared behind Arc)
pub struct UsbWatchdog {
    /// Whether the watchdog is enabled
    enabled: AtomicBool,
    /// Device name to watch for (set by user, resolved to hub/port at cycle time)
    device_name: Mutex<String>,
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
    pub fn new(enabled: bool, device_name: &str) -> Self {
        Self {
            enabled: AtomicBool::new(enabled),
            device_name: Mutex::new(device_name.to_string()),
            cycling: AtomicBool::new(false),
            last_cycle_epoch: AtomicU64::new(0),
            consecutive_failures: AtomicU64::new(0),
            fail_threshold: AtomicU64::new(2),
        }
    }

    /// Update configuration at runtime
    pub fn update_config(&self, enabled: bool, device_name: String) {
        self.enabled.store(enabled, Ordering::SeqCst);
        *self.device_name.lock().unwrap() = device_name;
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

    /// Attempt a USB hub port power cycle by resolving the device name
    /// to hub/port at cycle time. Returns `true` if a cycle was initiated.
    pub fn power_cycle(&self) -> bool {
        // Check cooldown
        let now_epoch = epoch_secs();
        let last = self.last_cycle_epoch.load(Ordering::SeqCst);
        if now_epoch > last && (now_epoch - last) < RESET_COOLDOWN_SECS {
            let remaining = RESET_COOLDOWN_SECS - (now_epoch - last);
            debug!("USB watchdog: cooldown active, {}s remaining", remaining);
            return false;
        }

        if self.cycling.swap(true, Ordering::SeqCst) {
            debug!("USB watchdog: cycle already in progress");
            return false;
        }

        let device_name = self.device_name.lock().unwrap().clone();
        if device_name.is_empty() {
            warn!("USB watchdog: device name not configured, skipping");
            self.cycling.store(false, Ordering::SeqCst);
            return false;
        }

        // Resolve device name → hub/port at cycle time
        let device = match resolve_device(&device_name) {
            Some(d) => d,
            None => {
                warn!("USB watchdog: device '{}' not found in USB tree, cannot cycle", device_name);
                self.cycling.store(false, Ordering::SeqCst);
                return false;
            }
        };

        self.last_cycle_epoch.store(now_epoch, Ordering::SeqCst);
        self.consecutive_failures.store(0, Ordering::SeqCst);

        info!(
            "USB watchdog: power cycling device '{}' (hub {} port {})",
            device_name, device.hub, device.port
        );

        // Spawn the uhubctl command and wait for it + settle time
        let hub = device.hub.clone();
        let port = device.port.clone();
        let name = device_name.clone();
        std::thread::spawn(move || {
            let start = Instant::now();
            match run_uhubctl_cycle(&hub, &port) {
                Ok(()) => {
                    info!(
                        "USB watchdog: uhubctl cycle completed for '{}' in {:?}",
                        name,
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

    /// Manually trigger a power cycle (e.g. from settings UI).
    /// Ignores cooldown. Resolves device name at call time.
    pub fn force_power_cycle(&self) -> bool {
        if self.cycling.swap(true, Ordering::SeqCst) {
            debug!("USB watchdog: cycle already in progress");
            return false;
        }

        let device_name = self.device_name.lock().unwrap().clone();
        if device_name.is_empty() {
            warn!("USB watchdog: device name not configured");
            self.cycling.store(false, Ordering::SeqCst);
            return false;
        }

        let device = match resolve_device(&device_name) {
            Some(d) => d,
            None => {
                warn!("USB watchdog: device '{}' not found for forced cycle", device_name);
                self.cycling.store(false, Ordering::SeqCst);
                return false;
            }
        };

        self.last_cycle_epoch.store(epoch_secs(), Ordering::SeqCst);
        self.consecutive_failures.store(0, Ordering::SeqCst);

        info!(
            "USB watchdog: FORCE power cycling device '{}' (hub {} port {})",
            device_name, device.hub, device.port
        );

        let hub = device.hub.clone();
        let port = device.port.clone();
        let name = device_name.clone();
        std::thread::spawn(move || {
            let start = Instant::now();
            match run_uhubctl_cycle(&hub, &port) {
                Ok(()) => {
                    info!(
                        "USB watchdog: forced uhubctl cycle completed for '{}' in {:?}",
                        name,
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

// ---------------------------------------------------------------------------
// Device listing & resolution
// ---------------------------------------------------------------------------

/// Resolve a device name substring to a UsbDevice by running `uhubctl`.
/// Matches the first device whose description contains `name` (case-sensitive).
fn resolve_device(name: &str) -> Option<UsbDevice> {
    let devices = list_usb_devices_inner();
    devices.into_iter().find(|d| d.name.contains(name))
}

/// List all USB devices connected to hubs visible to uhubctl.
/// Called from the Tauri command layer to populate the UI.
pub fn list_usb_devices() -> Vec<UsbDevice> {
    list_usb_devices_inner()
}

fn list_usb_devices_inner() -> Vec<UsbDevice> {
    let bin = match uhubctl_bin() {
        Some(b) => b,
        None => return Vec::new(),
    };

    let output = match std::process::Command::new(&bin)
        .env("PATH", "/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin")
        .output()
    {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
        _ => return Vec::new(),
    };

    parse_uhubctl_output(&output)
}

/// Parse the output of `uhubctl` (no arguments) into a list of devices.
///
/// Example input:
/// ```text
/// Current status for hub 8-3 [2109:2812 VIA Labs, Inc. USB2.0 Hub, USB 2.10, 4 ports, ppps]
///   Port 1: 0103 power enable connect [19f7:001a RØDE Microphones RØDE VideoMic NTG 762210B9]
///   Port 2: 0103 power enable connect [3297:1969 ZSA Technology Labs Moonlander Mark I default/latest]
/// ```
fn parse_uhubctl_output(output: &str) -> Vec<UsbDevice> {
    let mut devices = Vec::new();
    let mut current_hub: Option<String> = None;

    for line in output.lines() {
        let trimmed = line.trim();

        // Detect hub header: "Current status for hub 8-3 [2109:2812 ...]"
        if let Some(rest) = trimmed.strip_prefix("Current status for hub ") {
            // Extract hub ID (the first space-delimited token)
            if let Some(hub_id) = rest.split_whitespace().next() {
                current_hub = Some(hub_id.to_string());
            }
            continue;
        }

        // Detect port line with a connected device:
        // "Port 2: 0103 power enable connect [19f7:001a RØDE Microphones RØDE VideoMic NTG 762210B9]"
        if let Some(rest) = trimmed.strip_prefix("Port ") {
            // Extract port number
            if let Some(colon_pos) = rest.find(':') {
                let port_str = rest[..colon_pos].trim();
                // Check if device is connected (contains "connect")
                if !rest.contains("connect") {
                    continue;
                }
                // Extract device description from brackets [vid:pid name ...]
                if let Some(desc) = extract_device_description(rest) {
                    if let Some(ref hub) = current_hub {
                        devices.push(UsbDevice {
                            name: desc,
                            hub: hub.clone(),
                            port: port_str.to_string(),
                        });
                    }
                }
            }
        }
    }

    devices
}

/// Extract the device description from a port line after the colon.
/// Input: "2: 0103 power enable connect [19f7:001a RØDE Microphones RØDE VideoMic NTG 762210B9]"
/// Returns: "RØDE Microphones RØDE VideoMic NTG 762210B9"
fn extract_device_description(rest: &str) -> Option<String> {
    // Find the content between [ and ]
    let start = rest.find('[')?;
    let end = rest.rfind(']')?;
    let bracket_content = &rest[start + 1..end];

    // The format is "vid:pid description"
    // Skip the vid:pid part (first space-delimited token)
    let mut parts = bracket_content.splitn(2, ' ');
    parts.next(); // skip vid:pid
    parts.next().map(|s| s.to_string())
}

// ---------------------------------------------------------------------------
// uhubctl binary resolution & execution
// ---------------------------------------------------------------------------

const UHUBCTL_PATHS: &[&str] = &[
    "/usr/local/bin/uhubctl",
    "/opt/homebrew/bin/uhubctl",
];

/// Resolve the uhubctl binary path
fn uhubctl_bin() -> Option<std::path::PathBuf> {
    // Check standard paths first
    for path in UHUBCTL_PATHS {
        if std::path::Path::new(path).exists() {
            return Some(std::path::PathBuf::from(*path));
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
        .args(["-l", hub_id, "-p", port, "-a", "cycle", "-d", "3"])
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
/// Returns true if uhubctl is available after the attempt.
pub fn ensure_uhubctl_installed() -> bool {
    if is_uhubctl_available() {
        info!("uhubctl found — USB watchdog ready");
        return true;
    }

    info!("uhubctl not found, attempting to install via Homebrew…");

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