//! Port of `WhisperEngine.swift`'s `transcribeAudio` call sequence: VAD gate,
//! speech-segment stitching, `whisper_full` on a fresh per-run decoding state,
//! segment text extraction, tag cleanup. Audio decoding/resampling (the
//! `AVAudioFile`/`AVAudioConverter` half of the Swift file) is out of scope
//! here — callers hand in 16kHz mono f32 PCM, which is what whisper.cpp
//! itself consumes.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use whisper_rs::{
    FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperError,
    WhisperVadContext, WhisperVadContextParams, WhisperVadParams,
};

#[derive(Debug, thiserror::Error)]
pub enum TranscriptionError {
    #[error("failed to initialize the whisper context: {0}")]
    ContextInit(#[source] WhisperError),
    #[error("failed to initialize the VAD context: {0}")]
    VadInit(#[source] WhisperError),
    #[error("VAD segment detection failed: {0}")]
    VadRun(#[source] WhisperError),
    #[error("failed to create a decoding state: {0}")]
    StateInit(#[source] WhisperError),
    #[error("whisper_full failed: {0}")]
    Run(#[source] WhisperError),
}

/// Progress values reported to the caller, matching the Swift engine's
/// convention: 0-10% is reserved for the audio-conversion phase (done by the
/// caller before `transcribe`), whisper's own 0-100% maps onto 10-95%.
pub type ProgressFn = Box<dyn FnMut(f32) + Send>;

pub struct WhisperEngine {
    context: WhisperContext,
    /// Lazily created on first use, then kept for the engine's lifetime,
    /// mirroring the Swift `vadContext` field.
    vad_context: Option<WhisperVadContext>,
    vad_model_path: PathBuf,
    abort_flag: Arc<AtomicBool>,
}

impl WhisperEngine {
    /// Loads the model. Decoding state is *not* created here: each
    /// `transcribe` call gets a fresh `whisper_state` (see below), so runs
    /// share the model weights while keeping their decoding context —
    /// `prompt_past` — fully isolated, matching
    /// `MyWhisperContext.initFromFileNoState`.
    pub fn load(model_path: &Path, vad_model_path: &Path) -> Result<Self, TranscriptionError> {
        let context = WhisperContext::new_with_params(
            model_path.to_string_lossy().as_ref(),
            WhisperContextParameters::default(),
        )
        .map_err(TranscriptionError::ContextInit)?;

        Ok(Self {
            context,
            vad_context: None,
            vad_model_path: vad_model_path.to_path_buf(),
            abort_flag: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Handle for cancelling an in-flight `transcribe` from another thread,
    /// mirroring `WhisperEngine.cancelTranscription` / `AtomicFlag`.
    pub fn abort_handle(&self) -> Arc<AtomicBool> {
        self.abort_flag.clone()
    }

    /// VAD gate + `whisper_full`. `language` is a whisper language code
    /// (e.g. "it", "en"); `None` lets whisper auto-detect.
    pub fn transcribe(
        &mut self,
        samples: &[f32],
        language: Option<&str>,
        on_progress: Option<ProgressFn>,
    ) -> Result<String, TranscriptionError> {
        self.abort_flag.store(false, Ordering::SeqCst);

        // VAD gate: whisper never sees non-speech audio, so silence cannot
        // produce hallucinated text and long pauses are not decoded at all.
        let segments = self.detect_speech(samples)?;
        if segments.is_empty() {
            return Ok(String::new());
        }
        let speech_samples = speech_only_samples(samples, &segments);

        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 5 });
        params.set_n_threads(available_threads());
        // Fresh decoding state per call means text context flows between 30s
        // windows within one recording (better coherence) but never leaks
        // into the next one.
        params.set_no_context(false);
        params.set_suppress_blank(true);
        params.set_language(language);

        if let Some(mut on_progress) = on_progress {
            params.set_progress_callback_safe(move |percent: i32| {
                on_progress(0.10 + (percent as f32 / 100.0) * 0.85);
            });
        }

        // whisper-rs 0.16.0 bug: set_abort_callback_safe instantiates its C
        // trampoline with the caller's concrete closure type F, but stores the
        // closure double-boxed as Box<dyn FnMut() -> bool>. The trampoline then
        // misreads the fat pointer as F's captures and returns garbage, which
        // ggml takes as "abort" — whisper_full dies with "failed to encode"
        // (-6). Passing an already-boxed trait object makes F itself
        // Box<dyn FnMut() -> bool>, so the buggy instantiation coincides with
        // the correct one (and stays correct if upstream fixes it — the fixed
        // trampoline type no longer depends on F).
        let abort_flag = self.abort_flag.clone();
        let abort_callback: Box<dyn FnMut() -> bool> =
            Box::new(move || abort_flag.load(Ordering::SeqCst));
        params.set_abort_callback_safe(abort_callback);

        let mut state = self
            .context
            .create_state()
            .map_err(TranscriptionError::StateInit)?;
        state
            .full(params, &speech_samples)
            .map_err(TranscriptionError::Run)?;

        let mut text = String::new();
        for i in 0..state.full_n_segments() {
            let Some(segment) = state.get_segment(i) else {
                continue;
            };
            if let Ok(segment_text) = segment.to_str_lossy() {
                text.push_str(&segment_text);
                text.push('\n');
            }
        }

        Ok(clean_text(&text))
    }

    /// Speech segments as (start, end) centisecond pairs.
    fn detect_speech(&mut self, samples: &[f32]) -> Result<Vec<(i64, i64)>, TranscriptionError> {
        if self.vad_context.is_none() {
            let vad = WhisperVadContext::new(
                self.vad_model_path.to_string_lossy().as_ref(),
                WhisperVadContextParams::default(),
            )
            .map_err(TranscriptionError::VadInit)?;
            self.vad_context = Some(vad);
        }
        let vad = self.vad_context.as_mut().unwrap();

        let vad_segments = vad
            .segments_from_samples(WhisperVadParams::default(), samples)
            .map_err(TranscriptionError::VadRun)?;

        let mut segments = Vec::new();
        for i in 0..vad_segments.num_segments() {
            let (Some(start), Some(end)) = (
                vad_segments.get_segment_start_timestamp(i),
                vad_segments.get_segment_end_timestamp(i),
            ) else {
                continue;
            };
            segments.push((start as i64, end as i64));
        }
        Ok(segments)
    }
}

/// Keeps only speech, mirroring `WhisperEngine.speechOnlySamples` (itself
/// mirroring upstream whisper_full VAD stitching): each segment — already
/// padded by the VAD — gets 0.1s of the following audio as overlap, and
/// segments are separated by 0.1s of silence so the decoder still sees
/// natural pauses between phrases.
fn speech_only_samples(samples: &[f32], segments: &[(i64, i64)]) -> Vec<f32> {
    const SAMPLES_PER_CS: usize = 160; // 16 kHz / 100
    const OVERLAP_SAMPLES: usize = 1600; // 0.1 s
    const GAP_SAMPLES: usize = 1600; // 0.1 s

    let mut result = Vec::new();
    for (index, &(start_cs, end_cs)) in segments.iter().enumerate() {
        let is_last = index == segments.len() - 1;
        let start = (start_cs.max(0) as usize * SAMPLES_PER_CS).min(samples.len());
        let mut end = (end_cs.max(0) as usize * SAMPLES_PER_CS).min(samples.len());
        if !is_last {
            end = (end + OVERLAP_SAMPLES).min(samples.len());
        }
        if end <= start {
            continue;
        }
        result.extend_from_slice(&samples[start..end]);
        if !is_last {
            result.extend(std::iter::repeat_n(0.0f32, GAP_SAMPLES));
        }
    }
    result
}

/// Strips whisper's non-speech tags and trims, mirroring the Swift
/// `cleanedText` step.
fn clean_text(text: &str) -> String {
    text.replace("[MUSIC]", "")
        .replace("[BLANK_AUDIO]", "")
        .trim()
        .to_string()
}

fn available_threads() -> i32 {
    std::thread::available_parallelism()
        .map(|n| n.get() as i32)
        .unwrap_or(4)
        .clamp(2, 8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_text_strips_tags_and_trims() {
        assert_eq!(clean_text(" [MUSIC] ciao [BLANK_AUDIO]\n"), "ciao");
        assert_eq!(clean_text("[BLANK_AUDIO]"), "");
    }

    #[test]
    fn speech_only_keeps_single_segment_without_padding() {
        let samples: Vec<f32> = (0..16000).map(|i| i as f32).collect();
        // 0.2s-0.5s => samples 3200..8000; single (= last) segment gets no
        // trailing overlap and no gap.
        let out = speech_only_samples(&samples, &[(20, 50)]);
        assert_eq!(out, samples[3200..8000]);
    }

    #[test]
    fn speech_only_adds_overlap_and_gap_between_segments() {
        let samples: Vec<f32> = vec![1.0; 32000]; // 2s of "speech"
        let out = speech_only_samples(&samples, &[(0, 50), (100, 150)]);
        // First segment: 0..8000 plus 1600 overlap; then a 1600 gap of
        // silence; second (last) segment: 16000..24000 with no padding.
        assert_eq!(out.len(), 8000 + 1600 + 1600 + 8000);
        assert!(out[9600..11200].iter().all(|&s| s == 0.0));
    }

    #[test]
    fn speech_only_clamps_out_of_range_segments() {
        let samples = vec![1.0f32; 1600]; // 0.1s
        // End far past the buffer, start negative: both clamp; inverted
        // segment contributes nothing.
        assert_eq!(speech_only_samples(&samples, &[(-10, 500)]).len(), 1600);
        assert!(speech_only_samples(&samples, &[(50, 20)]).is_empty());
    }
}
