//! ReplayGain values.
//!
//! [`integrated_loudness`] is a line-by-line port of pyloudnorm 0.2.0's
//! `Meter(rate).integrated_loudness` (ITU-R BS.1770-4, "K-weighting" filter
//! class built from the RBJ cookbook biquads, applied the way
//! `scipy.signal.lfilter` applies them), not of libebur128, whose filter
//! coefficients differ slightly. Samples are scaled the way soundfile hands
//! them to the Python app: integer PCM divided by 2^(bits-1).
//!
//! The track/album values and tag strings follow
//! `musaic/services/audio_analysis.py`.

use std::f64::consts::PI;
use std::path::Path;

use crate::decode::{DecodeError, DecodedAudio, decode_flac};

/// `audio_analysis.REPLAYGAIN_REFERENCE_LUFS`.
pub const REPLAYGAIN_REFERENCE_LUFS: f64 = -18.0;

/// pyloudnorm `Meter` defaults.
const BLOCK_SIZE: f64 = 0.400;
const OVERLAP: f64 = 0.75;
/// BS.1770 channel weights, in pyloudnorm's channel order L, R, C, Ls, Rs.
const CHANNEL_GAINS: [f64; 5] = [1.0, 1.0, 1.0, 1.41, 1.41];
const ABSOLUTE_GATE: f64 = -70.0;

#[derive(Debug, thiserror::Error)]
pub enum LoudnessError {
    #[error(transparent)]
    Decode(#[from] DecodeError),
    /// pyloudnorm `valid_audio`: "Audio must have five channels or less."
    #[error("Audio must have five channels or less.")]
    TooManyChannels,
    /// pyloudnorm `valid_audio`: "Audio must have length greater than the
    /// block size." The Python app records the track as an error and leaves it
    /// out of the album aggregate.
    #[error("Audio must have length greater than the block size.")]
    TooShort,
}

/// Biquad coefficients normalised by a0, as `IIRfilter.generate_coefficients`.
#[derive(Debug, Clone, Copy)]
struct Biquad {
    b: [f64; 3],
    a: [f64; 3],
}

impl Biquad {
    /// pyloudnorm `IIRfilter(G, Q, fc, rate, 'high_shelf')`.
    fn high_shelf(g: f64, q: f64, fc: f64, rate: f64) -> Self {
        let a = 10f64.powf(g / 40.0);
        let w0 = 2.0 * PI * (fc / rate);
        let alpha = w0.sin() / (2.0 * q);
        let cos = w0.cos();
        let sq = 2.0 * a.sqrt() * alpha;
        let b0 = a * ((a + 1.0) + (a - 1.0) * cos + sq);
        let b1 = -2.0 * a * ((a - 1.0) + (a + 1.0) * cos);
        let b2 = a * ((a + 1.0) + (a - 1.0) * cos - sq);
        let a0 = (a + 1.0) - (a - 1.0) * cos + sq;
        let a1 = 2.0 * ((a - 1.0) - (a + 1.0) * cos);
        let a2 = (a + 1.0) - (a - 1.0) * cos - sq;
        Self::normalised([b0, b1, b2], [a0, a1, a2])
    }

    /// pyloudnorm `IIRfilter(G, Q, fc, rate, 'high_pass')`.
    fn high_pass(q: f64, fc: f64, rate: f64) -> Self {
        let w0 = 2.0 * PI * (fc / rate);
        let alpha = w0.sin() / (2.0 * q);
        let cos = w0.cos();
        let b0 = (1.0 + cos) / 2.0;
        let b1 = -(1.0 + cos);
        let b2 = (1.0 + cos) / 2.0;
        let a0 = 1.0 + alpha;
        let a1 = -2.0 * cos;
        let a2 = 1.0 - alpha;
        Self::normalised([b0, b1, b2], [a0, a1, a2])
    }

    fn normalised(b: [f64; 3], a: [f64; 3]) -> Self {
        let a0 = a[0];
        Self {
            b: b.map(|x| x / a0),
            a: a.map(|x| x / a0),
        }
    }

    /// In-place `scipy.signal.lfilter(b, a, x)` (direct form II transposed,
    /// zero initial state), same operation order as scipy's C loop.
    fn apply(&self, x: &mut [f64]) {
        let [b0, b1, b2] = self.b;
        let [_, a1, a2] = self.a;
        let (mut z0, mut z1) = (0.0f64, 0.0f64);
        for s in x.iter_mut() {
            let xn = *s;
            let yn = z0 + b0 * xn;
            z0 = z1 + xn * b1 - yn * a1;
            z1 = xn * b2 - yn * a2;
            *s = yn;
        }
    }
}

fn mean(values: impl Iterator<Item = f64>) -> f64 {
    let (sum, n) = values.fold((0.0, 0usize), |(s, n), v| (s + v, n + 1));
    if n == 0 { f64::NAN } else { sum / n as f64 }
}

/// Integrated loudness in LUFS of planar float audio (one `Vec` per
/// channel, samples in [-1, 1]). Consumes the buffers (they are filtered in
/// place). Returns `-inf` when no block passes the gates (silence), exactly
/// like pyloudnorm.
pub fn integrated_loudness_planar(
    mut channels: Vec<Vec<f64>>,
    rate: u32,
) -> Result<f64, LoudnessError> {
    let rate_f = f64::from(rate);
    if channels.len() > 5 {
        return Err(LoudnessError::TooManyChannels);
    }
    let num_samples = channels.first().map_or(0, Vec::len);
    if (num_samples as f64) < BLOCK_SIZE * rate_f {
        return Err(LoudnessError::TooShort);
    }

    let shelf = Biquad::high_shelf(4.0, 1.0 / 2f64.sqrt(), 1500.0, rate_f);
    let high_pass = Biquad::high_pass(0.5, 38.0, rate_f);
    for ch in channels.iter_mut() {
        shelf.apply(ch);
        high_pass.apply(ch);
    }

    let step = 1.0 - OVERLAP;
    let t = num_samples as f64 / rate_f;
    let num_blocks =
        (((t - BLOCK_SIZE) / (BLOCK_SIZE * step)).round_ties_even() as i64 + 1).max(0) as usize;
    let scale = 1.0 / (BLOCK_SIZE * rate_f);
    // z[i][j]: mean square of channel i over block j.
    let z: Vec<Vec<f64>> = channels
        .iter()
        .map(|ch| {
            (0..num_blocks)
                .map(|j| {
                    let jf = j as f64;
                    let l = (BLOCK_SIZE * (jf * step) * rate_f) as usize;
                    let u = (BLOCK_SIZE * (jf * step + 1.0) * rate_f) as usize;
                    let (l, u) = (l.min(num_samples), u.min(num_samples));
                    let sum: f64 = ch[l..u.max(l)].iter().map(|v| v * v).sum();
                    scale * sum
                })
                .collect()
        })
        .collect();

    let weighted = |per_channel: &dyn Fn(usize) -> f64| -> f64 {
        (0..channels.len())
            .map(|i| CHANNEL_GAINS[i] * per_channel(i))
            .sum::<f64>()
    };
    let block_loudness: Vec<f64> = (0..num_blocks)
        .map(|j| -0.691 + 10.0 * weighted(&|i| z[i][j]).log10())
        .collect();

    let gated_mean = |keep: &dyn Fn(f64) -> bool, i: usize| -> f64 {
        mean(
            block_loudness
                .iter()
                .enumerate()
                .filter(|(_, l)| keep(**l))
                .map(|(j, _)| z[i][j]),
        )
    };
    let abs_gate = |l: f64| l >= ABSOLUTE_GATE;
    let gamma_r = -0.691 + 10.0 * weighted(&|i| gated_mean(&abs_gate, i)).log10() - 10.0;
    let rel_gate = |l: f64| l > gamma_r && l > ABSOLUTE_GATE;
    // np.nan_to_num: an empty gate gives a NaN mean, which becomes 0.
    let lufs = -0.691
        + 10.0
            * weighted(&|i| {
                let m = gated_mean(&rel_gate, i);
                if m.is_nan() { 0.0 } else { m }
            })
            .log10();
    Ok(lufs)
}

/// Integrated loudness of interleaved float audio (see
/// [`integrated_loudness_planar`]).
pub fn integrated_loudness(
    interleaved: &[f64],
    channels: usize,
    rate: u32,
) -> Result<f64, LoudnessError> {
    let channels = channels.max(1);
    let planar = (0..channels)
        .map(|c| {
            interleaved
                .iter()
                .skip(c)
                .step_by(channels)
                .copied()
                .collect()
        })
        .collect();
    integrated_loudness_planar(planar, rate)
}

/// Sample peak `max(|x|)` (not true peak); 0.0 for no samples.
pub fn sample_peak(samples: &[f64]) -> f64 {
    samples.iter().fold(0.0, |m, v| m.max(v.abs()))
}

/// ReplayGain 2.0 gain string, `"%+.2f dB"` of `reference - measured`.
pub fn format_gain(reference_lufs: f64, measured_lufs: f64) -> String {
    let gain = reference_lufs - measured_lufs;
    if gain.is_nan() {
        // Python formats NaN with an explicit sign as "+nan".
        return "+nan dB".to_string();
    }
    format!("{gain:+.2} dB")
}

/// ReplayGain 2.0 peak string, `"%.6f"`.
pub fn format_peak(peak: f64) -> String {
    if peak.is_nan() {
        return "nan".to_string();
    }
    format!("{peak:.6}")
}

/// `REPLAYGAIN_REFERENCE_LOUDNESS` tag value, `"%.2f LUFS"` of the reference.
pub fn reference_loudness_tag() -> String {
    format!("{REPLAYGAIN_REFERENCE_LUFS:.2} LUFS")
}

/// One track's ReplayGain measurement and tag strings.
#[derive(Debug, Clone, PartialEq)]
pub struct TrackReplayGain {
    /// Integrated loudness; `-inf` for silence (the Python app then writes a
    /// `+inf dB` gain and drops the track from the album mean).
    pub lufs: f64,
    pub peak: f64,
    /// Stream length in seconds (mutagen `info.length`), the album weight.
    pub duration_s: f64,
    pub track_gain: String,
    pub track_peak: String,
}

/// Measure decoded audio.
pub fn track_replaygain(audio: &DecodedAudio) -> Result<TrackReplayGain, LoudnessError> {
    let channels = audio.channels.max(1) as usize;
    // soundfile's float64 scaling of integer PCM: x / 2^(bits-1).
    let full_scale = 2f64.powi(audio.bits_per_sample.clamp(1, 32) as i32 - 1);
    let frames = audio.frames();
    let mut planar: Vec<Vec<f64>> = (0..channels).map(|_| Vec::with_capacity(frames)).collect();
    let mut peak = 0.0f64;
    for frame in audio.samples.chunks_exact(channels) {
        for (c, &s) in frame.iter().enumerate() {
            let v = f64::from(s) / full_scale;
            peak = peak.max(v.abs());
            planar[c].push(v);
        }
    }
    let lufs = integrated_loudness_planar(planar, audio.sample_rate)?;
    Ok(TrackReplayGain {
        lufs,
        peak,
        duration_s: audio.length_seconds(),
        track_gain: format_gain(REPLAYGAIN_REFERENCE_LUFS, lufs),
        track_peak: format_peak(peak),
    })
}

/// Decode and measure one FLAC file.
pub fn track_replaygain_file(path: &Path) -> Result<TrackReplayGain, LoudnessError> {
    track_replaygain(&decode_flac(path)?)
}

/// Album aggregate and its tag strings.
#[derive(Debug, Clone, PartialEq)]
pub struct AlbumReplayGain {
    pub lufs: f64,
    pub peak: f64,
    pub album_gain: String,
    pub album_peak: String,
}

/// Port of `audio_analysis._aggregate_album` over `(duration_s, lufs, peak)`
/// per measured track: energy-weighted mean loudness over tracks with finite
/// loudness and positive duration, max peak over all tracks. `None` when no
/// track qualifies (the Python app then writes no album tags).
pub fn aggregate_album(tracks: &[(f64, f64, f64)]) -> Option<AlbumReplayGain> {
    // Python's max(): keep the first value, replace only on a strictly
    // greater one; 0.0 for no tracks.
    let peak = tracks
        .iter()
        .map(|&(_, _, p)| p)
        .reduce(|cur, p| if p > cur { p } else { cur })
        .unwrap_or(0.0);
    let finite: Vec<(f64, f64)> = tracks
        .iter()
        .filter(|(d, l, _)| l.is_finite() && *d > 0.0)
        .map(|&(d, l, _)| (d, l))
        .collect();
    if finite.is_empty() {
        return None;
    }
    let energy_sum: f64 = finite.iter().map(|(d, l)| d * 10f64.powf(l / 10.0)).sum();
    let duration_sum: f64 = finite.iter().map(|(d, _)| d).sum();
    if duration_sum == 0.0 || energy_sum <= 0.0 {
        return None;
    }
    let lufs = 10.0 * (energy_sum / duration_sum).log10();
    Some(AlbumReplayGain {
        lufs,
        peak,
        album_gain: format_gain(REPLAYGAIN_REFERENCE_LUFS, lufs),
        album_peak: format_peak(peak),
    })
}
