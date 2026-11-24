//! Shared harness for the musaic-ai golden tests: the goldens recorded from
//! the Python app and resolutions built from their specs (as the
//! generator's `build_resolution` does).

use musaic_resolve::{AlbumResolution, FieldFact, TrackResolution};
use serde_json::Value;

/// A golden under golden/ai/, with its private half merged in when available.
pub fn golden(name: &str) -> Value {
    musaic_testdata::golden(&format!("ai/{name}"))
}

fn fact(v: &Value) -> Option<FieldFact> {
    let a = v.as_array()?;
    Some(FieldFact::new(
        a[0].as_str().unwrap(),
        a[1].as_str().unwrap(),
        a[2].as_f64().unwrap(),
    ))
}

/// The generator's `build_resolution(spec)`.
pub fn resolution(spec: &Value, album_id: &str) -> AlbumResolution {
    let mut r = AlbumResolution::new(album_id);
    let f = |k: &str| spec.get(k).and_then(fact);
    r.anchor = f("anchor");
    r.anchor_strategy = spec
        .get("anchor_strategy")
        .and_then(Value::as_str)
        .unwrap_or("none")
        .to_string();
    r.canonical_title = f("canonical_title");
    r.canonical_album_artist = f("canonical_album_artist");
    r.canonical_year = f("canonical_year");
    r.canonical_album_artist_sort = f("canonical_album_artist_sort");
    r.canonical_genre = f("canonical_genre");
    r.cover_url = f("cover_url");
    for t in spec
        .get("tracks")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        let g = |k: &str| t.get(k).and_then(fact);
        r.tracks.push(TrackResolution {
            filename: t["filename"].as_str().unwrap().into(),
            recording_mbid: t
                .get("recording_mbid")
                .and_then(Value::as_str)
                .map(str::to_string),
            fingerprint_score: t.get("fingerprint_score").and_then(Value::as_f64),
            fingerprinted: t
                .get("fingerprinted")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            canonical_title: g("canonical_title"),
            canonical_artist: g("canonical_artist"),
            canonical_tracknumber: g("canonical_tracknumber"),
            canonical_duration_s: t.get("canonical_duration_s").and_then(Value::as_f64),
        });
    }
    r.unresolved_track_count = spec
        .get("unresolved_track_count")
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    r.notes = spec
        .get("notes")
        .and_then(Value::as_array)
        .map(|a| a.iter().map(|n| n.as_str().unwrap().to_string()).collect())
        .unwrap_or_default();
    r
}
