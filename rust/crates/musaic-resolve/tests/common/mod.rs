//! Test-only helpers: load the recorded AcoustID lookups and the fixture albums.

#![allow(dead_code)]

pub mod deltas;
pub mod env;

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::OnceLock;

use base64::Engine as _;
use musaic_resolve::{LookupResponse, LookupResult};
use regex::Regex;
use serde_json::Value;

pub const LOOKUP_URL: &str = "https://api.acoustid.org/v2/lookup";

/// A value recorded from MusicBrainz / AcoustID for the private tier (a real
/// release-group or release id, a real title). Such values live only in the
/// private test data (`expected/musaic-resolve.json`), never in this
/// repository; only tests gated on private data call this.
pub fn rec(key: &str) -> &'static str {
    static V: OnceLock<Value> = OnceLock::new();
    V.get_or_init(|| {
        musaic_testdata::private_json("expected/musaic-resolve.json")
            .expect("private test data: expected/musaic-resolve.json")
    })[key]
        .as_str()
        .unwrap_or_else(|| panic!("expected/musaic-resolve.json: no string {key:?}"))
}

pub fn read_json(p: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(p).unwrap_or_else(|e| panic!("{}: {e}", p.display())))
        .unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

/// `(fingerprint, duration)` → parsed response, for every recorded HTTPS
/// AcoustID lookup that asked for release-groups.
pub fn recorded_lookups() -> HashMap<(String, i64), LookupResponse> {
    let mut out = HashMap::new();
    for entry in std::fs::read_dir(musaic_testdata::private_required("replay")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let rec = read_json(&path);
        if rec["url"].as_str() != Some(LOOKUP_URL) {
            continue;
        }
        let Some(body) = rec["request_body"].as_str() else {
            continue;
        };
        let form: HashMap<String, String> = form_urlencoded::parse(body.as_bytes())
            .into_owned()
            .collect();
        if !form
            .get("meta")
            .is_some_and(|m| m.contains("releasegroups"))
        {
            continue;
        }
        assert_eq!(rec["status"].as_i64(), Some(200), "{}", path.display());
        let fp = form["fingerprint"].clone();
        let dur: i64 = form["duration"].parse().unwrap();
        let content = base64::engine::general_purpose::STANDARD
            .decode(rec["content_b64"].as_str().unwrap())
            .unwrap();
        let resp = LookupResponse::from_json(&content).unwrap();
        assert!(resp.is_ok(), "{}", path.display());
        out.insert((fp, dur), resp);
    }
    out
}

/// A DSP golden: the synthetic library's is public, the real-audio set's
/// private (the calling test is gated on it).
pub fn dsp_golden(rel: &str) -> Value {
    match musaic_testdata::private(rel) {
        Some(p) => read_json(&p),
        None if rel == DSP_GOLDENS[0] => musaic_testdata::public_json(rel),
        None => read_json(&musaic_testdata::private_required(rel)),
    }
}

pub const DSP_GOLDENS: [&str; 2] = [
    "captures/dsp-synthetic/dsp/goldens.json",
    "captures/dsp-real/dsp/goldens.json",
];

/// One fixture album: its directory and the AcoustID results of each file,
/// in sorted file order (as the reference rule iterates them).
pub struct FixtureAlbum {
    pub dir: String,
    pub tracks: Vec<Vec<LookupResult>>,
}

pub fn fixture_albums() -> Vec<FixtureAlbum> {
    let lookups = recorded_lookups();
    // rel path -> (fingerprint, duration); BTreeMap sorts by code point like Python sorted().
    let mut files: BTreeMap<String, (String, i64)> = BTreeMap::new();
    for g in DSP_GOLDENS {
        let v = dsp_golden(g);
        for (rel, f) in v["files"].as_object().unwrap() {
            let fp = &f["fingerprint"];
            if fp["status"].as_str() != Some("ok") {
                continue;
            }
            // Python: int(round(duration)) — round half to even.
            let dur = fp["duration"].as_f64().unwrap().round_ties_even() as i64;
            files.insert(
                rel.clone(),
                (fp["fingerprint"].as_str().unwrap().to_owned(), dur),
            );
        }
    }
    let mut albums: BTreeMap<String, Vec<Vec<LookupResult>>> = BTreeMap::new();
    for (rel, key) in &files {
        let (album, _name) = rel.split_once('/').unwrap();
        let resp = lookups
            .get(key)
            .unwrap_or_else(|| panic!("no recorded lookup for {rel}"));
        albums
            .entry(album.to_owned())
            .or_default()
            .push(resp.results.clone());
    }
    albums
        .into_iter()
        .map(|(dir, tracks)| FixtureAlbum { dir, tracks })
        .collect()
}

/// The reference rule's derivation of the tie-break title and the
/// multi-disc flag from the directory name.
pub fn title_and_multi_disc(dir: &str) -> (String, bool) {
    let title_re =
        Regex::new(r"^(?:.*? - )?(.*?)(?: \(disc \d+ of \d+\))?(?: \[FLAC\])?$").unwrap();
    let disc_re = Regex::new(r"(?i)\(disc \d+ of \d+\)").unwrap();
    let title = title_re
        .captures(dir)
        .map(|c| c.get(1).map_or("", |m| m.as_str()).to_owned())
        .unwrap_or_else(|| dir.to_owned());
    (title.trim().to_owned(), disc_re.is_match(dir))
}
