//! Property-based tests for audio utility functions.
//!
//! Uses proptest to verify invariants of `AudioVisualiser::feed` and
//! `NoiseSuppressor::process`.
//!
//! **Approach for NaN/inf**: We filter out NaN and infinity from strategies
//! where they would produce undefined metrics (log10 of NaN, etc.).
//! For the no-panic tests, we use `std::panic::catch_unwind` and assert
//! no panic occurs.
//!
//! **Removed symbols**: `AudioQualityMetrics`, `validate_audio`,
//! `AudioValidationResult`, and `compute_audio_quality` were removed during
//! upstream alignment. When they are re-added, add proptest tests for:
//! - `compute` never panics on finite input
//! - `compute` peak_amplitude == max(|samples|)
//! - `compute` peak_dbfs ≈ 20 * log10(peak_amplitude) for non-zero peaks
//! - `validate_audio` never panics on any input
//! - `validate_audio` empty input → Corrupted
//! - `validate_audio` Valid.sample_count == samples.len()
//! - `validate_audio` Silent iff max_amplitude < 0.01 && rms < 0.005

use handy_app_lib::audio_toolkit::audio::AudioVisualiser;
use handy_app_lib::audio_toolkit::{NoiseSuppressor, NOISE_SUPPRESSION_FRAME_SIZE};
use handy_app_lib::settings::NoiseSuppressionLevel;
use proptest::prelude::*;

// ─── Strategies ──────────────────────────────────────────────────────────

/// Generate f32 samples in normalized audio range [-1.0, 1.0].
fn audio_samples() -> impl Strategy<Value = Vec<f32>> {
    prop::collection::vec(-1.0f32..1.0f32, 0..1000)
}

const SAMPLE_RATE: u32 = 16000;

// ─── AudioVisualiser::feed ─────────────────────────────────────────────

proptest! {
    /// Invariant: when feed returns Some(vec), vec.len() == self.buckets
    #[test]
    fn proptest_visualiser_bucket_count(
        samples in prop::collection::vec(-1.0f32..1.0f32, 256..2048)
    ) {
        let mut vis = AudioVisualiser::new(16000, 256, 8, 80.0, 8000.0);
        let result: Option<Vec<f32>> = vis.feed(&samples);

        if let Some(ref buckets) = result {
            prop_assert_eq!(buckets.len(), 8,
                "Expected 8 buckets, got {}", buckets.len());
        }
    }

    /// Invariant: all bucket values are in [0.0, 1.0]
    #[test]
    fn proptest_visualiser_bucket_range(
        samples in prop::collection::vec(-1.0f32..1.0f32, 256..2048)
    ) {
        let mut vis = AudioVisualiser::new(16000, 256, 8, 80.0, 8000.0);
        let result: Option<Vec<f32>> = vis.feed(&samples);

        if let Some(ref buckets) = result {
            for (i, &val) in buckets.iter().enumerate() {
                prop_assert!(val >= 0.0 && val <= 1.0,
                    "Bucket {} out of range: {}", i, val);
            }
        }
    }

    /// Invariant: None when buffer not full (window_size=256)
    #[test]
    fn proptest_visualiser_none_when_buffer_not_full(
        samples in prop::collection::vec(-1.0f32..1.0f32, 0..255)
    ) {
        let mut vis = AudioVisualiser::new(16000, 256, 8, 80.0, 8000.0);
        let result: Option<Vec<f32>> = vis.feed(&samples);
        prop_assert!(result.is_none(),
            "Expected None when buffer not full, got Some");
    }
}

// ─── NoiseSuppressor::process ───────────────────────────────────────────

proptest! {
    /// Invariant: wrong frame size returns input unchanged
    #[test]
    fn proptest_noise_suppressor_wrong_frame_size(
        samples in prop::collection::vec(-1.0f32..1.0f32, 0..500)
            .prop_filter("not exactly 480 samples",
                |s| s.len() != NOISE_SUPPRESSION_FRAME_SIZE)
    ) {
        let mut ns = NoiseSuppressor::new(NoiseSuppressionLevel::Medium);
        let result = ns.process(&samples);
        prop_assert_eq!(result.len(), samples.len(),
            "Wrong frame size should return input unchanged (same length)");
        for (i, (a, b)) in samples.iter().zip(result.iter()).enumerate() {
            prop_assert!((a - b).abs() < f32::EPSILON,
                "Sample {} differs: {} vs {}", i, a, b);
        }
    }

    /// Invariant: first call with correct frame size returns original samples
    /// (the source returns samples.to_vec() for the first frame)
    #[test]
    fn proptest_noise_suppressor_first_call_returns_original(
        samples in prop::collection::vec(-1.0f32..1.0f32, NOISE_SUPPRESSION_FRAME_SIZE)
    ) {
        let mut ns = NoiseSuppressor::new(NoiseSuppressionLevel::Low);
        let result = ns.process(&samples);
        prop_assert_eq!(result.len(), NOISE_SUPPRESSION_FRAME_SIZE,
            "First call should return {} samples, got {}",
            NOISE_SUPPRESSION_FRAME_SIZE, result.len());
        // First call returns the original samples as-is
        for (i, (a, b)) in samples.iter().zip(result.iter()).enumerate() {
            prop_assert!((a - b).abs() < f32::EPSILON,
                "First call sample {} differs: {} vs {}", i, a, b);
        }
    }

    /// Invariant: subsequent calls with correct frame size return exactly 480 samples
    #[test]
    fn proptest_noise_suppressor_subsequent_calls(
        samples1 in prop::collection::vec(-1.0f32..1.0f32, NOISE_SUPPRESSION_FRAME_SIZE),
        samples2 in prop::collection::vec(-1.0f32..1.0f32, NOISE_SUPPRESSION_FRAME_SIZE),
    ) {
        let mut ns = NoiseSuppressor::new(NoiseSuppressionLevel::Medium);
        let _first = ns.process(&samples1);
        let result = ns.process(&samples2);

        prop_assert_eq!(result.len(), NOISE_SUPPRESSION_FRAME_SIZE,
            "Subsequent call should return {} samples, got {}",
            NOISE_SUPPRESSION_FRAME_SIZE, result.len());
    }
}
