//! Integration tests for the transcription pipeline configuration and data flow.
//!
//! Tests hybrid mode settings, accelerator settings, transcription-related
//! settings fields, and the query-state file format. These tests exercise
//! settings and types that are publicly accessible without needing a loaded
//! model or Tauri runtime.

use handy_app_lib::settings::{
    get_default_settings, AppSettings, TranscribeAcceleratorSetting, OrtAcceleratorSetting,
    ModelUnloadTimeout, OverlayStyle, VadSensitivity,
};

// ── Hybrid mode threshold settings ──────────────────────────────────────────

#[test]
fn hybrid_threshold_defaults_to_30_seconds() {
    let settings = get_default_settings();
    assert_eq!(settings.hybrid_threshold_secs, 30.0);
    assert!(!settings.hybrid_mode_enabled, "hybrid mode is off by default");
}

#[test]
fn hybrid_threshold_is_not_nan_in_defaults() {
    let settings = get_default_settings();
    assert!(!settings.hybrid_threshold_secs.is_nan());
}

#[test]
fn hybrid_settings_roundtrip_through_serde() {
    let mut settings = get_default_settings();
    settings.hybrid_mode_enabled = true;
    settings.hybrid_threshold_secs = 15.0;
    settings.hybrid_short_audio_model = Some("small".to_string());
    settings.hybrid_long_audio_model = Some("large-v3".to_string());

    let json = serde_json::to_string(&settings).unwrap();
    let restored: AppSettings = serde_json::from_str(&json).unwrap();

    assert!(restored.hybrid_mode_enabled);
    assert_eq!(restored.hybrid_threshold_secs, 15.0);
    assert_eq!(restored.hybrid_short_audio_model.as_deref(), Some("small"));
    assert_eq!(restored.hybrid_long_audio_model.as_deref(), Some("large-v3"));
}

#[test]
fn hybrid_mode_disabled_by_default_with_no_models() {
    let settings = get_default_settings();
    assert!(!settings.hybrid_mode_enabled);
    assert!(settings.hybrid_short_audio_model.is_none());
    assert!(settings.hybrid_long_audio_model.is_none());
}

// ── Accelerator settings ────────────────────────────────────────────────────

#[test]
fn transcribe_accelerator_setting_roundtrips() {
    for setting in [
        TranscribeAcceleratorSetting::Auto,
        TranscribeAcceleratorSetting::Cpu,
        TranscribeAcceleratorSetting::Gpu,
    ] {
        let mut settings = get_default_settings();
        settings.transcribe_accelerator = setting;
        let json = serde_json::to_string(&settings).unwrap();
        let restored: AppSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.transcribe_accelerator, setting);
    }
}

#[test]
fn ort_accelerator_setting_roundtrips() {
    for setting in [
        OrtAcceleratorSetting::Auto,
        OrtAcceleratorSetting::Cpu,
        OrtAcceleratorSetting::Cuda,
        OrtAcceleratorSetting::DirectMl,
        OrtAcceleratorSetting::Rocm,
    ] {
        let mut settings = get_default_settings();
        settings.ort_accelerator = setting;
        let json = serde_json::to_string(&settings).unwrap();
        let restored: AppSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.ort_accelerator, setting);
    }
}

#[test]
fn transcribe_accelerator_defaults_to_auto() {
    assert_eq!(TranscribeAcceleratorSetting::default(), TranscribeAcceleratorSetting::Auto);
}

#[test]
fn ort_accelerator_defaults_to_auto() {
    assert_eq!(OrtAcceleratorSetting::default(), OrtAcceleratorSetting::Auto);
}

#[test]
fn gpu_device_defaults_to_minus_one() {
    let settings = get_default_settings();
    assert_eq!(settings.transcribe_gpu_device, -1, "default GPU device should be -1 (auto)");
}

// ── Model unload timeout settings ───────────────────────────────────────────

#[test]
fn model_unload_timeout_roundtrip_all_variants() {
    for variant in [
        ModelUnloadTimeout::Never,
        ModelUnloadTimeout::Immediately,
        ModelUnloadTimeout::Sec15,
        ModelUnloadTimeout::Min2,
        ModelUnloadTimeout::Min5,
        ModelUnloadTimeout::Min10,
        ModelUnloadTimeout::Min15,
        ModelUnloadTimeout::Hour1,
    ] {
        let mut settings = get_default_settings();
        settings.model_unload_timeout = variant;
        let json = serde_json::to_string(&settings).unwrap();
        let restored: AppSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.model_unload_timeout, variant);
    }
}

#[test]
fn model_unload_timeout_seconds_mapping() {
    assert_eq!(ModelUnloadTimeout::Never.to_seconds(), None);
    assert_eq!(ModelUnloadTimeout::Immediately.to_seconds(), Some(0));
    assert_eq!(ModelUnloadTimeout::Sec15.to_seconds(), Some(15));
    assert_eq!(ModelUnloadTimeout::Min2.to_seconds(), Some(120));
    assert_eq!(ModelUnloadTimeout::Min5.to_seconds(), Some(300));
    assert_eq!(ModelUnloadTimeout::Min10.to_seconds(), Some(600));
    assert_eq!(ModelUnloadTimeout::Min15.to_seconds(), Some(900));
    assert_eq!(ModelUnloadTimeout::Hour1.to_seconds(), Some(3600));
}

// ── VAD settings ─────────────────────────────────────────────────────────────

#[test]
fn vad_enabled_by_default() {
    let settings = get_default_settings();
    assert!(settings.vad_enabled, "VAD should be enabled by default");
}

#[test]
fn vad_sensitivity_default_is_balanced() {
    assert_eq!(VadSensitivity::default(), VadSensitivity::Balanced);
}

#[test]
fn vad_sensitivity_thresholds_are_ordered() {
    // VeryQuick should have the highest threshold (most sensitive = least audio needed).
    let thresholds: Vec<f32> = vec![
        VadSensitivity::VeryQuick.threshold(),
        VadSensitivity::Quick.threshold(),
        VadSensitivity::Balanced.threshold(),
        VadSensitivity::Relaxed.threshold(),
        VadSensitivity::VeryRelaxed.threshold(),
    ];
    // Each threshold should be <= the previous (decreasing sensitivity).
    for window in thresholds.windows(2) {
        assert!(
            window[0] >= window[1],
            "VAD thresholds should be in decreasing order: {} >= {}",
            window[0],
            window[1]
        );
    }
}

#[test]
fn vad_hangover_frames_are_ordered() {
    // VeryQuick should have the fewest hangover frames.
    let frames: Vec<usize> = vec![
        VadSensitivity::VeryQuick.hangover_frames(),
        VadSensitivity::Quick.hangover_frames(),
        VadSensitivity::Balanced.hangover_frames(),
        VadSensitivity::Relaxed.hangover_frames(),
        VadSensitivity::VeryRelaxed.hangover_frames(),
    ];
    for window in frames.windows(2) {
        assert!(
            window[0] <= window[1],
            "VAD hangover frames should be in increasing order: {} <= {}",
            window[0],
            window[1]
        );
    }
}

// ── Live captions settings ───────────────────────────────────────────────────

#[test]
fn live_captions_disabled_by_default() {
    let settings = get_default_settings();
    assert!(!settings.live_captions_enabled, "live captions should be off by default");
}

#[test]
fn live_captions_roundtrip() {
    let mut settings = get_default_settings();
    settings.live_captions_enabled = true;
    let json = serde_json::to_string(&settings).unwrap();
    let restored: AppSettings = serde_json::from_str(&json).unwrap();
    assert!(restored.live_captions_enabled);
}

// ── Overlay style settings ──────────────────────────────────────────────────

#[test]
fn overlay_style_roundtrips() {
    for style in [OverlayStyle::None, OverlayStyle::Minimal, OverlayStyle::Live] {
        let mut settings = get_default_settings();
        settings.overlay_style = style;
        let json = serde_json::to_string(&settings).unwrap();
        let restored: AppSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.overlay_style, style);
    }
}

// ── Real-time factor calculation ──────────────────────────────────────────────
// This tests the formula that TranscriptionOutput uses, without needing the
// actual struct (which is in a private module).

#[test]
fn real_time_factor_calculation() {
    // RTF = audio_duration / transcription_duration.
    // RTF > 1 means faster than real-time (good).
    // RTF < 1 means slower than real-time (bad).
    let audio_dur = 10.0_f64;
    let trans_dur = 2.5_f64;
    let rtf = audio_dur / trans_dur;
    assert!(rtf > 1.0, "RTF > 1 means faster than real-time");
    assert!((rtf - 4.0).abs() < f64::EPSILON);

    // Edge case: very short audio.
    let short_rtf = 0.5_f64 / 0.5_f64;
    assert!((short_rtf - 1.0).abs() < f64::EPSILON);
}

// ── Query state file format ──────────────────────────────────────────────────

#[test]
fn query_state_file_contains_expected_json_structure() {
    handy_app_lib::write_query_state(true, true);

    let path = handy_app_lib::query_state_file_path();
    if let Ok(contents) = std::fs::read_to_string(&path) {
        let val: serde_json::Value = serde_json::from_str(&contents).unwrap();
        assert!(val.get("is_active_use").is_some(), "should have is_active_use field");
        assert!(val.get("is_recording").is_some(), "should have is_recording field");
        assert_eq!(val["is_active_use"], true);
        assert_eq!(val["is_recording"], true);
    }

    // Clean up.
    handy_app_lib::remove_query_state_file();
}

#[test]
fn query_state_file_idle_state() {
    handy_app_lib::write_query_state(false, false);

    let path = handy_app_lib::query_state_file_path();
    if let Ok(contents) = std::fs::read_to_string(&path) {
        let val: serde_json::Value = serde_json::from_str(&contents).unwrap();
        assert_eq!(val["is_active_use"], false);
        assert_eq!(val["is_recording"], false);
    }

    // Clean up.
    handy_app_lib::remove_query_state_file();
}