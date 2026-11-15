//! Resampling to the analysis rate, reproducing `librosa.load`'s default
//! `res_type="soxr_hq"` (python-soxr 1.1.0, libsoxr "HQ" quality).
//!
//! The 44100 -> 22050 Hz path, which every file in the fixture libraries
//! takes, is identified from the Python app itself: unit impulses fed through
//! `soxr.resample` show it is a linear-phase, zero-delay FIR decimator with
//! zero-padded edges, `y[m] = sum_k h[k] x[2m - k]`, whose taps fit a
//! Kaiser-windowed sinc to within 2.7e-8 (the measurement's own float32
//! noise is 2.0e-8). The four constants below are that fit, recorded with
//! the measured taps in `tests/data/bpm-stages.json`
//! (recorded from the Python app).
//!
//! Other rate pairs reuse the same continuous kernel scaled to the ratio.
//! That is NOT soxr: on white noise it differs from `soxr.resample` by
//! about 1e-3 relative RMS for 48000, 88200, 96000, 32000 and 16000 Hz
//! inputs (2e-7 at 44100), and no fixture covers another rate. The tempo
//! estimate is insensitive to ~1e-5 relative input noise but not to 1e-3
//! on near-tie files, so non-44.1 kHz parity is unverified.

/// Kaiser-sinc fit of soxr HQ 2:1 taps: `h[k] = GAIN * sinc(CUTOFF * k) *
/// I0(BETA * sqrt(1 - (k / HALF_LEN)^2)) / I0(BETA)`, k in input samples.
pub const SOXR_HQ_GAIN: f64 = 0.478_435_915_096_485_7;
pub const SOXR_HQ_CUTOFF: f64 = 0.478_435_913_926_551_55;
pub const SOXR_HQ_HALF_LEN: f64 = 194.500_264_584_143_17;
pub const SOXR_HQ_BETA: f64 = 13.040_071_808_346_934;

/// Modified Bessel function of the first kind, order 0 (power series).
fn bessel_i0(x: f64) -> f64 {
    let q = x * x / 4.0;
    let mut term = 1.0;
    let mut sum = 1.0;
    let mut k = 1.0;
    while term > sum * 1e-17 {
        term *= q / (k * k);
        sum += term;
        k += 1.0;
    }
    sum
}

fn sinc(x: f64) -> f64 {
    if x == 0.0 {
        1.0
    } else {
        let px = std::f64::consts::PI * x;
        px.sin() / px
    }
}

/// Continuous resampling kernel at offset `t` input samples, for an
/// output/input rate ratio whose anti-aliasing scale is `scale` (= ratio
/// when downsampling, 1 when upsampling). At scale 0.5 this is exactly the
/// fitted soxr HQ 2:1 kernel.
fn kernel(t: f64, scale: f64) -> f64 {
    let s = 2.0 * scale;
    let half_len = SOXR_HQ_HALF_LEN / s;
    let u = t / half_len;
    if u.abs() >= 1.0 {
        return 0.0;
    }
    let window = bessel_i0(SOXR_HQ_BETA * (1.0 - u * u).sqrt()) / bessel_i0(SOXR_HQ_BETA);
    SOXR_HQ_GAIN * s * sinc(SOXR_HQ_CUTOFF * s * t) * window
}

/// The 2:1 taps `h[-r..=r]` (index 0 is k = -r) for r = `HALF_LEN` floored.
pub fn soxr_hq_decimate2_taps() -> Vec<f64> {
    let r = SOXR_HQ_HALF_LEN.floor() as i64;
    (-r..=r).map(|k| kernel(k as f64, 0.5)).collect()
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// `librosa.resample(y, orig_sr, target_sr, res_type="soxr_hq", fix=True)`:
/// `ceil(len * target / orig)` output samples.
pub fn resample(x: &[f64], in_rate: u32, out_rate: u32) -> Vec<f64> {
    if in_rate == out_rate || in_rate == 0 || out_rate == 0 {
        return x.to_vec();
    }
    let ratio = f64::from(out_rate) / f64::from(in_rate);
    let n_out = (x.len() as f64 * ratio).ceil() as usize;
    let g = gcd(u64::from(in_rate), u64::from(out_rate));
    let (up, down) = (u64::from(out_rate) / g, u64::from(in_rate) / g);
    let scale = ratio.min(1.0);
    let reach = (SOXR_HQ_HALF_LEN / (2.0 * scale)).floor() as i64;
    // One tap table per phase: output m sits at input position
    // m * down / up = base + phase / up.
    let phases: Vec<Vec<f64>> = (0..up)
        .map(|p| {
            let frac = p as f64 / up as f64;
            (-reach..=reach + 1)
                .map(|j| kernel(frac - j as f64, scale))
                .collect()
        })
        .collect();
    let n = x.len() as i64;
    let mut out = Vec::with_capacity(n_out);
    for m in 0..n_out as u64 {
        let pos = m * down;
        let base = (pos / up) as i64;
        let taps = &phases[(pos % up) as usize];
        // taps[i] weighs x[base - reach + i]
        let first = base - reach;
        let last = first + taps.len() as i64; // exclusive
        let acc = if first >= 0 && last <= n {
            dot(&x[first as usize..last as usize], taps)
        } else {
            let lo = first.max(0);
            let hi = last.min(n);
            if lo >= hi {
                0.0
            } else {
                dot(
                    &x[lo as usize..hi as usize],
                    &taps[(lo - first) as usize..(hi - first) as usize],
                )
            }
        };
        out.push(acc);
    }
    out
}

fn dot(a: &[f64], b: &[f64]) -> f64 {
    let mut acc = [0.0f64; 8];
    let (ca, ra) = a.as_chunks::<8>();
    let (cb, rb) = b.as_chunks::<8>();
    for (x, y) in ca.iter().zip(cb) {
        for i in 0..8 {
            acc[i] += x[i] * y[i];
        }
    }
    let mut s: f64 = acc.iter().sum();
    for (x, y) in ra.iter().zip(rb) {
        s += x * y;
    }
    s
}
