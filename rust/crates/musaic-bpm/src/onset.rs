//! `librosa.onset.onset_strength(y=y, sr=22050, hop_length=512,
//! aggregate=np.median)` with its defaults: mel spectrogram
//! (`n_fft=2048`, Hann, `center=True`, `pad_mode="constant"`, power 2,
//! 128 Slaney-normalised bands, `fmax=sr/2`), `power_to_db(ref=1.0,
//! amin=1e-10, top_db=80)`, `lag=1`, `max_size=1` (no max filter),
//! `detrend=False`.

use realfft::RealFftPlanner;

use crate::ANALYSIS_RATE;

pub const N_FFT: usize = 2048;
pub const HOP: usize = 512;
pub const N_MELS: usize = 128;
pub const LAG: usize = 1;
const AMIN: f64 = 1e-10;
const TOP_DB: f64 = 80.0;

/// Slaney mel scale (`librosa.hz_to_mel(htk=False)`).
pub fn hz_to_mel(f: f64) -> f64 {
    let f_sp = 200.0 / 3.0;
    let min_log_hz = 1000.0;
    let min_log_mel = min_log_hz / f_sp;
    let logstep = 6.4f64.ln() / 27.0;
    if f >= min_log_hz {
        min_log_mel + (f / min_log_hz).ln() / logstep
    } else {
        f / f_sp
    }
}

/// Inverse of [`hz_to_mel`] (`librosa.mel_to_hz(htk=False)`).
pub fn mel_to_hz(m: f64) -> f64 {
    let f_sp = 200.0 / 3.0;
    let min_log_hz = 1000.0;
    let min_log_mel = min_log_hz / f_sp;
    let logstep = 6.4f64.ln() / 27.0;
    if m >= min_log_mel {
        min_log_hz * (logstep * (m - min_log_mel)).exp()
    } else {
        f_sp * m
    }
}

/// One mel band: its first FFT bin and the contiguous non-zero weights.
#[derive(Debug, Clone)]
pub struct MelBand {
    pub start: usize,
    pub weights: Vec<f64>,
}

/// `librosa.filters.mel(sr=22050, n_fft=2048)` (128 bands, fmin 0,
/// fmax sr/2, Slaney norm), stored sparsely. librosa keeps the basis in
/// float32, so the weights are rounded through f32 as well.
pub fn mel_basis() -> Vec<MelBand> {
    let sr = f64::from(ANALYSIS_RATE);
    let n_bins = N_FFT / 2 + 1;
    let fft_freqs: Vec<f64> = (0..n_bins).map(|k| k as f64 * sr / N_FFT as f64).collect();
    // np.linspace(min_mel, max_mel, n_mels + 2), then mel_to_hz
    let (lo, hi) = (hz_to_mel(0.0), hz_to_mel(sr / 2.0));
    let n = N_MELS + 2;
    let step = (hi - lo) / (n - 1) as f64;
    let mel_f: Vec<f64> = (0..n)
        .map(|i| mel_to_hz(if i == n - 1 { hi } else { lo + i as f64 * step }))
        .collect();
    (0..N_MELS)
        .map(|i| {
            let fdiff_lo = mel_f[i + 1] - mel_f[i];
            let fdiff_hi = mel_f[i + 2] - mel_f[i + 1];
            let enorm = 2.0 / (mel_f[i + 2] - mel_f[i]);
            let row: Vec<f64> = fft_freqs
                .iter()
                .map(|&f| {
                    let lower = -(mel_f[i] - f) / fdiff_lo;
                    let upper = (mel_f[i + 2] - f) / fdiff_hi;
                    // weights[i] = max(0, min(lower, upper)) into a float32
                    // array, then `weights *= enorm` (float64 loop, cast back)
                    let w = 0.0f64.max(lower.min(upper)) as f32;
                    f64::from((f64::from(w) * enorm) as f32)
                })
                .collect();
            match (
                row.iter().position(|&w| w != 0.0),
                row.iter().rposition(|&w| w != 0.0),
            ) {
                (Some(a), Some(b)) => MelBand {
                    start: a,
                    weights: row[a..=b].to_vec(),
                },
                _ => MelBand {
                    start: 0,
                    weights: Vec::new(),
                },
            }
        })
        .collect()
}

/// Periodic Hann window (`scipy.signal.get_window("hann", n, fftbins=True)`).
pub fn hann_periodic(n: usize) -> Vec<f64> {
    (0..n)
        .map(|k| 0.5 - 0.5 * (2.0 * std::f64::consts::PI * k as f64 / n as f64).cos())
        .collect()
}

/// Number of STFT frames for `len` samples (`center=True`).
pub fn n_frames(len: usize) -> usize {
    1 + len / HOP
}

/// Log-mel spectrogram `power_to_db(melspectrogram(y))`, frame-major
/// (`out[t * N_MELS + b]`), plus the pre-clip maxima of the mel power and dB
/// values (for stage comparison).
pub struct LogMel {
    pub n_frames: usize,
    pub db: Vec<f64>,
    pub power_max: f64,
    pub db_max: f64,
}

pub fn log_mel_spectrogram(y: &[f64]) -> LogMel {
    let frames = n_frames(y.len());
    let mut padded = vec![0.0f64; y.len() + N_FFT];
    padded[N_FFT / 2..N_FFT / 2 + y.len()].copy_from_slice(y);
    let window = hann_periodic(N_FFT);
    let basis = mel_basis();
    let fft = RealFftPlanner::<f64>::new().plan_fft_forward(N_FFT);
    let mut input = fft.make_input_vec();
    let mut spectrum = fft.make_output_vec();
    let mut scratch = fft.make_scratch_vec();
    let mut power = vec![0.0f64; N_FFT / 2 + 1];
    let mut db = vec![0.0f64; frames * N_MELS];
    let mut power_max = f64::NEG_INFINITY;
    for t in 0..frames {
        let seg = &padded[t * HOP..t * HOP + N_FFT];
        for ((d, &s), &w) in input.iter_mut().zip(seg).zip(&window) {
            *d = s * w;
        }
        fft.process_with_scratch(&mut input, &mut spectrum, &mut scratch)
            .expect("fixed-size real FFT");
        for (p, c) in power.iter_mut().zip(&spectrum) {
            *p = c.re * c.re + c.im * c.im;
        }
        for (b, band) in basis.iter().enumerate() {
            let m: f64 = band
                .weights
                .iter()
                .zip(&power[band.start..])
                .map(|(w, p)| w * p)
                .sum();
            power_max = power_max.max(m);
            db[t * N_MELS + b] = 10.0 * m.max(AMIN).log10();
        }
    }
    let db_max = db.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let floor = db_max - TOP_DB;
    for v in &mut db {
        *v = v.max(floor);
    }
    LogMel {
        n_frames: frames,
        db,
        power_max,
        db_max,
    }
}

/// The onset-strength envelope, one value per STFT frame.
pub fn onset_strength(y: &[f64]) -> Vec<f64> {
    onset_strength_from_log_mel(&log_mel_spectrogram(y))
}

/// `S[..., lag:] - S[..., :-lag]`, clipped at 0, median over bands, padded
/// with `lag + n_fft // (2 * hop)` leading zeros, cut to the frame count.
pub fn onset_strength_from_log_mel(s: &LogMel) -> Vec<f64> {
    let frames = s.n_frames;
    let pad = LAG + N_FFT / (2 * HOP);
    let mut env = vec![0.0f64; frames];
    let mut diffs = [0.0f64; N_MELS];
    for (t, slot) in env.iter_mut().enumerate().skip(pad) {
        let j = t - pad; // difference index: S[j + lag] - S[j]
        if j + LAG >= frames {
            break;
        }
        let a = &s.db[(j + LAG) * N_MELS..(j + LAG + 1) * N_MELS];
        let b = &s.db[j * N_MELS..(j + 1) * N_MELS];
        for ((d, x), y) in diffs.iter_mut().zip(a).zip(b) {
            *d = (x - y).max(0.0);
        }
        diffs.sort_unstable_by(f64::total_cmp);
        *slot = 0.5 * (diffs[N_MELS / 2 - 1] + diffs[N_MELS / 2]);
    }
    env
}
