//! Tempo estimation across sample rates, channel counts and bit depths
//! other than 44.1 kHz stereo 16-bit.
//!
//! Everything is built here: a click-and-chord track defined in continuous
//! time (so every rate carries the same beats), and, for the end-to-end
//! cases, a minimal FLAC writer (verbatim subframes, no compression) so that
//! `detect_bpm` runs on real files of each format. Nothing is read from the
//! repository.
//!
//! Expectations follow `synthetic_tempo.rs`: a beat period of exactly `k`
//! analysis frames gives tempo bin `k`. The estimator applies no half/double
//! tempo correction, so these tests use a tempo where the beat period itself
//! is the strongest lag and never ask for an octave decision.

use std::f64::consts::PI;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use musaic_bpm::{ANALYSIS_RATE, bpm_from_mono, detect_bpm, downmix};

const HOP: f64 = 512.0;
/// 22 analysis frames per beat: 117.45 BPM.
const K: usize = 22;
/// Long enough for several 8 s autocorrelation windows, short enough to keep
/// the 96 kHz cases quick.
const SECONDS: f64 = 10.0;
const FIRST_BEAT_S: f64 = 0.31;

fn bin_bpm(k: usize) -> f64 {
    60.0 * f64::from(ANALYSIS_RATE) / (HOP * k as f64)
}

fn period_s() -> f64 {
    K as f64 * HOP / f64::from(ANALYSIS_RATE)
}

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

fn chord(t: f64) -> f64 {
    [220.0, 261.63, 329.63]
        .iter()
        .map(|f| 0.03 * (2.0 * PI * f * t).sin())
        .sum()
}

fn track(rate: u32) -> Vec<f64> {
    let n = (SECONDS * f64::from(rate)) as usize;
    let p = period_s();
    (0..n)
        .map(|i| {
            let t = i as f64 / f64::from(rate);
            let since = t - FIRST_BEAT_S;
            let k = if since < 0.0 {
                0.0
            } else {
                kick(since - (since / p).floor() * p)
            };
            k + chord(t)
        })
        .collect()
}

#[test]
fn low_and_high_sample_rates_give_the_same_tempo_bin() {
    // Up-sampling (8 kHz), a non-integer ratio (32 kHz), 4:1 (88.2 kHz) and
    // the hi-res 96 kHz all resample to 22050 Hz.
    // 8 kHz has no content above 4 kHz, so the 5.2 kHz part of the click is
    // missing there; the envelope still repeats every K frames.
    for rate in [8000u32, 32000, 88200, 96000] {
        let bpm = bpm_from_mono(&track(rate), rate);
        assert_eq!(bpm, bin_bpm(K), "{rate} Hz: got {bpm}, want {}", bin_bpm(K));
    }
}

#[test]
fn downmix_scales_each_bit_depth_to_unit_range() {
    // Full-scale negative is exactly -1.0 at every depth; positive full
    // scale is one step short of 1.0.
    for bits in [8u32, 16, 24] {
        let max = (1i32 << (bits - 1)) - 1;
        let min = -(1i32 << (bits - 1));
        let out = downmix(&[min, max, 0], 1, bits);
        assert_eq!(out[0], -1.0, "{bits}-bit min");
        assert!((out[1] - (1.0 - 1.0 / f64::from(1u32 << (bits - 1)))).abs() < 1e-12);
        assert_eq!(out[2], 0.0);
    }
}

#[test]
fn downmix_averages_channels_and_drops_a_partial_frame() {
    // Stereo: mean of the two channels.
    assert_eq!(downmix(&[16384, -16384, 8192, 24576], 2, 16), [0.0, 0.5]);
    // Mono passes through; 3 channels average over 3.
    assert_eq!(downmix(&[16384], 1, 16), [0.5]);
    assert_eq!(downmix(&[16384, 0, 16384], 3, 16), [1.0 / 3.0]);
    // A trailing incomplete frame is ignored rather than panicking.
    assert_eq!(downmix(&[16384, 16384, 16384], 2, 16), [0.5]);
    // Zero channels is treated as mono rather than dividing by zero.
    assert_eq!(downmix(&[16384], 0, 16), [0.5]);
}

// ---------------------------------------------------------------------------
// A minimal FLAC writer: verbatim subframes only.

fn crc8(data: &[u8]) -> u8 {
    let mut crc = 0u8;
    for &b in data {
        crc ^= b;
        for _ in 0..8 {
            crc = if crc & 0x80 != 0 {
                (crc << 1) ^ 0x07
            } else {
                crc << 1
            };
        }
    }
    crc
}

fn crc16(data: &[u8]) -> u16 {
    let mut crc = 0u16;
    for &b in data {
        crc ^= u16::from(b) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x8005
            } else {
                crc << 1
            };
        }
    }
    crc
}

const BLOCK: usize = 4096;

/// `channels[c][i]` are integer samples that fit `bits` (8, 16 or 24).
fn flac_bytes(rate: u32, bits: u32, channels: &[Vec<i32>]) -> Vec<u8> {
    let n = channels[0].len();
    let nch = channels.len() as u32;
    let mut out = b"fLaC".to_vec();
    out.extend([0x80, 0, 0, 34]);
    out.extend((BLOCK as u16).to_be_bytes());
    out.extend((BLOCK as u16).to_be_bytes());
    out.extend([0u8; 6]); // min/max frame size unknown
    // 20 bits rate, 3 bits channels-1, 5 bits bps-1, 36 bits total samples.
    let packed: u64 = (u64::from(rate) << 44)
        | (u64::from(nch - 1) << 41)
        | (u64::from(bits - 1) << 36)
        | n as u64;
    out.extend(packed.to_be_bytes());
    out.extend([0u8; 16]); // MD5 not set
    let size_code = match bits {
        8 => 1u8,
        16 => 4,
        24 => 6,
        _ => panic!("unsupported depth"),
    };
    for (frame_no, start) in (0..n).step_by(BLOCK).enumerate() {
        let len = BLOCK.min(n - start);
        let mut f = vec![0xFF, 0xF8];
        // Block size: 4096 for full blocks, else "16-bit value follows".
        let (bs_code, tail) = if len == BLOCK {
            (0b1100u8, None)
        } else {
            (0b0111, Some((len - 1) as u16))
        };
        f.push(bs_code << 4); // sample rate from STREAMINFO
        f.push((((nch - 1) as u8) << 4) | (size_code << 1));
        let mut buf = [0u8; 4];
        f.extend(
            char::from_u32(frame_no as u32)
                .expect("frame number")
                .encode_utf8(&mut buf)
                .as_bytes(),
        );
        if let Some(t) = tail {
            f.extend(t.to_be_bytes());
        }
        f.push(crc8(&f));
        for ch in channels {
            f.push(0b0000_0010); // verbatim, no wasted bits
            for &s in &ch[start..start + len] {
                let b = s.to_be_bytes();
                f.extend(&b[4 - (bits / 8) as usize..]);
            }
        }
        let c = crc16(&f);
        f.extend(c.to_be_bytes());
        out.extend(f);
    }
    out
}

struct TempFile(PathBuf);
impl TempFile {
    fn new(bytes: &[u8]) -> TempFile {
        static N: AtomicUsize = AtomicUsize::new(0);
        let p = std::env::temp_dir().join(format!(
            "musaic-bpm-test-{}-{}.flac",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&p, bytes).expect("write temp flac");
        TempFile(p)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn quantise(y: &[f64], bits: u32) -> Vec<i32> {
    let scale = f64::from(1u32 << (bits - 1));
    y.iter()
        .map(|v| (v * scale).round().clamp(-scale, scale - 1.0) as i32)
        .collect()
}

fn detect(rate: u32, bits: u32, nch: usize) -> f64 {
    let ch = quantise(&track(rate), bits);
    let file = TempFile::new(&flac_bytes(rate, bits, &vec![ch; nch]));
    detect_bpm(file.path()).expect("detect_bpm")
}

#[test]
fn flac_files_of_other_formats_give_the_same_tempo_bin() {
    // (rate, bits, channels): hi-res 24-bit stereo, 24-bit mono, 16-bit
    // mono and 8-bit stereo at a rate that is not a multiple of 22050.
    let cases = [
        (96000u32, 24u32, 2usize),
        (48000, 24, 1),
        (44100, 16, 1),
        (32000, 8, 2),
    ];
    for (rate, bits, nch) in cases {
        let bpm = detect(rate, bits, nch);
        assert_eq!(
            bpm,
            bin_bpm(K),
            "{rate} Hz, {bits}-bit, {nch} ch: got {bpm}, want {}",
            bin_bpm(K)
        );
    }
}

#[test]
fn a_flac_with_a_partial_last_block_decodes() {
    // The sample count is not a multiple of the 4096-sample block.
    let rate = 44100;
    let mut y = track(rate);
    y.truncate(y.len() - 1234);
    let file = TempFile::new(&flac_bytes(rate, 16, &[quantise(&y, 16)]));
    assert_eq!(detect_bpm(file.path()).expect("detect"), bin_bpm(K));
}
