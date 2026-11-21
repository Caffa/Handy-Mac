use rodio::cpal::{
    self,
    traits::{DeviceTrait, HostTrait},
};

/// An output device for feedback sounds. Microphones are listed by
/// `handy_recorder::list_input_devices`.
pub struct OutputDeviceInfo {
    pub index: String,
    pub name: String,
    pub is_default: bool,
}

/// The name feedback sounds match `selected_output_device` against.
pub fn output_device_name(device: &cpal::Device) -> Option<String> {
    device.description().ok().map(|d| d.name().to_owned())
}

/// An input device for recording. Feedback-sound outputs are listed by
/// `list_output_devices`.
pub fn input_device_name(device: &cpal::Device) -> Option<String> {
    device.description().ok().map(|d| d.name().to_owned())
}

pub fn list_output_devices() -> Result<Vec<OutputDeviceInfo>, Box<dyn std::error::Error>> {
    let host = crate::audio_toolkit::get_cpal_host();
    let default_name = host
        .default_output_device()
        .and_then(|d| output_device_name(&d));

    let mut out = Vec::<OutputDeviceInfo>::new();

    for (index, device) in host.output_devices()?.enumerate() {
        let name = output_device_name(&device).unwrap_or_else(|| "Unknown".into());

        let is_default = Some(name.clone()) == default_name;

        out.push(OutputDeviceInfo {
            index: index.to_string(),
            name,
            is_default,
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
pub fn is_bluetooth_device_name(device_name: &str) -> bool {
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

/// Backwards-compatible alias: check whether an *output* device name is a
/// Bluetooth device. See `is_bluetooth_device_name` for the matching rules.
pub fn is_bluetooth_output_device(device_name: &str) -> bool {
    is_bluetooth_device_name(device_name)
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
        if let Some(name) = output_device_name(&default_device) {
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

/// Returns true if the system default input device is a Bluetooth device,
/// or if the given selected input device is Bluetooth.
///
/// A held Bluetooth *input* stream is what makes macOS reroute AirPods/Beats
/// stem presses as mute commands aimed at this app (the "Cannot Control Mic"
/// audioaccessord banner, upstream cjpais/Handy#2045) and locks the headset
/// in HFP call quality — so callers must never hold one open (see
/// research/airpods-cannot-control-mic-alert-hfp-mechanics.md).
pub fn is_bluetooth_input_active(selected_input: Option<&str>) -> bool {
    let host = crate::audio_toolkit::get_cpal_host();

    // Check the explicitly selected input device first
    if let Some(name) = selected_input {
        if name != "Default" && is_bluetooth_output_device(name) {
            log::debug!(
                "Bluetooth input device detected: '{}', will not hold mic stream open",
                name
            );
            return true;
        }
    }

    // Fall back to checking the system default input device
    if let Some(default_device) = host.default_input_device() {
        if let Some(name) = input_device_name(&default_device) {
            if is_bluetooth_output_device(&name) {
                log::debug!(
                    "Bluetooth default input device detected: '{}', will not hold mic stream open",
                    name
                );
                return true;
            }
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bluetooth_device_names_match_positive() {
        for name in [
            "AirPods Pro",
            "WH-1000XM4",
            "Beats Solo 3",
            "JBL Live",
            "Galaxy Buds2",
        ] {
            assert!(
                is_bluetooth_device_name(name),
                "expected '{name}' to be detected as Bluetooth"
            );
        }
    }

    #[test]
    fn bluetooth_device_names_match_negative() {
        for name in [
            "",
            "Default",
            "Fifine USB Mic",
            "MacBook Pro Microphone",
            "USB Audio CODEC",
        ] {
            assert!(
                !is_bluetooth_device_name(name),
                "expected '{name}' NOT to be detected as Bluetooth"
            );
        }
    }

    #[test]
    fn output_device_check_delegates_to_name_check() {
        assert!(is_bluetooth_output_device("AirPods Pro"));
        assert!(!is_bluetooth_output_device("USB Audio CODEC"));
        assert_eq!(
            is_bluetooth_output_device("JBL Live"),
            is_bluetooth_device_name("JBL Live")
        );
    }

    #[test]
    fn bluetooth_input_names_match_by_name() {
        // The name-classification part of `is_bluetooth_input_active` (the
        // host default-input fallback is not unit-testable). Positive names
        // return true before any host query runs; a non-Bluetooth selected
        // name falls through to the host query, so for it only the shared
        // classifier itself is asserted.
        assert!(is_bluetooth_input_active(Some("Pamela's AirPods Pro")));
        assert!(is_bluetooth_input_active(Some("WH-1000XM5 Microphone")));
        assert!(!is_bluetooth_output_device("MacBook Pro Microphone"));
    }

    #[test]
    fn default_input_name_does_not_short_circuit_to_bluetooth() {
        // `Some("Default")` must NOT short-circuit to true: it skips the
        // selected-name check (`!= "Default"`) and falls through to the
        // host default-input query, whose result the name classification
        // alone cannot influence.
        assert!(!is_bluetooth_output_device("Default"));
    }
}
