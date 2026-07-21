// Parakeet Adaptive Threshold Test Harness
//
// Analyzes stored recordings to evaluate audio quality metrics and
// reports which recordings would benefit from lowered confidence thresholds.
//
// Run the synthetic tests with:
//   cargo test -p handy --test parakeet_thresholds -- --nocapture
//
// Run the real recordings analysis with:
//   cargo test -p handy --test parakeet_thresholds analyze_parakeet_thresholds -- --nocapture --ignored
//
// TODO: `AudioQualityMetrics` and `compute_audio_quality` were removed during
// upstream alignment. All tests in this file are ignored until they are re-added.

#[test]
#[ignore = "AudioQualityMetrics and compute_audio_quality not yet ported from main"]
fn placeholder_parakeet_thresholds() {
    // All tests in this file depend on AudioQualityMetrics which doesn't exist yet
}
