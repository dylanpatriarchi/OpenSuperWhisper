//! Offline resampling of a captured buffer to whisper's expected 16kHz.
//! Replaces the `AVAudioConverter` half of the Swift engine's
//! `convertAudioToPCM` for the capture path (file decoding is a separate
//! concern, handled when file-drop transcription is ported).

use rubato::{
    Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};

pub const WHISPER_SAMPLE_RATE: u32 = 16_000;

#[derive(Debug, thiserror::Error)]
pub enum ResampleError {
    #[error("failed to construct the resampler: {0}")]
    Construction(#[from] rubato::ResamplerConstructionError),
    #[error("resampling failed: {0}")]
    Process(#[from] rubato::ResampleError),
}

/// Downmixes interleaved frames to mono by averaging channels, mirroring the
/// simple mixdown case of the Swift engine. (The Swift RMS-based
/// active-channel selection for multi-channel interfaces can be layered on
/// later; averaging is correct for the mono/stereo mics that dominate.)
pub fn downmix_to_mono(interleaved: &[f32], channels: u16) -> Vec<f32> {
    let channels = channels.max(1) as usize;
    if channels == 1 {
        return interleaved.to_vec();
    }
    interleaved
        .chunks_exact(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect()
}

/// Resamples mono f32 samples from `input_rate` to 16kHz. Input is consumed
/// in fixed chunks with a final partial flush, so arbitrary buffer lengths
/// round-trip without dropping the tail.
pub fn resample_to_whisper_rate(samples: &[f32], input_rate: u32) -> Result<Vec<f32>, ResampleError> {
    if input_rate == WHISPER_SAMPLE_RATE || samples.is_empty() {
        return Ok(samples.to_vec());
    }

    const CHUNK: usize = 1024;
    let ratio = WHISPER_SAMPLE_RATE as f64 / input_rate as f64;
    let params = SincInterpolationParameters {
        sinc_len: 128,
        f_cutoff: 0.95,
        interpolation: SincInterpolationType::Linear,
        oversampling_factor: 128,
        window: WindowFunction::BlackmanHarris2,
    };
    let mut resampler = SincFixedIn::<f32>::new(ratio, 1.0, params, CHUNK, 1)?;

    // The sinc filter introduces a fixed leading delay, and the final
    // zero-padded flush emits up to a chunk of trailing silence; trim the
    // former and truncate to the expected length so the output duration
    // matches the input exactly.
    let delay = resampler.output_delay();
    let expected_len = (samples.len() as f64 * ratio).round() as usize;

    let mut out = Vec::with_capacity((samples.len() as f64 * ratio) as usize + CHUNK);
    let mut chunks = samples.chunks_exact(CHUNK);
    for chunk in &mut chunks {
        let processed = resampler.process(&[chunk], None)?;
        out.extend_from_slice(&processed[0]);
    }
    let remainder = chunks.remainder();
    if !remainder.is_empty() {
        let processed = resampler.process_partial(Some(&[remainder]), None)?;
        out.extend_from_slice(&processed[0]);
    }
    // Flush the tail still buffered inside the filter.
    let processed = resampler.process_partial::<&[f32]>(None, None)?;
    out.extend_from_slice(&processed[0]);

    out.drain(0..delay.min(out.len()));
    out.truncate(expected_len);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downmix_averages_stereo_frames() {
        let interleaved = [1.0, 0.0, 0.5, 0.5, -1.0, 1.0];
        assert_eq!(downmix_to_mono(&interleaved, 2), vec![0.5, 0.5, 0.0]);
    }

    #[test]
    fn downmix_passes_mono_through() {
        let mono = [0.1, 0.2, 0.3];
        assert_eq!(downmix_to_mono(&mono, 1), mono.to_vec());
    }

    #[test]
    fn resample_48k_produces_expected_length_and_preserves_tone() {
        // 1s of a 440Hz sine at 48kHz should come out as ~1s at 16kHz with
        // the tone intact (rough energy check, not a spectral assertion).
        let input: Vec<f32> = (0..48_000)
            .map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 48_000.0).sin())
            .collect();
        let out = resample_to_whisper_rate(&input, 48_000).unwrap();
        assert_eq!(out.len(), 16_000);
        let rms = (out.iter().map(|s| s * s).sum::<f32>() / out.len() as f32).sqrt();
        assert!(rms > 0.5, "sine energy lost in resampling (rms={rms})");
    }

    #[test]
    fn resample_non_integer_ratio_44100() {
        let input = vec![0.25f32; 44_100];
        let out = resample_to_whisper_rate(&input, 44_100).unwrap();
        assert_eq!(out.len(), 16_000);
    }

    #[test]
    fn resample_short_buffer_smaller_than_one_chunk() {
        let input = vec![0.5f32; 300];
        let out = resample_to_whisper_rate(&input, 48_000).unwrap();
        assert!(!out.is_empty());
    }

    #[test]
    fn resample_at_target_rate_is_identity() {
        let input = vec![0.1f32, 0.2, 0.3];
        assert_eq!(resample_to_whisper_rate(&input, 16_000).unwrap(), input);
    }
}
