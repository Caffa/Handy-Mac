use anyhow::Result;
use hound::{WavReader, WavSpec, WavWriter};
use log::debug;
use std::path::Path;

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
        amps.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap());

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

/// Save audio samples as a WAV file
pub fn save_wav_file<P: AsRef<Path>>(file_path: P, samples: &[f32]) -> Result<()> {
    let spec = WavSpec {
        channels: 1,
        sample_rate: 16000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };

    let mut writer = WavWriter::create(file_path.as_ref(), spec)?;

    // Convert f32 samples to i16 for WAV
    for sample in samples {
        let sample_i16 = (sample * i16::MAX as f32) as i16;
        writer.write_sample(sample_i16)?;
    }

    writer.finalize()?;
    debug!("Saved WAV file: {:?}", file_path.as_ref());
    Ok(())
}
