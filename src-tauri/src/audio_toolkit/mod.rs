pub mod accent_map;
pub mod audio;
pub mod chunking;
pub mod constants;
pub mod lang_id;
pub mod noise_suppression;
pub mod spelling_dictionaries;
pub mod text;
pub mod utils;
pub mod vad;

pub use audio::{
    is_bluetooth_audio_active, is_bluetooth_output_device, list_output_devices, output_device_name,
    read_wav_samples, save_wav_file, validate_audio, validate_wav_file, verify_wav_file,
    AudioValidationResult, CaptureControl, CaptureSink, CaptureSinkConfig, CaptureVad,
    OutputDeviceInfo, VadPolicy,
};
pub use lang_id::detect_output_language;
pub use noise_suppression::{NoiseSuppressor, NOISE_SUPPRESSION_FRAME_SIZE};
pub use spelling_dictionaries::SpellingDictionary;
pub use text::{
    apply_advanced_custom_words, apply_custom_words, apply_word_replacements,
    convert_us_to_british, detect_repeated_words, normalize_transcription_output,
    process_transcription_text, remove_filler_words, suppress_repeated_words,
    OutputLanguageEvidence,
};
pub use utils::get_cpal_host;
pub use vad::{trim_trailing_silence, EarshotVad, SileroVad, SpeechScorer, VoiceActivityDetector};
