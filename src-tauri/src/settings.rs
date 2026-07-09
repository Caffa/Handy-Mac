use log::{debug, error, warn};
use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use specta::Type;
use std::collections::HashMap;
use std::fmt;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;
use tauri::{AppHandle, Manager};
use tauri_plugin_store::StoreExt;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use crate::audio_toolkit::SpellingDictionary;

pub const APPLE_INTELLIGENCE_PROVIDER_ID: &str = "apple_intelligence";
pub const APPLE_INTELLIGENCE_DEFAULT_MODEL_ID: &str = "Apple Intelligence";

#[derive(Serialize, Debug, Clone, Copy, PartialEq, Eq, Type)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

// Custom deserializer to handle both old numeric format (1-5) and new string format ("trace", "debug", etc.)
impl<'de> Deserialize<'de> for LogLevel {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct LogLevelVisitor;

        impl<'de> Visitor<'de> for LogLevelVisitor {
            type Value = LogLevel;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a string or integer representing log level")
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<LogLevel, E> {
                match value.to_lowercase().as_str() {
                    "trace" => Ok(LogLevel::Trace),
                    "debug" => Ok(LogLevel::Debug),
                    "info" => Ok(LogLevel::Info),
                    "warn" => Ok(LogLevel::Warn),
                    "error" => Ok(LogLevel::Error),
                    _ => Err(E::unknown_variant(
                        value,
                        &["trace", "debug", "info", "warn", "error"],
                    )),
                }
            }

            fn visit_u64<E: de::Error>(self, value: u64) -> Result<LogLevel, E> {
                match value {
                    1 => Ok(LogLevel::Trace),
                    2 => Ok(LogLevel::Debug),
                    3 => Ok(LogLevel::Info),
                    4 => Ok(LogLevel::Warn),
                    5 => Ok(LogLevel::Error),
                    _ => Err(E::invalid_value(de::Unexpected::Unsigned(value), &"1-5")),
                }
            }
        }

        deserializer.deserialize_any(LogLevelVisitor)
    }
}

impl From<LogLevel> for tauri_plugin_log::LogLevel {
    fn from(level: LogLevel) -> Self {
        match level {
            LogLevel::Trace => tauri_plugin_log::LogLevel::Trace,
            LogLevel::Debug => tauri_plugin_log::LogLevel::Debug,
            LogLevel::Info => tauri_plugin_log::LogLevel::Info,
            LogLevel::Warn => tauri_plugin_log::LogLevel::Warn,
            LogLevel::Error => tauri_plugin_log::LogLevel::Error,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct ShortcutBinding {
    pub id: String,
    pub name: String,
    pub description: String,
    pub default_binding: String,
    pub current_binding: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct LLMPrompt {
    pub id: String,
    pub name: String,
    pub prompt: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct PostProcessProvider {
    pub id: String,
    pub label: String,
    pub base_url: String,
    #[serde(default)]
    pub allow_base_url_edit: bool,
    #[serde(default)]
    pub models_endpoint: Option<String>,
    #[serde(default)]
    pub supports_structured_output: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Type)]
#[serde(rename_all = "lowercase")]
pub enum OverlayPosition {
    None,
    Top,
    Bottom,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Type)]
#[serde(rename_all = "snake_case")]
pub enum OverlayScreenTarget {
    Cursor,
    SideScreen,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Type)]
#[serde(rename_all = "snake_case")]
pub enum ModelUnloadTimeout {
    Never,
    Immediately,
    Min2,
    Min5,
    Min10,
    Min15,
    Hour1,
    Sec15, // Debug mode only
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Type)]
#[serde(rename_all = "snake_case")]
pub enum PasteMethod {
    CtrlV,
    Direct,
    None,
    ShiftInsert,
    CtrlShiftV,
    ExternalScript,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Type)]
#[serde(rename_all = "snake_case")]
pub enum ClipboardHandling {
    DontModify,
    CopyToClipboard,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Type)]
#[serde(rename_all = "snake_case")]
pub enum AutoSubmitKey {
    Enter,
    CtrlEnter,
    CmdEnter,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Type)]
#[serde(rename_all = "snake_case")]
pub enum RecordingRetentionPeriod {
    Never,
    PreserveLimit,
    Days3,
    Weeks2,
    Months3,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Type)]
#[serde(rename_all = "snake_case")]
pub enum NoiseSuppressionLevel {
    Low,
    Medium,
    High,
}

impl Default for NoiseSuppressionLevel {
    fn default() -> Self {
        NoiseSuppressionLevel::Medium
    }
}

impl NoiseSuppressionLevel {
    pub fn display_name(&self) -> &'static str {
        match self {
            NoiseSuppressionLevel::Low => "Low",
            NoiseSuppressionLevel::Medium => "Medium",
            NoiseSuppressionLevel::High => "High",
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Type)]
#[serde(rename_all = "snake_case")]
pub enum VadSensitivity {
    VeryQuick,
    Quick,
    Balanced,
    Relaxed,
    VeryRelaxed,
}

impl Default for VadSensitivity {
    fn default() -> Self {
        VadSensitivity::Balanced
    }
}

impl VadSensitivity {
    pub fn threshold(&self) -> f32 {
        match self {
            VadSensitivity::VeryQuick => 0.45,
            VadSensitivity::Quick => 0.38,
            VadSensitivity::Balanced => 0.30,
            VadSensitivity::Relaxed => 0.25,
            VadSensitivity::VeryRelaxed => 0.20,
        }
    }

    pub fn hangover_frames(&self) -> usize {
        match self {
            VadSensitivity::VeryQuick => 8,
            VadSensitivity::Quick => 12,
            VadSensitivity::Balanced => 15,
            VadSensitivity::Relaxed => 20,
            VadSensitivity::VeryRelaxed => 30,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Type)]
#[serde(rename_all = "snake_case")]
pub enum KeyboardImplementation {
    Tauri,
    HandyKeys,
}

impl Default for KeyboardImplementation {
    fn default() -> Self {
        #[cfg(target_os = "linux")]
        return KeyboardImplementation::Tauri;
        #[cfg(not(target_os = "linux"))]
        return KeyboardImplementation::HandyKeys;
    }
}

impl Default for ModelUnloadTimeout {
    fn default() -> Self {
        ModelUnloadTimeout::Min5
    }
}

impl Default for PasteMethod {
    fn default() -> Self {
        // Default to CtrlV for macOS and Windows, Direct for Linux
        #[cfg(target_os = "linux")]
        return PasteMethod::Direct;
        #[cfg(not(target_os = "linux"))]
        return PasteMethod::CtrlV;
    }
}

impl Default for ClipboardHandling {
    fn default() -> Self {
        ClipboardHandling::DontModify
    }
}

impl Default for AutoSubmitKey {
    fn default() -> Self {
        AutoSubmitKey::Enter
    }
}

impl ModelUnloadTimeout {
    pub fn to_minutes(self) -> Option<u64> {
        match self {
            ModelUnloadTimeout::Never => None,
            ModelUnloadTimeout::Immediately => Some(0), // Special case for immediate unloading
            ModelUnloadTimeout::Min2 => Some(2),
            ModelUnloadTimeout::Min5 => Some(5),
            ModelUnloadTimeout::Min10 => Some(10),
            ModelUnloadTimeout::Min15 => Some(15),
            ModelUnloadTimeout::Hour1 => Some(60),
            ModelUnloadTimeout::Sec15 => Some(0), // Special case for debug - handled separately
        }
    }

    pub fn to_seconds(self) -> Option<u64> {
        match self {
            ModelUnloadTimeout::Never => None,
            ModelUnloadTimeout::Immediately => Some(0), // Special case for immediate unloading
            ModelUnloadTimeout::Sec15 => Some(15),
            _ => self.to_minutes().map(|m| m * 60),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Type)]
#[serde(rename_all = "snake_case")]
pub enum SoundTheme {
    Marimba,
    Pop,
    Custom,
}

impl SoundTheme {
    fn as_str(&self) -> &'static str {
        match self {
            SoundTheme::Marimba => "marimba",
            SoundTheme::Pop => "pop",
            SoundTheme::Custom => "custom",
        }
    }

    pub fn to_start_path(&self) -> String {
        format!("resources/{}_start.wav", self.as_str())
    }

    pub fn to_stop_path(&self) -> String {
        format!("resources/{}_stop.wav", self.as_str())
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Type)]
#[serde(rename_all = "snake_case")]
pub enum TypingTool {
    Auto,
    Wtype,
    Kwtype,
    Dotool,
    Ydotool,
    Xdotool,
}

impl Default for TypingTool {
    fn default() -> Self {
        TypingTool::Auto
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Type)]
#[serde(rename_all = "snake_case")]
pub enum WhisperAcceleratorSetting {
    Auto,
    Cpu,
    Gpu,
}

impl Default for WhisperAcceleratorSetting {
    fn default() -> Self {
        WhisperAcceleratorSetting::Auto
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Type)]
#[serde(rename_all = "snake_case")]
pub enum OrtAcceleratorSetting {
    Auto,
    Cpu,
    Cuda,
    #[serde(rename = "directml")]
    DirectMl,
    Rocm,
}

impl Default for OrtAcceleratorSetting {
    fn default() -> Self {
        OrtAcceleratorSetting::Auto
    }
}

/// A custom word with optional pronunciation variants for advanced fuzzy matching.
///
/// When `pronunciations` is non-empty, the matching algorithm will also compare
/// transcription n-grams against each pronunciation variant. Any match replaces
/// the transcript text with the canonical `word`.
///
/// Example: `CustomWord { word: "ChargeBee", pronunciations: vec!["charge b", "charge bee"] }`
/// will match "charge b" or "charge bee" in the transcript and replace it with "ChargeBee".
#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct CustomWord {
    pub word: String,
    #[serde(default)]
    pub pronunciations: Vec<String>,
}

/// A word replacement rule for exact word-to-word substitution.
///
/// This is a simpler mode than pronunciation matching: when the `mistranslation`
/// appears in the transcript (respecting word boundaries), it is replaced with
/// the `correction`.
///
/// Example: `WordReplacement { mistranslation: "open a i", correction: "OpenAI" }`
/// will replace "open a i" with "OpenAI" in the transcript.
#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct WordReplacement {
    /// The word or phrase that appears incorrectly in transcripts
    pub mistranslation: String,
    /// The correct word or phrase to replace it with
    pub correction: String,
}

/// Word correction mode selection.
///
/// Determines which word correction algorithm to apply:
/// - `WordBias` ("Prefer Custom Words"): Simple word bias using fuzzy matching (Levenshtein + Soundex)
/// - `Pronunciation` ("Match Pronunciations"): Advanced matching with pronunciation variants
/// - `Replacement` ("Exact Replacements"): Direct word-to-word substitution with exact matching
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Type, Default)]
#[serde(rename_all = "snake_case")]
pub enum WordCorrectionMode {
    #[default]
    WordBias,
    Pronunciation,
    Replacement,
}

impl WordCorrectionMode {
    /// Returns a user-friendly display name for the mode.
    pub fn display_name(&self) -> &'static str {
        match self {
            WordCorrectionMode::WordBias => "Prefer Custom Words",
            WordCorrectionMode::Pronunciation => "Match Pronunciations",
            WordCorrectionMode::Replacement => "Exact Replacements",
        }
    }
}

#[derive(Clone, Serialize, Deserialize, Type)]
#[serde(transparent)]
pub(crate) struct SecretMap(HashMap<String, String>);

impl fmt::Debug for SecretMap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let redacted: HashMap<&String, &str> = self
            .0
            .iter()
            .map(|(k, v)| (k, if v.is_empty() { "" } else { "[REDACTED]" }))
            .collect();
        redacted.fmt(f)
    }
}

impl std::ops::Deref for SecretMap {
    type Target = HashMap<String, String>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for SecretMap {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

/* still handy for composing the initial JSON in the store ------------- */
#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct AppSettings {
    pub bindings: HashMap<String, ShortcutBinding>,
    pub push_to_talk: bool,
    pub audio_feedback: bool,
    #[serde(default = "default_audio_feedback_volume")]
    pub audio_feedback_volume: f32,
    #[serde(default = "default_sound_theme")]
    pub sound_theme: SoundTheme,
    #[serde(default = "default_start_hidden")]
    pub start_hidden: bool,
    #[serde(default = "default_autostart_enabled")]
    pub autostart_enabled: bool,
    #[serde(default = "default_update_checks_enabled")]
    pub update_checks_enabled: bool,
    #[serde(default = "default_model")]
    pub selected_model: String,
    #[serde(default = "default_always_on_microphone")]
    pub always_on_microphone: bool,
    #[serde(default)]
    pub selected_microphone: Option<String>,
    #[serde(default)]
    pub clamshell_microphone: Option<String>,
    #[serde(default)]
    pub selected_output_device: Option<String>,
    #[serde(default = "default_translate_to_english")]
    pub translate_to_english: bool,
    #[serde(default = "default_app_language")]
    pub selected_language: String,
    #[serde(default = "default_overlay_position")]
    pub overlay_position: OverlayPosition,
    #[serde(default = "default_overlay_screen_target")]
    pub overlay_screen_target: OverlayScreenTarget,
    #[serde(default = "default_debug_mode")]
    pub debug_mode: bool,
    #[serde(default = "default_log_level")]
    pub log_level: LogLevel,
    #[serde(default)]
    pub custom_words: Vec<String>,
    #[serde(default)]
    pub advanced_custom_words: Vec<CustomWord>,
    #[serde(default)]
    pub word_replacements: Vec<WordReplacement>,
    /// Deprecated: Use `word_correction_mode` instead.
    /// Kept for backward compatibility during migration.
    #[serde(default)]
    pub use_advanced_custom_words: bool,
    /// Word correction mode. Defaults to WordBias for backward compatibility.
    /// Migrated from `use_advanced_custom_words` if that field is true.
    #[serde(default)]
    pub word_correction_mode: WordCorrectionMode,
    #[serde(default)]
    pub model_unload_timeout: ModelUnloadTimeout,
    #[serde(default = "default_word_correction_threshold")]
    pub word_correction_threshold: f64,
    #[serde(default = "default_history_limit")]
    pub history_limit: usize,
    #[serde(default = "default_recording_retention_period")]
    pub recording_retention_period: RecordingRetentionPeriod,
    #[serde(default)]
    pub paste_method: PasteMethod,
    #[serde(default)]
    pub clipboard_handling: ClipboardHandling,
    #[serde(default = "default_auto_submit")]
    pub auto_submit: bool,
    #[serde(default)]
    pub auto_submit_key: AutoSubmitKey,
    #[serde(default = "default_post_process_enabled")]
    pub post_process_enabled: bool,
    #[serde(default = "default_post_process_provider_id")]
    pub post_process_provider_id: String,
    #[serde(default = "default_post_process_providers")]
    pub post_process_providers: Vec<PostProcessProvider>,
    #[serde(default = "default_post_process_api_keys")]
    pub post_process_api_keys: SecretMap,
    #[serde(default = "default_post_process_models")]
    pub post_process_models: HashMap<String, String>,
    #[serde(default = "default_post_process_prompts")]
    pub post_process_prompts: Vec<LLMPrompt>,
    #[serde(default)]
    pub post_process_selected_prompt_id: Option<String>,
    #[serde(default)]
    pub mute_while_recording: bool,
    #[serde(default)]
    pub append_trailing_space: bool,
    #[serde(default = "default_app_language")]
    pub app_language: String,
    #[serde(default)]
    pub experimental_enabled: bool,
    #[serde(default)]
    pub lazy_stream_close: bool,
    #[serde(default)]
    pub keyboard_implementation: KeyboardImplementation,
    #[serde(default = "default_show_tray_icon")]
    pub show_tray_icon: bool,
    #[serde(default = "default_paste_delay_ms")]
    pub paste_delay_ms: u64,
    #[serde(default = "default_typing_tool")]
    pub typing_tool: TypingTool,
    pub external_script_path: Option<String>,
    /// Path to boss_router.py for the "Transcribe with Router" action.
    /// Set to Some(path) to enable router integration.
    #[serde(default)]
    pub router_script_path: Option<String>,
    /// Path to a .env file containing env vars (e.g. TELEGRAM_DAILY_LOG_BOT)
    /// to pass to the router script subprocess.
    #[serde(default)]
    pub router_env_file: Option<String>,
    #[serde(default)]
    pub custom_filler_words: Option<Vec<String>>,
    #[serde(default)]
    pub whisper_accelerator: WhisperAcceleratorSetting,
    #[serde(default)]
    pub ort_accelerator: OrtAcceleratorSetting,
    #[serde(default = "default_whisper_gpu_device")]
    pub whisper_gpu_device: i32,
    #[serde(default)]
    pub extra_recording_buffer_ms: u64,
    /// Pre-recording buffer in milliseconds for always-on microphone mode.
    /// Captures audio from the last N ms before the hotkey is pressed.
    /// Useful for catching the beginning of speech that starts before pressing the button.
    /// Default: 0 (disabled). Recommended: 1000-3000ms.
    #[serde(default)]
    pub pre_recording_buffer_ms: u64,
    #[serde(default)]
    pub usb_watchdog_enabled: bool,
    #[serde(default)]
    pub usb_watchdog_device_name: String,
    /// Automatically power-cycle the USB device when macOS wakes from sleep.
    #[serde(default)]
    pub usb_watchdog_cycle_on_wake: bool,
    #[serde(default)]
    pub hybrid_mode_enabled: bool,
    #[serde(default = "default_hybrid_threshold_secs")]
    pub hybrid_threshold_secs: f64,
    #[serde(default)]
    pub hybrid_short_audio_model: Option<String>,
    #[serde(default)]
    pub hybrid_long_audio_model: Option<String>,
    #[serde(default = "default_adaptive_parakeet_thresholds")]
    pub adaptive_parakeet_thresholds: bool,
    #[serde(default)]
    pub verification_mode: bool,
    #[serde(default)]
    pub vad_sensitivity: VadSensitivity,
    /// Show live captions during recording. When enabled, partial transcriptions
    /// are displayed in real-time as you speak, below the volume bars.
    #[serde(default)]
    pub live_captions_enabled: bool,
    /// UI scale factor for the overlay (1.0 = normal, 2.0 = double size).
    /// Scales the pill, live captions, and window dimensions proportionally.
    #[serde(default = "default_overlay_scale")]
    pub overlay_scale: f64,
    /// Convert US English spelling to British English after transcription.
    /// Applies common spelling conversions like: color → colour, analyze → analyse, etc.
    #[serde(default)]
    pub convert_us_to_british: bool,
    /// Spelling dictionary source for US-to-British conversion.
    /// Options: "dwyl" (curated, ~180 pairs) or "cspell" (comprehensive).
    /// DWYL is recommended for speech-to-text (excludes archaic spellings).
    #[serde(default)]
    pub spelling_dictionary: SpellingDictionary,
    /// Repetition suppression level (0-3): 0=off, 1=light, 2=moderate, 3=aggressive.
    /// Manual control only - user adjusts when they notice artifacts.
    #[serde(default)]
    pub repetition_suppression_level: u8,
    /// Whether noise suppression is enabled before VAD processing.
    /// Noise suppression removes background noise to improve VAD accuracy
    /// in noisy environments. Disabled by default as it adds CPU overhead.
    #[serde(default)]
    pub noise_suppression_enabled: bool,
    /// Noise suppression intensity level. Higher levels remove more noise
    /// but may introduce subtle artifacts in speech.
    #[serde(default)]
    pub noise_suppression_level: NoiseSuppressionLevel,
}

fn default_model() -> String {
    "".to_string()
}

fn default_always_on_microphone() -> bool {
    false
}

fn default_translate_to_english() -> bool {
    false
}

fn default_start_hidden() -> bool {
    false
}

fn default_autostart_enabled() -> bool {
    false
}

fn default_update_checks_enabled() -> bool {
    true
}

fn default_overlay_scale() -> f64 {
    1.0
}

#[allow(dead_code)]
fn default_convert_us_to_british() -> bool {
    false
}

fn default_overlay_position() -> OverlayPosition {
    #[cfg(target_os = "linux")]
    return OverlayPosition::None;
    #[cfg(not(target_os = "linux"))]
    return OverlayPosition::Bottom;
}

fn default_overlay_screen_target() -> OverlayScreenTarget {
    OverlayScreenTarget::Cursor
}

fn default_debug_mode() -> bool {
    false
}

fn default_log_level() -> LogLevel {
    LogLevel::Debug
}

fn default_word_correction_threshold() -> f64 {
    0.18
}

fn default_paste_delay_ms() -> u64 {
    60
}

fn default_auto_submit() -> bool {
    false
}

fn default_history_limit() -> usize {
    // Keep last 100 unsaved transcriptions before auto-cleanup.
    // This provides a reasonable buffer for users who don't save entries.
    // Saved entries are never auto-deleted.
    100
}

fn default_recording_retention_period() -> RecordingRetentionPeriod {
    RecordingRetentionPeriod::PreserveLimit
}

fn default_audio_feedback_volume() -> f32 {
    1.0
}

fn default_sound_theme() -> SoundTheme {
    SoundTheme::Marimba
}

fn default_post_process_enabled() -> bool {
    false
}

fn default_app_language() -> String {
    tauri_plugin_os::locale()
        .map(|l| l.replace('_', "-"))
        .unwrap_or_else(|| "en".to_string())
}

fn default_show_tray_icon() -> bool {
    true
}

fn default_post_process_provider_id() -> String {
    "openai".to_string()
}

fn default_post_process_providers() -> Vec<PostProcessProvider> {
    let mut providers = vec![
        PostProcessProvider {
            id: "openai".to_string(),
            label: "OpenAI".to_string(),
            base_url: "https://api.openai.com/v1".to_string(),
            allow_base_url_edit: false,
            models_endpoint: Some("/models".to_string()),
            supports_structured_output: true,
        },
        PostProcessProvider {
            id: "zai".to_string(),
            label: "Z.AI".to_string(),
            base_url: "https://api.z.ai/api/paas/v4".to_string(),
            allow_base_url_edit: false,
            models_endpoint: Some("/models".to_string()),
            supports_structured_output: true,
        },
        PostProcessProvider {
            id: "openrouter".to_string(),
            label: "OpenRouter".to_string(),
            base_url: "https://openrouter.ai/api/v1".to_string(),
            allow_base_url_edit: false,
            models_endpoint: Some("/models".to_string()),
            supports_structured_output: true,
        },
        PostProcessProvider {
            id: "anthropic".to_string(),
            label: "Anthropic".to_string(),
            base_url: "https://api.anthropic.com/v1".to_string(),
            allow_base_url_edit: false,
            models_endpoint: Some("/models".to_string()),
            supports_structured_output: false,
        },
        PostProcessProvider {
            id: "groq".to_string(),
            label: "Groq".to_string(),
            base_url: "https://api.groq.com/openai/v1".to_string(),
            allow_base_url_edit: false,
            models_endpoint: Some("/models".to_string()),
            supports_structured_output: false,
        },
        PostProcessProvider {
            id: "cerebras".to_string(),
            label: "Cerebras".to_string(),
            base_url: "https://api.cerebras.ai/v1".to_string(),
            allow_base_url_edit: false,
            models_endpoint: Some("/models".to_string()),
            supports_structured_output: true,
        },
    ];

    // Note: We always include Apple Intelligence on macOS ARM64 without checking availability
    // at startup. The availability check is deferred to when the user actually tries to use it
    // (in actions.rs). This prevents crashes on macOS 26.x beta where accessing
    // SystemLanguageModel.default during early app initialization causes SIGABRT.
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        providers.push(PostProcessProvider {
            id: APPLE_INTELLIGENCE_PROVIDER_ID.to_string(),
            label: "Apple Intelligence".to_string(),
            base_url: "apple-intelligence://local".to_string(),
            allow_base_url_edit: false,
            models_endpoint: None,
            supports_structured_output: true,
        });
    }

    // AWS Bedrock via Mantle (OpenAI-compatible endpoint)
    providers.push(PostProcessProvider {
        id: "bedrock_mantle".to_string(),
        label: "AWS Bedrock (Mantle)".to_string(),
        base_url: "https://bedrock-mantle.us-east-1.api.aws/v1".to_string(),
        allow_base_url_edit: false,
        models_endpoint: Some("/models".to_string()),
        supports_structured_output: true,
    });

    // Custom provider always comes last
    providers.push(PostProcessProvider {
        id: "custom".to_string(),
        label: "Custom".to_string(),
        base_url: "http://localhost:11434/v1".to_string(),
        allow_base_url_edit: true,
        models_endpoint: Some("/models".to_string()),
        supports_structured_output: false,
    });

    providers
}

fn default_post_process_api_keys() -> SecretMap {
    let mut map = HashMap::new();
    for provider in default_post_process_providers() {
        map.insert(provider.id, String::new());
    }
    SecretMap(map)
}

fn default_model_for_provider(provider_id: &str) -> String {
    if provider_id == APPLE_INTELLIGENCE_PROVIDER_ID {
        return APPLE_INTELLIGENCE_DEFAULT_MODEL_ID.to_string();
    }
    String::new()
}

fn default_post_process_models() -> HashMap<String, String> {
    let mut map = HashMap::new();
    for provider in default_post_process_providers() {
        map.insert(
            provider.id.clone(),
            default_model_for_provider(&provider.id),
        );
    }
    map
}

fn default_post_process_prompts() -> Vec<LLMPrompt> {
    vec![LLMPrompt {
        id: "default_improve_transcriptions".to_string(),
        name: "Improve Transcriptions".to_string(),
        prompt: "Clean this transcript:\n1. Fix spelling, capitalization, and punctuation errors\n2. Convert number words to digits (twenty-five → 25, ten percent → 10%, five dollars → $5)\n3. Replace spoken punctuation with symbols (period → ., comma → ,, question mark → ?)\n4. Remove filler words (um, uh, like as filler)\n5. Keep the language in the original version (if it was french, keep it in french for example)\n\nPreserve exact meaning and word order. Do not paraphrase or reorder content.\n\nReturn only the cleaned transcript.\n\nTranscript:\n${output}".to_string(),
    }]
}

fn default_whisper_gpu_device() -> i32 {
    -1 // auto
}

fn default_typing_tool() -> TypingTool {
    TypingTool::Auto
}

fn default_hybrid_threshold_secs() -> f64 {
    30.0
}

fn default_adaptive_parakeet_thresholds() -> bool {
    true
}

fn ensure_post_process_defaults(settings: &mut AppSettings) -> bool {
    let mut changed = false;
    for provider in default_post_process_providers() {
        // Use match to do a single lookup - either sync existing or add new
        match settings
            .post_process_providers
            .iter_mut()
            .find(|p| p.id == provider.id)
        {
            Some(existing) => {
                // Sync supports_structured_output field for existing providers (migration)
                if existing.supports_structured_output != provider.supports_structured_output {
                    debug!(
                        "Updating supports_structured_output for provider '{}' from {} to {}",
                        provider.id,
                        existing.supports_structured_output,
                        provider.supports_structured_output
                    );
                    existing.supports_structured_output = provider.supports_structured_output;
                    changed = true;
                }
            }
            None => {
                // Provider doesn't exist, add it
                settings.post_process_providers.push(provider.clone());
                changed = true;
            }
        }

        if !settings.post_process_api_keys.contains_key(&provider.id) {
            settings
                .post_process_api_keys
                .insert(provider.id.clone(), String::new());
            changed = true;
        }

        let default_model = default_model_for_provider(&provider.id);
        match settings.post_process_models.get_mut(&provider.id) {
            Some(existing) => {
                if existing.is_empty() && !default_model.is_empty() {
                    *existing = default_model.clone();
                    changed = true;
                }
            }
            None => {
                settings
                    .post_process_models
                    .insert(provider.id.clone(), default_model);
                changed = true;
            }
        }
    }

    changed
}

pub const SETTINGS_STORE_PATH: &str = "settings_store.json";

pub fn get_default_settings() -> AppSettings {
    #[cfg(target_os = "windows")]
    let default_shortcut = "ctrl+space";
    #[cfg(target_os = "macos")]
    let default_shortcut = "option+space";
    #[cfg(target_os = "linux")]
    let default_shortcut = "ctrl+space";
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    let default_shortcut = "alt+space";

    let mut bindings = HashMap::new();
    bindings.insert(
        "transcribe".to_string(),
        ShortcutBinding {
            id: "transcribe".to_string(),
            name: "Transcribe".to_string(),
            description: "Converts your speech into text.".to_string(),
            default_binding: default_shortcut.to_string(),
            current_binding: default_shortcut.to_string(),
        },
    );
    #[cfg(target_os = "windows")]
    let default_post_process_shortcut = "ctrl+shift+space";
    #[cfg(target_os = "macos")]
    let default_post_process_shortcut = "option+shift+space";
    #[cfg(target_os = "linux")]
    let default_post_process_shortcut = "ctrl+shift+space";
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    let default_post_process_shortcut = "alt+shift+space";

    bindings.insert(
        "transcribe_with_post_process".to_string(),
        ShortcutBinding {
            id: "transcribe_with_post_process".to_string(),
            name: "Transcribe with Post-Processing".to_string(),
            description: "Converts your speech into text and applies AI post-processing."
                .to_string(),
            default_binding: default_post_process_shortcut.to_string(),
            current_binding: default_post_process_shortcut.to_string(),
        },
    );
    #[cfg(target_os = "macos")]
    let default_router_shortcut = "option+ctrl+space";
    #[cfg(target_os = "windows")]
    let default_router_shortcut = "ctrl+alt+space";
    #[cfg(target_os = "linux")]
    let default_router_shortcut = "ctrl+alt+space";
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    let default_router_shortcut = "ctrl+alt+space";

    bindings.insert(
        "transcribe_with_router".to_string(),
        ShortcutBinding {
            id: "transcribe_with_router".to_string(),
            name: "Transcribe with Router".to_string(),
            description: "Records speech, transcribes, and sends text to your router system for classification and filing.".to_string(),
            default_binding: default_router_shortcut.to_string(),
            current_binding: default_router_shortcut.to_string(),
        },
    );

    #[cfg(target_os = "macos")]
    let default_router_shortcut = "option+ctrl+space";
    #[cfg(target_os = "windows")]
    let default_router_shortcut = "ctrl+alt+space";
    #[cfg(target_os = "linux")]
    let default_router_shortcut = "ctrl+alt+space";
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    let default_router_shortcut = "alt+ctrl+space";

    bindings.insert(
        "transcribe_with_router".to_string(),
        ShortcutBinding {
            id: "transcribe_with_router".to_string(),
            name: "Transcribe with Router".to_string(),
            description: "Records speech, transcribes, and routes to your notes via boss_router."
                .to_string(),
            default_binding: default_router_shortcut.to_string(),
            current_binding: default_router_shortcut.to_string(),
        },
    );
    bindings.insert(
        "cancel".to_string(),
        ShortcutBinding {
            id: "cancel".to_string(),
            name: "Cancel".to_string(),
            description: "Cancels the current recording.".to_string(),
            default_binding: "escape".to_string(),
            current_binding: "escape".to_string(),
        },
    );

    AppSettings {
        bindings,
        push_to_talk: true,
        audio_feedback: false,
        audio_feedback_volume: default_audio_feedback_volume(),
        sound_theme: default_sound_theme(),
        start_hidden: default_start_hidden(),
        autostart_enabled: default_autostart_enabled(),
        update_checks_enabled: default_update_checks_enabled(),
        selected_model: "".to_string(),
        always_on_microphone: false,
        selected_microphone: None,
        clamshell_microphone: None,
        selected_output_device: None,
        translate_to_english: false,
        selected_language: "auto".to_string(),
        overlay_position: default_overlay_position(),
        overlay_screen_target: default_overlay_screen_target(),
        debug_mode: false,
        log_level: default_log_level(),
        custom_words: Vec::new(),
        advanced_custom_words: Vec::new(),
        word_replacements: Vec::new(),
        use_advanced_custom_words: false,
        word_correction_mode: WordCorrectionMode::WordBias,
        model_unload_timeout: ModelUnloadTimeout::default(),
        word_correction_threshold: default_word_correction_threshold(),
        history_limit: default_history_limit(),
        recording_retention_period: default_recording_retention_period(),
        paste_method: PasteMethod::default(),
        clipboard_handling: ClipboardHandling::default(),
        auto_submit: default_auto_submit(),
        auto_submit_key: AutoSubmitKey::default(),
        post_process_enabled: default_post_process_enabled(),
        post_process_provider_id: default_post_process_provider_id(),
        post_process_providers: default_post_process_providers(),
        post_process_api_keys: default_post_process_api_keys(),
        post_process_models: default_post_process_models(),
        post_process_prompts: default_post_process_prompts(),
        post_process_selected_prompt_id: None,
        mute_while_recording: false,
        append_trailing_space: false,
        app_language: default_app_language(),
        experimental_enabled: false,
        lazy_stream_close: false,
        keyboard_implementation: KeyboardImplementation::default(),
        show_tray_icon: default_show_tray_icon(),
        paste_delay_ms: default_paste_delay_ms(),
        typing_tool: default_typing_tool(),
        external_script_path: None,
        router_script_path: Some(
            "/Users/caffae/Local-Projects-2026/Router-Actuator/boss_router.py".to_string(),
        ),
        router_env_file: Some(
            "/Users/caffae/Local-Projects-2026/Voice-Memo-to-Router/.env".to_string(),
        ),
        custom_filler_words: None,
        whisper_accelerator: WhisperAcceleratorSetting::default(),
        ort_accelerator: OrtAcceleratorSetting::default(),
        whisper_gpu_device: default_whisper_gpu_device(),
        extra_recording_buffer_ms: 0,
        pre_recording_buffer_ms: 0,
        usb_watchdog_enabled: false,
        usb_watchdog_device_name: String::new(),
        usb_watchdog_cycle_on_wake: true,
        hybrid_mode_enabled: false,
        hybrid_threshold_secs: default_hybrid_threshold_secs(),
        hybrid_short_audio_model: None,
        hybrid_long_audio_model: None,
        adaptive_parakeet_thresholds: default_adaptive_parakeet_thresholds(),
        verification_mode: false,
        vad_sensitivity: VadSensitivity::Balanced,
        live_captions_enabled: false,
        overlay_scale: default_overlay_scale(),
        convert_us_to_british: false,
        spelling_dictionary: SpellingDictionary::default(),
        repetition_suppression_level: 0,
        noise_suppression_enabled: false,
        noise_suppression_level: NoiseSuppressionLevel::default(),
    }
}

impl AppSettings {
    pub fn active_post_process_provider(&self) -> Option<&PostProcessProvider> {
        self.post_process_providers
            .iter()
            .find(|provider| provider.id == self.post_process_provider_id)
    }

    pub fn post_process_provider(&self, provider_id: &str) -> Option<&PostProcessProvider> {
        self.post_process_providers
            .iter()
            .find(|provider| provider.id == provider_id)
    }

    pub fn post_process_provider_mut(
        &mut self,
        provider_id: &str,
    ) -> Option<&mut PostProcessProvider> {
        self.post_process_providers
            .iter_mut()
            .find(|provider| provider.id == provider_id)
    }
}

/// Validate that float fields are not NaN before serialization.
/// NaN values cause serde_json serialization to fail (produces `null` which isn't valid for numbers).
fn sanitize_floats(settings: &mut AppSettings) {
    if settings.audio_feedback_volume.is_nan() {
        error!("audio_feedback_volume is NaN, resetting to default");
        settings.audio_feedback_volume = default_audio_feedback_volume();
    }
    if settings.word_correction_threshold.is_nan() {
        error!("word_correction_threshold is NaN, resetting to default");
        settings.word_correction_threshold = default_word_correction_threshold();
    }
    if settings.overlay_scale.is_nan() {
        error!("overlay_scale is NaN, resetting to default");
        settings.overlay_scale = default_overlay_scale();
    }
    if settings.hybrid_threshold_secs.is_nan() {
        error!("hybrid_threshold_secs is NaN, resetting to default");
        settings.hybrid_threshold_secs = default_hybrid_threshold_secs();
    }
}

/// Helper: serialize settings to a serde_json::Value, logging errors instead of panicking.
fn settings_to_value(settings: &AppSettings) -> Option<serde_json::Value> {
    match serde_json::to_value(settings) {
        Ok(v) => Some(v),
        Err(e) => {
            error!("Failed to serialize settings to JSON: {}", e);
            None
        }
    }
}

/// Helper: open the settings store, logging errors instead of panicking.
fn open_settings_store(app: &AppHandle) -> Option<Arc<tauri_plugin_store::Store<tauri::Wry>>> {
    match app.store(crate::portable::store_path(SETTINGS_STORE_PATH)) {
        Ok(store) => Some(store),
        Err(e) => {
            error!("Failed to initialize settings store: {}", e);
            None
        }
    }
}

/// Execute a settings operation safely, catching any panics before they can
/// propagate to WebKit's URL scheme handler (which calls `abort()` on panic).
///
/// Uses `AssertUnwindSafe` unconditionally because `AppHandle` is not
/// `UnwindSafe` — but we never actually unwind past this point since we
/// catch and log the panic instead.
fn safe_settings_operation<F, T>(label: &str, op: F) -> Option<T>
where
    F: FnOnce() -> T,
{
    match catch_unwind(AssertUnwindSafe(op)) {
        Ok(result) => Some(result),
        Err(panic_info) => {
            error!(
                "Panic in settings operation ({}) — caught to prevent WebKit abort: {:?}",
                label, panic_info
            );
            None
        }
    }
}

/// Safe wrapper around [`load_or_create_app_settings`] that catches panics
/// and falls back to defaults.
///
/// Use this when calling from contexts where a panic would propagate to
/// WebKit (e.g. URL scheme handlers, Tauri command handlers, startup code).
pub fn load_or_create_app_settings_safe(app: &AppHandle) -> AppSettings {
    safe_settings_operation("load_or_create_app_settings", || {
        load_or_create_app_settings(app)
    })
    .unwrap_or_else(|| {
        error!("Falling back to default settings after panic in load_or_create_app_settings");
        get_default_settings()
    })
}

/// Safe wrapper around [`get_settings`] that catches panics and falls back
/// to defaults.
///
/// Use this when calling from contexts where a panic would propagate to
/// WebKit (e.g. URL scheme handlers, Tauri command handlers, startup code).
pub fn get_settings_safe(app: &AppHandle) -> AppSettings {
    safe_settings_operation("get_settings", || get_settings(app)).unwrap_or_else(|| {
        error!("Falling back to default settings after panic in get_settings");
        get_default_settings()
    })
}

/// Safe wrapper around [`write_settings`] that catches panics.
///
/// If the write panics, the error is logged but the app continues running.
/// This prevents WebKit's URL scheme handler from calling `abort()`.
pub fn write_settings_safe(app: &AppHandle, settings: AppSettings) {
    let _ = safe_settings_operation("write_settings", || {
        write_settings(app, settings);
    });
}

/// Safe wrapper around [`write_settings_immediate`] that catches panics.
///
/// If the write panics, the error is logged but the app continues running.
#[allow(dead_code)] // Available for direct use when immediate safe writes are needed
pub fn write_settings_immediate_safe(app: &AppHandle, settings: AppSettings) {
    let _ = safe_settings_operation("write_settings_immediate", || {
        write_settings_immediate(app, settings);
    });
}

pub fn load_or_create_app_settings(app: &AppHandle) -> AppSettings {
    // Initialize store
    let Some(store) = open_settings_store(app) else {
        error!("Cannot load settings: store initialization failed, returning defaults");
        return get_default_settings();
    };

    let mut settings = if let Some(settings_value) = store.get("settings") {
        // Parse the entire settings object
        match serde_json::from_value::<AppSettings>(settings_value) {
            Ok(mut settings) => {
                debug!("Found existing settings: {:?}", settings);
                let default_settings = get_default_settings();
                let mut updated = false;

                // Merge default bindings into existing settings
                for (key, value) in default_settings.bindings {
                    if !settings.bindings.contains_key(&key) {
                        debug!("Adding missing binding: {}", key);
                        settings.bindings.insert(key, value);
                        updated = true;
                    }
                }

                // Migrate new settings fields: if they're None, fill in defaults.
                // This handles the case where the settings JSON was created before
                // the field existed, so it deserializes as None.
                if settings.router_script_path.is_none()
                    && default_settings.router_script_path.is_some()
                {
                    debug!("Migrating router_script_path from default");
                    settings.router_script_path = default_settings.router_script_path.clone();
                    updated = true;
                }
                if settings.router_env_file.is_none() && default_settings.router_env_file.is_some()
                {
                    debug!("Migrating router_env_file from default");
                    settings.router_env_file = default_settings.router_env_file.clone();
                    updated = true;
                }

                // Migrate usb_watchdog_cycle_on_wake
                if settings.usb_watchdog_enabled && !settings.usb_watchdog_cycle_on_wake {
                    debug!("Migrating usb_watchdog_cycle_on_wake to true for enabled watchdog");
                    settings.usb_watchdog_cycle_on_wake = true;
                    updated = true;
                }

                // Migrate use_advanced_custom_words to word_correction_mode
                // The old boolean field is kept for backward compatibility but the
                // new enum field takes precedence.
                if settings.use_advanced_custom_words
                    && settings.word_correction_mode == WordCorrectionMode::WordBias
                {
                    debug!("Migrating use_advanced_custom_words=true to word_correction_mode=Pronunciation");
                    settings.word_correction_mode = WordCorrectionMode::Pronunciation;
                    updated = true;
                }

                if updated {
                    debug!("Settings updated with new bindings");
                    sanitize_floats(&mut settings);
                    if let Some(value) = settings_to_value(&settings) {
                        store.set("settings", value);
                        let _ = store.save(); // Persist binding migrations to disk
                    }
                }

                settings
            }
            Err(e) => {
                warn!("Failed to parse settings: {}", e);
                // Fall back to default settings if parsing fails
                let default_settings = get_default_settings();
                if let Some(value) = settings_to_value(&default_settings) {
                    store.set("settings", value);
                }
                default_settings
            }
        }
    } else {
        let default_settings = get_default_settings();
        if let Some(value) = settings_to_value(&default_settings) {
            store.set("settings", value);
        }
        default_settings
    };

    if ensure_post_process_defaults(&mut settings) {
        sanitize_floats(&mut settings);
        if let Some(value) = settings_to_value(&settings) {
            store.set("settings", value);
        }
    }

    settings
}

pub fn get_settings(app: &AppHandle) -> AppSettings {
    let Some(store) = open_settings_store(app) else {
        error!("Cannot get settings: store initialization failed, returning defaults");
        return get_default_settings();
    };

    let mut settings = if let Some(settings_value) = store.get("settings") {
        serde_json::from_value::<AppSettings>(settings_value).unwrap_or_else(|e| {
            warn!("Failed to parse settings: {}, returning defaults", e);
            let default_settings = get_default_settings();
            if let Some(value) = settings_to_value(&default_settings) {
                store.set("settings", value);
            }
            default_settings
        })
    } else {
        let default_settings = get_default_settings();
        if let Some(value) = settings_to_value(&default_settings) {
            store.set("settings", value);
        }
        default_settings
    };

    // Migrate new settings fields that may be None in existing configs
    let default_settings = get_default_settings();
    let mut needs_save = false;

    if settings.router_script_path.is_none() && default_settings.router_script_path.is_some() {
        debug!("Migrating router_script_path from default");
        settings.router_script_path = default_settings.router_script_path.clone();
        needs_save = true;
    }
    if settings.router_env_file.is_none() && default_settings.router_env_file.is_some() {
        debug!("Migrating router_env_file from default");
        settings.router_env_file = default_settings.router_env_file.clone();
        needs_save = true;
    }

    // Migrate usb_watchdog_cycle_on_wake: if usb_watchdog_enabled is true,
    // we want this to default to true for existing users as well.
    if settings.usb_watchdog_enabled && !settings.usb_watchdog_cycle_on_wake {
        debug!("Migrating usb_watchdog_cycle_on_wake to true for enabled watchdog");
        settings.usb_watchdog_cycle_on_wake = true;
        needs_save = true;
    }

    // Merge missing bindings too
    for (key, value) in default_settings.bindings {
        if !settings.bindings.contains_key(&key) {
            debug!("Adding missing binding: {}", key);
            settings.bindings.insert(key, value);
            needs_save = true;
        }
    }

    if needs_save {
        sanitize_floats(&mut settings);
        if let Some(value) = settings_to_value(&settings) {
            store.set("settings", value);
            let _ = store.save(); // Persist migration to disk immediately
        }
    }

    if ensure_post_process_defaults(&mut settings) {
        sanitize_floats(&mut settings);
        if let Some(value) = settings_to_value(&settings) {
            store.set("settings", value);
        }
    }

    settings
}

/// Write settings to disk using the debounced writer.
///
/// If a debounced writer is available in Tauri's managed state, the write is
/// deferred by the debounce interval so that rapid successive calls (e.g. from
/// a slider being dragged) are coalesced into a single disk flush. If the
/// writer is not yet initialised (e.g. during startup), falls back to an
/// immediate write.
///
/// This function is wrapped in `catch_unwind` to prevent panics from
/// propagating to WebKit's URL scheme handler (which calls `abort()` on panic).
pub fn write_settings(app: &AppHandle, settings: AppSettings) {
    let _ = safe_settings_operation("write_settings", || {
        // Try to use the debounced writer. During app startup the writer may not
        // yet be registered in Tauri state, so we fall back to an immediate write.
        if let Some(writer) = app.try_state::<Arc<SettingsWriter>>() {
            let app_clone = app.clone(); // AppHandle is cheaply clonable (Arc internally)
            let writer = writer.inner().clone(); // Clone the Arc<SettingsWriter> for 'static spawn
            tokio::spawn(async move {
                writer.write(app_clone, settings).await;
            });
        } else {
            // Fallback: no debounce state yet (e.g. during initialisation)
            write_settings_immediate(app, settings);
        }
    });
}

/// Write settings to disk immediately, bypassing the debounce timer.
///
/// This is used internally by the debounced writer's flush and during
/// app startup when the writer isn't yet available. It can also be called
/// directly when an immediate write is known to be necessary (e.g. migration).
///
/// This function is wrapped in `catch_unwind` to prevent panics from
/// propagating to WebKit's URL scheme handler (which calls `abort()` on panic).
pub fn write_settings_immediate(app: &AppHandle, mut settings: AppSettings) {
    let _ = safe_settings_operation("write_settings_immediate", || {
        let Some(store) = open_settings_store(app) else {
            error!("Cannot write settings: store initialization failed, settings not saved");
            return;
        };

        sanitize_floats(&mut settings);

        let Some(value) = settings_to_value(&settings) else {
            error!("Cannot write settings: serialization failed, settings not saved");
            return;
        };

        store.set("settings", value);
        let _ = store.save(); // Persist to disk immediately
    });
}

/// Flush any pending debounced settings to disk.
///
/// Should be called on app shutdown (via `RunEvent::ExitRequested`) to
/// guarantee that the most recent settings value is persisted.
pub fn flush_settings(app: &AppHandle) {
    let _ = safe_settings_operation("flush_settings", || {
        if let Some(writer) = app.try_state::<Arc<SettingsWriter>>() {
            let writer = writer.inner().clone();
            // Use block_in_place so we can await the async flush from
            // a synchronous context (the Tauri run callback is not async).
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    writer.flush(app).await;
                })
            });
        }
    });
}

pub fn get_bindings(app: &AppHandle) -> HashMap<String, ShortcutBinding> {
    let settings = get_settings_safe(app);

    settings.bindings
}

pub fn get_stored_binding(app: &AppHandle, id: &str) -> ShortcutBinding {
    let bindings = get_bindings(app);

    if let Some(binding) = bindings.get(id) {
        return binding.clone();
    }

    // Not found in current settings — check defaults
    warn!(
        "Binding '{}' not found in current settings, falling back to defaults",
        id
    );
    let default_settings = get_default_settings();

    if let Some(default_binding) = default_settings.bindings.get(id) {
        return default_binding.clone();
    }

    // Not in defaults either — create a sensible fallback
    warn!(
        "Binding '{}' not found in defaults either, creating fallback binding",
        id
    );
    ShortcutBinding {
        id: id.to_string(),
        name: id.to_string(),
        description: format!("{} shortcut", id),
        default_binding: String::new(),
        current_binding: String::new(),
    }
}

pub fn get_history_limit(app: &AppHandle) -> usize {
    let settings = get_settings_safe(app);
    settings.history_limit
}

pub fn get_recording_retention_period(app: &AppHandle) -> RecordingRetentionPeriod {
    let settings = get_settings_safe(app);
    settings.recording_retention_period
}

// ---------------------------------------------------------------------------
// Debounced settings writer
// ---------------------------------------------------------------------------

/// Default debounce interval in milliseconds.
/// Multiple rapid settings changes within this window are coalesced into a
/// single disk write, reducing I/O thrash when users adjust settings quickly
/// (e.g. dragging a slider).
pub const SETTINGS_DEBOUNCE_MS: u64 = 500;

/// State for the debounced settings writer.
///
/// The writer batches writes so that rapid successive calls to
/// [`write_settings`] only trigger one disk flush after the debounce window
/// elapses.  A call to [`SettingsWriter::flush`] bypasses the timer and
/// persists immediately — this is used on app shutdown to guarantee no
/// settings are lost.
pub struct SettingsWriter {
    /// The most recent settings value that has not yet been flushed to disk.
    pending: Mutex<Option<AppSettings>>,
    /// Handle for the currently-scheduled debounce timer task.
    timer: Mutex<Option<JoinHandle<()>>>,
    /// Debounce interval. Configurable so tests can speed things up.
    debounce_ms: u64,
}

impl SettingsWriter {
    /// Create a new writer with the default debounce interval.
    pub fn new() -> Self {
        Self {
            pending: Mutex::new(None),
            timer: Mutex::new(None),
            debounce_ms: SETTINGS_DEBOUNCE_MS,
        }
    }

    /// Create a writer with a custom debounce interval (useful in tests).
    #[allow(dead_code)]
    pub fn with_debounce_ms(ms: u64) -> Self {
        Self {
            pending: Mutex::new(None),
            timer: Mutex::new(None),
            debounce_ms: ms,
        }
    }

    /// Schedule a settings write.  If a write is already pending the new value
    /// replaces it and the debounce timer is restarted.
    pub async fn write(&self, app: AppHandle, settings: AppSettings) {
        // Store the latest settings value.
        {
            let mut pending = self.pending.lock().await;
            *pending = Some(settings);
        }

        // Cancel any existing timer.
        {
            let mut timer = self.timer.lock().await;
            if let Some(handle) = timer.take() {
                handle.abort();
            }
        }

        // Spawn a debounce timer task. The task re-acquires the SettingsWriter
        // from Tauri managed state (Arc<SettingsWriter>) so we don't need
        // to pass self as an Arc.
        let debounce_ms = self.debounce_ms;
        let new_handle = tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_millis(debounce_ms)).await;

            // Re-acquire the SettingsWriter from app state
            let Some(writer) = app.try_state::<Arc<SettingsWriter>>() else {
                warn!("SettingsWriter not available, skipping debounced flush");
                return;
            };
            writer.flush_inner(&app).await;
        });

        // Store the timer handle.
        {
            let mut timer = self.timer.lock().await;
            *timer = Some(new_handle);
        }
    }

    /// Flush any pending settings to disk immediately.
    ///
    /// Called on app shutdown to guarantee that the most recent settings
    /// value is persisted even if the debounce timer hasn't fired yet.
    pub async fn flush(&self, app: &AppHandle) {
        // Cancel any pending timer first.
        {
            let mut timer = self.timer.lock().await;
            if let Some(handle) = timer.take() {
                handle.abort();
            }
        }
        self.flush_inner(app).await;
    }

    /// Internal flush: write the pending settings (if any) to the store.
    async fn flush_inner(&self, app: &AppHandle) {
        let maybe_settings = {
            let mut pending = self.pending.lock().await;
            pending.take()
        };

        if let Some(settings) = maybe_settings {
            debug!("Flushing debounced settings to disk");
            write_settings_immediate(app, settings);
        }
    }
}

// ---------------------------------------------------------------------------
#[cfg(test)]
#[allow(unused_imports)]
mod tests {
    use super::*;

    #[test]
    fn default_settings_disable_auto_submit() {
        let settings = get_default_settings();
        assert!(!settings.auto_submit);
        assert_eq!(settings.auto_submit_key, AutoSubmitKey::Enter);
    }

    #[test]
    fn debug_output_redacts_api_keys() {
        let mut settings = get_default_settings();
        settings
            .post_process_api_keys
            .insert("openai".to_string(), "sk-proj-secret-key-12345".to_string());
        settings.post_process_api_keys.insert(
            "anthropic".to_string(),
            "sk-ant-secret-key-67890".to_string(),
        );
        settings
            .post_process_api_keys
            .insert("empty_provider".to_string(), "".to_string());

        let debug_output = format!("{:?}", settings);

        assert!(!debug_output.contains("sk-proj-secret-key-12345"));
        assert!(!debug_output.contains("sk-ant-secret-key-67890"));
        assert!(debug_output.contains("[REDACTED]"));
    }

    #[test]
    fn secret_map_debug_redacts_values() {
        let map = SecretMap(HashMap::from([("key".into(), "secret".into())]));
        let out = format!("{:?}", map);
        assert!(!out.contains("secret"));
        assert!(out.contains("[REDACTED]"));
    }
}
