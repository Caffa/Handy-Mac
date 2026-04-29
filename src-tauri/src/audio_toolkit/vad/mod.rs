use anyhow::Result;

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
    /// Primary streaming API: feed one 30-ms frame, get keep/drop decision.
    fn push_frame<'a>(&'a mut self, frame: &'a [f32]) -> Result<VadFrame<'a>>;

    fn is_voice(&mut self, frame: &[f32]) -> Result<bool> {
        Ok(self.push_frame(frame)?.is_speech())
    }

    fn reset(&mut self) {}
}

mod silero;
mod smoothed;

pub use silero::SileroVad;
pub use smoothed::SmoothedVad;

/// Trim trailing silence from audio samples using Silero VAD.
///
/// Runs Silero VAD forward through the audio in 30ms frames, tracks the last
/// frame classified as speech, then truncates the audio after that point
/// (plus a small hangover pad to avoid clipping final consonants).
///
/// Returns the trimmed samples. If VAD creation fails or no speech is detected,
/// returns the original audio unchanged (safe fallback).
pub fn trim_trailing_silence(
    audio: &[f32],
    vad_path: &str,
    threshold: f32,
) -> Vec<f32> {
    use crate::audio_toolkit::constants;

    const FRAME_MS: u32 = 30;
    const FRAME_SAMPLES: usize =
        (constants::WHISPER_SAMPLE_RATE as usize * FRAME_MS as usize) / 1000;
    // Keep 150ms of audio after the last detected speech frame
    // to avoid clipping final consonants/tails of words
    const HANGOVER_FRAMES: usize = 5;
    const HANGOVER_SAMPLES: usize = HANGOVER_FRAMES * FRAME_SAMPLES;

    if audio.len() < FRAME_SAMPLES {
        return audio.to_vec();
    }

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

    // Scan forward through the audio, tracking the last speech frame.
    let total_frames = audio.len() / FRAME_SAMPLES;
    let mut last_speech_frame_end: usize = 0;

    for frame_idx in 0..total_frames {
        let start = frame_idx * FRAME_SAMPLES;
        let end = start + FRAME_SAMPLES;
        let frame = &audio[start..end];

        match vad.push_frame(frame) {
            Ok(vad_frame) if vad_frame.is_speech() => {
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

    // Pad the cut point with a small hangover to avoid clipping
    let trimmed_len = (last_speech_frame_end + HANGOVER_SAMPLES).min(audio.len());

    if trimmed_len >= audio.len() {
        // Nothing to trim
        return audio.to_vec();
    }

    log::debug!(
        "VAD trim: {} samples -> {} samples (removed {}ms of trailing silence)",
        audio.len(),
        trimmed_len,
        (audio.len() - trimmed_len) * 1000 / constants::WHISPER_SAMPLE_RATE as usize,
    );

    audio[..trimmed_len].to_vec()
}
