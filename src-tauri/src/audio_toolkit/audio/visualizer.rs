use rustfft::{num_complex::Complex32, Fft, FftPlanner};
use std::sync::Arc;

const GAIN: f32 = 1.3;
const CURVE_POWER: f32 = 0.7;
/// Fixed normalization window — wide enough to cover any mic from very
/// quiet (-70 dB) to loud (-5 dB).  This is the base range; the peak
/// boost below auto-amplifies quiet mics so their speech fills the bars.
const DB_MIN: f32 = -70.0;
const DB_MAX: f32 = -5.0;
/// Maximum gain boost applied to quiet mics.  If the user's peak speech
/// only reaches 25 % of the fixed window, the boost is 4× so their bars
/// still move meaningfully.  Capped to prevent noise amplification.
const MAX_BOOST: f32 = 4.0;
/// Peak tracker decay rate.  The peak snaps up instantly when speech
/// exceeds it, and decays slowly so brief pauses don't collapse the
/// boost.  At ~30 fps, 0.003 gives a half-life of ~4 s.
const PEAK_DECAY_ALPHA: f32 = 0.003;

pub struct AudioVisualiser {
    fft: Arc<dyn Fft<f32>>,
    window: Vec<f32>,
    bucket_ranges: Vec<(usize, usize)>,
    fft_input: Vec<Complex32>,
    /// Running peak of the *normalized* 0..1 level per bucket.  Snaps up
    /// instantly, decays slowly.  Used to compute a gain boost so quiet
    /// mics fill the bar range.
    peak_normalized: Vec<f32>,
    buffer: Vec<f32>,
    window_size: usize,
    buckets: usize,
}

impl AudioVisualiser {
    pub fn new(
        sample_rate: u32,
        window_size: usize,
        buckets: usize,
        freq_min: f32,
        freq_max: f32,
    ) -> Self {
        let mut planner = FftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(window_size);

        // Pre-compute Hann window
        let window: Vec<f32> = (0..window_size)
            .map(|i| {
                0.5 * (1.0 - (2.0 * std::f32::consts::PI * i as f32 / window_size as f32).cos())
            })
            .collect();

        // Pre-compute bucket frequency ranges
        let nyquist = sample_rate as f32 / 2.0;
        let freq_min = freq_min.min(nyquist);
        let freq_max = freq_max.min(nyquist);

        let mut bucket_ranges = Vec::with_capacity(buckets);

        for b in 0..buckets {
            // Use logarithmic spacing for better perceptual representation
            let log_start = (b as f32 / buckets as f32).powi(2);
            let log_end = ((b + 1) as f32 / buckets as f32).powi(2);

            let start_hz = freq_min + (freq_max - freq_min) * log_start;
            let end_hz = freq_min + (freq_max - freq_min) * log_end;

            let start_bin = ((start_hz * window_size as f32) / sample_rate as f32) as usize;
            let mut end_bin = ((end_hz * window_size as f32) / sample_rate as f32) as usize;

            // Ensure each bucket has at least one bin
            if end_bin <= start_bin {
                end_bin = start_bin + 1;
            }

            // Clamp to valid range
            let start_bin = start_bin.min(window_size / 2);
            let end_bin = end_bin.min(window_size / 2);

            bucket_ranges.push((start_bin, end_bin));
        }

        Self {
            fft,
            window,
            bucket_ranges,
            fft_input: vec![Complex32::new(0.0, 0.0); window_size],
            peak_normalized: vec![0.0; buckets], // Start at zero — first speech frame snaps it up
            buffer: Vec::with_capacity(window_size * 2),
            window_size,
            buckets,
        }
    }

    pub fn feed(&mut self, samples: &[f32]) -> Option<Vec<f32>> {
        // Add new samples to buffer
        self.buffer.extend_from_slice(samples);

        // Only process if we have enough samples
        if self.buffer.len() < self.window_size {
            return None;
        }

        // Take the required window of samples
        let window_samples = &self.buffer[..self.window_size];

        // Remove DC component
        let mean = window_samples.iter().sum::<f32>() / self.window_size as f32;

        // Apply window function and prepare FFT input
        for (i, &sample) in window_samples.iter().enumerate() {
            let windowed_sample = (sample - mean) * self.window[i];
            self.fft_input[i] = Complex32::new(windowed_sample, 0.0);
        }

        // Perform FFT
        self.fft.process(&mut self.fft_input);

        // Compute power spectrum and bucket levels
        let mut buckets = vec![0.0; self.buckets];

        for (bucket_idx, &(start_bin, end_bin)) in self.bucket_ranges.iter().enumerate() {
            if start_bin >= end_bin || end_bin > self.fft_input.len() / 2 {
                continue;
            }

            // Calculate average power in this frequency range
            let mut power_sum = 0.0;
            for bin_idx in start_bin..end_bin {
                let magnitude = self.fft_input[bin_idx].norm();
                power_sum += magnitude * magnitude;
            }

            let avg_power = power_sum / (end_bin - start_bin) as f32;

            // Convert to dB with proper scaling
            let db = if avg_power > 1e-12 {
                20.0 * (avg_power.sqrt() / self.window_size as f32).log10()
            } else {
                -80.0 // Very low floor for zero power
            };

            // Fixed-window normalization (base level, 0..1)
            let normalized = ((db - DB_MIN) / (DB_MAX - DB_MIN)).clamp(0.0, 1.0);

            // Peak tracking: instant snap up, slow decay.
            // The peak is tracked on the *normalized* value so the boost
            // auto-calibrates to the mic's loudness.
            if normalized > self.peak_normalized[bucket_idx] {
                self.peak_normalized[bucket_idx] = normalized; // instant snap
            } else {
                self.peak_normalized[bucket_idx] = PEAK_DECAY_ALPHA * normalized
                    + (1.0 - PEAK_DECAY_ALPHA) * self.peak_normalized[bucket_idx];
            }

            // Gain boost: scale up so the peak fills the bar range.
            // A quiet mic whose peak is at 0.3 gets a 3.3× boost, making
            // their bars move as if they had a loud mic.
            let boost = if self.peak_normalized[bucket_idx] > 0.05 {
                (1.0 / self.peak_normalized[bucket_idx]).min(MAX_BOOST)
            } else {
                1.0
            };
            let boosted = (normalized * boost).clamp(0.0, 1.0);
            let bucket_value = (boosted * GAIN).powf(CURVE_POWER).clamp(0.0, 1.0);
            buckets[bucket_idx] = bucket_value;

            // Debug: Log first bucket value periodically to track signal levels
            if bucket_idx == 0 {
                log::debug!(
                    "[visualizer] bucket_0: db={:.1} normalized={:.3} peak={:.3} boost={:.2} value={:.3}",
                    db, normalized, self.peak_normalized[bucket_idx], boost, bucket_value
                );
            }
        }

        // Apply minimal smoothing to reduce jitter while maintaining responsiveness
        for i in 1..buckets.len() - 1 {
            buckets[i] = buckets[i] * 0.85 + buckets[i - 1] * 0.075 + buckets[i + 1] * 0.075;
        }

        // Clear processed samples from buffer
        self.buffer.clear();

        Some(buckets)
    }

    pub fn reset(&mut self) {
        self.buffer.clear();
        self.peak_normalized.fill(0.0);
        log::debug!("AudioVisualiser reset: peak_normalized reset to 0");
    }
}
