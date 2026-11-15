//! Golden tests over the real-audio fixture library (5 albums, 52 FLACs)
//! against its DSP goldens (captures/dsp-real/dsp/goldens.json). Tier 2: both
//! are private test data; every test skips without them.
//! Decodes ~1 GB of FLAC: run with `cargo test --release -p musaic-dsp`.

mod common;

use std::sync::OnceLock;

use common::*;
use serde_json::Value;

fn goldens() -> &'static Value {
    static G: OnceLock<Value> = OnceLock::new();
    G.get_or_init(|| load_private_json(REAL_GOLDENS))
}

fn measured() -> &'static [Measured] {
    static M: OnceLock<Vec<Measured>> = OnceLock::new();
    M.get_or_init(|| {
        let lib = real_library_dir();
        measure_library(&lib, goldens())
    })
}

/// Compressed base64 <-> raw round-trips exactly for every golden.
#[test]
fn compression_round_trips_every_golden() {
    musaic_testdata::require_private!(REAL_GOLDENS, REAL_LIBRARY);
    assert_eq!(check_compression_round_trip(goldens()), 52);
}

/// Fingerprint length, similarity >= 0.95, integer duration and
/// round-trip of the Rust fingerprints against fpcalc.
#[test]
fn fingerprints_meet_gate() {
    musaic_testdata::require_private!(REAL_GOLDENS, REAL_LIBRARY);
    check_fingerprints("real", goldens(), measured());
}

/// The md5 of the PCM the Rust decoder yields equals
/// the STREAMINFO MD5 the Python app recorded (`stream.md5_signature`) and the
/// md5 of the PCM the Python app's decoder (soundfile) yields, for every file
/// (testdata/golden/dsp/pcm-md5.json, recorded from the Python app); its
/// float64 form equals the array the Python app measures loudness on.
#[test]
fn decoded_pcm_md5_matches_golden_md5_signature() {
    musaic_testdata::require_private!(REAL_GOLDENS, REAL_LIBRARY);
    assert_eq!(check_pcm_md5("real", goldens(), measured()), 52);
}

/// Track LUFS / gain within 0.1 dB, peak exact to six decimals.
#[test]
fn track_replaygain_within_tolerance() {
    musaic_testdata::require_private!(REAL_GOLDENS, REAL_LIBRARY);
    check_track_replaygain("real", goldens(), measured());
}

/// Album aggregate within tolerance; formula exact on the Python app's inputs.
#[test]
fn album_replaygain_within_tolerance() {
    musaic_testdata::require_private!(REAL_GOLDENS, REAL_LIBRARY);
    check_album_replaygain("real", goldens(), measured());
}

/// Clusters from Rust fingerprints (compared only with
/// each other) equal the clusters the rule yields on the Python app's fpcalc
/// fingerprints.
#[test]
fn computed_clusters_match_golden_clusters() {
    musaic_testdata::require_private!(REAL_GOLDENS, REAL_LIBRARY);
    let golden = golden_clusters(goldens());
    let computed = computed_clusters(goldens(), measured());
    println!("[duplicates real] clusters from fpcalc goldens: {golden:?}");
    assert_eq!(computed, golden);
}

/// The path API (decodes only the first 120 s plus STREAMINFO)
/// gives the same result as fingerprinting the fully decoded file, on every
/// track longer than 120 s.
#[test]
fn fingerprint_file_matches_full_decode() {
    musaic_testdata::require_private!(REAL_GOLDENS, REAL_LIBRARY);
    let lib = real_library_dir();
    let mut checked = 0;
    for m in measured() {
        let full = m.fingerprint.as_ref().expect("fingerprint");
        if full.duration_s <= 120 {
            continue;
        }
        let direct = musaic_dsp::fingerprint_file(&lib.join(&m.rel)).expect("fingerprint_file");
        assert_eq!(&direct, full, "{}", m.rel);
        checked += 1;
    }
    assert!(checked > 0, "no track longer than 120 s");
}
