use cpal::traits::{DeviceTrait, HostTrait};

pub struct CpalDeviceInfo {
    pub index: String,
    pub name: String,
    pub is_default: bool,
    pub device: cpal::Device,
}

pub fn list_input_devices() -> Result<Vec<CpalDeviceInfo>, Box<dyn std::error::Error>> {
    let host = crate::audio_toolkit::get_cpal_host();
    let default_name = host.default_input_device().and_then(|d| d.name().ok());

    let mut out = Vec::<CpalDeviceInfo>::new();

    for (index, device) in host.input_devices()?.enumerate() {
        let name = device.name().unwrap_or_else(|_| "Unknown".into());

        let is_default = Some(name.clone()) == default_name;

        out.push(CpalDeviceInfo {
            index: index.to_string(),
            name,
            is_default,
            device,
        });
    }

    Ok(out)
}

pub fn list_output_devices() -> Result<Vec<CpalDeviceInfo>, Box<dyn std::error::Error>> {
    let host = crate::audio_toolkit::get_cpal_host();
    let default_name = host.default_output_device().and_then(|d| d.name().ok());

    let mut out = Vec::<CpalDeviceInfo>::new();

    for (index, device) in host.output_devices()?.enumerate() {
        let name = device.name().unwrap_or_else(|_| "Unknown".into());

        let is_default = Some(name.clone()) == default_name;

        out.push(CpalDeviceInfo {
            index: index.to_string(),
            name,
            is_default,
            device,
        });
    }

    Ok(out)
}

/// Check whether the current default output device (or a specific named device)
/// appears to be a Bluetooth audio device based on its name.
///
/// Bluetooth devices on macOS typically contain one of these substrings:
/// - "Bluetooth" (generic)
/// - "AirPods" (Apple)
/// - "Powerbeats" (Beats)
/// - "Beats" (Beats Electronics)
/// - "WH-" or "WF-" (Sony)
/// - "JBL" (JBL)
/// - "Bose" (Bose)
/// - "Jabra" (Jabra)
/// - "Marshall" (Marshall)
/// - "Galaxy Buds" (Samsung)
/// - "FreeBuds" (Huawei)
///
/// When a Bluetooth headset with microphone capability is connected, macOS
/// switches it from A2DP (high quality stereo) to HFP/SCO (low quality mono)
/// whenever *any* audio input stream is opened — even on a different device.
/// This causes a brief audio dropout on the Bluetooth output. Keeping the
/// mic stream alive prevents the repeated profile switching.
pub fn is_bluetooth_output_device(device_name: &str) -> bool {
    let lower = device_name.to_lowercase();

    // Explicit "Bluetooth" in the name (macOS sometimes shows this)
    if lower.contains("bluetooth") {
        return true;
    }

    // Apple AirPods family
    if lower.contains("airpods") {
        return true;
    }

    // Beats family
    if lower.contains("beats") || lower.contains("powerbeats") {
        return true;
    }

    // Sony WH/WF series
    if lower.starts_with("wh-") || lower.starts_with("wf-") {
        return true;
    }

    // Other common Bluetooth brands
    if lower.contains("jbl")
        || lower.contains("bose")
        || lower.contains("jabra")
        || lower.contains("marshall")
    {
        return true;
    }

    // Samsung, Huawei
    if lower.contains("galaxy buds") || lower.contains("freebuds") {
        return true;
    }

    false
}

/// Returns true if the system default output device is a Bluetooth device,
/// or if the given selected output device is Bluetooth.
pub fn is_bluetooth_audio_active(selected_output: Option<&str>) -> bool {
    let host = crate::audio_toolkit::get_cpal_host();

    // Check the explicitly selected output device first
    if let Some(name) = selected_output {
        if name != "Default" && is_bluetooth_output_device(name) {
            log::debug!(
                "Bluetooth output device detected: '{}', will keep mic stream alive",
                name
            );
            return true;
        }
    }

    // Fall back to checking the system default output device
    if let Some(default_device) = host.default_output_device() {
        if let Ok(name) = default_device.name() {
            if is_bluetooth_output_device(&name) {
                log::debug!(
                    "Bluetooth default output device detected: '{}', will keep mic stream alive",
                    name
                );
                return true;
            }
        }
    }

    false
}
