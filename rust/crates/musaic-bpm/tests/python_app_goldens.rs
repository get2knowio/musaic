//! The Rust tempo estimate against the Python app's BPM goldens for
//! every file of both fixture libraries (42 synthetic + 52 real), classified
//! by the goldens' own `tolerance_policy.bpm`: match (|rust - golden| <= 2),
//! octave (within 2 of golden * 2 or golden / 2, a distinct outcome, not a
//! pass), mismatch. Every file must be `match`.

mod common;

use std::time::Instant;

/// (golden, library kind, private): the real-audio set's goldens and audio
/// are private test data.
const GOLDENS: [(&str, &str, bool); 2] = [
    (
        "captures/dsp-synthetic/dsp/goldens.json",
        "synthetic",
        false,
    ),
    ("captures/dsp-real/dsp/goldens.json", "real", true),
];

#[derive(Debug, PartialEq)]
enum Outcome {
    Match,
    Octave,
    Mismatch,
}

fn classify(candidate: f64, golden: f64, tol: f64) -> Outcome {
    if (candidate - golden).abs() <= tol {
        Outcome::Match
    } else if (candidate - golden * 2.0).abs() <= tol || (candidate - golden / 2.0).abs() <= tol {
        Outcome::Octave
    } else {
        Outcome::Mismatch
    }
}

// All 94 BPM goldens of the Python app, match / octave / mismatch.
#[test]
fn every_golden_file_matches_within_tolerance() {
    let started = Instant::now();
    let mut jobs = Vec::new();
    let with_private = musaic_testdata::private_available(&[GOLDENS[1].0, "real-library/library"]);
    for (golden_rel, kind, private) in GOLDENS {
        if private && !with_private {
            musaic_testdata::skip_private_cases();
            continue;
        }
        let golden = common::read_json(golden_rel, private);
        let policy = &golden["tolerance_policy"]["bpm"];
        assert_eq!(
            policy["compare"], "estimated",
            "{golden_rel}: unexpected bpm policy"
        );
        let tol = policy["tolerance_bpm"].as_f64().expect("tolerance_bpm");
        let lib = common::library(kind);
        let files = golden["files"].as_object().expect("files");
        assert_eq!(
            files.len() as u64,
            golden["file_count"].as_u64().unwrap(),
            "{golden_rel}"
        );
        for (rel, entry) in files {
            let bpm = &entry["bpm"];
            assert_eq!(
                bpm["status"], "ok",
                "{rel}: golden BPM unavailable (unknown, not a pass)"
            );
            jobs.push((
                lib.join(rel),
                rel.clone(),
                bpm["bpm"].as_f64().unwrap(),
                tol,
            ));
        }
    }
    assert_eq!(jobs.len(), if with_private { 94 } else { 42 });

    let results = common::par_map(&jobs, |(path, _, _, _)| {
        musaic_bpm::detect_bpm(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    });

    let mut worst = (0.0f64, String::new());
    let mut tally = [0usize; 3];
    let mut misses = Vec::new();
    let mut exact_tags = 0;
    for ((_, rel, golden, tol), rust) in jobs.iter().zip(&results) {
        let dev = (rust - golden).abs();
        if dev >= worst.0 {
            worst = (dev, rel.clone());
        }
        if musaic_bpm::format_bpm_tag(*rust) == format!("{golden:.2}") {
            exact_tags += 1;
        }
        let outcome = classify(*rust, *golden, *tol);
        tally[match outcome {
            Outcome::Match => 0,
            Outcome::Octave => 1,
            Outcome::Mismatch => 2,
        }] += 1;
        if outcome != Outcome::Match {
            misses.push(format!(
                "{outcome:?}: {rel}: rust {rust:.4} golden {golden:.4} (|d| {dev:.4})"
            ));
        }
    }
    println!(
        "BPM: {} files in {:.1?}: match {} / octave {} / mismatch {}; identical \"%.2f\" tag {}/{}",
        jobs.len(),
        started.elapsed(),
        tally[0],
        tally[1],
        tally[2],
        exact_tags,
        jobs.len()
    );
    println!("worst |rust - golden| = {:e} BPM ({})", worst.0, worst.1);
    for m in &misses {
        println!("  {m}");
    }
    assert!(
        misses.is_empty(),
        "{} of {} files not `match`:\n{}",
        misses.len(),
        jobs.len(),
        misses.join("\n")
    );
}
