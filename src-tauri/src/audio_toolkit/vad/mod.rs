use anyhow::Result;

use crate::audio_toolkit::constants;

pub const VAD_PREFILL_MS: u64 = 450;
pub const VAD_OFFLINE_HANGOVER_MS: u64 = 450;
pub const VAD_STREAMING_HANGOVER_MS: u64 = 1650;
pub const VAD_ONSET_MS: u64 = 60;

/// Convert a VAD timing duration to whole detector frames, rounding up so an
/// alternate backend never shortens Handy's onset, pre-roll, or hangover tail.
pub const fn frames_for_duration_ms(duration_ms: u64, frame_samples: usize) -> usize {
    assert!(frame_samples > 0, "VAD frame size must be non-zero");
    let numerator = duration_ms * constants::WHISPER_SAMPLE_RATE as u64;
    let denominator = frame_samples as u64 * 1000;
    numerator.div_ceil(denominator) as usize
}

pub enum VadFrame<'a> {
    /// Speech – may aggregate several frames (prefill + current + hangover)
    Speech(&'a [f32]),
    /// Non-speech (silence, noise). Down-stream code can ignore it.
    Noise,
}

impl<'a> VadFrame<'a> {
    #[inline]
    pub fn is_speech(&self) -> bool {
        matches!(self, VadFrame::Speech(_))
    }
}

pub trait VoiceActivityDetector: Send + Sync {
    /// Primary streaming API: feed one backend-sized frame, get a keep/drop decision.
    fn push_frame<'a>(&'a mut self, frame: &'a [f32]) -> Result<VadFrame<'a>>;

    /// Required number of mono 16 kHz samples per prediction.
    fn frame_samples(&self) -> usize;

    fn is_voice(&mut self, frame: &[f32]) -> Result<bool> {
        Ok(self.push_frame(frame)?.is_speech())
    }

    /// Set the post-speech hangover tail (in backend-sized frames) applied to
    /// subsequent frames. Detectors without a smoothing tail can ignore this.
    fn set_hangover_frames(&mut self, _frames: usize) {}

    /// End-of-recording diagnostic snapshot, taken after the final frame.
    /// Purely observational — implementations must not change what they emit.
    /// Detectors without smoothing state return None.
    fn tail_report(&self) -> Option<VadTailReport> {
        None
    }

    fn reset(&mut self) {}
}

/// End-of-recording snapshot of a smoothing detector's state. Voiced frames
/// in the withheld tail suggest — but don't prove — a final word cut off at
/// the stop; a clean report doesn't rule VAD loss out either (soft trailing
/// speech can be classified as noise).
#[derive(Debug, Clone, Copy)]
pub struct VadTailReport {
    /// Trailing frames buffered but never emitted downstream.
    pub withheld_frames: usize,
    /// How many of those withheld frames the inner VAD classified as voiced.
    pub withheld_voiced_frames: usize,
    pub in_speech: bool,
    /// Voiced frames counted toward an unconfirmed speech onset.
    pub onset_counter: usize,
    pub hangover_counter: usize,
}

mod earshot;
mod silero;
mod smoothed;

pub use earshot::EarshotVad;
pub use silero::SileroVad;
pub use smoothed::SmoothedVad;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_profiles_preserve_silero_timings() {
        // Silero v6.2 (transcribe.cpp GGUF): 512-sample frames = 32 ms.
        assert_eq!(frames_for_duration_ms(VAD_PREFILL_MS, 512), 15);
        assert_eq!(frames_for_duration_ms(VAD_OFFLINE_HANGOVER_MS, 512), 15);
        assert_eq!(frames_for_duration_ms(VAD_STREAMING_HANGOVER_MS, 512), 52);
        assert_eq!(frames_for_duration_ms(VAD_ONSET_MS, 512), 2);
    }

    #[test]
    fn duration_profiles_round_up_for_earshot_frames() {
        assert_eq!(frames_for_duration_ms(VAD_PREFILL_MS, 256), 29);
        assert_eq!(frames_for_duration_ms(VAD_OFFLINE_HANGOVER_MS, 256), 29);
        assert_eq!(frames_for_duration_ms(VAD_STREAMING_HANGOVER_MS, 256), 104);
        assert_eq!(frames_for_duration_ms(VAD_ONSET_MS, 256), 4);
    }
}

/// Trim trailing silence from audio samples using Silero VAD.
///
/// Runs Silero VAD forward through the audio in the model's frame size
/// (512 samples = 32 ms for Silero v6.2), tracks the last frame classified as
/// speech, then truncates the audio after that point (plus a small hangover
/// pad to avoid clipping final consonants).
///
/// Also preserves a small buffer of leading silence before the first speech
/// to avoid clipping the beginning of short sentences.
///
/// Returns the trimmed samples. If VAD creation fails or no speech is detected,
/// returns the original audio unchanged (safe fallback).
pub fn trim_trailing_silence(audio: &[f32], vad_path: &str, threshold: f32) -> Vec<f32> {
    use crate::audio_toolkit::constants;

    // Keep 300ms of audio after the last detected speech frame
    // to avoid clipping final consonants/tails of words (increased from 150ms
    // to fix short sentence clipping)
    const HANGOVER_MS: u64 = 300;
    // Keep 200ms of audio before the first detected speech frame
    // to avoid clipping the beginning of short sentences.
    // Increased from 100ms (3 frames) to 200ms (7 frames) to provide more
    // margin when VAD detects speech slightly late (e.g., soft first syllable).
    // The recording VAD has an onset delay of ~60-90ms before it marks speech,
    // and the first syllable may be quieter than the detection threshold.
    // A 200ms buffer ensures we capture the full first word even if VAD
    // triggers partway through it.
    const LEADING_BUFFER_MS: u64 = 200;

    let mut vad = match SileroVad::new(vad_path, threshold) {
        Ok(v) => v,
        Err(e) => {
            log::warn!(
                "VAD trim: failed to create Silero VAD ({}), skipping trim",
                e
            );
            return audio.to_vec();
        }
    };

    // Frame geometry comes from the loaded model (512 samples = 32 ms for
    // Silero v6.2) so this stays correct across model swaps.
    let frame_samples = vad.frame_samples();
    let hangover_samples = crate::audio_toolkit::vad::frames_for_duration_ms(
        HANGOVER_MS,
        frame_samples,
    ) * frame_samples;
    let leading_buffer_samples = crate::audio_toolkit::vad::frames_for_duration_ms(
        LEADING_BUFFER_MS,
        frame_samples,
    ) * frame_samples;

    if audio.len() < frame_samples {
        return audio.to_vec();
    }

    // Scan forward through the audio, tracking first and last speech frames.
    let total_frames = audio.len() / frame_samples;
    let mut first_speech_frame_start: Option<usize> = None;
    let mut last_speech_frame_end: usize = 0;

    for frame_idx in 0..total_frames {
        let start = frame_idx * frame_samples;
        let end = start + frame_samples;
        let frame = &audio[start..end];

        match vad.push_frame(frame) {
            Ok(vad_frame) if vad_frame.is_speech() => {
                if first_speech_frame_start.is_none() {
                    first_speech_frame_start = Some(start);
                }
                last_speech_frame_end = end;
            }
            Ok(_) => {} // Noise frame, continue scanning
            Err(e) => {
                log::debug!("VAD trim: frame {} error ({}), stopping scan", frame_idx, e);
                break;
            }
        }
    }

    if last_speech_frame_end == 0 {
        // No speech detected — return the original audio as-is
        // rather than returning empty (could be VAD misfire on very quiet speech)
        return audio.to_vec();
    }

    // Calculate the start point with leading buffer to preserve beginning of speech
    let trimmed_start = match first_speech_frame_start {
        Some(first_start) => first_start.saturating_sub(leading_buffer_samples),
        None => 0,
    };

    // Pad the cut point with a small hangover to avoid clipping
    let trimmed_len = (last_speech_frame_end + hangover_samples).min(audio.len());

    if trimmed_len >= audio.len() && trimmed_start == 0 {
        // Nothing to trim
        return audio.to_vec();
    }

    log::debug!(
        "VAD trim: {} samples -> {} samples (trimmed {}ms from start, {}ms from end)",
        audio.len(),
        trimmed_len - trimmed_start,
        trimmed_start * 1000 / constants::WHISPER_SAMPLE_RATE as usize,
        (audio.len() - trimmed_len) * 1000 / constants::WHISPER_SAMPLE_RATE as usize,
    );

    audio[trimmed_start..trimmed_len].to_vec()
}
