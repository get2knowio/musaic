//! Tempo estimation on a synthetic, music-like signal built here: a kick
//! drum on every beat (a short decaying burst with a few harmonics and a
//! bright attack) over a quiet sustained chord. Nothing is read from disk.
//!
//! The estimator (see `src/tempo.rs`) answers one of its tempo bins,
//! `60 * 22050 / (512 * k)` BPM for an autocorrelation lag of `k` onset
//! frames (512 samples at 22050 Hz each). The rules pinned here:
//!
//! * a beat period of exactly `k` analysis frames makes the onset envelope
//!   periodic with period `k`, so the estimate is bin `k` exactly;
//! * any other tempo `T` lies between two bins, the lags
//!   `floor(k*)` and `ceil(k*)` with `k* = 60 * 22050 / (512 * T)`, and the
//!   estimate is one of those two;
//! * this holds whatever the input sample rate (22050, 44100, 48000 Hz):
//!   the signal is defined in continuous time, so every rate carries the
//!   same beats, and resampling to 22050 Hz keeps their period.
//!
//! Why the tempos are taken between 95 and 145 BPM: the kick is the only
//! event, once per beat, so the envelope has no energy at half the period
//! (no off-beat), and the strongest autocorrelation peaks are at the beat
//! period and its multiples. The Hann-windowed 8 s autocorrelation is
//! lower at twice the lag than at the lag itself, and the log-normal prior
//! (centred on 120 BPM, one octave wide) costs the doubled period (48-72
//! BPM, about one octave from 120) more than the period itself (within
//! about 0.35 octave of 120), so both push towards the beat period. The
//! bins are far from the 320 BPM cap. No claim is made for tempos where
//! the prior could favour another multiple: the estimator promises no
//! octave correction.

use std::f64::consts::PI;

use musaic_bpm::{ANALYSIS_RATE, bpm_from_mono, format_bpm_tag};

/// Samples per onset frame at the analysis rate.
const HOP: f64 = 512.0;

/// The tempo of lag `k` frames.
fn bin_bpm(k: usize) -> f64 {
    60.0 * f64::from(ANALYSIS_RATE) / (HOP * k as f64)
}

/// The lag (in frames, fractional) of a tempo.
fn lag_of(bpm: f64) -> f64 {
    60.0 * f64::from(ANALYSIS_RATE) / (HOP * bpm)
}

/// One kick, `t` seconds after it starts: a 0.12 s burst, exponentially
/// decaying, with a low fundamental, two harmonics and a short bright click
/// at the attack so several mel bands see the onset.
fn kick(t: f64) -> f64 {
    if !(0.0..0.12).contains(&t) {
        return 0.0;
    }
    let body = (-t / 0.03).exp()
        * ((2.0 * PI * 60.0 * t).sin()
            + 0.5 * (2.0 * PI * 120.0 * t).sin()
            + 0.25 * (2.0 * PI * 180.0 * t).sin());
    let click = (-t / 0.004).exp()
        * (0.4 * (2.0 * PI * 2500.0 * t).sin() + 0.3 * (2.0 * PI * 5200.0 * t).sin());
    0.6 * (body + click)
}

/// A quiet A-minor chord that never changes (no onsets of its own).
fn chord(t: f64) -> f64 {
    [220.0, 261.63, 329.63]
        .iter()
        .map(|f| 0.03 * (2.0 * PI * f * t).sin())
        .sum()
}

/// `seconds` of the signal at `rate`: a kick every `period_s` seconds from
/// `first_s`, over the chord. Defined in continuous time, so the beats are
/// the same at every sample rate.
fn kick_track(rate: u32, seconds: f64, period_s: f64, first_s: f64) -> Vec<f64> {
    let n = (seconds * f64::from(rate)) as usize;
    (0..n)
        .map(|i| {
            let t = i as f64 / f64::from(rate);
            let since = t - first_s;
            let k = if since < 0.0 {
                0.0
            } else {
                kick(since - (since / period_s).floor() * period_s)
            };
            k + chord(t)
        })
        .collect()
}

/// 30 s of audio: about 1300 onset frames, several 8 s autocorrelation
/// windows' worth.
const SECONDS: f64 = 30.0;
/// The first beat, off any frame boundary (0.31 s is 13.3 hops).
const FIRST_BEAT_S: f64 = 0.31;

#[test]
fn beat_period_of_whole_frames_gives_that_tempo_bin_at_every_sample_rate() {
    // k = 20, 22, 24 frames: 129.20, 117.45, 107.67 BPM.
    for k in [20usize, 22, 24] {
        let period_s = k as f64 * HOP / f64::from(ANALYSIS_RATE);
        for rate in [22050u32, 44100, 48000] {
            let y = kick_track(rate, SECONDS, period_s, FIRST_BEAT_S);
            let bpm = bpm_from_mono(&y, rate);
            assert_eq!(
                bpm,
                bin_bpm(k),
                "period {k} frames at {rate} Hz: got {bpm}, want bin {k} ({})",
                bin_bpm(k)
            );
        }
    }
}

#[test]
fn any_tempo_lands_on_one_of_the_two_neighbouring_bins() {
    for (bpm_in, rate) in [
        (100.0, 44100u32),
        (112.5, 48000),
        (120.0, 22050),
        (128.0, 44100),
        (140.0, 48000),
    ] {
        let lag = lag_of(bpm_in);
        let (lo, hi) = (lag.floor() as usize, lag.ceil() as usize);
        let y = kick_track(rate, SECONDS, 60.0 / bpm_in, FIRST_BEAT_S);
        let bpm = bpm_from_mono(&y, rate);
        assert!(
            bpm == bin_bpm(lo) || bpm == bin_bpm(hi),
            "{bpm_in} BPM at {rate} Hz: got {bpm}, want bin {lo} ({}) or {hi} ({})",
            bin_bpm(lo),
            bin_bpm(hi)
        );
        // Being one of the two bins around the tempo, the estimate is at
        // most one bin step away from it.
        assert!(
            (bpm - bpm_in).abs() <= (bin_bpm(lo) - bin_bpm(hi)).abs(),
            "{bpm_in} BPM: {bpm} is farther than one bin step"
        );
    }
}

#[test]
fn the_estimate_does_not_depend_on_where_the_first_beat_falls() {
    // Shifting the whole pattern by a fraction of a frame changes no
    // period, so the bin stays the same.
    let k = 22usize;
    let period_s = k as f64 * HOP / f64::from(ANALYSIS_RATE);
    for first in [0.05, 0.2, 0.333, 0.5] {
        let y = kick_track(44100, SECONDS, period_s, first);
        assert_eq!(bpm_from_mono(&y, 44100), bin_bpm(k), "first beat {first}");
    }
}

#[test]
fn silence_has_no_tempo() {
    // No onset at all: the envelope is all zero and the estimate is 0.0.
    assert_eq!(bpm_from_mono(&vec![0.0; 44100 * 5], 44100), 0.0);
}

#[test]
fn bpm_tag_has_two_decimals() {
    // The tag is the tempo formatted with two decimals.
    assert_eq!(format_bpm_tag(bin_bpm(20)), "129.20");
    assert_eq!(format_bpm_tag(bin_bpm(24)), "107.67");
    assert_eq!(format_bpm_tag(0.0), "0.00");
}
