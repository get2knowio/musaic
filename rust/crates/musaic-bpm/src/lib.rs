//! Tempo detection, ported from the Python app.
//!
//! The Python app (`musaic/services/audio_analysis.py::_detect_bpm`) runs
//!
//! ```python
//! y, sr = librosa.load(path, sr=22050, mono=True)
//! tempo, _beats = librosa.beat.beat_track(y=y, sr=sr)
//! ```
//!
//! and writes `"%.2f" % tempo` as the `bpm` Vorbis comment. In librosa 1.0.0
//! `beat_track` returns the tempo from `feature.tempo` unchanged: the
//! dynamic-programming beat tracker runs afterwards and only produces the
//! (discarded) beat positions. So the returned BPM depends on exactly:
//!
//! 1. loading: integer PCM scaled by `2^(bits-1)`, channel mean, then soxr
//!    "HQ" resampling to 22050 Hz ([`resample`]);
//! 2. the onset-strength envelope: 2048-point Hann STFT (hop 512, centred,
//!    zero padded), 128-band Slaney mel power, `power_to_db` (amin 1e-10,
//!    top_db 80), first difference with lag 1, half-wave rectification,
//!    median over bands, 3 frames of leading zero padding ([`onset`]);
//! 3. the tempo estimator: autocorrelation tempogram (8 s = 344 frames,
//!    Hann window, linear-ramp padding, inf-norm), time average, log-normal
//!    prior around 120 BPM (one octave std), periods faster than 320 BPM
//!    excluded, argmax ([`tempo`]);
//! 4. the `onset_envelope.any()` short-circuit, which returns 0.0.
//!
//! No half/double-tempo correction is applied: the Python app has none.

use std::path::Path;

pub mod onset;
pub mod resample;
pub mod tempo;

/// Sample rate the Python app analyses at (`librosa.load(..., sr=22050)`).
pub const ANALYSIS_RATE: u32 = 22050;

/// Errors from [`detect_bpm`].
#[derive(Debug, thiserror::Error)]
pub enum BpmError {
    #[error(transparent)]
    Decode(#[from] musaic_dsp::decode::DecodeError),
}

/// Tempo of a FLAC file in beats per minute, as the Python app's `_detect_bpm`.
pub fn detect_bpm(path: &Path) -> Result<f64, BpmError> {
    let audio = musaic_dsp::decode::decode_flac(path)?;
    let mono = downmix(&audio.samples, audio.channels, audio.bits_per_sample);
    Ok(bpm_from_mono(&mono, audio.sample_rate))
}

/// Tempo of a mono signal (floats in [-1, 1]) at `sample_rate`: resample to
/// 22050 Hz, then [`bpm_from_analysis_signal`].
pub fn bpm_from_mono(mono: &[f64], sample_rate: u32) -> f64 {
    let y = resample::resample(mono, sample_rate, ANALYSIS_RATE);
    bpm_from_analysis_signal(&y)
}

/// Tempo of a mono signal already at 22050 Hz (`beat_track(y=y, sr=22050)`).
pub fn bpm_from_analysis_signal(y: &[f64]) -> f64 {
    let env = onset::onset_strength(y);
    // `if not onset_envelope.any(): return (0.0, ...)`
    if !env.iter().any(|&v| v != 0.0) {
        return 0.0;
    }
    tempo::estimate(&env).tempo
}

/// `soundfile.read(dtype="float32")` then `librosa.to_mono`: each integer
/// sample over `2^(bits-1)`, averaged across channels.
pub fn downmix(interleaved: &[i32], channels: u32, bits_per_sample: u32) -> Vec<f64> {
    let channels = channels.max(1) as usize;
    let scale = 1.0 / f64::from(1u32 << (bits_per_sample.clamp(1, 32) - 1));
    interleaved
        .chunks_exact(channels)
        .map(|frame| frame.iter().map(|&s| f64::from(s) * scale).sum::<f64>() / channels as f64)
        .collect()
}

/// The tag string the Python app writes: `f"{bpm:.2f}"`.
pub fn format_bpm_tag(bpm: f64) -> String {
    format!("{bpm:.2}")
}
