use std::{
    io::Error,
    sync::{
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
        mpsc, Arc, Mutex,
    },
    time::{Duration, Instant},
};

use cpal::{
    traits::{DeviceTrait, HostTrait, StreamTrait},
    Device, Sample, SizedSample,
};

use crate::audio_toolkit::{
    audio::{AudioVisualiser, FrameResampler},
    constants,
    vad::{self, VadFrame},
    VoiceActivityDetector,
};

enum Cmd {
    Start,
    Stop(mpsc::Sender<Vec<f32>>),
    /// Continue recording for up to `max_buffer_ms` after the hotkey is
    /// released, but stop early when sustained silence is detected (based
    /// on the microphone volume relative to the noise floor).
    SmartStop {
        max_buffer_ms: u64,
        reply_tx: mpsc::Sender<Vec<f32>>,
    },
    Shutdown,
}

/// Classification of a VAD-processed audio frame, used for noise
/// floor estimation during recording.
#[derive(Debug)]
enum FrameClass {
    /// Frame classified as speech by VAD (or no VAD configured).
    Speech,
    /// Frame classified as noise by VAD.
    Noise,
    /// Not recording – frame was ignored.
    NotRecording,
}

/// State tracked during a smart-stop (volume-aware) trailing buffer.
struct SmartStopState {
    /// When the smart buffer period started.
    start: Instant,
    /// Maximum duration of the smart buffer.
    max_duration: Duration,
    /// Channel to send the final audio samples on completion.
    reply_tx: mpsc::Sender<Vec<f32>>,
    /// Estimated noise floor (RMS) built from VAD-noise frames during
    /// the preceding recording.
    noise_floor: f32,
    /// Timestamp of the most recent frame whose RMS exceeded the
    /// silence threshold (noise_floor × multiplier).
    last_voice_above_floor: Instant,
}

// ─── Smart-stop tuning constants ──────────────────────────────────────
//
// SILENCE_RMS_MULTIPLIER: A frame is considered "voice" when its RMS
//   exceeds `noise_floor * SILENCE_RMS_MULTIPLIER`.  3× means that
//   even modest speech (3× louder than background noise) keeps the
//   buffer open, while pure silence or steady ambient noise closes it.
const SILENCE_RMS_MULTIPLIER: f32 = 3.0;

// SILENCE_THRESHOLD_MS: How long the volume must stay below the
//   threshold before we decide the user has finished speaking.
//   300 ms ≈ the length of a very short pause; it avoids cutting off
//   natural micro-pauses in continuous speech.
const SILENCE_THRESHOLD_MS: u64 = 300;

// MIN_BUFFER_MS: The shortest time we *always* wait before considering
//   an early stop.  Guarantees we capture trailing consonants or a
//   brief final syllable that might dip below threshold for a few ms.
const MIN_BUFFER_MS: u64 = 100;

// NOISE_ALPHA: Exponential moving average (EMA) coefficient for noise
//   floor estimation.  0.05 adapts fairly quickly to background changes
//   while staying resistant to brief speech spikes classified as noise.
const NOISE_ALPHA: f32 = 0.05;

// DEFAULT_NOISE_FLOOR: Fallback noise floor when no VAD-noise frames
//   have been observed yet (e.g. user spoke continuously with no gaps).
//   ≈ RMS of –46 dBFS in 16-bit audio – quiet but not silence.
const DEFAULT_NOISE_FLOOR: f32 = 0.005;

enum AudioChunk {
    Samples(Vec<f32>),
    EndOfStream,
}

pub struct AudioRecorder {
    device: Option<Device>,
    cmd_tx: Option<mpsc::Sender<Cmd>>,
    worker_handle: Option<std::thread::JoinHandle<()>>,
    vad: Option<Arc<Mutex<Box<dyn vad::VoiceActivityDetector>>>>,
    level_cb: Option<Arc<dyn Fn(Vec<f32>) + Send + Sync + 'static>>,
    /// Timestamp (ms since epoch) of the last audio chunk received by
    /// the consumer thread. Used to detect dead microphone streams.
    last_chunk_ms: Arc<AtomicU64>,
    /// Timestamp (ms since epoch) when the stream was opened.
    /// Used to provide a grace period for liveness checks.
    opened_at_ms: Arc<AtomicU64>,
    /// Maximum audio level (RMS * 1_000_000) seen during the current recording.
    /// Used to detect "no audio" situations where the mic might be dead.
    /// Stored as AtomicU32 to avoid float atomics. Multiply by 1_000_000
    /// to preserve precision when storing, divide when retrieving.
    max_level: Arc<AtomicU32>,
}

impl AudioRecorder {
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        Ok(AudioRecorder {
            device: None,
            cmd_tx: None,
            worker_handle: None,
            vad: None,
            level_cb: None,
            last_chunk_ms: Arc::new(AtomicU64::new(0)),
            opened_at_ms: Arc::new(AtomicU64::new(0)),
            max_level: Arc::new(AtomicU32::new(0)),
        })
    }

    pub fn with_vad(mut self, vad: Box<dyn VoiceActivityDetector>) -> Self {
        self.vad = Some(Arc::new(Mutex::new(vad)));
        self
    }

    pub fn with_level_callback<F>(mut self, cb: F) -> Self
    where
        F: Fn(Vec<f32>) + Send + Sync + 'static,
    {
        self.level_cb = Some(Arc::new(cb));
        self
    }

    pub fn open(&mut self, device: Option<Device>) -> Result<(), Box<dyn std::error::Error>> {
        if self.worker_handle.is_some() {
            return Ok(()); // already open
        }

        // Reset stream liveness timestamp
        self.last_chunk_ms.store(0, Ordering::Relaxed);
        // Record when stream was opened for grace period
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        self.opened_at_ms.store(now_ms, Ordering::Relaxed);

        let (sample_tx, sample_rx) = mpsc::channel::<AudioChunk>();
        let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>();
        let (init_tx, init_rx) = mpsc::sync_channel::<Result<(), String>>(1);

        let host = crate::audio_toolkit::get_cpal_host();
        let device = match device {
            Some(dev) => dev,
            None => host
                .default_input_device()
                .ok_or_else(|| Error::new(std::io::ErrorKind::NotFound, "No input device found"))?,
        };

        let thread_device = device.clone();
        let vad = self.vad.clone();
        // Move the optional level callback into the worker thread
        let level_cb = self.level_cb.clone();
        let last_chunk_ms = self.last_chunk_ms.clone();
        let max_level = self.max_level.clone();

        let worker = std::thread::spawn(move || {
            let stop_flag = Arc::new(AtomicBool::new(false));
            let stop_flag_for_stream = stop_flag.clone();
            let init_result = (|| -> Result<(cpal::Stream, u32), String> {
                let config = AudioRecorder::get_preferred_config(&thread_device)
                    .map_err(|e| format!("Failed to fetch preferred config: {e}"))?;

                let sample_rate = config.sample_rate().0;
                let channels = config.channels() as usize;

                log::info!(
                    "Using device: {:?}\nSample rate: {}\nChannels: {}\nFormat: {:?}",
                    thread_device.name(),
                    sample_rate,
                    channels,
                    config.sample_format()
                );

                let stream = match config.sample_format() {
                    cpal::SampleFormat::U8 => AudioRecorder::build_stream::<u8>(
                        &thread_device,
                        &config,
                        sample_tx,
                        channels,
                        stop_flag_for_stream,
                    )
                    .map_err(|e| format!("Failed to build input stream: {e}"))?,
                    cpal::SampleFormat::I8 => AudioRecorder::build_stream::<i8>(
                        &thread_device,
                        &config,
                        sample_tx,
                        channels,
                        stop_flag_for_stream,
                    )
                    .map_err(|e| format!("Failed to build input stream: {e}"))?,
                    cpal::SampleFormat::I16 => AudioRecorder::build_stream::<i16>(
                        &thread_device,
                        &config,
                        sample_tx,
                        channels,
                        stop_flag_for_stream,
                    )
                    .map_err(|e| format!("Failed to build input stream: {e}"))?,
                    cpal::SampleFormat::I32 => AudioRecorder::build_stream::<i32>(
                        &thread_device,
                        &config,
                        sample_tx,
                        channels,
                        stop_flag_for_stream,
                    )
                    .map_err(|e| format!("Failed to build input stream: {e}"))?,
                    cpal::SampleFormat::F32 => AudioRecorder::build_stream::<f32>(
                        &thread_device,
                        &config,
                        sample_tx,
                        channels,
                        stop_flag_for_stream,
                    )
                    .map_err(|e| format!("Failed to build input stream: {e}"))?,
                    sample_format => {
                        return Err(format!("Unsupported sample format: {sample_format:?}"));
                    }
                };

                stream
                    .play()
                    .map_err(|e| format!("Failed to start microphone stream: {e}"))?;

                Ok((stream, sample_rate))
            })();

            match init_result {
                Ok((stream, sample_rate)) => {
                    let _ = init_tx.send(Ok(()));
                    // Keep the stream alive while we process samples.
                    run_consumer(
                        sample_rate,
                        vad,
                        sample_rx,
                        cmd_rx,
                        level_cb,
                        stop_flag,
                        last_chunk_ms,
                        max_level,
                    );

                    // Pause the stream before dropping to prevent heap corruption.
                    // On macOS, cpal::Stream::pause() is asynchronous — CoreAudio's IO
                    // thread may still be executing the last callback invocation when
                    // pause() returns. If we drop immediately, internal buffers are
                    // deallocated while the callback is still in-flight, causing
                    // nanov2 malloc heap corruption → SIGABRT.
                    // 100ms is a generous safety margin (typical buffer period is 5–23ms).
                    let _ = stream.pause();
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    drop(stream);
                }
                Err(error_message) => {
                    log::error!("{error_message}");
                    let _ = init_tx.send(Err(error_message));
                }
            }
        });

        match init_rx.recv() {
            Ok(Ok(())) => {
                self.device = Some(device);
                self.cmd_tx = Some(cmd_tx);
                self.worker_handle = Some(worker);
                Ok(())
            }
            Ok(Err(error_message)) => {
                let _ = worker.join();
                let kind = if is_microphone_access_denied(&error_message) {
                    std::io::ErrorKind::PermissionDenied
                } else {
                    std::io::ErrorKind::Other
                };
                Err(Box::new(Error::new(kind, error_message)))
            }
            Err(recv_error) => {
                let _ = worker.join();
                Err(Box::new(Error::new(
                    std::io::ErrorKind::Other,
                    format!("Failed to initialize microphone worker: {recv_error}"),
                )))
            }
        }
    }

    pub fn start(&self) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(tx) = &self.cmd_tx {
            tx.send(Cmd::Start)?;
        }
        Ok(())
    }

    pub fn stop(&self) -> Result<Vec<f32>, Box<dyn std::error::Error>> {
        let (resp_tx, resp_rx) = mpsc::channel();
        if let Some(tx) = &self.cmd_tx {
            tx.send(Cmd::Stop(resp_tx))?;
        }
        Ok(resp_rx.recv()?) // wait for the samples
    }

    /// Volume-aware stop: continues recording for up to `max_buffer_ms`
    /// after the hotkey is released, but stops early when the microphone
    /// level drops below the noise floor for a sustained period.
    ///
    /// The noise floor is estimated from VAD-classified "noise" frames
    /// collected during the preceding recording, so noisy environments
    /// are handled naturally — the threshold adapts to whatever
    /// background level was present while the user was speaking.
    pub fn smart_stop(&self, max_buffer_ms: u64) -> Result<Vec<f32>, Box<dyn std::error::Error>> {
        let (resp_tx, resp_rx) = mpsc::channel();
        if let Some(tx) = &self.cmd_tx {
            tx.send(Cmd::SmartStop {
                max_buffer_ms,
                reply_tx: resp_tx,
            })?;
        }
        Ok(resp_rx.recv()?) // wait for the samples
    }

    pub fn close(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(tx) = self.cmd_tx.take() {
            let _ = tx.send(Cmd::Shutdown);
        }
        if let Some(h) = self.worker_handle.take() {
            let _ = h.join();
        }
        self.device = None;
        self.last_chunk_ms.store(0, Ordering::Relaxed);
        self.max_level.store(0, Ordering::Relaxed);
        Ok(())
    }

    /// Returns the maximum audio level (RMS) seen during the last recording.
    /// The value is normalized (0.0 to ~1.0 for f32 samples).
    /// Returns 0.0 if no recording has occurred or the recorder wasn't used.
    pub fn get_max_level(&self) -> f32 {
        self.max_level.load(Ordering::Relaxed) as f32 / 1_000_000.0
    }

    /// Resets the max level counter. Should be called before starting a new recording.
    pub fn reset_max_level(&self) {
        self.max_level.store(0, Ordering::Relaxed);
    }

    /// Returns true if the microphone stream has received audio data within
    /// the last `timeout_ms` milliseconds. Returns false if the stream has
    /// never received data or if data stopped flowing.
    /// 
    /// Grace period: For the first 500ms after the stream opens, we return true
    /// even if no audio has been received yet. This allows CoreAudio time to start
    /// delivering samples. After 500ms, we require actual audio data.
    pub fn is_stream_alive(&self, timeout_ms: u64) -> bool {
        if self.cmd_tx.is_none() {
            // Stream not open
            return false;
        }
        let last = self.last_chunk_ms.load(Ordering::Relaxed);
        let opened_at = self.opened_at_ms.load(Ordering::Relaxed);
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        
        // Grace period: give CoreAudio ~500ms to start delivering samples
        const GRACE_PERIOD_MS: u64 = 500;
        let stream_age_ms = now_ms.saturating_sub(opened_at);
        
        if last == 0 {
            // No audio received yet
            // Return true only during grace period
            if stream_age_ms < GRACE_PERIOD_MS {
                return true;
            }
            // Grace period expired with no audio = zombie stream
            return false;
        }
        
        // Check if audio is flowing
        now_ms.saturating_sub(last) < timeout_ms
    }

    fn build_stream<T>(
        device: &cpal::Device,
        config: &cpal::SupportedStreamConfig,
        sample_tx: mpsc::Sender<AudioChunk>,
        channels: usize,
        stop_flag: Arc<AtomicBool>,
    ) -> Result<cpal::Stream, cpal::BuildStreamError>
    where
        T: Sample + SizedSample + Send + 'static,
        f32: cpal::FromSample<T>,
    {
        let mut output_buffer = Vec::new();
        let mut eos_sent = false;

        let stream_cb = move |data: &[T], _: &cpal::InputCallbackInfo| {
            if stop_flag.load(Ordering::Relaxed) {
                if !eos_sent {
                    let _ = sample_tx.send(AudioChunk::EndOfStream);
                    eos_sent = true;
                }
                return;
            }
            eos_sent = false;

            output_buffer.clear();

            if channels == 1 {
                output_buffer.extend(data.iter().map(|&sample| sample.to_sample::<f32>()));
            } else {
                let frame_count = data.len() / channels;
                output_buffer.reserve(frame_count);

                for frame in data.chunks_exact(channels) {
                    let mono_sample = frame
                        .iter()
                        .map(|&sample| sample.to_sample::<f32>())
                        .sum::<f32>()
                        / channels as f32;
                    output_buffer.push(mono_sample);
                }
            }

            if sample_tx
                .send(AudioChunk::Samples(output_buffer.clone()))
                .is_err()
            {
                log::error!("Failed to send samples");
            }
        };

        device.build_input_stream(
            &config.clone().into(),
            stream_cb,
            |err| log::error!("Stream error: {}", err),
            None,
        )
    }

    fn get_preferred_config(
        device: &cpal::Device,
    ) -> Result<cpal::SupportedStreamConfig, Box<dyn std::error::Error>> {
        // Use the device's native/default sample rate and let the FrameResampler
        // in run_consumer() downsample to 16kHz. This avoids forcing hardware into
        // a non-native rate which can cause issues on some devices (Bluetooth
        // codecs, certain ALSA drivers, etc.).
        let default_config = device.default_input_config()?;
        let target_rate = default_config.sample_rate();

        // Try to find the best sample format at the device's default rate
        let supported_configs = match device.supported_input_configs() {
            Ok(configs) => configs,
            Err(e) => {
                log::warn!("Could not enumerate input configs ({e}), using device default");
                return Ok(default_config);
            }
        };
        let mut best_config: Option<cpal::SupportedStreamConfigRange> = None;

        for config_range in supported_configs {
            if config_range.min_sample_rate() <= target_rate
                && config_range.max_sample_rate() >= target_rate
            {
                match best_config {
                    None => best_config = Some(config_range),
                    Some(ref current) => {
                        // Prioritize F32 > I16 > I32 > others
                        let score = |fmt: cpal::SampleFormat| match fmt {
                            cpal::SampleFormat::F32 => 4,
                            cpal::SampleFormat::I16 => 3,
                            cpal::SampleFormat::I32 => 2,
                            _ => 1,
                        };

                        if score(config_range.sample_format()) > score(current.sample_format()) {
                            best_config = Some(config_range);
                        }
                    }
                }
            }
        }

        if let Some(config) = best_config {
            return Ok(config.with_sample_rate(target_rate));
        }

        // Fall back to device default if no config matched (exotic/virtual devices)
        log::warn!(
            "No supported config matched device default rate {:?}, using default config",
            target_rate
        );
        Ok(default_config)
    }
}

pub fn is_microphone_access_denied(error_message: &str) -> bool {
    let normalized = error_message.to_lowercase();
    normalized.contains("access is denied")
        || normalized.contains("permission denied")
        || normalized.contains("0x80070005")
}

pub fn is_no_input_device_error(error_message: &str) -> bool {
    let normalized = error_message.to_lowercase();
    normalized.contains("no input device found")
        || (normalized.contains("failed to fetch preferred config")
            && normalized.contains("coreaudio"))
}

#[cfg(test)]
mod tests {
    use super::{is_microphone_access_denied, is_no_input_device_error};

    #[test]
    fn detects_access_is_denied() {
        assert!(is_microphone_access_denied("Access is denied"));
    }

    #[test]
    fn detects_permission_denied() {
        assert!(is_microphone_access_denied("permission denied"));
    }

    #[test]
    fn detects_windows_error_code() {
        assert!(is_microphone_access_denied("WASAPI error: 0x80070005"));
    }

    #[test]
    fn does_not_match_unrelated_errors() {
        assert!(!is_microphone_access_denied("device not found"));
    }

    #[test]
    fn detects_no_input_device() {
        assert!(is_no_input_device_error("No input device found"));
    }

    #[test]
    fn detects_coreaudio_config_error() {
        assert!(is_no_input_device_error(
            "Failed to fetch preferred config: A backend-specific error has occurred: An unknown error unknown to the coreaudio-rs API occurred"
        ));
    }

    #[test]
    fn does_not_match_other_errors_for_no_device() {
        assert!(!is_no_input_device_error("permission denied"));
        assert!(!is_no_input_device_error("device not found"));
    }
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

/// Process a resampled audio frame through VAD and optionally append
/// speech to the output buffer.  Returns the VAD classification so
/// the caller can update the noise floor estimate.
fn handle_frame(
    samples: &[f32],
    recording: bool,
    vad: &Option<Arc<Mutex<Box<dyn vad::VoiceActivityDetector>>>>,
    out_buf: &mut Vec<f32>,
) -> FrameClass {
    if !recording {
        return FrameClass::NotRecording;
    }

    if let Some(vad_arc) = vad {
        let mut det = vad_arc.lock().unwrap();
        match det.push_frame(samples).unwrap_or(VadFrame::Speech(samples)) {
            VadFrame::Speech(buf) => {
                out_buf.extend_from_slice(buf);
                FrameClass::Speech
            }
            VadFrame::Noise => FrameClass::Noise,
        }
    } else {
        out_buf.extend_from_slice(samples);
        FrameClass::Speech
    }
}

fn run_consumer(
    in_sample_rate: u32,
    vad: Option<Arc<Mutex<Box<dyn vad::VoiceActivityDetector>>>>,
    sample_rx: mpsc::Receiver<AudioChunk>,
    cmd_rx: mpsc::Receiver<Cmd>,
    level_cb: Option<Arc<dyn Fn(Vec<f32>) + Send + Sync + 'static>>,
    stop_flag: Arc<AtomicBool>,
    last_chunk_ms: Arc<AtomicU64>,
    max_level: Arc<AtomicU32>,
) {
    let mut frame_resampler = FrameResampler::new(
        in_sample_rate as usize,
        constants::WHISPER_SAMPLE_RATE as usize,
        Duration::from_millis(30),
    );

    let mut processed_samples = Vec::<f32>::new();
    let mut recording = false;

    // Running estimate of background noise level, built from the RMS of
    // frames that VAD classifies as Noise during an active recording.
    let mut noise_floor: f32 = DEFAULT_NOISE_FLOOR;
    // Whether at least one noise frame has been observed (so noise_floor
    // reflects actual data rather than the default).
    let mut noise_floor_initialised = false;

    // Active smart-stop state.  When Some, we are in the trailing buffer
    // period after the user released the hotkey, continuing to record
    // until silence is detected or the maximum time expires.
    let mut smart_stop: Option<SmartStopState> = None;

    // ---------- spectrum visualisation setup ---------------------------- //
    const BUCKETS: usize = 16;
    const WINDOW_SIZE: usize = 512;
    let mut visualizer = AudioVisualiser::new(
        in_sample_rate,
        WINDOW_SIZE,
        BUCKETS,
        400.0,  // vocal_min_hz
        4000.0, // vocal_max_hz
    );

    loop {
        // ------------------------------------------------------------------
        // Receive audio chunk.
        // When a smart-stop is in progress we use recv_timeout() so the loop
        // can check whether the max buffer duration has expired even if no
        // audio is arriving (e.g. mic stream died mid-buffer).  Without this
        // the consumer would block on recv() forever and never finalise.
        // ------------------------------------------------------------------
        let chunk = if smart_stop.is_some() {
            match sample_rx.recv_timeout(Duration::from_millis(50)) {
                Ok(c) => c,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    // No audio in 50 ms — check whether the smart buffer
                    // should be finalised due to elapsed time.
                    if let Some(ref ss) = smart_stop {
                        let elapsed = Instant::now() - ss.start;
                        if elapsed >= ss.max_duration {
                            // Max time reached without audio: finalise now.
                            recording = false;
                            frame_resampler.finish(&mut |frame: &[f32]| {
                                let _ = handle_frame(
                                    frame,
                                    true, // force-record remaining frames
                                    &vad,
                                    &mut processed_samples,
                                );
                            });
                            if let Some(ss) = smart_stop.take() {
                                log::debug!(
                                    "Smart-stop: max duration reached (no audio) after {}ms",
                                    elapsed.as_millis(),
                                );
                                let _ = ss.reply_tx.send(std::mem::take(&mut processed_samples));
                            }
                        }
                    }
                    continue;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        } else {
            match sample_rx.recv_timeout(Duration::from_millis(100)) {
                Ok(c) => c,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    // No audio data in 100ms — the stream may be dead (e.g.
                    // after macOS sleep/wake where the CoreAudio input unit
                    // is suspended and never resumes).  Check for pending
                    // commands so that Cmd::Shutdown can be processed even
                    // when no audio is flowing; without this, close() hangs
                    // forever trying to join the worker thread.
                    //
                    // During normal streaming, data arrives every ~10–50ms
                    // (depending on buffer size), so this branch is rarely
                    // exercised.
                    while let Ok(cmd) = cmd_rx.try_recv() {
                        if let Cmd::Shutdown = cmd {
                            stop_flag.store(true, Ordering::Relaxed);
                            return;
                        }
                    }
                    continue;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        };

        let raw = match chunk {
            AudioChunk::Samples(s) => s,
            AudioChunk::EndOfStream => continue,
        };

        // Track stream liveness: update timestamp whenever audio is received
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        last_chunk_ms.store(now_ms, Ordering::Relaxed);

        // ---------- spectrum processing ---------------------------------- //
        if let Some(buckets) = visualizer.feed(&raw) {
            if let Some(cb) = &level_cb {
                cb(buckets);
            }
        }

        // ---------- audio pipeline + noise/smart-stop tracking ---------- //
        // Flag set inside the push closure when the smart-stop condition
        // is met, so we can finalise after the closure returns (we cannot
        // borrow frame_resampler inside the closure).
        //
        // We intentionally do NOT set recording=false inside the closure
        // so that subsequent frames in the same push() batch are still
        // processed by VAD and added to processed_samples.  Finalisation
        // sets recording=false after push() returns.
        let mut smart_stop_triggered = false;

        frame_resampler.push(&raw, &mut |frame: &[f32]| {
            let rms = compute_rms(frame);
            
            // Track max audio level during recording for noise detection
            if recording {
                // Store as u32 (RMS * 1_000_000) to avoid float atomics
                let rms_scaled = (rms * 1_000_000.0) as u32;
                let mut current_max = max_level.load(Ordering::Relaxed);
                while rms_scaled > current_max {
                    match max_level.compare_exchange_weak(
                        current_max,
                        rms_scaled,
                        Ordering::Relaxed,
                        Ordering::Relaxed,
                    ) {
                        Ok(_) => break,
                        Err(actual) => current_max = actual,
                    }
                }
            }
            
            let class = handle_frame(frame, recording, &vad, &mut processed_samples);

            // Update the running noise floor from VAD-noise frames while
            // recording (but NOT during smart-stop – those frames are
            // still part of the utterance tail and should not shift the
            // baseline).
            if recording && matches!(class, FrameClass::Noise) && smart_stop.is_none() {
                if noise_floor_initialised {
                    noise_floor = NOISE_ALPHA * rms + (1.0 - NOISE_ALPHA) * noise_floor;
                } else {
                    noise_floor = rms;
                    noise_floor_initialised = true;
                }
            }

            // ---- smart-stop (volume-aware trailing buffer) ---- //
            if let Some(ref mut ss) = smart_stop {
                let now = Instant::now();
                let elapsed = now - ss.start;

                // If volume exceeds the silence threshold, mark it as voice.
                if rms > ss.noise_floor * SILENCE_RMS_MULTIPLIER {
                    ss.last_voice_above_floor = now;
                }

                let silence_duration = now - ss.last_voice_above_floor;
                let min_buffer_elapsed = elapsed >= Duration::from_millis(MIN_BUFFER_MS);
                let max_buffer_elapsed = elapsed >= ss.max_duration;
                let silence_detected = min_buffer_elapsed
                    && silence_duration >= Duration::from_millis(SILENCE_THRESHOLD_MS);

                if max_buffer_elapsed || silence_detected {
                    // Do NOT set recording=false here; do it in the
                    // finalisation block after push() returns, so that
                    // subsequent frames in this batch are still collected.
                    smart_stop_triggered = true;
                    log::debug!(
                        "Smart-stop: finalising after {}ms (silence detected: {})",
                        elapsed.as_millis(),
                        silence_detected,
                    );
                }
            }
        });

        // ---- smart-stop finalisation (outside the push closure) ---- //
        if smart_stop_triggered {
            recording = false;

            frame_resampler.finish(&mut |frame: &[f32]| {
                let _ = handle_frame(frame, true, &vad, &mut processed_samples);
            });

            if let Some(ss) = smart_stop.take() {
                let _ = ss.reply_tx.send(std::mem::take(&mut processed_samples));
            }
        }

        // non-blocking check for a command
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                Cmd::Start => {
                    stop_flag.store(false, Ordering::Relaxed);
                    processed_samples.clear();
                    recording = true;
                    noise_floor = DEFAULT_NOISE_FLOOR;
                    noise_floor_initialised = false;
                    smart_stop = None;
                    visualizer.reset();
                    max_level.store(0, Ordering::Relaxed); // Reset max level for new recording
                    if let Some(v) = &vad {
                        v.lock().unwrap().reset();
                    }
                }
                Cmd::Stop(reply_tx) => {
                    // If a smart-stop is in progress, resolve it with
                    // whatever samples we have so far so the caller
                    // doesn’t hang forever on resp_rx.recv().
                    if let Some(ss) = smart_stop.take() {
                        frame_resampler.finish(&mut |frame: &[f32]| {
                            let _ = handle_frame(frame, true, &vad, &mut processed_samples);
                        });
                        let _ = ss.reply_tx.send(std::mem::take(&mut processed_samples));
                        log::debug!("Smart-stop: resolved by Cmd::Stop (cancel)");
                    }

                    recording = false;
                    stop_flag.store(true, Ordering::Relaxed);

                    // Drain all remaining audio until the producer confirms end-of-stream.
                    // The cpal callback sees the stop flag, sends EndOfStream, and goes
                    // silent — guaranteeing every captured sample is in the channel
                    // ahead of the sentinel.
                    loop {
                        match sample_rx.recv_timeout(Duration::from_secs(2)) {
                            Ok(AudioChunk::Samples(remaining)) => {
                                frame_resampler.push(&remaining, &mut |frame: &[f32]| {
                                    let _ = handle_frame(frame, true, &vad, &mut processed_samples);
                                });
                            }
                            Ok(AudioChunk::EndOfStream) => break,
                            Err(_) => {
                                log::warn!("Timed out waiting for EndOfStream from audio callback");
                                break;
                            }
                        }
                    }

                    frame_resampler.finish(&mut |frame: &[f32]| {
                        let _ = handle_frame(frame, true, &vad, &mut processed_samples);
                    });

                    let _ = reply_tx.send(std::mem::take(&mut processed_samples));

                    // Resume the audio callback so the consumer loop can continue
                    // receiving chunks (important for always-on microphone mode).
                    stop_flag.store(false, Ordering::Relaxed);
                }
                Cmd::SmartStop {
                    max_buffer_ms,
                    reply_tx,
                } => {
                    // Enter volume-aware trailing buffer mode.
                    // Recording continues; we monitor volume and stop
                    // early when sustained silence is detected.
                    let now = Instant::now();
                    smart_stop = Some(SmartStopState {
                        start: now,
                        max_duration: Duration::from_millis(max_buffer_ms),
                        reply_tx,
                        noise_floor,
                        last_voice_above_floor: now,
                    });
                    log::debug!(
                        "Smart-stop: entering trailing buffer (max {}ms, noise_floor={:.6})",
                        max_buffer_ms,
                        noise_floor,
                    );
                }
                Cmd::Shutdown => {
                    stop_flag.store(true, Ordering::Relaxed);
                    return;
                }
            }
        }
    }
}
