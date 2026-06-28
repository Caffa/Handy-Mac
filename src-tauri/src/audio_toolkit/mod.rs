pub mod audio;
pub mod constants;
pub mod noise_suppression;
pub mod spelling_dictionaries;
pub mod text;
pub mod utils;
pub mod vad;

pub use audio::{
    is_bluetooth_audio_active, is_bluetooth_output_device, is_microphone_access_denied,
    is_no_input_device_error, list_input_devices, list_output_devices, read_wav_samples,
    save_wav_file, validate_audio, validate_wav_file, verify_wav_file, AudioRecorder,
    AudioValidationResult, CpalDeviceInfo,
};
pub use spelling_dictionaries::SpellingDictionary;
pub use text::{
    apply_advanced_custom_words, apply_custom_words, apply_word_replacements, convert_us_to_british,
    detect_repeated_words, filter_transcription_output, process_transcription_text,
    suppress_repeated_words,
};
pub use utils::get_cpal_host;
pub use noise_suppression::{NoiseSuppressor, NOISE_SUPPRESSION_FRAME_SIZE};
pub use vad::{trim_trailing_silence, SileroVad, VoiceActivityDetector};
