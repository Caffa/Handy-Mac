use clap::Parser;
use std::path::PathBuf;
use std::process;
use std::time::Instant;

const SAMPLE_RATE: u32 = 16_000;

#[derive(Parser, Debug, Clone)]
#[command(
    name = "transcribe_wav",
    about = "Re-transcribe a 16kHz mono 16-bit WAV using the same transcribe-cpp path as Handy"
)]
struct Args {
    /// Input WAV file (16kHz mono 16-bit PCM)
    #[arg(long, value_name = "PATH")]
    wav: PathBuf,

    /// Path to the GGUF model
    #[arg(long, value_name = "PATH")]
    model: PathBuf,

    /// Milliseconds of silence (zeros) to prepend before inference
    #[arg(long, value_name = "MS", default_value = "0")]
    pad_ms: u32,

    /// Number of times to run inference
    #[arg(long, value_name = "N", default_value = "1")]
    repeat: u32,
}

fn main() {
    let args = Args::parse();

    if let Err(e) = run(&args) {
        eprintln!("transcribe_wav failed: {}", e);
        process::exit(1);
    }
}

fn run(args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    // Read the WAV file using hound.
    let mut reader = hound::WavReader::open(&args.wav)?;
    let spec = reader.spec();

    if spec.sample_rate != SAMPLE_RATE {
        eprintln!(
            "warning: expected {} Hz sample rate, got {} Hz",
            SAMPLE_RATE, spec.sample_rate
        );
    }
    if spec.channels != 1 {
        eprintln!("warning: expected 1 channel, got {}", spec.channels);
    }
    if spec.sample_format != hound::SampleFormat::Int || spec.bits_per_sample != 16 {
        eprintln!(
            "warning: expected 16-bit integer PCM, got {:?} {}-bit",
            spec.sample_format, spec.bits_per_sample
        );
    }

    let samples: Vec<f32> = reader
        .samples::<i16>()
        .map(|s| s.map(|v| v as f32 / i16::MAX as f32))
        .collect::<Result<Vec<_>, _>>()?;

    // Prepend silence requested via --pad-ms.
    let pad_samples = (args.pad_ms as f32 * (SAMPLE_RATE as f32 / 1000.0)).round() as usize;
    let mut padded = Vec::with_capacity(pad_samples + samples.len());
    padded.resize(pad_samples, 0.0_f32);
    padded.extend_from_slice(&samples);

    let total_samples = padded.len();
    let duration_sec = total_samples as f32 / SAMPLE_RATE as f32;

    println!("samples: {}", total_samples);
    println!("duration: {:.3}s", duration_sec);

    // Initialize the transcribe-cpp backend exactly as Handy does.
    transcribe_cpp::init_logging();
    if let Err(e) = transcribe_cpp::init_backends_default() {
        eprintln!(
            "warning: failed to initialize transcribe-cpp backends: {}",
            e
        );
    }

    // Load the model and create a session.
    let model = transcribe_cpp::Model::load_with(
        &args.model,
        &transcribe_cpp::ModelOptions {
            backend: transcribe_cpp::Backend::Auto,
            device: None,
        },
    )?;
    let mut session = model.session()?;

    let run_options = transcribe_cpp::RunOptions {
        task: transcribe_cpp::Task::Transcribe,
        language: None,
        target_language: None,
        family: None,
        ..Default::default()
    };

    for i in 0..args.repeat {
        let start = Instant::now();
        let result = session.run(&padded, &run_options)?;
        let elapsed = start.elapsed();

        println!(
            "run {}/{} ({:?}): {}",
            i + 1,
            args.repeat,
            elapsed,
            result.text.trim()
        );
    }

    Ok(())
}
