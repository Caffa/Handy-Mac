//! Capture-sink preroll integration tests (handy-recorder swap acceptance).
//!
//! Proves the fork's ddffa435 first-word fix on the new path: with noise
//! suppression off, the FULL VAD-emitted slice — including the frames the
//! detector withheld before confirming speech onset — reaches the recording.
//! Uses a scripted VoiceActivityDetector stub instead of Silero/Earshot, so
//! the tests are deterministic and need no model files. The stub's contract
//! mirrors SmoothedVad: it withholds the first frames, then emits the whole
//! voiced slice at once (prefill + current).
//!
//! Run with:
//!   cargo test --test capture_sink_preroll -- --nocapture

use std::sync::{Arc, Mutex};

use handy_app_lib::audio_toolkit::audio::{
    CaptureControl, CaptureSink, CaptureSinkConfig, VadPolicy,
};
use handy_app_lib::audio_toolkit::vad::{VadFrame, VoiceActivityDetector};
use handy_app_lib::audio_toolkit::NoiseSuppressor;
use handy_app_lib::settings::NoiseSuppressionLevel;
use handy_recorder::{AudioChunk, Sink};

const FRAME: usize = 480; // 30 ms at 16 kHz

/// Scripted detector: classifies frames 0..WITHHELD as noise, then — on the
/// frame at index WITHHELD — emits the slice frames[1..=WITHHELD] (one frame
/// of pre-onset "preroll" plus everything since). Later frames are noise.
struct ScriptedVad {
    frames: Vec<Vec<f32>>,
    withheld: usize,
}

impl ScriptedVad {
    fn new(withheld: usize) -> Self {
        Self {
            frames: Vec::new(),
            withheld,
        }
    }
}

impl VoiceActivityDetector for ScriptedVad {
    fn push_frame<'a>(&'a mut self, frame: &'a [f32]) -> anyhow::Result<VadFrame<'a>> {
        self.frames.push(frame.to_vec());
        let n = self.frames.len() - 1;
        if n < self.withheld {
            return Ok(VadFrame::Noise);
        }
        if n == self.withheld {
            // Emits preroll: frames 1..=withheld — frame 0 stays dropped (the
            // detector "never saw speech" there).
            let slice: Vec<f32> = self.frames[1..=self.withheld].concat();
            self.frames.push(slice);
            let end = self.frames.len() - 1;
            return Ok(VadFrame::Speech(&self.frames[end][..]));
        }
        Ok(VadFrame::Noise)
    }

    fn frame_samples(&self) -> usize {
        FRAME
    }
}

fn frame(index: usize, amplitude: f32) -> Vec<f32> {
    (0..FRAME)
        .map(|i| ((index * FRAME + i) as f32 * 0.01).sin() * amplitude)
        .collect()
}

fn chunk(samples: &[f32]) -> AudioChunk<'_> {
    AudioChunk {
        samples,
        sample_rate: 16_000,
        channels: 1,
        valid_frames: samples.len(),
    }
}

struct Rig {
    sink: CaptureSink,
    control: Arc<CaptureControl>,
    captured: Arc<Mutex<Vec<f32>>>,
}

fn build(policy: VadPolicy, ns: Option<NoiseSuppressionLevel>) -> Rig {
    let control = Arc::new(CaptureControl::new(0, false));
    control.arm("test-binding");
    *control.policy.lock() = policy;
    let captured: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
    let cap_for_cb = Arc::clone(&captured);
    let sink = CaptureSink::new(
        handy_app_lib::audio_toolkit::audio::CaptureVad::new(
            Box::new(ScriptedVad::new(4)),
            15,
            55,
        ),
        CaptureSinkConfig {
            on_levels: Arc::new(|_| {}),
            on_audio: Arc::new(move |frame| cap_for_cb.lock().unwrap().extend_from_slice(frame)),
            on_raw_frame: None,
            noise_suppressor: ns.map(|level| {
                Arc::new(parking_lot::Mutex::new(NoiseSuppressor::new(level)))
            }),
            control: Arc::clone(&control),
            emergency_backup: None,
        },
    );
    Rig {
        sink,
        control,
        captured,
    }
}

/// THE first-word fix (ddffa435): the emitted slice begins with the pre-onset
/// frames the detector withheld — the recording must start there, not at the
/// frame that triggered speech.
#[test]
fn preroll_slice_reaches_recording_when_ns_off() {
    let mut rig = build(VadPolicy::Offline, None);

    for i in 0..6 {
        let f = frame(i, 0.5);
        rig.sink.process_chunk(chunk(&f));
    }

    let recorded = rig.control.processed.lock().clone();
    // Frame 0 was dropped by the (scripted) detector before speech onset.
    let f0 = frame(0, 0.5);
    assert!(
        !recorded.starts_with(&f0[..64]),
        "frame 0 predates the detector's window and must not open the recording"
    );
    // The recording must OPEN with frame 1: the withheld pre-onset slice.
    let f1 = frame(1, 0.5);
    assert!(
        recorded.starts_with(&f1[..]),
        "recording must open with the withheld preroll frame 1 (ddffa435); got {} samples, expected start {}",
        recorded.len(),
        f1[0]
    );
    // And it must contain frames 1..=4 contiguously (preroll + onset + tail).
    let f2 = frame(2, 0.5);
    let f4 = frame(4, 0.5);
    let offset1 = recorded.windows(FRAME).position(|w| w == &f1[..]).unwrap();
    let offset2 = recorded.windows(FRAME).position(|w| w == &f2[..]).unwrap();
    let offset4 = recorded.windows(FRAME).position(|w| w == &f4[..]).unwrap();
    assert_eq!(offset1 + FRAME, offset2, "frames 1 and 2 must be contiguous");
    assert_eq!(offset2 + 2 * FRAME, offset4, "frames 2..=4 must be contiguous");
    // Frame 5 (post-emission noise) is withheld by the detector.
    let f5 = frame(5, 0.5);
    assert!(
        !recorded.windows(FRAME).any(|w| w == &f5[..]),
        "post-emission noise frame must stay withheld"
    );

    // The live-captions seam sees exactly what the recording sees.
    let streamed = rig.captured.lock().unwrap();
    assert_eq!(&*streamed, &recorded[..], "on_audio mirrors the recording");

    // Raw capture keeps every real frame, pre-VAD.
    assert!(recorded.len() < 6 * FRAME);
}

/// With noise suppression ON the emitted slice is denoised and cannot be
/// mapped back to originals: the sink keeps the pre-fix behavior of appending
/// only the current ORIGINAL frame (inventory row 4).
#[test]
fn ns_on_appends_current_original_frame_only() {
    let mut rig = build(VadPolicy::Offline, Some(NoiseSuppressionLevel::Medium));

    for i in 0..6 {
        let f = frame(i, 0.5);
        rig.sink.process_chunk(chunk(&f));
    }

    let recorded = rig.control.processed.lock().clone();
    // Exactly one frame's worth of audio (the onset frame 4, original).
    assert_eq!(
        recorded.len(),
        FRAME,
        "NS-on must append only the current original frame; got {} samples",
        recorded.len()
    );
    let f4 = frame(4, 0.5);
    assert!(
        recorded.windows(FRAME).any(|w| w == &f4[..]),
        "appended frame must be the unmodified original"
    );
}

/// Policy Disabled bypasses the VAD entirely: every frame reaches the
/// recording, and telemetry still updates.
#[test]
fn disabled_policy_passes_every_frame() {
    let mut rig = build(VadPolicy::Disabled, None);

    for i in 0..3 {
        let f = frame(i, 0.5);
        rig.sink.process_chunk(chunk(&f));
    }

    let recorded = rig.control.processed.lock().clone();
    assert_eq!(recorded.len(), 3 * FRAME);
    let f0 = frame(0, 0.5);
    assert!(recorded.starts_with(&f0[..]));

    // Telemetry: every chunk stamps liveness; loud frames move max level.
    assert!(rig.control.last_chunk_ms() > 0);
    assert!(rig.control.get_max_level() > 0.3); // sine RMS = amp/√2 ≈ 0.354
    assert_eq!(rig.control.speech_frames.load(Ordering::Relaxed), 0);
}

use std::sync::atomic::Ordering;

/// Real-model preroll proof: run an actual user recording through CaptureSink
/// with the REAL Silero VAD, then verify the recording opens with at least one
/// pre-onset frame (audio the standalone detector classifies as noise) — the
/// first-word fix with a real detector and real speech.
///
/// Needs a 16 kHz mono WAV (Handy writes raw 16 kHz mono):
///   HANDY_TEST_WAV=/path/to/handy-*.wav \
///   HANDY_TEST_VAD=src-tauri/resources/models/silero_vad_v4.onnx \
///   cargo test --test capture_sink_preroll real_vad -- --ignored --nocapture
#[test]
#[ignore]
fn real_vad_preroll_slice() {
    use handy_app_lib::audio_toolkit::audio::read_wav_samples;
    use handy_app_lib::audio_toolkit::SileroVad;

    let wav = std::env::var("HANDY_TEST_WAV").expect("set HANDY_TEST_WAV");
    let vad_path = std::env::var("HANDY_TEST_VAD")
        .unwrap_or_else(|_| "resources/models/silero_vad_v4.onnx".into());

    let samples = read_wav_samples(&wav).expect("read wav");
    assert!(samples.len() > FRAME * 10, "recording too short");

    // Pass 1: standalone detector — first voiced frame index.
    let mut probe = SileroVad::new(std::path::PathBuf::from(&vad_path), 0.5).unwrap();
    let first_voiced = (0..samples.len() / FRAME)
        .find(|&i| {
            probe
                .is_voice(&samples[i * FRAME..(i + 1) * FRAME])
                .unwrap_or(false)
        })
        .expect("recording has no voiced frames");

    // Pass 2: the full sink pipeline (policy Offline, NS off).
    let control = Arc::new(CaptureControl::new(0, false));
    control.arm("acceptance");
    *control.policy.lock() = VadPolicy::Offline;
    let sink = CaptureSink::new(
        handy_app_lib::audio_toolkit::audio::CaptureVad::new(
            Box::new(handy_app_lib::audio_toolkit::vad::SmoothedVad::new(
                Box::new(SileroVad::new(std::path::PathBuf::from(&vad_path), 0.5).unwrap()),
                15, // prefill: 450 ms of pre-onset audio withheld then emitted
                15,
                2,  // onset: 2 consecutive voiced frames confirm speech
            )),
            15,
            55,
        ),
        CaptureSinkConfig {
            on_levels: Arc::new(|_| {}),
            on_audio: Arc::new(|_| {}),
            on_raw_frame: None,
            noise_suppressor: None,
            control: Arc::clone(&control),
            emergency_backup: None,
        },
    );
    let mut sink = sink;
    for i in 0..samples.len() / FRAME {
        sink.process_chunk(chunk(&samples[i * FRAME..(i + 1) * FRAME]));
    }
    let recorded = control.processed.lock().clone();
    assert!(!recorded.is_empty(), "sink produced no audio");

    // The recording must OPEN with pre-onset audio: locate the recorded
    // opening frame inside the input and verify it precedes the first frame
    // the standalone detector classifies as voiced. Without the ddffa435
    // slice-emission behavior the recording would open at (or after) the
    // onset frame instead.
    let opening = &recorded[..FRAME];
    let open_frame = (0..samples.len() / FRAME)
        .find(|&i| &samples[i * FRAME..(i + 1) * FRAME] == opening)
        .expect("recorded opening frame not found in the input");
    assert!(
        open_frame < first_voiced,
        "recording must open BEFORE the first voiced frame: opened at frame {open_frame} ({} ms),          first voiced at frame {first_voiced} ({} ms)",
        open_frame * 30,
        first_voiced * 30
    );

    println!(
        "real_vad_preroll: first_voiced_frame={} ({} ms); recording opens at frame {} ({} ms) — \
         {} ms of pre-onset preroll captured; recorded {} samples from {} input",
        first_voiced,
        first_voiced * 30,
        open_frame,
        open_frame * 30,
        (first_voiced - open_frame) * 30,
        recorded.len(),
        samples.len()
    );
}
