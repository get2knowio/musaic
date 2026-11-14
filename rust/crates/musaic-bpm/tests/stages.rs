//! BPM stage tests: each stage of the Rust tempo pipeline against the
//! intermediates librosa / soxr produced for a small subset of files
//! (`tests/data/bpm-stages.json`, recorded from the Python app's librosa / soxr
//! pipeline),
//! plus the Python app's own behaviour on silence, mono and very short input.

mod common;

use musaic_bpm::{onset, resample, tempo};
use serde_json::Value;

fn max_abs_diff(a: &[f64], b: &[f64]) -> f64 {
    assert_eq!(a.len(), b.len(), "length differs");
    a.iter()
        .zip(b)
        .map(|(x, y)| (x - y).abs())
        .fold(0.0, f64::max)
}

// The resampling kernel equals soxr HQ's measured 44100 -> 22050
// impulse response. Tolerance 5e-8: the measurement is float32 (its own
// asymmetry is ~2e-8) and the closed-form fit residual is 2.7e-8.
#[test]
fn soxr_hq_kernel_matches_measured_impulse_response() {
    let (g, _) = common::stage_golden();
    let s = &g["soxr_hq_44100_to_22050"];
    let fit = &s["kaiser_fit"];
    // The crate constants are the generator's fit, copied verbatim.
    for (name, ours) in [
        ("gain", resample::SOXR_HQ_GAIN),
        ("cutoff", resample::SOXR_HQ_CUTOFF),
        ("half_len", resample::SOXR_HQ_HALF_LEN),
        ("beta", resample::SOXR_HQ_BETA),
    ] {
        assert_eq!(ours, fit[name].as_f64().unwrap(), "fit parameter {name}");
    }
    let measured = common::f64s(&s["taps"]);
    let half = (measured.len() - 1) / 2;
    let ours = resample::soxr_hq_decimate2_taps();
    let r = (ours.len() - 1) / 2;
    let worst = (0..measured.len())
        .map(|i| {
            let k = i as i64 - half as i64;
            let o = if k.unsigned_abs() as usize <= r {
                ours[(k + r as i64) as usize]
            } else {
                0.0
            };
            (o - measured[i]).abs()
        })
        .fold(0.0, f64::max);
    println!("kernel vs soxr impulse response: max |d| = {worst:.3e}");
    assert!(worst <= 5e-8, "max |d| {worst:e}");
}

// Slaney mel filterbank equals librosa.filters.mel(sr=22050, n_fft=2048)
// (same non-zero support; weights within 1 float32 ulp-ish, rel 1e-6).
#[test]
fn mel_basis_matches_librosa() {
    let (g, _) = common::stage_golden();
    let golden = g["mel_basis"].as_array().unwrap();
    let ours = onset::mel_basis();
    assert_eq!(ours.len(), golden.len());
    let mut worst = 0.0f64;
    for (b, (o, gb)) in ours.iter().zip(golden).enumerate() {
        assert_eq!(
            o.start as u64,
            gb["start"].as_u64().unwrap(),
            "band {b} start"
        );
        let gw = common::f64s(&gb["weights"]);
        assert_eq!(o.weights.len(), gw.len(), "band {b} width");
        for (x, y) in o.weights.iter().zip(&gw) {
            worst = worst.max((x - y).abs() / y.abs().max(1e-12));
        }
    }
    println!("mel basis: max relative |d| = {worst:.3e}");
    assert!(worst <= 1e-6, "{worst:e}");
}

struct StageReport {
    file: String,
    excerpt: f64,
    stats: f64,
    db_max: f64,
    power_max_rel: f64,
    env: f64,
    env_scale: f64,
    tg: f64,
    score: f64,
    score_top: f64,
    margin: f64,
    best: (usize, usize),
    tempo: (f64, f64),
    tag: (String, String),
}

fn run_stages(f: &Value) -> StageReport {
    let lib = common::library(f["library"].as_str().unwrap());
    let path = lib.join(f["file"].as_str().unwrap());
    let audio = musaic_dsp::decode::decode_flac(&path).expect("decode");
    let mono = musaic_bpm::downmix(&audio.samples, audio.channels, audio.bits_per_sample);
    let y = resample::resample(&mono, audio.sample_rate, musaic_bpm::ANALYSIS_RATE);

    let r = &f["resampled"];
    assert_eq!(
        y.len() as u64,
        r["len"].as_u64().unwrap(),
        "resampled length"
    );
    let start = r["excerpt_start"].as_u64().unwrap() as usize;
    let gx = common::f64s(&r["excerpt"]);
    let excerpt = max_abs_diff(&y[start..start + gx.len()], &gx);
    let sum: f64 = y.iter().sum();
    let rms = (y.iter().map(|v| v * v).sum::<f64>() / y.len() as f64).sqrt();
    let max_abs = y.iter().fold(0.0f64, |m, v| m.max(v.abs()));
    let stats = [
        (sum - r["sum"].as_f64().unwrap()).abs() / y.len() as f64,
        (rms - r["rms"].as_f64().unwrap()).abs(),
        (max_abs - r["max_abs"].as_f64().unwrap()).abs(),
    ]
    .into_iter()
    .fold(0.0, f64::max);

    let lm = onset::log_mel_spectrogram(&y);
    assert_eq!(
        lm.n_frames as u64,
        f["n_frames"].as_u64().unwrap(),
        "frame count"
    );
    let db_max = (lm.db_max - f["mel_db_max"].as_f64().unwrap()).abs();
    let gp = f["mel_power_max"].as_f64().unwrap();
    let power_max_rel = (lm.power_max - gp).abs() / gp;

    let env = onset::onset_strength_from_log_mel(&lm);
    let genv = common::f64s(&f["onset_envelope"]);
    let env_d = max_abs_diff(&env, &genv);
    let env_scale = genv.iter().fold(0.0f64, |m, v| m.max(v.abs()));

    let est = tempo::estimate(&env);
    let tg = max_abs_diff(&est.tempogram_mean, &common::f64s(&f["tempogram_mean"]));
    let gscore = common::f64s(&f["score"]);
    assert_eq!(est.score.len(), gscore.len());
    let top = [
        f["best_period"].as_u64().unwrap() as usize,
        f["runner_up_period"].as_u64().unwrap() as usize,
    ];
    let score_top = top
        .iter()
        .map(|&k| (est.score[k] - gscore[k]).abs())
        .fold(0.0, f64::max);
    let score = est
        .score
        .iter()
        .zip(&gscore)
        .map(|(a, b)| {
            assert_eq!(a.is_finite(), b.is_finite(), "excluded periods differ");
            if a.is_finite() { (a - b).abs() } else { 0.0 }
        })
        .fold(0.0, f64::max);
    StageReport {
        file: f["file"].as_str().unwrap().to_string(),
        excerpt,
        stats,
        db_max,
        power_max_rel,
        env: env_d,
        env_scale,
        tg,
        score,
        score_top,
        margin: f["margin"].as_f64().unwrap(),
        best: (est.best_period, f["best_period"].as_u64().unwrap() as usize),
        tempo: (est.tempo, f["tempo"].as_f64().unwrap()),
        tag: (
            musaic_bpm::format_bpm_tag(est.tempo),
            f["bpm_tag"].as_str().unwrap().to_string(),
        ),
    }
}

// Per-stage agreement on the chosen subset (4 synthetic, 3 real,
// including the smallest top-1/top-2 score margins of all 94 files).
// Tolerances: resampled samples 1e-6 abs (measured <= 8e-8: soxr computes
// in float32, the port in f64); log-mel max 1e-3 dB; mel power max 1e-5
// relative; onset envelope 1e-3 abs (dB units); tempogram mean 1e-4 abs;
// prior-weighted score at the Python app's best and runner-up periods within half
// the Python app's own top-1/top-2 margin (the ordering of the top two cannot
// flip; the max drift over all periods is printed for information); best period, tempo and "%.2f" tag exact.
// The onset/tempogram drift on the synthetic files (env ~2e-4, tempogram
// ~2e-5) is entirely the resampling stage: librosa fed the f64 closed-form
// kernel's output reproduces the same deviations from the soxr golden,
// while librosa's own f32-vs-f64 difference is ~6e-6 / ~1e-6.
#[test]
fn stages_match_librosa_on_subset() {
    // The real-library files of the subset are private test data.
    let (g, private) = common::stage_golden();
    if !private {
        musaic_testdata::skip_private_cases();
    }
    let files = g["files"].as_array().unwrap().clone();
    let reports = common::par_map(&files, run_stages);
    let mut failures = Vec::new();
    for r in &reports {
        println!(
            "{}: resampled excerpt {:.2e}, stats {:.2e}; dB max {:.2e}; power max rel {:.2e}; \
             onset env {:.2e} (scale {:.2}); tempogram {:.2e}; score {:.2e}, at top-2 periods {:.2e} (margin {:.2e}); \
             period {}/{}; tempo {:.4}/{:.4}",
            r.file,
            r.excerpt,
            r.stats,
            r.db_max,
            r.power_max_rel,
            r.env,
            r.env_scale,
            r.tg,
            r.score,
            r.score_top,
            r.margin,
            r.best.0,
            r.best.1,
            r.tempo.0,
            r.tempo.1
        );
        let checks = [
            ("resampled excerpt", r.excerpt <= 1e-6),
            ("resampled stats", r.stats <= 1e-6),
            ("log-mel max", r.db_max <= 1e-3),
            ("mel power max", r.power_max_rel <= 1e-5),
            ("onset envelope", r.env <= 1e-3),
            ("tempogram mean", r.tg <= 1e-4),
            (
                "score at top-2 periods vs margin",
                r.score_top <= r.margin / 2.0,
            ),
            ("best period", r.best.0 == r.best.1),
            ("tempo", r.tempo.0 == r.tempo.1),
            ("bpm tag", r.tag.0 == r.tag.1),
        ];
        for (stage, ok) in checks {
            if !ok {
                failures.push(format!("{}: {stage}", r.file));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "stage mismatches:\n{}",
        failures.join("\n")
    );
}

/// The integer PCM recipes the golden generator used (`edge_pcm`).
fn edge_pcm(case: &Value) -> (Vec<i32>, u32) {
    let frames = case["frames"].as_u64().unwrap() as i64;
    let channels = case["channels"].as_u64().unwrap() as u32;
    let mono: Vec<i32> = match case["recipe"].as_str().unwrap() {
        "zeros" => vec![0; frames as usize],
        "clicks" => (0..frames)
            .map(|n| {
                let k = n % 22050;
                if k < 441 {
                    // numpy floor division, then the alternating sign
                    let v = (16000 * (441 - k)).div_euclid(441);
                    (if k % 2 == 0 { v } else { -v }) as i32
                } else {
                    0
                }
            })
            .collect(),
        "saw" => (0..frames)
            .map(|k| ((((k * 7919) % 2001) - 1000) * 10) as i16 as i32)
            .collect(),
        other => panic!("unknown recipe {other}"),
    };
    let interleaved = mono
        .iter()
        .flat_map(|&s| std::iter::repeat_n(s, channels as usize))
        .collect();
    (interleaved, channels)
}

// Edge cases as the Python app handles them (run through `_detect_bpm`
// on a temporary FLAC): digital silence -> 0.00, a mono click train ->
// 117.45 (not 120: periods are whole frames), audio too short for any onset
// (<= 4410 samples here) -> 0.00, one second -> 136.00, empty -> 0.00.
#[test]
fn edge_cases_match_python_app() {
    let (g, _) = common::stage_golden();
    let mut failures = Vec::new();
    for case in g["edge_cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        assert_eq!(
            case["outcome"], "ok",
            "{name}: the Python app raised; no Rust counterpart defined"
        );
        let (pcm, channels) = edge_pcm(case);
        let mono = musaic_bpm::downmix(&pcm, channels, 16);
        let bpm = musaic_bpm::bpm_from_mono(&mono, 44100);
        let want = case["tempo"].as_f64().unwrap();
        let tag_ok = case
            .get("bpm_tag")
            .and_then(Value::as_str)
            .is_none_or(|t| t == musaic_bpm::format_bpm_tag(bpm));
        println!("{name}: rust {bpm} python {want}");
        if bpm != want || !tag_ok {
            failures.push(format!("{name}: rust {bpm} python {want}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
