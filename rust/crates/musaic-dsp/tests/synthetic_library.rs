//! Golden tests over the synthetic fixture library (13 albums, 42 FLACs)
//! against testdata/captures/dsp-synthetic/dsp/goldens.json.

mod common;

use std::sync::OnceLock;

use common::*;
use serde_json::Value;

fn goldens() -> &'static Value {
    static G: OnceLock<Value> = OnceLock::new();
    G.get_or_init(|| load_json(SYNTHETIC_GOLDENS))
}

fn measured() -> &'static [Measured] {
    static M: OnceLock<Vec<Measured>> = OnceLock::new();
    M.get_or_init(|| {
        let lib = synthetic_library_dir();
        measure_library(&lib, goldens())
    })
}

/// Compressed base64 <-> raw round-trips exactly for every golden.
#[test]
fn compression_round_trips_every_golden() {
    assert_eq!(check_compression_round_trip(goldens()), 42);
}

/// Fingerprint length, similarity >= 0.95, integer duration and
/// round-trip of the Rust fingerprints against fpcalc.
#[test]
fn fingerprints_meet_gate() {
    check_fingerprints("synthetic", goldens(), measured());
}

/// The md5 of the PCM the Rust decoder yields equals
/// the STREAMINFO MD5 the Python app recorded (`stream.md5_signature`) and the
/// md5 of the PCM the Python app's decoder (soundfile) yields, for every file
/// (testdata/golden/dsp/pcm-md5.json, recorded from the Python app); its
/// float64 form equals the array the Python app measures loudness on.
#[test]
fn decoded_pcm_md5_matches_golden_md5_signature() {
    assert_eq!(check_pcm_md5("synthetic", goldens(), measured()), 42);
}

/// Track LUFS / gain within 0.1 dB, peak exact to six decimals.
#[test]
fn track_replaygain_within_tolerance() {
    check_track_replaygain("synthetic", goldens(), measured());
}

/// Album aggregate within tolerance; formula exact on the Python app's inputs.
#[test]
fn album_replaygain_within_tolerance() {
    check_album_replaygain("synthetic", goldens(), measured());
}

/// On the Python app's fpcalc fingerprints the clustering rule yields
/// exactly the clusters the fixture manifest designed (`dup_of`): the Dupes
/// album's 01/03 pair and nothing anywhere else.
#[test]
fn golden_fingerprints_cluster_as_designed() {
    let expected = manifest_clusters(SYNTHETIC_MANIFEST);
    let got = nfc_keys(golden_clusters(goldens()));
    for (album, clusters) in &got {
        let want = expected
            .get(album)
            .unwrap_or_else(|| panic!("album {album} not in the manifest"));
        assert_eq!(clusters, want, "{album}");
    }
    let total: usize = got.values().map(|c| c.len()).sum();
    assert_eq!(
        total, 1,
        "exactly one duplicate cluster in the synthetic library: {got:?}"
    );
    println!("[duplicates synthetic golden] clusters: {got:?}");
}

/// The same rule on Rust-computed fingerprints (compared
/// only with each other) yields the same clusters.
#[test]
fn computed_fingerprints_cluster_as_designed() {
    let expected = manifest_clusters(SYNTHETIC_MANIFEST);
    let got = nfc_keys(computed_clusters(goldens(), measured()));
    for (album, clusters) in &got {
        assert_eq!(clusters, &expected[album], "{album}");
    }
    assert_eq!(got, nfc_keys(golden_clusters(goldens())));
}
