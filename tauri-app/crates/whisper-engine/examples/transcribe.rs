//! Phase-1 proof-of-concept CLI: load a ggml model, transcribe a 16kHz mono
//! WAV, print the text.
//!
//! From the repo root:
//! ```sh
//! cargo run -p whisper-engine --example transcribe -- \
//!     --model ../../../ggml-tiny.en.bin \
//!     --vad ../../../OpenSuperWhisper/ggml-silero-v5.1.2.bin \
//!     --wav ../../../jfk.wav
//! ```

use std::path::PathBuf;
use std::process::ExitCode;

use whisper_engine::model_validation::validate_downloaded_model;
use whisper_engine::transcribe::WhisperEngine;

struct Args {
    model: PathBuf,
    vad: PathBuf,
    wav: PathBuf,
    language: Option<String>,
    /// Flip the abort flag this many ms after starting, to exercise the
    /// cancellation path.
    abort_after_ms: Option<u64>,
}

fn parse_args() -> Result<Args, String> {
    let mut model = None;
    let mut vad = None;
    let mut wav = None;
    let mut language = None;
    let mut abort_after_ms = None;

    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let mut value = |name: &str| args.next().ok_or(format!("{name} requires a value"));
        match flag.as_str() {
            "--model" => model = Some(PathBuf::from(value("--model")?)),
            "--vad" => vad = Some(PathBuf::from(value("--vad")?)),
            "--wav" => wav = Some(PathBuf::from(value("--wav")?)),
            "--language" => language = Some(value("--language")?),
            "--abort-after-ms" => {
                abort_after_ms = Some(
                    value("--abort-after-ms")?
                        .parse()
                        .map_err(|e| format!("--abort-after-ms: {e}"))?,
                )
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }

    Ok(Args {
        model: model.ok_or("--model is required")?,
        vad: vad.ok_or("--vad is required")?,
        wav: wav.ok_or("--wav is required")?,
        language,
        abort_after_ms,
    })
}

fn read_wav_16k_mono_f32(path: &PathBuf) -> Result<Vec<f32>, String> {
    let reader = hound::WavReader::open(path).map_err(|e| format!("open {path:?}: {e}"))?;
    let spec = reader.spec();
    if spec.sample_rate != 16_000 || spec.channels != 1 {
        return Err(format!(
            "expected 16kHz mono WAV, got {}Hz {}ch (resampling lands in the audio-capture crate, phase 2)",
            spec.sample_rate, spec.channels
        ));
    }
    match (spec.sample_format, spec.bits_per_sample) {
        (hound::SampleFormat::Int, 16) => Ok(reader
            .into_samples::<i16>()
            .filter_map(Result::ok)
            .map(|s| s as f32 / 32768.0)
            .collect()),
        (hound::SampleFormat::Float, 32) => {
            Ok(reader.into_samples::<f32>().filter_map(Result::ok).collect())
        }
        (format, bits) => Err(format!("unsupported WAV sample format: {format:?} {bits}-bit")),
    }
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(args) => args,
        Err(e) => {
            eprintln!("error: {e}");
            eprintln!("usage: transcribe --model <ggml.bin> --vad <silero.bin> --wav <16kHz-mono.wav> [--language it]");
            return ExitCode::FAILURE;
        }
    };

    if let Err(e) = validate_downloaded_model(&args.model) {
        eprintln!("model validation failed: {e}");
        return ExitCode::FAILURE;
    }

    let samples = match read_wav_16k_mono_f32(&args.wav) {
        Ok(samples) => samples,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    eprintln!("read {} samples ({:.1}s)", samples.len(), samples.len() as f32 / 16000.0);

    let mut engine = match WhisperEngine::load(&args.model, &args.vad) {
        Ok(engine) => engine,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    if let Some(ms) = args.abort_after_ms {
        let abort = engine.abort_handle();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(ms));
            eprintln!("\nflipping abort flag after {ms}ms");
            abort.store(true, std::sync::atomic::Ordering::SeqCst);
        });
    }

    let on_progress = Box::new(|p: f32| eprint!("\rtranscribing… {:3.0}%", p * 100.0));
    match engine.transcribe(&samples, args.language.as_deref(), Some(on_progress)) {
        Ok(text) => {
            eprintln!();
            println!("{text}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("\nerror: {e}");
            ExitCode::FAILURE
        }
    }
}
