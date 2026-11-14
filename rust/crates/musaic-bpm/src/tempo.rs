//! `librosa.feature.tempo(onset_envelope=env, sr=22050, hop_length=512)`
//! with its defaults (`start_bpm=120`, `std_bpm=1`, `ac_size=8`,
//! `max_tempo=320`, `aggregate=np.mean`, no explicit prior).

use realfft::RealFftPlanner;

use crate::ANALYSIS_RATE;
use crate::onset::{HOP, hann_periodic};

/// `time_to_frames(8.0, sr=22050, hop_length=512)`.
pub const WIN: usize = (8 * ANALYSIS_RATE as usize) / HOP;
pub const START_BPM: f64 = 120.0;
pub const STD_BPM: f64 = 1.0;
pub const MAX_TEMPO: f64 = 320.0;
/// Autocorrelation FFT length: any size >= 2 * WIN - 1 gives the same
/// linear autocorrelation.
const AC_FFT: usize = 1024;

/// Every intermediate of the estimator, for stage comparison.
#[derive(Debug, Clone)]
pub struct TempoEstimate {
    /// Time-averaged, inf-normalised autocorrelation tempogram (`WIN` lags).
    pub tempogram_mean: Vec<f64>,
    /// `log1p(1e6 * tempogram_mean) + logprior` (`-inf` where excluded).
    pub score: Vec<f64>,
    pub best_period: usize,
    pub tempo: f64,
}

/// `tempo_frequencies(WIN)`: BPM of each lag (lag 0 is +inf).
pub fn tempo_frequencies() -> Vec<f64> {
    (0..WIN)
        .map(|k| {
            if k == 0 {
                f64::INFINITY
            } else {
                60.0 * f64::from(ANALYSIS_RATE) / (HOP as f64 * k as f64)
            }
        })
        .collect()
}

/// `tempogram(onset_envelope=env, win_length=WIN).mean(axis=-1)`.
pub fn tempogram_mean(env: &[f64]) -> Vec<f64> {
    let n = env.len();
    let half = WIN / 2;
    // np.pad(mode="linear_ramp", end_values=0): linspace(0, edge, half, endpoint=False)
    let mut padded = Vec::with_capacity(n + 2 * half);
    let (first, last) = (
        env.first().copied().unwrap_or(0.0),
        env.last().copied().unwrap_or(0.0),
    );
    padded.extend((0..half).map(|i| first * i as f64 / half as f64));
    padded.extend_from_slice(env);
    padded.extend((0..half).map(|j| last * (half - 1 - j) as f64 / half as f64));

    let window = hann_periodic(WIN);
    let mut planner = RealFftPlanner::<f64>::new();
    let fwd = planner.plan_fft_forward(AC_FFT);
    let inv = planner.plan_fft_inverse(AC_FFT);
    let mut buf = fwd.make_input_vec();
    let mut spec = fwd.make_output_vec();
    let mut scratch_f = fwd.make_scratch_vec();
    let mut scratch_i = inv.make_scratch_vec();
    let mut sum = vec![0.0f64; WIN];
    for t in 0..n {
        buf.iter_mut().for_each(|v| *v = 0.0);
        for ((d, &x), &w) in buf.iter_mut().zip(&padded[t..t + WIN]).zip(&window) {
            *d = x * w;
        }
        fwd.process_with_scratch(&mut buf, &mut spec, &mut scratch_f)
            .expect("fixed-size real FFT");
        for c in spec.iter_mut() {
            *c = realfft::num_complex::Complex::new(c.re * c.re + c.im * c.im, 0.0);
        }
        inv.process_with_scratch(&mut spec, &mut buf, &mut scratch_i)
            .expect("fixed-size inverse real FFT");
        let ac = &buf[..WIN];
        // util.normalize(norm=inf): divide by max |.|; columns below `tiny`
        // are left as they are.
        let len = ac.iter().fold(0.0f64, |m, v| m.max(v.abs()));
        let len = if len < f64::from(f32::MIN_POSITIVE) {
            1.0
        } else {
            len
        };
        for (s, v) in sum.iter_mut().zip(ac) {
            *s += v / len;
        }
    }
    let n = n.max(1) as f64;
    sum.iter().map(|s| s / n).collect()
}

/// Run the estimator on an onset envelope.
pub fn estimate(env: &[f64]) -> TempoEstimate {
    let tg = tempogram_mean(env);
    let bpms = tempo_frequencies();
    let max_idx = bpms.iter().position(|&b| b < MAX_TEMPO).unwrap_or(0);
    let score: Vec<f64> = tg
        .iter()
        .zip(&bpms)
        .enumerate()
        .map(|(k, (&g, &b))| {
            let logprior = if k < max_idx {
                f64::NEG_INFINITY
            } else {
                -0.5 * ((b.log2() - START_BPM.log2()) / STD_BPM).powi(2)
            };
            (1e6 * g).ln_1p() + logprior
        })
        .collect();
    let best_period = argmax(&score);
    TempoEstimate {
        tempo: bpms[best_period],
        best_period,
        score,
        tempogram_mean: tg,
    }
}

/// `np.argmax`: first maximum, and a NaN wins (numpy propagates it).
fn argmax(v: &[f64]) -> usize {
    let mut best = 0;
    for (i, &x) in v.iter().enumerate() {
        if x.is_nan() {
            return i;
        }
        if x > v[best] {
            best = i;
        }
    }
    best
}
