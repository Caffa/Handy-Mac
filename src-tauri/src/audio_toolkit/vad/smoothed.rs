use super::{VadFrame, VadTailReport, VoiceActivityDetector};
use anyhow::Result;
use std::collections::VecDeque;

/// One pre-roll buffer slot. `emitted` and `voiced` exist only to power the
/// end-of-recording `tail_report()` diagnostic; they never affect emission.
struct BufferedFrame {
    samples: Vec<f32>,
    emitted: bool,
    voiced: bool,
}

pub struct SmoothedVad {
    inner_vad: Box<dyn VoiceActivityDetector>,
    prefill_frames: usize,
    hangover_frames: usize,
    onset_frames: usize,

    frame_buffer: VecDeque<BufferedFrame>,
    hangover_counter: usize,
    onset_counter: usize,
    in_speech: bool,

    temp_out: Vec<f32>,
}

impl SmoothedVad {
    pub fn new(
        inner_vad: Box<dyn VoiceActivityDetector>,
        prefill_frames: usize,
        hangover_frames: usize,
        onset_frames: usize,
    ) -> Self {
        Self {
            inner_vad,
            prefill_frames,
            hangover_frames,
            onset_frames,
            frame_buffer: VecDeque::new(),
            hangover_counter: 0,
            onset_counter: 0,
            in_speech: false,
            temp_out: Vec::new(),
        }
    }

    fn mark_last_emitted(&mut self) {
        if let Some(frame) = self.frame_buffer.back_mut() {
            frame.emitted = true;
        }
    }
}

impl VoiceActivityDetector for SmoothedVad {
    fn push_frame<'a>(&'a mut self, frame: &'a [f32]) -> Result<VadFrame<'a>> {
        // 1. Buffer every incoming frame for possible pre-roll
        self.frame_buffer.push_back(BufferedFrame {
            samples: frame.to_vec(),
            emitted: false,
            voiced: false,
        });
        while self.frame_buffer.len() > self.prefill_frames + 1 {
            self.frame_buffer.pop_front();
        }

        // 2. Delegate to the wrapped boolean VAD
        let is_voice = self.inner_vad.is_voice(frame)?;
        if let Some(last) = self.frame_buffer.back_mut() {
            last.voiced = is_voice;
        }

        match (self.in_speech, is_voice) {
            // Potential start of speech - need to accumulate onset frames
            (false, true) => {
                self.onset_counter += 1;
                if self.onset_counter >= self.onset_frames {
                    // We have enough consecutive voice frames to trigger speech
                    self.in_speech = true;
                    self.hangover_counter = self.hangover_frames;
                    self.onset_counter = 0; // Reset for next time

                    // Collect prefill + current frame (skip frames already
                    // emitted during a previous speech segment's hangover —
                    // re-emitting them duplicates audio when speech pauses
                    // past the hangover and re-triggers onset mid-recording)
                    self.temp_out.clear();
                    for buffered in self.frame_buffer.iter_mut() {
                        if !buffered.emitted {
                            self.temp_out.extend(buffered.samples.iter());
                        }
                        buffered.emitted = true;
                    }
                    Ok(VadFrame::Speech(&self.temp_out))
                } else {
                    // Not enough frames yet, still silence
                    Ok(VadFrame::Noise)
                }
            }

            // Ongoing Speech
            (true, true) => {
                self.hangover_counter = self.hangover_frames;
                self.mark_last_emitted();
                Ok(VadFrame::Speech(frame))
            }

            // End of Speech or interruption during onset phase
            (true, false) => {
                if self.hangover_counter > 0 {
                    self.hangover_counter -= 1;
                    self.mark_last_emitted();
                    Ok(VadFrame::Speech(frame))
                } else {
                    self.in_speech = false;
                    Ok(VadFrame::Noise)
                }
            }

            // Silence or broken onset sequence
            (false, false) => {
                self.onset_counter = 0; // Reset onset counter on silence
                Ok(VadFrame::Noise)
            }
        }
    }

    fn frame_samples(&self) -> usize {
        self.inner_vad.frame_samples()
    }

    fn set_hangover_frames(&mut self, frames: usize) {
        self.hangover_frames = frames;
    }

    /// Trailing run of withheld frames plus smoothing state. Interior
    /// withheld frames (before already-emitted speech) are not counted.
    fn tail_report(&self) -> Option<VadTailReport> {
        let mut withheld_frames = 0;
        let mut withheld_voiced_frames = 0;
        for frame in self
            .frame_buffer
            .iter()
            .rev()
            .take_while(|frame| !frame.emitted)
        {
            withheld_frames += 1;
            if frame.voiced {
                withheld_voiced_frames += 1;
            }
        }

        Some(VadTailReport {
            withheld_frames,
            withheld_voiced_frames,
            in_speech: self.in_speech,
            onset_counter: self.onset_counter,
            hangover_counter: self.hangover_counter,
        })
    }

    fn reset(&mut self) {
        self.inner_vad.reset();
        self.frame_buffer.clear();
        self.hangover_counter = 0;
        self.onset_counter = 0;
        self.in_speech = false;
        self.temp_out.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A trivial energy-based inner VAD: voiced for frames whose RMS exceeds a threshold.
    struct EnergyVad {
        threshold: f32,
        frame_samples: usize,
    }

    impl VoiceActivityDetector for EnergyVad {
        fn push_frame<'a>(&'a mut self, frame: &'a [f32]) -> Result<VadFrame<'a>> {
            let rms = (frame.iter().map(|s| s * s).sum::<f32>() / frame.len() as f32).sqrt();
            if rms > self.threshold {
                Ok(VadFrame::Speech(frame))
            } else {
                Ok(VadFrame::Noise)
            }
        }

        fn frame_samples(&self) -> usize {
            self.frame_samples
        }
    }

    #[test]
    fn smoothed_vad_preroll_is_emitted_into_out_buf() {
        // 10 frames of silence, then 4 voiced frames (onset = 4), then more voiced.
        // SmoothedVad::new(inner, prefill=20, hangover=5, onset=4).
        // After the onset, the FIRST emission must contain 20 prefill + onset
        // frames worth of samples (silence + voiced), NOT just the current frame.
        let mut vad = SmoothedVad::new(
            Box::new(EnergyVad {
                threshold: 0.01,
                frame_samples: 256,
            }),
            20,
            5,
            4,
        );

        let silence = vec![0.0f32; 256];
        let voiced = vec![0.5f32; 256];
        let mut buf: Vec<f32> = Vec::new();

        for _ in 0..10 {
            let _ = vad.push_frame(&silence).unwrap();
        }

        // Feed frames one by one; simulate handle_frame's contract manually.
        for _ in 0..6 {
            match vad.push_frame(&voiced).unwrap() {
                VadFrame::Speech(emitted) => {
                    // FIRST emission must include the pre-roll: silence frames
                    // (10*256) + voiced-so-far frames. It must be larger than
                    // one frame (256 samples) — the essence of the bug fix.
                    buf.extend_from_slice(emitted);
                }
                _ => {}
            }
        }

        assert!(
            buf.len() > 256,
            "pre-roll must be emitted with speech onset; got {} samples",
            buf.len()
        );
        // The pre-roll silence must be at the start (10 frames were silence).
        assert!(buf[..10 * 256].iter().all(|s| s.abs() < 1e-6));
        assert_eq!(buf.len() % 256, 0);
    }

    #[test]
    fn re_onset_does_not_duplicate_hangover_frames() {
        // speech -> pause long enough to exhaust hangover -> speech again.
        // The second onset must emit only un-emitted frames (the pause frames
        // still in the prefill window), NOT repeat the hangover-tail frames.
        // Feed: 8 silence, 4 voiced (onset), 3 voiced (ongoing), 8 silence
        // (hangover exhausts after 5), 4 voiced (re-onset).
        // Track total appended across ALL Speech emissions; must equal the
        // number of unique frames fed (i.e., no frame appended twice).
        let mut vad = SmoothedVad::new(
            Box::new(EnergyVad {
                threshold: 0.01,
                frame_samples: 256,
            }),
            20,
            5,
            4,
        );
        let silence = vec![0.0f32; 256];
        let voiced = vec![0.5f32; 256];
        let mut total = 0usize;
        let mut fed = 0usize;
        let mut feed = |frame: &[f32], vad: &mut SmoothedVad| {
            fed += 1;
            if let Ok(VadFrame::Speech(emitted)) = vad.push_frame(frame) {
                total += emitted.len() / 256;
            }
        };
        for _ in 0..8 {
            feed(&silence, &mut vad);
        }
        for _ in 0..7 {
            feed(&voiced, &mut vad);
        } // onset(4) + ongoing(3)
        for _ in 0..8 {
            feed(&silence, &mut vad);
        } // hangover exhausts after 5
        for _ in 0..4 {
            feed(&voiced, &mut vad);
        } // re-onset
        assert_eq!(
            total,
            fed,
            "every fed frame must be appended at most once (re-onset duplicated {} frames)",
            total.saturating_sub(fed)
        );
    }
}
