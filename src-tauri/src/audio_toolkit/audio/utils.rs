use anyhow::Result;
use hound::{WavReader, WavSpec, WavWriter};
use log::{debug, warn};
use std::fs;
use std::path::Path;

/// Suffix used for temporary files during atomic writes.
/// Files with this suffix are orphaned if the process crashes mid-write.
/// HistoryManager cleans these up at startup.
#[allow(dead_code)]
pub const TEMP_FILE_SUFFIX: &str = ".wav.tmp";

/// Audio quality metrics computed from raw PCM samples (f32, -1..1).
///
/// Used by the adaptive Parakeet threshold logic to estimate recording
/// quality and adjust confidence thresholds dynamically.
#[derive(Debug, Clone, Copy)]
pub struct AudioQualityMetrics {
    /// Peak amplitude (linear scale, 0..1). Values near 0.0 mean the audio
    /// is very quiet.
    pub peak_amplitude: f32,
    /// Peak level in dBFS (0 = clipping, -96 ≈ silence).
    pub peak_dbfs: f32,
    /// Estimated signal-to-noise ratio in dB, derived from the ratio of the
    /// top-decile peak frames to the bottom-decile "noise floor" frames.
    /// A rough heuristic; not a rigorous SNR measurement.
    pub estimated_snr_db: f32,
    /// Duration in seconds (assuming 16 kHz sample rate).
    pub duration_secs: f32,
}

impl AudioQualityMetrics {
    /// Compute quality metrics from a single-channel f32 sample buffer.
    ///
    /// Samples should be in the range [-1.0, 1.0] (normalised i16 or f32
    /// native). The sample rate is assumed to be 16000 Hz for duration.
    pub fn compute(samples: &[f32]) -> Self {
        let len = samples.len();
        let duration_secs = len as f32 / 16000.0;

        if len == 0 {
            return Self {
                peak_amplitude: 0.0,
                peak_dbfs: -96.0,
                estimated_snr_db: 0.0,
                duration_secs: 0.0,
            };
        }

        // Peak amplitude (largest absolute value)
        let peak_amplitude = samples.iter().map(|s| s.abs()).fold(0.0f32, f32::max);

        // Peak dBFS (relative to full scale, 0 dBFS = max)
        let peak_dbfs = if peak_amplitude > 1e-10 {
            20.0 * peak_amplitude.log10()
        } else {
            -96.0
        };

        // Faster SNR estimation: sample a subset of the audio to avoid sorting the whole buffer.
        // Sorting the entire buffer is O(N log N) which is too slow for large buffers.
        // Sampling every 100th sample gives a good approximation with O(M log M) where M = N/100.
        let sample_step = 100;
        let mut amps: Vec<f32> = samples
            .iter()
            .step_by(sample_step)
            .map(|s| s.abs())
            .collect();
        // Use total_cmp instead of partial_cmp().unwrap() to handle NaN
        // values deterministically — NaN sorts as the smallest value.
        // Without this, a corrupted audio buffer containing NaN would panic.
        amps.sort_unstable_by(|a, b| a.total_cmp(b));

        let n = amps.len();
        let signal_end = (n * 90 / 100).max(1); // top 10%
        let noise_end = (n * 10 / 100).max(1); // bottom 10%

        let signal_rms: f32 =
            amps[n - signal_end..].iter().map(|&a| a * a).sum::<f32>() / signal_end as f32;
        let noise_rms: f32 =
            amps[..noise_end].iter().map(|&a| a * a).sum::<f32>() / noise_end as f32;

        let estimated_snr_db = if noise_rms > 1e-15 && signal_rms > noise_rms {
            10.0 * (signal_rms / noise_rms).log10()
        } else {
            0.0
        };

        Self {
            peak_amplitude,
            peak_dbfs,
            estimated_snr_db,
            duration_secs,
        }
    }

    /// Returns `true` if the audio appears to be very quiet / low-quality.
    /// Used to decide whether to lower confidence thresholds further.
    pub fn is_quiet(&self) -> bool {
        self.peak_dbfs < -30.0 || self.estimated_snr_db < 10.0
    }

    /// Returns `true` if the audio is clean and loud, suggesting thresholds
    /// could be raised slightly to reduce false positives.
    pub fn is_clean(&self) -> bool {
        self.peak_dbfs > -12.0 && self.estimated_snr_db > 25.0
    }
}

/// Read a WAV file and return normalised f32 samples.
pub fn read_wav_samples<P: AsRef<Path>>(file_path: P) -> Result<Vec<f32>> {
    let reader = WavReader::open(file_path.as_ref())?;
    let samples = reader
        .into_samples::<i16>()
        .map(|s| s.map(|v| v as f32 / i16::MAX as f32))
        .collect::<Result<Vec<f32>, _>>()?;
    Ok(samples)
}

/// Verify a WAV file by reading it back and checking the sample count.
pub fn verify_wav_file<P: AsRef<Path>>(file_path: P, expected_samples: usize) -> Result<()> {
    let reader = WavReader::open(file_path.as_ref())?;
    let actual_samples = reader.len() as usize;
    if actual_samples != expected_samples {
        anyhow::bail!(
            "WAV sample count mismatch: expected {}, got {}",
            expected_samples,
            actual_samples
        );
    }
    Ok(())
}

/// Result of audio validation.
#[derive(Debug, Clone)]
pub enum AudioValidationResult {
    /// Audio is valid and contains speech
    Valid {
        /// Duration in seconds
        duration_secs: f64,
        /// Number of samples
        sample_count: usize,
    },
    /// Audio is silent or near-silent (no speech detected)
    Silent {
        /// Maximum amplitude detected
        max_amplitude: f32,
        /// Duration in seconds
        duration_secs: f64,
    },
    /// Audio file is corrupted or unreadable
    Corrupted {
        /// Error message
        error: String,
    },
}

/// Validate audio samples to detect silence vs corruption vs valid speech.
/// Returns a classification result for intelligent retry decisions.
pub fn validate_audio(samples: &[f32], sample_rate: u32) -> AudioValidationResult {
    if samples.is_empty() {
        return AudioValidationResult::Corrupted {
            error: "Empty audio samples".to_string(),
        };
    }

    let duration_secs = samples.len() as f64 / sample_rate as f64;

    // Calculate audio statistics
    let mut max_amplitude = 0.0f32;
    let mut rms_sum = 0.0f32;

    for sample in samples {
        let abs_sample = sample.abs();
        max_amplitude = max_amplitude.max(abs_sample);
        rms_sum += abs_sample * abs_sample;
    }

    let rms = (rms_sum / samples.len() as f32).sqrt();

    // Threshold for silence detection (empirically determined)
    // - Max amplitude < 0.01 (very quiet, likely silence)
    // - RMS < 0.005 (sustained low energy)
    const SILENCE_MAX_THRESHOLD: f32 = 0.01;
    const SILENCE_RMS_THRESHOLD: f32 = 0.005;

    if max_amplitude < SILENCE_MAX_THRESHOLD && rms < SILENCE_RMS_THRESHOLD {
        debug!(
            "Audio classified as silent: max_amplitude={:.6}, rms={:.6}, duration={:.2}s",
            max_amplitude, rms, duration_secs
        );
        AudioValidationResult::Silent {
            max_amplitude,
            duration_secs,
        }
    } else {
        debug!(
            "Audio classified as valid: max_amplitude={:.6}, rms={:.6}, duration={:.2}s",
            max_amplitude, rms, duration_secs
        );
        AudioValidationResult::Valid {
            duration_secs,
            sample_count: samples.len(),
        }
    }
}

/// Validate a WAV file and return the validation result.
/// This is a convenience function that reads the file and validates the samples.
pub fn validate_wav_file<P: AsRef<Path>>(file_path: P) -> AudioValidationResult {
    let samples = match read_wav_samples(file_path.as_ref()) {
        Ok(s) => s,
        Err(e) => {
            return AudioValidationResult::Corrupted {
                error: format!("Failed to read WAV file: {}", e),
            };
        }
    };

    validate_audio(&samples, 16000) // Standard sample rate for Handy
}

/// Save audio samples as a WAV file using atomic write-then-rename.
///
/// Writes to a temporary file (`.tmp` suffix) first, then atomically
/// renames it to the final path. This prevents partial/corrupted WAV
/// files if the process crashes mid-write. On failure, the temp file
/// is cleaned up.
pub fn save_wav_file<P: AsRef<Path>>(file_path: P, samples: &[f32]) -> Result<()> {
    let file_path = file_path.as_ref();
    let spec = WavSpec {
        channels: 1,
        sample_rate: 16000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };

    // Write to a temporary file first for atomicity
    let temp_path = file_path.with_extension("wav.tmp");

    let write_result = (|| -> Result<()> {
        let mut writer = WavWriter::create(&temp_path, spec)?;

        // Convert f32 samples to i16 for WAV
        for sample in samples {
            let sample_i16 = (sample * i16::MAX as f32) as i16;
            writer.write_sample(sample_i16)?;
        }

        writer.finalize()?;
        Ok(())
    })();

    match write_result {
        Ok(()) => {
            // Atomic rename: on the same filesystem this is atomic,
            // replacing any existing file or creating a new one.
            if let Err(e) = fs::rename(&temp_path, file_path) {
                // If rename fails (e.g., cross-filesystem), fall back to
                // non-atomic write but still clean up the temp file.
                warn!(
                    "Atomic rename failed for {:?}: {}. Falling back to direct write.",
                    file_path, e
                );
                let _ = fs::remove_file(&temp_path);
                // Direct write as fallback
                let mut writer = WavWriter::create(file_path, spec)?;
                for sample in samples {
                    let sample_i16 = (sample * i16::MAX as f32) as i16;
                    writer.write_sample(sample_i16)?;
                }
                writer.finalize()?;
            }
            debug!("Saved WAV file: {:?}", file_path);
        }
        Err(e) => {
            // Clean up the temp file on write failure
            let _ = fs::remove_file(&temp_path);
            return Err(e);
        }
    }

    Ok(())
}
