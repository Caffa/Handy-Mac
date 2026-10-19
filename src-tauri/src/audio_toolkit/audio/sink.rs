//! App-side consumer of `handy-recorder` capture: the VAD-filtering sink.
//!
//! One [`CaptureSink`] per recording. It runs on the recorder's delivery
//! thread (never the real-time audio thread), receives exactly
//! `frames_per_chunk`-sized 16 kHz mono chunks, and turns them into:
//!
//! - spectrum levels for the overlay,
//! - raw (pre-VAD) frames for the live-captions `StreamRouter` seam,
//! - VAD-filtered recording audio in the shared [`CaptureControl`],
//! - liveness / level / voice-activity telemetry for the audio manager.
//!
//! Fork behaviors carried over from the deleted in-house recorder:
//! - `ddffa435`: with noise suppression off, the FULL VAD-emitted slice
//!   (pre-roll + current frame) reaches the recording — the first-word fix.
//! - Noise suppression runs before VAD; VAD sees the denoised chunk while the
//!   recording keeps the original samples (the emitted slice cannot be mapped
//!   back to originals).
//! - Always-on pre-hotkey buffer: a standing sink maintains a rolling raw
//!   buffer; arming it drains the buffer into the recording (bypassing VAD)
//!   and resets VAD/NS state, matching the old `apply_start` semantics.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Instant;

use handy_recorder::{AudioChunk, Sink};
use parking_lot::Mutex;

use crate::audio_toolkit::{
    audio::AudioVisualiser,
    constants,
    noise_suppression::NoiseSuppressor,
    vad::{VadFrame, VoiceActivityDetector},
};
use crate::emergency_save;

/// How 16 kHz mono frames should be filtered for one recording session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VadPolicy {
    /// Bypass VAD and forward every frame.
    Disabled,
    /// Current offline-tuned VAD profile.
    Offline,
    /// VAD profile with a longer post-speech tail for streaming-capable models.
    Streaming,
}

/// A single VAD engine plus the two hangover-tail lengths its smoothing wrapper
/// should use. The offline and streaming policies are never active
/// concurrently, so one detector is reconfigured per recording rather than
/// kept as two resident engines.
pub struct CaptureVad {
    detector: Box<dyn VoiceActivityDetector>,
    offline_hangover_frames: usize,
    streaming_hangover_frames: usize,
}

impl CaptureVad {
    pub fn new(
        detector: Box<dyn VoiceActivityDetector>,
        offline_hangover_frames: usize,
        streaming_hangover_frames: usize,
    ) -> Self {
        assert!(
            detector.frame_samples() > 0,
            "VAD frame size must be non-zero"
        );
        Self {
            detector,
            offline_hangover_frames,
            streaming_hangover_frames,
        }
    }

    /// Samples per detector frame; the recorder delivers chunks of this size.
    pub fn frame_samples(&self) -> usize {
        self.detector.frame_samples()
    }

    /// Post-speech hangover tail (in detector frames) for the given policy.
    /// `Disabled` never reaches the detector, so it maps to the offline value.
    pub fn hangover_for(&self, policy: VadPolicy) -> usize {
        match policy {
            VadPolicy::Streaming => self.streaming_hangover_frames,
            VadPolicy::Offline | VadPolicy::Disabled => self.offline_hangover_frames,
        }
    }
}

/// Receives spectrum levels for the recording overlay.
pub type LevelCallback = Arc<dyn Fn(Vec<f32>) + Send + Sync + 'static>;
/// Receives each 16 kHz mono frame that passes the recording's VAD policy, in
/// order. Used by the audio manager for the live-captions batch gate.
pub type AudioFrameCallback = Arc<dyn Fn(&[f32]) + Send + Sync + 'static>;

/// What a finished recording hands back.
pub struct CapturedAudio {
    /// 16 kHz mono samples that passed the VAD policy (plus any pre-hotkey
    /// buffer drained at arm time).
    pub samples: Vec<f32>,
    /// 16 kHz mono samples as captured — every real frame while armed,
    /// including the ones VAD classified as noise.
    pub raw_samples: Vec<f32>,
    /// The detector the recording borrowed, for the next one.
    pub vad: CaptureVad,
}

/// Manager-owned state shared with the active sink. One instance lives for
/// the whole [`AudioRecordingManager`](crate::managers::audio::AudioRecordingManager)
/// lifetime; only one sink runs at a time, so there is never a concurrent
/// writer. The manager resets the per-recording fields when it arms.
pub struct CaptureControl {
    /// Armed: the sink is capturing a real hotkey recording (as opposed to a
    /// standing always-on recording that only maintains the pre-hotkey buffer).
    pub armed: AtomicBool,
    /// Epoch ms when the current recording was armed (0 = none). Reference for
    /// the voice-activity elapsed stamp and the batch-invoke interval.
    pub armed_at_ms: AtomicU64,
    /// VAD policy for the armed recording; the sink applies the policy's
    /// hangover at arm time. Settable without recreating the sink (a standing
    /// always-on sink outlives the setting change).
    pub policy: Mutex<VadPolicy>,
    /// Rolling pre-hotkey buffer (always-on pre-buffer): raw 16 kHz frames
    /// captured while standing, drained into the recording at arm time.
    pub pre_buffer: Mutex<VecDeque<f32>>,
    pub pre_buffer_max_samples: usize,
    /// True when a dedicated live-captions model's `StreamRouter` consumes the
    /// raw frames; the 2.5s batch fallback stays off while the router runs.
    pub router_attached: bool,
    /// VAD-processed samples of the current recording (the old `out_buf`).
    /// Drained by `CaptureSink::finish`.
    pub processed: Mutex<Vec<f32>>,
    /// Epoch ms of the last delivered chunk — stream liveness.
    pub last_chunk_ms: AtomicU64,
    /// Max RMS·1_000_000 seen while armed (dead-mic detection).
    pub max_level: AtomicU32,
    /// VAD-speech frame count of the current recording (batch gate). Reset to
    /// zero on every noise frame, so the gate counts *consecutive* speech.
    pub speech_frames: AtomicU32,
    /// Elapsed ms (relative to `armed_at_ms`) of the most recent VAD-speech
    /// frame. The coordinator reads this for the sliding recording timeout.
    pub last_voice_activity_ms: AtomicU64,
    /// Running noise-floor estimate (EMA of VAD-noise frame RMS).
    pub noise_floor: Mutex<f32>,
    pub noise_floor_initialised: AtomicBool,
    /// Epoch ms when RMS last exceeded `noise_floor × SILENCE_RMS_MULTIPLIER`.
    pub last_voice_above_floor_ms: AtomicU64,
    /// True while the manager's trailing (smart-stop) window runs: the
    /// noise-floor EMA freezes so the tail does not shift the baseline.
    pub trailing: AtomicBool,
    /// Epoch ms of the last live-captions batch invoke (interval gate).
    pub last_batch_invoke_ms: AtomicU64,
    /// Runtime toggle for the batch streaming callback (Strategy pattern).
    pub streaming_enabled: AtomicBool,
    /// First-chunk readiness signal. The manager installs a sender right
    /// before a recording starts; the sink fires it on the next armed chunk.
    /// The device, not the speech, is what this signals: silence counts.
    pub ready: Mutex<Option<mpsc::Sender<()>>>,
}

impl CaptureControl {
    pub fn new(pre_buffer_max_samples: usize, router_attached: bool) -> Self {
        Self {
            armed: AtomicBool::new(false),
            armed_at_ms: AtomicU64::new(0),
            policy: Mutex::new(VadPolicy::Offline),
            pre_buffer: Mutex::new(VecDeque::with_capacity(pre_buffer_max_samples)),
            pre_buffer_max_samples,
            router_attached,
            processed: Mutex::new(Vec::new()),
            last_chunk_ms: AtomicU64::new(0),
            max_level: AtomicU32::new(0),
            speech_frames: AtomicU32::new(0),
            last_voice_activity_ms: AtomicU64::new(0),
            noise_floor: Mutex::new(DEFAULT_NOISE_FLOOR),
            noise_floor_initialised: AtomicBool::new(false),
            last_voice_above_floor_ms: AtomicU64::new(0),
            trailing: AtomicBool::new(false),
            last_batch_invoke_ms: AtomicU64::new(0),
            streaming_enabled: AtomicBool::new(false),
            ready: Mutex::new(None),
        }
    }

    /// Arm the shared state for a new recording. Called by the manager right
    /// before the recording starts (or before a standing sink is armed).
    pub fn arm(&self, binding_id: &str) {
        let now = epoch_ms();
        self.armed_at_ms.store(now, Ordering::Release);
        self.processed.lock().clear();
        self.max_level.store(0, Ordering::Relaxed);
        self.speech_frames.store(0, Ordering::Relaxed);
        self.last_voice_activity_ms.store(0, Ordering::Relaxed);
        *self.noise_floor.lock() = DEFAULT_NOISE_FLOOR;
        self.noise_floor_initialised.store(false, Ordering::Release);
        self.last_voice_above_floor_ms.store(now, Ordering::Relaxed);
        self.trailing.store(false, Ordering::Release);
        self.last_batch_invoke_ms.store(0, Ordering::Relaxed);
        // Belt-and-suspenders: a new session must not inherit stale state.
        self.armed.store(true, Ordering::Release);
        log::debug!(
            "capture armed for binding '{}' (pre_buffer_max_samples={})",
            binding_id,
            self.pre_buffer_max_samples
        );
    }

    /// Disarm after a recording is collected (or discarded).
    pub fn disarm(&self) {
        self.armed.store(false, Ordering::Release);
        self.trailing.store(false, Ordering::Release);
    }

    /// Max RMS seen while armed, normalized to f32 sample units.
    pub fn get_max_level(&self) -> f32 {
        self.max_level.load(Ordering::Relaxed) as f32 / 1_000_000.0
    }

    /// Epoch ms of the last delivered chunk, for liveness checks.
    pub fn last_chunk_ms(&self) -> u64 {
        self.last_chunk_ms.load(Ordering::Relaxed)
    }

    /// Elapsed ms since arm at which VAD last detected speech.
    pub fn last_voice_activity_ms(&self) -> u64 {
        self.last_voice_activity_ms.load(Ordering::Acquire)
    }
}

// ─── Smart-stop tuning constants (from the deleted in-house recorder) ────
//
// SILENCE_RMS_MULTIPLIER: a frame is "voice" when its RMS exceeds
//   `noise_floor × 3.0` — modest speech keeps the trailing buffer open.
pub const SILENCE_RMS_MULTIPLIER: f32 = 3.0;
// NOISE_ALPHA: EMA coefficient for the noise-floor estimate (0.05 adapts
//   quickly to background changes while resisting brief speech spikes).
pub const NOISE_ALPHA: f32 = 0.05;
// DEFAULT_NOISE_FLOOR: fallback floor when no VAD-noise frame has been
//   observed yet (≈ RMS of −46 dBFS in 16-bit audio).
pub const DEFAULT_NOISE_FLOOR: f32 = 0.005;

fn epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Compute the root-mean-square (RMS) of a slice of f32 samples.
/// Returns 0.0 for empty slices.
#[inline]
fn compute_rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f64 = samples.iter().map(|&s| (s as f64) * (s as f64)).sum();
    (sum / samples.len() as f64).sqrt() as f32
}

/// Everything a recording's sink needs beyond the borrowed detector. Built by
/// the audio manager; the sink never sees transcription types.
pub struct CaptureSinkConfig {
    /// Callbacks for telemetry and the live-captions seams.
    pub on_levels: LevelCallback,
    /// Post-VAD emitted slices, in order (upstream seam).
    pub on_audio: AudioFrameCallback,
    /// Pre-VAD raw frames, in order — the fork's `StreamRouter` seam.
    pub on_raw_frame: Option<AudioFrameCallback>,
    /// Noise suppression before VAD, when the setting is on.
    pub noise_suppressor: Option<Arc<Mutex<NoiseSuppressor>>>,
    /// Shared manager/sink state.
    pub control: Arc<CaptureControl>,
    /// Crash-resilience backup; the sink flushes it periodically, the manager
    /// owns start/complete/cancel.
    pub emergency_backup: Option<Arc<emergency_save::EmergencyBackup>>,
}

/// One recording's consumer of 16 kHz mono audio from handy-recorder. Runs on
/// the recorder's delivery thread, never the real-time audio thread, so it
/// can run VAD inline. Chunks are exactly one VAD frame long.
pub struct CaptureSink {
    vad: CaptureVad,
    on_levels: LevelCallback,
    on_audio: AudioFrameCallback,
    on_raw_frame: Option<AudioFrameCallback>,
    noise_suppressor: Option<Arc<Mutex<NoiseSuppressor>>>,
    control: Arc<CaptureControl>,
    emergency_backup: Option<Arc<emergency_save::EmergencyBackup>>,
    last_backup_update: Option<Instant>,
    /// Raw (pre-VAD) samples of the armed recording.
    raw_samples: Vec<f32>,
    /// Local mirror of `control.armed`, to detect the standing→armed edge.
    armed_locally: bool,
    /// Spectrum window at 16 kHz: 32 ms, near the overlay's ~30 Hz refresh.
    visualizer: AudioVisualiser,
}

const VISUALIZER_WINDOW: usize = 512;
const VISUALIZER_BUCKETS: usize = 16;

impl CaptureSink {
    /// A sink for one recording. Resets the detector and applies the policy's
    /// hangover tail, so no state carries over from the previous recording.
    pub fn new(vad: CaptureVad, config: CaptureSinkConfig) -> Self {
        let mut sink = Self {
            vad,
            on_levels: config.on_levels,
            on_audio: config.on_audio,
            on_raw_frame: config.on_raw_frame,
            noise_suppressor: config.noise_suppressor,
            control: config.control,
            emergency_backup: config.emergency_backup,
            last_backup_update: None,
            raw_samples: Vec::new(),
            armed_locally: false,
            visualizer: AudioVisualiser::new(
                constants::WHISPER_SAMPLE_RATE,
                VISUALIZER_WINDOW,
                VISUALIZER_BUCKETS,
                400.0,
                4000.0,
            ),
        };
        sink.apply_policy_to_vad();
        sink
    }

    fn apply_policy_to_vad(&mut self) {
        let policy = *self.control.policy.lock();
        if policy != VadPolicy::Disabled {
            let hangover = self.vad.hangover_for(policy);
            self.vad.detector.set_hangover_frames(hangover);
            self.vad.detector.reset();
        }
    }

    /// The detector, from a sink that never recorded (its start failed).
    pub fn into_vad(self) -> CaptureVad {
        self.vad
    }

    /// Hands back the recording and the detector. Logs what VAD still held
    /// back when the recording stopped.
    pub fn finish(mut self) -> CapturedAudio {
        // Diagnostic for audio still withheld when capture stopped; it is not
        // conclusive in either direction.
        if *self.control.policy.lock() != VadPolicy::Disabled {
            if let Some(report) = self.vad.detector.tail_report() {
                log::debug!(
                    "VAD at stop: withheld tail {} frames (~{}ms, {} voiced), in_speech={}, onset_counter={}, hangover_counter={}",
                    report.withheld_frames,
                    report.withheld_frames * self.vad.frame_samples() * 1000
                        / constants::WHISPER_SAMPLE_RATE as usize,
                    report.withheld_voiced_frames,
                    report.in_speech,
                    report.onset_counter,
                    report.hangover_counter
                );
            }
        }
        CapturedAudio {
            samples: std::mem::take(&mut *self.control.processed.lock()),
            raw_samples: std::mem::take(&mut self.raw_samples),
            vad: self.vad,
        }
    }

    /// First armed chunk (or arm of a fresh sink): drain the always-on
    /// pre-hotkey buffer into the recording — raw, bypassing VAD — and reset
    /// VAD/NS/visualizer state (the deleted recorder's `apply_start`).
    fn begin_armed(&mut self) {
        let drained: Vec<f32> = {
            let mut buf = self.control.pre_buffer.lock();
            std::mem::take(&mut *buf).into_iter().collect()
        };
        if !drained.is_empty() {
            log::debug!(
                "Pre-buffer: prepended {} samples ({}ms) to recording",
                drained.len(),
                drained.len() * 1000 / constants::WHISPER_SAMPLE_RATE as usize
            );
            self.control.processed.lock().extend_from_slice(&drained);
            self.raw_samples.extend_from_slice(&drained);
            (self.on_audio)(&drained);
        }
        self.apply_policy_to_vad();
        if let Some(ns) = &self.noise_suppressor {
            ns.lock().reset();
        }
        self.visualizer.reset();
    }

    /// Throttled emergency-backup flush (fork `BACKUP_INTERVAL` = 5 s): file
    /// I/O stays off the per-chunk path while keeping the backup fresh.
    fn maybe_update_backup(&mut self) {
        const BACKUP_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);
        let should_update = match self.last_backup_update {
            None => true,
            Some(last) => last.elapsed() >= BACKUP_INTERVAL,
        };
        if should_update {
            if let Some(backup) = self.emergency_backup.as_ref() {
                let samples = self.control.processed.lock().clone();
                backup.update_samples(&samples, constants::WHISPER_SAMPLE_RATE);
                self.last_backup_update = Some(Instant::now());
            }
        }
    }

    /// The VAD input for one chunk: the whole chunk (final chunk included,
    /// zero padding and all) passes through noise suppression first — the
    /// detector needs whole frames, and at most one frame of silence reaches
    /// the recording (upstream semantics).
    fn vad_input_for(&mut self, chunk: AudioChunk<'_>) -> Vec<f32> {
        match &self.noise_suppressor {
            Some(ns) => ns.lock().process(chunk.samples),
            None => chunk.samples.to_vec(),
        }
    }

    /// Append an emitted slice to the recording and forward it. A static
    /// method on the shared pieces (not `&self`): the emitted slice borrows
    /// the VAD detector, so nothing else may touch `self` meanwhile.
    /// `accumulate` is false for the standing path, where VAD output only
    /// feeds the live-captions seams and never becomes recording audio.
    fn emit(
        control: &Arc<CaptureControl>,
        on_audio: &AudioFrameCallback,
        frame: &[f32],
        accumulate: bool,
    ) {
        if accumulate {
            control.processed.lock().extend_from_slice(frame);
        }
        (on_audio)(frame);
    }

}

impl Sink for CaptureSink {
    fn process_chunk(&mut self, chunk: AudioChunk<'_>) {
        let control = Arc::clone(&self.control);
        control.last_chunk_ms.store(epoch_ms(), Ordering::Relaxed);

        // Mono by construction: the recorder mixes down or picks one channel.
        let real = &chunk.samples[..chunk.valid_frames];

        // Spectrum and the raw router seam run in every mode: the fork's
        // always-on stream fed the visualizer and StreamRouter continuously.
        if let Some(levels) = self.visualizer.feed(real) {
            (self.on_levels)(levels);
        }
        if let Some(feed) = &self.on_raw_frame {
            feed(real);
        }

        // Detect the standing→armed edge (or a fresh sink starting armed).
        let armed = control.armed.load(Ordering::Acquire);
        if armed && !self.armed_locally {
            self.armed_locally = true;
            self.begin_armed();
            // Readiness: the manager installed a sender before arming; the
            // first armed chunk proves the device is delivering.
            if let Some(ready) = control.ready.lock().take() {
                let _ = ready.send(());
            }
        } else if !armed {
            self.armed_locally = false;
        }

        if !armed {
            // ── Standing path ────────────────────────────────────────────
            // Maintain the rolling pre-hotkey buffer, and — when the batch
            // live-captions fallback is the active caption path (no
            // dedicated model) — keep VAD so post-VAD slices keep flowing.
            if control.pre_buffer_max_samples > 0 {
                let mut buf = control.pre_buffer.lock();
                buf.extend(real.iter().copied());
                while buf.len() > control.pre_buffer_max_samples {
                    buf.pop_front();
                }
            }
            let batch_live =
                control.streaming_enabled.load(Ordering::Acquire) && !control.router_attached;
            if !batch_live {
                return;
            }
            // Inline the classify step: the emitted classification borrows
            // the local input, and only the flag matters here.
            let vad_input = self.vad_input_for(chunk);
            match self
                .vad
                .detector
                .push_frame(&vad_input)
                .unwrap_or(VadFrame::Speech(&vad_input))
            {
                VadFrame::Speech(_) => {
                    control.speech_frames.fetch_add(1, Ordering::Relaxed);
                }
                VadFrame::Noise => {
                    control.speech_frames.store(0, Ordering::Relaxed);
                    return;
                }
            }
            // Forward the original (pre-NS) frames: the batch accumulator
            // transcribes what the mic captured, not the denoised signal.
            (self.on_audio)(real);
            return;
        }

        // ── Armed recording path ─────────────────────────────────────────
        // Raw accumulation: every real frame, pre-VAD (fork `raw_samples`).
        self.raw_samples.extend_from_slice(real);

        let rms = compute_rms(real);
        // Track max level for dead-mic detection (u32 to avoid float atomics).
        let rms_scaled = (rms * 1_000_000.0) as u32;
        let mut current_max = control.max_level.load(Ordering::Relaxed);
        while rms_scaled > current_max {
            match control.max_level.compare_exchange_weak(
                current_max,
                rms_scaled,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(actual) => current_max = actual,
            }
        }
        // Smart-stop sensor: when the level rises above floor × multiplier.
        if rms > *control.noise_floor.lock() * SILENCE_RMS_MULTIPLIER {
            control
                .last_voice_above_floor_ms
                .store(epoch_ms(), Ordering::Relaxed);
        }

        self.maybe_update_backup();

        let on_audio = Arc::clone(&self.on_audio);
        let ns_on = self.noise_suppressor.is_some();

        if *control.policy.lock() == VadPolicy::Disabled {
            Self::emit(&control, &on_audio, real, true);
            return;
        }
        let vad_input = self.vad_input_for(chunk);

        match self
            .vad
            .detector
            .push_frame(&vad_input)
            .unwrap_or(VadFrame::Speech(&vad_input))
        {
            VadFrame::Speech(emitted) => {
                control.speech_frames.fetch_add(1, Ordering::Relaxed);
                if !ns_on {
                    // No noise suppression: the emitted slice is made of the
                    // exact frames we pushed in, so appending it restores the
                    // full pre-roll (frames withheld during the VAD onset
                    // delay). THE first-word fix (ddffa435).
                    Self::emit(&control, &on_audio, emitted, true);
                } else {
                    // Noise suppression active: the emitted buffer holds
                    // suppressed frames we cannot map back to originals, so
                    // keep the pre-fix behavior (append only the current
                    // original frame).
                    Self::emit(&control, &on_audio, real, true);
                }
                // Voice stamp for the coordinator's sliding timeout.
                let armed_at = control.armed_at_ms.load(Ordering::Relaxed);
                if armed_at > 0 {
                    control.last_voice_activity_ms.store(
                        epoch_ms().saturating_sub(armed_at),
                        Ordering::Relaxed,
                    );
                }
            }
            VadFrame::Noise => {
                // Consecutive-speech gate for the batch fallback.
                control.speech_frames.store(0, Ordering::Relaxed);
                // Update the running noise floor from VAD-noise frames —
                // but not during the smart-stop window (those frames are
                // still part of the utterance tail and must not shift the
                // baseline).
                if !control.trailing.load(Ordering::Acquire) {
                    let mut floor = control.noise_floor.lock();
                    if control.noise_floor_initialised.load(Ordering::Acquire) {
                        *floor = NOISE_ALPHA * rms + (1.0 - NOISE_ALPHA) * *floor;
                    } else {
                        *floor = rms;
                        control
                            .noise_floor_initialised
                            .store(true, Ordering::Release);
                    }
                }
            }
        }
    }
}
