// Re-export all audio components
mod device;
mod sink;
mod utils;
mod visualizer;

pub use device::{
    is_bluetooth_audio_active, is_bluetooth_output_device, list_output_devices, output_device_name,
    OutputDeviceInfo,
};
pub use sink::{
    AudioFrameCallback, CaptureControl, CaptureSink, CaptureSinkConfig, CaptureVad, CapturedAudio,
    LevelCallback, VadPolicy, DEFAULT_NOISE_FLOOR, NOISE_ALPHA, SILENCE_RMS_MULTIPLIER,
};
pub use utils::{
    read_wav_samples, save_wav_file, validate_audio, validate_wav_file, verify_wav_file,
    AudioQualityMetrics, AudioValidationResult,
};
pub use visualizer::AudioVisualiser;
