//! Property-based tests for serde round-trip invariants of settings enums.
//!
//! For each enum in `settings/types.rs` that derives `Serialize + Deserialize`,
//! verifies that `serde_json::from_str(&serde_json::to_string(&x).unwrap()).unwrap() == x`.

use handy_app_lib::audio_toolkit::SpellingDictionary;
use handy_app_lib::settings::{
    AutoSubmitKey, ClipboardHandling, KeyboardImplementation, ModelUnloadTimeout,
    NoiseSuppressionLevel, OrtAcceleratorSetting, OverlayPosition, OverlayScreenTarget,
    PasteMethod, RecordingRetentionPeriod, SoundTheme, TypingTool, VadSensitivity,
    WhisperAcceleratorSetting, WordCorrectionMode,
};
use proptest::prelude::*;

// ─── Strategy helpers ───────────────────────────────────────────────────

fn log_level() -> impl Strategy<Value = handy_app_lib::settings::LogLevel> {
    use handy_app_lib::settings::LogLevel;
    prop_oneof![
        Just(LogLevel::Trace),
        Just(LogLevel::Debug),
        Just(LogLevel::Info),
        Just(LogLevel::Warn),
        Just(LogLevel::Error),
    ]
}

fn overlay_position() -> impl Strategy<Value = OverlayPosition> {
    prop_oneof![
        Just(OverlayPosition::None),
        Just(OverlayPosition::Top),
        Just(OverlayPosition::Bottom),
    ]
}

fn overlay_screen_target() -> impl Strategy<Value = OverlayScreenTarget> {
    prop_oneof![
        Just(OverlayScreenTarget::Cursor),
        Just(OverlayScreenTarget::SideScreen),
    ]
}

fn model_unload_timeout() -> impl Strategy<Value = ModelUnloadTimeout> {
    prop_oneof![
        Just(ModelUnloadTimeout::Never),
        Just(ModelUnloadTimeout::Immediately),
        Just(ModelUnloadTimeout::Min2),
        Just(ModelUnloadTimeout::Min5),
        Just(ModelUnloadTimeout::Min10),
        Just(ModelUnloadTimeout::Min15),
        Just(ModelUnloadTimeout::Hour1),
        Just(ModelUnloadTimeout::Sec15),
    ]
}

fn paste_method() -> impl Strategy<Value = PasteMethod> {
    prop_oneof![
        Just(PasteMethod::CtrlV),
        Just(PasteMethod::Direct),
        Just(PasteMethod::None),
        Just(PasteMethod::ShiftInsert),
        Just(PasteMethod::CtrlShiftV),
        Just(PasteMethod::ExternalScript),
    ]
}

fn clipboard_handling() -> impl Strategy<Value = ClipboardHandling> {
    prop_oneof![
        Just(ClipboardHandling::DontModify),
        Just(ClipboardHandling::CopyToClipboard),
    ]
}

fn auto_submit_key() -> impl Strategy<Value = AutoSubmitKey> {
    prop_oneof![
        Just(AutoSubmitKey::Enter),
        Just(AutoSubmitKey::CtrlEnter),
        Just(AutoSubmitKey::CmdEnter),
    ]
}

fn recording_retention_period() -> impl Strategy<Value = RecordingRetentionPeriod> {
    prop_oneof![
        Just(RecordingRetentionPeriod::Never),
        Just(RecordingRetentionPeriod::PreserveLimit),
        Just(RecordingRetentionPeriod::Days3),
        Just(RecordingRetentionPeriod::Weeks2),
        Just(RecordingRetentionPeriod::Months3),
    ]
}

fn noise_suppression_level() -> impl Strategy<Value = NoiseSuppressionLevel> {
    prop_oneof![
        Just(NoiseSuppressionLevel::Low),
        Just(NoiseSuppressionLevel::Medium),
        Just(NoiseSuppressionLevel::High),
    ]
}

fn vad_sensitivity() -> impl Strategy<Value = VadSensitivity> {
    prop_oneof![
        Just(VadSensitivity::VeryQuick),
        Just(VadSensitivity::Quick),
        Just(VadSensitivity::Balanced),
        Just(VadSensitivity::Relaxed),
        Just(VadSensitivity::VeryRelaxed),
    ]
}

fn sound_theme() -> impl Strategy<Value = SoundTheme> {
    prop_oneof![
        Just(SoundTheme::Marimba),
        Just(SoundTheme::Pop),
        Just(SoundTheme::Custom),
    ]
}

fn keyboard_implementation() -> impl Strategy<Value = KeyboardImplementation> {
    prop_oneof![
        Just(KeyboardImplementation::Tauri),
        Just(KeyboardImplementation::HandyKeys),
    ]
}

fn word_correction_mode() -> impl Strategy<Value = WordCorrectionMode> {
    prop_oneof![
        Just(WordCorrectionMode::WordBias),
        Just(WordCorrectionMode::Pronunciation),
        Just(WordCorrectionMode::Replacement),
    ]
}

fn typing_tool() -> impl Strategy<Value = TypingTool> {
    prop_oneof![
        Just(TypingTool::Auto),
        Just(TypingTool::Wtype),
        Just(TypingTool::Kwtype),
        Just(TypingTool::Dotool),
        Just(TypingTool::Ydotool),
        Just(TypingTool::Xdotool),
    ]
}

fn whisper_accelerator_setting() -> impl Strategy<Value = WhisperAcceleratorSetting> {
    prop_oneof![
        Just(WhisperAcceleratorSetting::Auto),
        Just(WhisperAcceleratorSetting::Cpu),
        Just(WhisperAcceleratorSetting::Gpu),
    ]
}

fn ort_accelerator_setting() -> impl Strategy<Value = OrtAcceleratorSetting> {
    prop_oneof![
        Just(OrtAcceleratorSetting::Auto),
        Just(OrtAcceleratorSetting::Cpu),
        Just(OrtAcceleratorSetting::Cuda),
        Just(OrtAcceleratorSetting::DirectMl),
        Just(OrtAcceleratorSetting::Rocm),
    ]
}

fn spelling_dictionary() -> impl Strategy<Value = SpellingDictionary> {
    prop_oneof![
        Just(SpellingDictionary::Dwyl),
        Just(SpellingDictionary::Cspell),
    ]
}

// ─── Round-trip tests ───────────────────────────────────────────────────

proptest! {
    #[test]
    fn proptest_serde_roundtrip_log_level(v in log_level()) {
        let json = serde_json::to_string(&v).unwrap();
        let back: handy_app_lib::settings::LogLevel = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(back, v);
    }

    #[test]
    fn proptest_serde_roundtrip_overlay_position(v in overlay_position()) {
        let json = serde_json::to_string(&v).unwrap();
        let back: OverlayPosition = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(back, v);
    }

    #[test]
    fn proptest_serde_roundtrip_overlay_screen_target(v in overlay_screen_target()) {
        let json = serde_json::to_string(&v).unwrap();
        let back: OverlayScreenTarget = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(back, v);
    }

    #[test]
    fn proptest_serde_roundtrip_model_unload_timeout(v in model_unload_timeout()) {
        let json = serde_json::to_string(&v).unwrap();
        let back: ModelUnloadTimeout = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(back, v);
    }

    #[test]
    fn proptest_serde_roundtrip_paste_method(v in paste_method()) {
        let json = serde_json::to_string(&v).unwrap();
        let back: PasteMethod = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(back, v);
    }

    #[test]
    fn proptest_serde_roundtrip_clipboard_handling(v in clipboard_handling()) {
        let json = serde_json::to_string(&v).unwrap();
        let back: ClipboardHandling = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(back, v);
    }

    #[test]
    fn proptest_serde_roundtrip_auto_submit_key(v in auto_submit_key()) {
        let json = serde_json::to_string(&v).unwrap();
        let back: AutoSubmitKey = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(back, v);
    }

    #[test]
    fn proptest_serde_roundtrip_recording_retention_period(v in recording_retention_period()) {
        let json = serde_json::to_string(&v).unwrap();
        let back: RecordingRetentionPeriod = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(back, v);
    }

    #[test]
    fn proptest_serde_roundtrip_noise_suppression_level(v in noise_suppression_level()) {
        let json = serde_json::to_string(&v).unwrap();
        let back: NoiseSuppressionLevel = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(back, v);
    }

    #[test]
    fn proptest_serde_roundtrip_vad_sensitivity(v in vad_sensitivity()) {
        let json = serde_json::to_string(&v).unwrap();
        let back: VadSensitivity = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(back, v);
    }

    #[test]
    fn proptest_serde_roundtrip_sound_theme(v in sound_theme()) {
        let json = serde_json::to_string(&v).unwrap();
        let back: SoundTheme = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(back, v);
    }

    #[test]
    fn proptest_serde_roundtrip_keyboard_implementation(v in keyboard_implementation()) {
        let json = serde_json::to_string(&v).unwrap();
        let back: KeyboardImplementation = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(back, v);
    }

    #[test]
    fn proptest_serde_roundtrip_word_correction_mode(v in word_correction_mode()) {
        let json = serde_json::to_string(&v).unwrap();
        let back: WordCorrectionMode = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(back, v);
    }

    #[test]
    fn proptest_serde_roundtrip_typing_tool(v in typing_tool()) {
        let json = serde_json::to_string(&v).unwrap();
        let back: TypingTool = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(back, v);
    }

    #[test]
    fn proptest_serde_roundtrip_whisper_accelerator_setting(v in whisper_accelerator_setting()) {
        let json = serde_json::to_string(&v).unwrap();
        let back: WhisperAcceleratorSetting = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(back, v);
    }

    #[test]
    fn proptest_serde_roundtrip_ort_accelerator_setting(v in ort_accelerator_setting()) {
        let json = serde_json::to_string(&v).unwrap();
        let back: OrtAcceleratorSetting = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(back, v);
    }

    #[test]
    fn proptest_serde_roundtrip_spelling_dictionary(v in spelling_dictionary()) {
        let json = serde_json::to_string(&v).unwrap();
        let back: SpellingDictionary = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(back, v);
    }
}
