//! Microphone capture via cpal, replacing the `AVAudioRecorder` half of
//! `AudioRecorder.swift`. The cpal `Stream` is `!Send` on some platforms, so
//! a dedicated thread owns it for the whole recording; audio callbacks push
//! interleaved f32 frames into a shared buffer, and `stop()` downmixes to
//! mono and resamples to whisper's 16kHz.
//!
//! Device selection is deliberately absent for now: the system default input
//! is used, mirroring what the Swift app does after
//! `switchSystemDefaultInput(to:)` has run. Per-device selection and the
//! Bluetooth "needs connection time" heuristics are a later, cross-platform
//! concern (see docs/TAURI_REWRITE.md).

use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::resample::{downmix_to_mono, resample_to_whisper_rate, ResampleError};

#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("no default input device (is a microphone connected and permitted?)")]
    NoInputDevice,
    #[error("failed to query the input configuration: {0}")]
    Config(String),
    #[error("unsupported input sample format: {0}")]
    UnsupportedFormat(String),
    #[error("failed to open the input stream: {0}")]
    StreamBuild(String),
    #[error("failed to start the input stream: {0}")]
    StreamPlay(String),
    #[error("the capture thread terminated unexpectedly")]
    ThreadDied,
    #[error(transparent)]
    Resample(#[from] ResampleError),
}

pub struct Recorder {
    stop_tx: mpsc::Sender<()>,
    thread: std::thread::JoinHandle<()>,
    buffer: Arc<Mutex<Vec<f32>>>,
    sample_rate: u32,
    channels: u16,
}

impl Recorder {
    /// Opens the default input device and starts capturing immediately.
    /// Blocks until the stream is live (or failed to open).
    pub fn start() -> Result<Self, CaptureError> {
        let buffer: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let (init_tx, init_rx) = mpsc::channel::<Result<(u32, u16), CaptureError>>();

        let thread_buffer = buffer.clone();
        let thread = std::thread::Builder::new()
            .name("audio-capture".into())
            .spawn(move || capture_thread(thread_buffer, stop_rx, init_tx))
            .expect("failed to spawn the capture thread");

        match init_rx.recv() {
            Ok(Ok((sample_rate, channels))) => Ok(Self {
                stop_tx,
                thread,
                buffer,
                sample_rate,
                channels,
            }),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => Err(CaptureError::ThreadDied),
        }
    }

    /// Native sample rate the device is being captured at.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Seconds of audio captured so far (native rate).
    pub fn elapsed_secs(&self) -> f32 {
        let frames = self.buffer.lock().unwrap().len() / self.channels.max(1) as usize;
        frames as f32 / self.sample_rate as f32
    }

    /// Stops the stream and returns the recording as 16kHz mono f32 PCM.
    pub fn stop(self) -> Result<Vec<f32>, CaptureError> {
        let _ = self.stop_tx.send(());
        let _ = self.thread.join();

        let interleaved = std::mem::take(&mut *self.buffer.lock().unwrap());
        let mono = downmix_to_mono(&interleaved, self.channels);
        Ok(resample_to_whisper_rate(&mono, self.sample_rate)?)
    }
}

/// Owns the cpal stream for the whole recording; exits when `stop_rx` fires
/// (or the `Recorder` is dropped, which closes the channel).
fn capture_thread(
    buffer: Arc<Mutex<Vec<f32>>>,
    stop_rx: mpsc::Receiver<()>,
    init_tx: mpsc::Sender<Result<(u32, u16), CaptureError>>,
) {
    let stream = match open_stream(buffer) {
        Ok((stream, rate, channels)) => {
            let _ = init_tx.send(Ok((rate, channels)));
            stream
        }
        Err(e) => {
            let _ = init_tx.send(Err(e));
            return;
        }
    };

    // Blocks until stop is requested or the Recorder is dropped.
    let _ = stop_rx.recv();
    drop(stream);
}

fn open_stream(
    buffer: Arc<Mutex<Vec<f32>>>,
) -> Result<(cpal::Stream, u32, u16), CaptureError> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or(CaptureError::NoInputDevice)?;
    let supported = device
        .default_input_config()
        .map_err(|e| CaptureError::Config(e.to_string()))?;

    let sample_format = supported.sample_format();
    let config: cpal::StreamConfig = supported.into();
    let sample_rate = config.sample_rate;
    let channels = config.channels;

    let err_fn = |e: cpal::Error| eprintln!("audio-capture stream error: {e}");

    let stream = match sample_format {
        cpal::SampleFormat::F32 => device
            .build_input_stream(
                config,
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    buffer.lock().unwrap().extend_from_slice(data);
                },
                err_fn,
                None,
            )
            .map_err(|e| CaptureError::StreamBuild(e.to_string()))?,
        cpal::SampleFormat::I16 => device
            .build_input_stream(
                config,
                move |data: &[i16], _: &cpal::InputCallbackInfo| {
                    let mut buf = buffer.lock().unwrap();
                    buf.extend(data.iter().map(|&s| s as f32 / 32768.0));
                },
                err_fn,
                None,
            )
            .map_err(|e| CaptureError::StreamBuild(e.to_string()))?,
        cpal::SampleFormat::U16 => device
            .build_input_stream(
                config,
                move |data: &[u16], _: &cpal::InputCallbackInfo| {
                    let mut buf = buffer.lock().unwrap();
                    buf.extend(data.iter().map(|&s| (s as f32 - 32768.0) / 32768.0));
                },
                err_fn,
                None,
            )
            .map_err(|e| CaptureError::StreamBuild(e.to_string()))?,
        other => return Err(CaptureError::UnsupportedFormat(other.to_string())),
    };

    stream
        .play()
        .map_err(|e| CaptureError::StreamPlay(e.to_string()))?;

    Ok((stream, sample_rate, channels))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Needs a real input device and mic permission — run manually with
    /// `cargo test -p audio-capture -- --ignored`.
    #[test]
    #[ignore]
    fn capture_smoke() {
        let recorder = Recorder::start().expect("start");
        assert!(recorder.sample_rate() > 0);
        std::thread::sleep(std::time::Duration::from_millis(300));
        let samples = recorder.stop().expect("stop");
        // ~0.3s at 16kHz; generous bounds, we only prove the pipeline runs.
        assert!(samples.len() > 1000, "got {} samples", samples.len());
    }
}
