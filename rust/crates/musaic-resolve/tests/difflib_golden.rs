//! The title-similarity tie-break must equal Python's
//! `round(difflib.SequenceMatcher(None, _norm_title(a), _norm_title(b)).ratio(), 2)`.
//!
//! Golden: crates/musaic-resolve/difflib_golden.json in the private test
//! data (Tier 2: its titles come from recorded MusicBrainz answers and a real
//! library), produced by CPython's own difflib (titles taken from the recorded
//! AcoustID fixtures and the DSP goldens, plus unicode, empty and >= 200-char
//! pairs that exercise `autojunk`).

use musaic_resolve::similarity::{
    SequenceMatcher, norm_title, ratio, round2_hundredths, title_similarity_hundredths,
};
use serde_json::Value;

const GOLDEN: &str = "crates/musaic-resolve/difflib_golden.json";

fn golden() -> Vec<Value> {
    let v = musaic_testdata::read_json(musaic_testdata::private_required(GOLDEN));
    v["pairs"].as_array().unwrap().clone()
}

fn s(v: &Value) -> &str {
    v.as_str().unwrap()
}

// SequenceMatcher.ratio() and get_matching_blocks() on raw strings.
#[test]
fn ratio_and_blocks_match_cpython() {
    musaic_testdata::require_private!(GOLDEN);
    let pairs = golden();
    assert!(pairs.len() > 2000, "golden too small: {}", pairs.len());
    let mut autojunk_cases = 0;
    let mut long_cases = 0;
    for p in &pairs {
        let (a, b) = (s(&p["a"]), s(&p["b"]));
        let sm = SequenceMatcher::new(a.chars().collect(), b.chars().collect());
        let blocks: Vec<Vec<u64>> = sm
            .matching_blocks()
            .iter()
            .map(|&(i, j, k)| vec![i as u64, j as u64, k as u64])
            .collect();
        let want_blocks: Vec<Vec<u64>> = serde_json::from_value(p["blocks"].clone()).unwrap();
        assert_eq!(blocks, want_blocks, "blocks for {a:?} / {b:?}");
        assert_eq!(sm.matches() as u64, p["matches"].as_u64().unwrap());
        assert_eq!(
            ratio(a, b),
            p["ratio"].as_f64().unwrap(),
            "ratio for {a:?} / {b:?}"
        );
        if p["autojunk_changes_ratio"].as_bool().unwrap() {
            autojunk_cases += 1;
        }
        if b.chars().count() >= 200 {
            long_cases += 1;
        }
    }
    assert!(
        autojunk_cases > 0,
        "golden has no case where autojunk matters"
    );
    assert!(long_cases > 0, "golden has no b with >= 200 elements");
}

// _norm_title and the rounded, normalised similarity.
#[test]
fn normalised_similarity_matches_python() {
    musaic_testdata::require_private!(GOLDEN);
    for p in &golden() {
        let (a, b) = (s(&p["a"]), s(&p["b"]));
        assert_eq!(norm_title(a), s(&p["norm_a"]), "norm of {a:?}");
        assert_eq!(norm_title(b), s(&p["norm_b"]), "norm of {b:?}");
        let r = ratio(&norm_title(a), &norm_title(b));
        assert_eq!(r, p["norm_ratio"].as_f64().unwrap(), "{a:?} / {b:?}");
        let want = p["norm_ratio_hundredths"].as_i64().unwrap();
        assert_eq!(round2_hundredths(r), want);
        assert_eq!(title_similarity_hundredths(a, b), want);
    }
}
