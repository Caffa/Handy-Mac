//! Property-based tests for audio utility functions.
//!
//! Uses proptest to verify invariants of `AudioQualityMetrics::compute`,
//! `validate_audio`, `AudioVisualiser::feed`, and `NoiseSuppressor::process`.
//!
//! **Approach for NaN/inf**: We filter out NaN and infinity from strategies
//! where they would produce undefined metrics (log10 of NaN, etc.).
//! For the no-panic tests, we use `std::panic::catch_unwind` and assert
//! no panic occurs.
//!
//! **Bug found and fixed**: `AudioQualityMetrics::compute` previously panicked
//! on NaN input because `sort_unstable_by` used `.partial_cmp(b).unwrap()`,
//! which returns `None` for NaN. Fixed by switching to `total_cmp()`, which
//! provides a total order including NaN. See `compute_nan_does_not_panic`
//! regression test.

use handy_app_lib::audio_toolkit::audio::{AudioQualityMetrics, AudioVisualiser};
use handy_app_lib::audio_toolkit::{validate_audio, AudioValidationResult, NoiseSuppressor, NOISE_SUPPRESSION_FRAME_SIZE};
use handy_app_lib::settings::NoiseSuppressionLevel;
use proptest::prelude::*;

// ─── Strategies ──────────────────────────────────────────────────────────

/// Generate f32 samples in normalized audio range [-1.0, 1.0].
fn audio_samples() -> impl Strategy<Value = Vec<f32>> {
    prop::collection::vec(-1.0f32..1.0f32, 0..1000)
}

/// Generate f32 samples including edge cases (NaN, infinity, very small, very large).
/// Used for no-panic tests where we just verify the function doesn't crash.
fn audio_samples_any() -> impl Strategy<Value = Vec<f32>> {
    prop::collection::vec(prop::num::f32::ANY, 0..500)
}

/// Generate f32 samples excluding NaN and infinity (finite values only).
/// Used for tests where NaN/inf would produce undefined metrics.
fn audio_samples_finite() -> impl Strategy<Value = Vec<f32>> {
    prop::collection::vec(
        prop::num::f32::ANY.prop_filter("finite", |v| v.is_finite()),
        0..500,
    )
}

const SAMPLE_RATE: u32 = 16000;

// ─── AudioQualityMetrics::compute ───────────────────────────────────────

proptest! {
    /// Invariant: peak_amplitude == max(|samples|), for well-behaved samples.
    #[test]
    fn proptest_compute_peak_amplitude(samples in audio_samples()) {
        prop_assume!(!samples.is_empty());

        let metrics = AudioQualityMetrics::compute(&samples);

        let expected_peak = samples.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
        prop_assert!((metrics.peak_amplitude - expected_peak).abs() < f32::EPSILON,
            "peak_amplitude mismatch: got {}, expected {}",
            metrics.peak_amplitude, expected_peak);

        let expected_duration = samples.len() as f32 / SAMPLE_RATE as f32;
        prop_assert!((metrics.duration_secs - expected_duration).abs() < 1e-6,
            "duration_secs mismatch: got {}, expected {}",
            metrics.duration_secs, expected_duration);
    }

    /// Invariant: for non-empty samples with peak_amplitude > 1e-10,
    /// peak_dbfs ≈ 20 * log10(peak_amplitude).
    #[test]
    fn proptest_compute_peak_dbfs(samples in audio_samples()) {
        prop_assume!(!samples.is_empty());

        let metrics = AudioQualityMetrics::compute(&samples);

        if metrics.peak_amplitude > 1e-10 {
            let expected_dbfs = 20.0 * metrics.peak_amplitude.log10();
            // Allow ~0.1 dB tolerance for floating-point imprecision
            prop_assert!((metrics.peak_dbfs - expected_dbfs).abs() < 0.1,
                "peak_dbfs mismatch: got {}, expected {}",
                metrics.peak_dbfs, expected_dbfs);
        } else {
            // Very quiet audio should clamp to -96.0
            prop_assert!((metrics.peak_dbfs - (-96.0f32)).abs() < f32::EPSILON,
                "peak_dbfs should be -96.0 for near-silent audio, got {}",
                metrics.peak_dbfs);
        }
    }

    /// Invariant: compute never panics on well-behaved (finite) input.
    ///
    /// NaN behavior is tested separately in `compute_nan_does_not_panic`.
    #[test]
    fn proptest_compute_no_panic(samples in audio_samples_finite()) {
        let result = std::panic::catch_unwind(|| {
            AudioQualityMetrics::compute(&samples)
        });
        prop_assert!(result.is_ok(),
            "AudioQualityMetrics::compute panicked on finite input: {:?}", samples);
    }
}

// Separate plain tests for invariants that don't need proptest strategies:

#[test]
fn compute_empty_input_metrics() {
    let metrics = AudioQualityMetrics::compute(&[]);
    assert_eq!(metrics.peak_amplitude, 0.0);
    assert!((metrics.peak_dbfs - (-96.0f32)).abs() < f32::EPSILON);
    assert_eq!(metrics.duration_secs, 0.0);
}

// ─── NaN Regression Test ────────────────────────────────────────────────

/// Regression test: `compute` must NOT panic on NaN input.
///
/// Previously, `sort_unstable_by(|a, b| a.partial_cmp(b).unwrap())` would
/// panic because `partial_cmp(NaN, x)` returns `None`. This was fixed by
/// switching to `total_cmp`, which handles NaN deterministically.
///
/// We need ≥101 samples so that `step_by(100)` yields ≥2 elements for sorting.
#[test]
fn compute_nan_does_not_panic() {
    let mut samples = vec![0.0f32; 200];
    samples[0] = f32::NAN;
    samples[100] = f32::NAN;
    let result = std::panic::catch_unwind(|| {
        AudioQualityMetrics::compute(&samples)
    });
    assert!(result.is_ok(),
        "AudioQualityMetrics::compute panicked on NaN input — regression of the total_cmp fix.");
}

// ─── validate_audio ─────────────────────────────────────────────────────

proptest! {
    /// Invariant: never panics on any input including NaN/inf.
    #[test]
    fn proptest_validate_no_panic(samples in audio_samples_any(), sample_rate in 1u32..96000) {
        let _ = validate_audio(&samples, sample_rate);
    }

    /// Invariant: Valid.sample_count == samples.len()
    #[test]
    fn proptest_validate_sample_count(samples in audio_samples()) {
        prop_assume!(!samples.is_empty());

        let result = validate_audio(&samples, SAMPLE_RATE);
        if let AudioValidationResult::Valid { sample_count, .. } = result {
            prop_assert_eq!(sample_count, samples.len(),
                "sample_count should equal samples.len()");
        }
    }

    /// Invariant: Silent iff max_amplitude < 0.01 && rms < 0.005
    #[test]
    fn proptest_validate_silent_classification(samples in audio_samples()) {
        prop_assume!(!samples.is_empty());

        let result = validate_audio(&samples, SAMPLE_RATE);

        let max_amp = samples.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
        let rms = (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt();

        let is_silent = max_amp < 0.01 && rms < 0.005;

        match result {
            AudioValidationResult::Silent { max_amplitude, .. } => {
                prop_assert!(is_silent,
                    "Classified as Silent but max_amp={} rms={}", max_amp, rms);
                prop_assert!((max_amplitude - max_amp).abs() < f32::EPSILON);
            }
            AudioValidationResult::Valid { .. } => {
                prop_assert!(!is_silent,
                    "Classified as Valid but should be Silent (max_amp={} rms={})", max_amp, rms);
            }
            AudioValidationResult::Corrupted { .. } => {
                prop_assert!(false, "Non-empty audio should not be Corrupted");
            }
        }
    }
}

#[test]
fn validate_empty_input_is_corrupted() {
    let result = validate_audio(&[], SAMPLE_RATE);
    if let AudioValidationResult::Corrupted { error } = result {
        assert!(error.contains("Empty"), "Expected 'Empty' in error, got: {}", error);
    } else {
        panic!("Expected Corrupted for empty input, got {:?}", result);
    }
}

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