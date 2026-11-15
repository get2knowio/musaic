//! Deliberate changes from the Python app's record, as explicit deltas.
//!
//! A [`Delta`] names one scenario (and step), one place in the Python app's
//! recorded step (a JSON pointer into `{body, events, store_writes,
//! store_after, same_object_as_previous_resolve}`, or one request of the
//! request set), the Python app's value there, the new value, and the
//! deliberate change behind it. [`apply`] checks that the Python app's record really holds
//! the stated old value before replacing it, so a delta can never silently
//! paper over something else; callers then require exact equality, so any
//! difference not listed fails, and a listed delta the build does not
//! produce fails too.
//!
//! New values come from recorded data: the release listings and searches in
//! the recorded replay set (private) (read here through the same replay transport), the
//! golden's own constructed overlays, or the case's own inputs.

#![allow(dead_code)]

use std::collections::BTreeSet;

use musaic_net::{MusicBrainzClient, ReplayTransport};
use musaic_resolve::source::MusicBrainzSource;
use serde_json::{Value, json};

use super::env::{ReplayMb, cover_head_request, release_group_lookup_request};
use super::rec;

/// One scenario of the Python app's cascade golden.
pub fn cascade_case(name: &str) -> Value {
    musaic_testdata::golden("resolver/cascade.json")["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == name)
        .unwrap_or_else(|| panic!("{name}"))
        .clone()
}

pub type ReqKey = (String, String);

#[derive(Debug, Clone)]
pub enum Change {
    Set {
        path: String,
        old: Value,
        new: Value,
    },
    AddRequest(ReqKey),
    RemoveRequest(ReqKey),
}

#[derive(Debug, Clone)]
pub struct Delta {
    pub scenario: String,
    pub steps: Vec<usize>,
    pub why: &'static str,
    pub change: Change,
}

pub fn set(
    scenario: &str,
    steps: &[usize],
    why: &'static str,
    path: &str,
    old: Value,
    new: Value,
) -> Delta {
    Delta {
        scenario: scenario.to_owned(),
        steps: steps.to_vec(),
        why,
        change: Change::Set {
            path: path.to_owned(),
            old,
            new,
        },
    }
}

pub fn add_req(scenario: &str, steps: &[usize], why: &'static str, key: ReqKey) -> Delta {
    Delta {
        scenario: scenario.to_owned(),
        steps: steps.to_vec(),
        why,
        change: Change::AddRequest(key),
    }
}

pub fn remove_req(scenario: &str, steps: &[usize], why: &'static str, key: ReqKey) -> Delta {
    Delta {
        scenario: scenario.to_owned(),
        steps: steps.to_vec(),
        why,
        change: Change::RemoveRequest(key),
    }
}

/// Apply every delta for `(scenario, step)` to the Python app's step record and
/// request set. Panics when the record does not hold a delta's old value.
/// Returns how many deltas applied.
pub fn apply(
    deltas: &[Delta],
    scenario: &str,
    step: usize,
    record: &mut Value,
    requests: &mut BTreeSet<ReqKey>,
) -> usize {
    let mut n = 0;
    for d in deltas
        .iter()
        .filter(|d| d.scenario == scenario && d.steps.contains(&step))
    {
        let ctx = format!("{scenario} / step {step} ({})", d.why);
        match &d.change {
            Change::Set { path, old, new } => {
                let slot = record
                    .pointer_mut(path)
                    .unwrap_or_else(|| panic!("{ctx}: no {path} in the Python app's record"));
                assert_eq!(
                    slot, old,
                    "{ctx}: the Python app's {path} is not the stated old value"
                );
                *slot = new.clone();
            }
            Change::AddRequest(k) => {
                assert!(
                    requests.insert(k.clone()),
                    "{ctx}: the Python app already asked {k:?}"
                );
            }
            Change::RemoveRequest(k) => {
                assert!(
                    requests.remove(k),
                    "{ctx}: the Python app never asked {k:?}"
                );
            }
        }
        n += 1;
    }
    n
}

pub fn fact(v: &str) -> Value {
    json!({"value": v, "source": "musicbrainz", "confidence": 0.95})
}

pub fn get(req: &musaic_net::Request) -> ReqKey {
    (req.method.as_str().to_owned(), req.replay_key())
}

pub fn browse_key(rg: &str) -> ReqKey {
    get(&MusicBrainzClient::<ReplayTransport>::browse_request(rg))
}

pub fn search_key(artist: &str, title: &str) -> ReqKey {
    get(&MusicBrainzClient::<ReplayTransport>::search_request(
        artist, title,
    ))
}

pub fn lookup_key(rg: &str) -> ReqKey {
    get(&release_group_lookup_request(rg))
}

pub fn cover_key(rg: &str) -> ReqKey {
    get(&cover_head_request(rg))
}

/// Release-groups whose lookup was recorded by live runs (the first three by
/// one run; then the groups the three recorded hints select on the "Night
/// Mix" search, and the un-hinted winner). Every other release-group's lookup
/// misses the replay set.
pub fn recorded_group_lookups() -> [&'static str; 7] {
    [
        disc_album_rg(),
        compilation_rg(),
        conniff_rg(),
        night_mix_rg(),
        night_mix_hint_a_rg(),
        night_mix_hint_b_rg(),
        night_mix_hint_c_rg(),
    ]
}

/// The recorded "Night Mix" search's un-hinted winner (a real release-group
/// the synthetic album's name matches) and the groups the three recorded
/// hints select (the private half of resolver/find_release.json).
pub fn night_mix_rg() -> &'static str {
    rec("night_mix_rg")
}
/// The release Tier 1 aligns against for [`night_mix_rg`].
pub fn night_mix_rel() -> &'static str {
    rec("night_mix_rel")
}
pub fn night_mix_hint_a_rg() -> &'static str {
    rec("night_mix_hint_a_rg")
}
pub fn night_mix_hint_b_rg() -> &'static str {
    rec("night_mix_hint_b_rg")
}
pub fn night_mix_hint_c_rg() -> &'static str {
    rec("night_mix_hint_c_rg")
}
/// The title of the real release-group [`night_mix_rg`] names.
pub fn rec_night_mix_title() -> &'static str {
    rec("night_mix_title")
}
/// The three recorded hints, in the order of the cases that use them.
pub fn night_mix_hints() -> [&'static str; 3] {
    [
        rec("night_mix_hint_a"),
        rec("night_mix_hint_b"),
        rec("night_mix_hint_c"),
    ]
}

/// The recorded answer to the release-group lookup of `rg` (the recorded replay set (private),
/// read through the same replay source the resolver uses).
pub fn recorded_group(rg: &str) -> Value {
    assert!(
        recorded_group_lookups().contains(&rg),
        "{rg}: not a recorded lookup"
    );
    let body = ReplayMb::new(&json!([]))
        .lookup_release_group(rg)
        .unwrap_or_else(|e| panic!("no recorded release-group lookup for {rg}: {e}"));
    serde_json::from_slice(&body).unwrap()
}

/// Album-field values read from the recorded lookup of `rg`, by the rules the
/// Python app applies to the same release-group object in a search hit
/// (mb_lookup.py:221-236): title (stripped), the credit names joined with
/// their joinphrases (`_artist_credit_string`), the first credit's
/// sort-name, the first four digits of `first-release-date`, and the genre by
/// `_pick_primary_genre` (the tag with the highest count, first on ties,
/// only at count >= 2; confidence 0.55 as services/resolver.py:358). The
/// genre is listed only when one qualifies (otherwise it stays null, as in
/// the Python app's record).
pub fn group_fields(rg: &str) -> Vec<(&'static str, Value)> {
    let g = recorded_group(rg);
    assert_eq!(g["id"], rg);
    let credit = g["artist-credit"].as_array().unwrap();
    let artist: String = credit
        .iter()
        .map(|c| {
            format!(
                "{}{}",
                c["name"].as_str().unwrap(),
                c["joinphrase"].as_str().unwrap_or("")
            )
        })
        .collect();
    let date = g["first-release-date"].as_str().unwrap();
    assert!(date.len() >= 4 && date[..4].bytes().all(|b| b.is_ascii_digit()));
    let mut out = vec![
        ("canonical_title", fact(g["title"].as_str().unwrap().trim())),
        ("canonical_album_artist", fact(artist.trim())),
        ("canonical_year", fact(&date[..4])),
        (
            "canonical_album_artist_sort",
            fact(credit[0]["artist"]["sort-name"].as_str().unwrap()),
        ),
    ];
    let mut best: Option<(&str, i64)> = None;
    for t in g["tags"].as_array().unwrap() {
        let n = t["count"].as_i64().unwrap_or(0);
        if best.is_none_or(|(_, b)| n > b) {
            best = Some((t["name"].as_str().unwrap(), n));
        }
    }
    if let Some((name, n)) = best
        && n >= 2
        && !name.trim().is_empty()
    {
        out.push((
            "canonical_genre",
            json!({"value": name.trim(), "source": "musicbrainz", "confidence": 0.55}),
        ));
    }
    out
}

/// Album fields for an anchor that is not a Tier 0b hit, when the lookup is
/// recorded: the album fields (null in the Python app) come from the lookup, no
/// trace line is added, and the lookup is one request more than the Python app
/// makes.
pub fn group_field_deltas(
    scenario: &str,
    steps: &[usize],
    request_steps: &[usize],
    rg: &str,
) -> Vec<Delta> {
    let mut out: Vec<Delta> = group_fields(rg)
        .into_iter()
        .map(|(k, v)| {
            set(
                scenario,
                steps,
                "album fields",
                &format!("/body/{k}"),
                Value::Null,
                v,
            )
        })
        .collect();
    out.push(add_req(
        scenario,
        request_steps,
        "album fields",
        lookup_key(rg),
    ));
    out
}

/// The trace line for a release-group lookup that is not in the replay set
/// (album fields): the error is the replay transport's own.
pub fn lookup_failed_note(rg: &str, from_listing: bool) -> String {
    assert!(
        !recorded_group_lookups().contains(&rg),
        "{rg}: the lookup is recorded"
    );
    let e = ReplayMb::new(&json!([]))
        .lookup_release_group(rg)
        .expect_err("the release-group lookup was never recorded");
    format!(
        "Tier 1: release-group lookup failed ({e}); {}",
        if from_listing {
            "album fields taken from the release listing"
        } else {
            "no album fields"
        }
    )
}

/// The recorded release listing of `rg` (the recorded replay set (private)).
pub fn recorded_listing(rg: &str) -> Value {
    let body = ReplayMb::new(&json!([]))
        .browse_releases(rg)
        .unwrap_or_else(|e| panic!("no recorded listing for {rg}: {e}"));
    serde_json::from_slice(&body).unwrap()
}

pub fn recorded_release(rg: &str, release: &str) -> Value {
    recorded_listing(rg)["releases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == release)
        .unwrap_or_else(|| panic!("{release} not in the listing of {rg}"))
        .clone()
}

/// Album-field fallback values, read from the recorded listing: title, single
/// artist credit and its sort-name of the release Tier 1 aligns against, and
/// the earliest four-digit year of any listed release.
pub fn listing_fields(rg: &str, release: &str) -> [(&'static str, Value); 4] {
    let r = recorded_release(rg, release);
    let credit = r["artist-credit"].as_array().unwrap();
    assert_eq!(credit.len(), 1, "single credit expected in {release}");
    assert_eq!(credit[0]["joinphrase"], "");
    let year = recorded_listing(rg)["releases"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["date"].as_str())
        .filter(|d| d.len() >= 4 && d[..4].bytes().all(|b| b.is_ascii_digit()))
        .map(|d| d[..4].to_owned())
        .min()
        .unwrap();
    [
        ("canonical_title", fact(r["title"].as_str().unwrap())),
        (
            "canonical_album_artist",
            fact(credit[0]["name"].as_str().unwrap()),
        ),
        ("canonical_year", fact(&year)),
        (
            "canonical_album_artist_sort",
            fact(credit[0]["artist"]["sort-name"].as_str().unwrap()),
        ),
    ]
}

/// Album fields for an anchor that is not a Tier 0b hit, when the lookup misses
/// the replay set: the four fallback fields (null in the Python app), the trace
/// line, and the lookup request.
pub fn listing_field_deltas(
    scenario: &str,
    steps: &[usize],
    request_steps: &[usize],
    rg: &str,
    release: &str,
    old_notes: &[&str],
) -> Vec<Delta> {
    let mut out: Vec<Delta> = listing_fields(rg, release)
        .into_iter()
        .map(|(k, v)| {
            set(
                scenario,
                steps,
                "album fields",
                &format!("/body/{k}"),
                Value::Null,
                v,
            )
        })
        .collect();
    let mut notes: Vec<Value> = old_notes.iter().map(|n| json!(n)).collect();
    let old = Value::from(notes.clone());
    notes.push(json!(lookup_failed_note(rg, true)));
    out.push(set(
        scenario,
        steps,
        "album fields",
        "/body/notes",
        old,
        Value::from(notes),
    ));
    out.push(add_req(
        scenario,
        request_steps,
        "album fields",
        lookup_key(rg),
    ));
    out
}

/// The real albums of the private tier, by role. rip-0001 is a Ray Conniff
/// album; real-album-4 is disc 1 of a two-disc set; real-album-5 is a
/// soundtrack compilation. Ids and titles are in the private data.
pub fn conniff_rg() -> &'static str {
    rec("conniff_rg")
}
pub fn conniff_rel() -> &'static str {
    rec("conniff_rel")
}
/// The title of the Ray Conniff album (rip-0001) as MusicBrainz spells it.
pub fn conniff_mb_title() -> &'static str {
    rec("conniff_mb_title")
}
/// The title of the Ray Conniff album (rip-0001), as searched.
pub fn conniff_album_title() -> &'static str {
    rec("conniff_album_title")
}
pub fn disc_album_rg() -> &'static str {
    rec("disc_album_rg")
}
pub fn disc_album_rel() -> &'static str {
    rec("disc_album_rel")
}
pub fn compilation_rg() -> &'static str {
    rec("compilation_rg")
}
/// The Python app's pick on the compilation: the first release of its year in
/// listing order.
pub fn compilation_python_rel() -> &'static str {
    rec("compilation_python_rel")
}
/// The best-fit pick: same track count, smallest total duration difference
/// (track 10 is 174000 ms here against 175400 ms on the Python app's pick; the
/// FLAC is 174.4 s).
pub fn compilation_fit_rel() -> &'static str {
    rec("compilation_fit_rel")
}

/// The real folder names of real-album-4 and real-album-5 (the private DSP
/// golden and expected anchors are keyed by them).
pub fn disc_album_dir() -> &'static str {
    rec("disc_album_dir")
}
pub fn compilation_dir() -> &'static str {
    rec("compilation_dir")
}

/// The scenario names of the two real albums named after their folders.
pub const DISC_ALBUM_CASE: &str = "real 0a real-album-4";
pub const COMPILATION_CASE: &str = "real 0a real-album-5";

/// The five real albums' "real 0a" scenarios: (scenario, release-group,
/// release Tier 1 aligns against). Release ids from the recorded listings.
pub fn real_albums() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        ("real 0a rip-0001", conniff_rg(), conniff_rel()),
        ("real 0a rip-0002", rec("rip_0002_rg"), rec("rip_0002_rel")),
        ("real 0a rip-0003", rec("rip_0003_rg"), rec("rip_0003_rel")),
        (DISC_ALBUM_CASE, disc_album_rg(), disc_album_rel()),
        (COMPILATION_CASE, compilation_rg(), compilation_fit_rel()),
    ]
}

/// The best-fit release on the compilation: the release written back and the
/// one track whose length differs between the two pressings.
pub fn compilation_deltas(scenario: &str) -> Vec<Delta> {
    let track10 = |rel: &str| {
        let r = recorded_release(compilation_rg(), rel);
        let t = &r["media"][0]["tracks"][9];
        assert_eq!(t["position"], 10);
        json!(t["length"].as_f64().unwrap() / 1000.0)
    };
    vec![
        set(
            scenario,
            &[0],
            "best-fit release",
            "/store_writes/0/release_mbid",
            json!(compilation_python_rel()),
            json!(compilation_fit_rel()),
        ),
        set(
            scenario,
            &[0],
            "best-fit release",
            "/store_after/release_mbid",
            json!(compilation_python_rel()),
            json!(compilation_fit_rel()),
        ),
        set(
            scenario,
            &[0],
            "best-fit release",
            "/body/tracks/9/canonical_duration_s",
            track10(compilation_python_rel()),
            track10(compilation_fit_rel()),
        ),
    ]
}

/// Deltas of the five "real 0a" scenarios (fresh cached anchor, Tier 1 only).
/// Album fields: rip-0001, real-album-4 and real-album-5 take theirs from
/// the recorded release-group lookup; rip-0002 and rip-0003, whose lookup
/// was never recorded, from the release listing.
pub fn real_album_deltas() -> Vec<Delta> {
    let mut out = Vec::new();
    for (scenario, rg, rel) in real_albums() {
        if recorded_group_lookups().contains(&rg) {
            out.extend(group_field_deltas(scenario, &[0], &[0], rg));
        } else {
            out.extend(listing_field_deltas(
                scenario,
                &[0],
                &[0],
                rg,
                rel,
                &["Tier 0a: anchored from cache (score 100)"],
            ));
        }
    }
    out.extend(compilation_deltas(COMPILATION_CASE));
    // Positional alignment: rip-0001 aligns 6 of 6 by position (the Python app: 0 of 6). The
    // other four are unchanged: rip-0002 / rip-0003 have no release with
    // their track count; real-album-4 and real-album-5 already align fully.
    let s = "real 0a rip-0001";
    out.extend(conniff_positional_deltas(
        s,
        &[0],
        &cascade_case(s),
        &["Tier 0a: anchored from cache (score 100)".to_owned()],
        1,
    ));
    out
}

/// Album fields for a Tier 0b anchor: the fields come from the recorded search
/// hit (title, credit names joined by ", ", first-release-date year, and the
/// first credit's sort-name); the genre stays null (the hit's only tag has
/// count 1, below the Python app's threshold of 2).
pub fn conniff_search_hit_deltas(scenario: &str) -> Vec<Delta> {
    search_hit_deltas(
        scenario,
        &[0],
        "Ray Conniff",
        conniff_album_title(),
        conniff_rg(),
    )
}

/// Album fields for a Tier 0b anchor on release-group `rg`, picked from the
/// recorded search `(artist, title)` (by the hint, or as the top hit): title,
/// the single credit's name, the first-release-date year and the credit's
/// sort-name, read from that group's entry in the recorded search. The genre
/// stays null: asserted here that no tag of the hit reaches the Python app's
/// threshold of 2 votes (`_pick_primary_genre`).
pub fn search_hit_deltas(
    scenario: &str,
    steps: &[usize],
    artist: &str,
    title: &str,
    rg: &str,
) -> Vec<Delta> {
    let body = ReplayMb::new(&json!([]))
        .search_release_groups(artist, title)
        .unwrap();
    let doc: Value = serde_json::from_slice(&body).unwrap();
    let top = doc["release-groups"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["id"] == rg)
        .unwrap_or_else(|| panic!("{rg} not in the recorded search"));
    let credit = top["artist-credit"].as_array().unwrap();
    assert_eq!(credit.len(), 1);
    assert!(
        top["tags"]
            .as_array()
            .is_none_or(|t| t.iter().all(|t| t["count"].as_i64().unwrap_or(0) < 2)),
        "{rg}: a tag of the hit qualifies as genre"
    );
    let year = &top["first-release-date"].as_str().unwrap()[..4];
    vec![
        set(
            scenario,
            steps,
            "album fields",
            "/body/canonical_title",
            Value::Null,
            fact(top["title"].as_str().unwrap()),
        ),
        set(
            scenario,
            steps,
            "album fields",
            "/body/canonical_album_artist",
            Value::Null,
            fact(credit[0]["name"].as_str().unwrap()),
        ),
        set(
            scenario,
            steps,
            "album fields",
            "/body/canonical_year",
            Value::Null,
            fact(year),
        ),
        set(
            scenario,
            steps,
            "album fields",
            "/body/canonical_album_artist_sort",
            Value::Null,
            fact(credit[0]["artist"]["sort-name"].as_str().unwrap()),
        ),
    ]
}

/// Without a tracklist: the case's own FLACs, listed with their stored fingerprint
/// fields and nothing canonical.
pub fn bare_tracks(case: &Value) -> Value {
    let fps = case["fingerprints"].as_array().unwrap();
    Value::from(
        case["tracks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| {
                let name = t["filename"].as_str().unwrap();
                let fp = fps.iter().find(|f| f["filename"] == name);
                json!({
                    "filename": name,
                    "recording_mbid": fp.map_or(Value::Null, |f| f["acoustid_recording_mbid"].clone()),
                    "fingerprint_score": fp.map_or(Value::Null, |f| f["acoustid_score"].clone()),
                    "fingerprinted": fp.is_some(),
                    "canonical_title": null,
                    "canonical_artist": null,
                    "canonical_tracknumber": null,
                    "canonical_duration_s": null,
                })
            })
            .collect::<Vec<_>>(),
    )
}

/// Positional alignment on the Conniff FLACs (rip-0001; the same six durations in every
/// "conniff" scenario): the trace line of the positional fallback. The
/// figures are computed from the case's own durations and the recorded
/// lengths of the release Tier 1 aligns against (conniff_rel(); its sister
/// release lists the same lengths).
pub fn conniff_positional_note(case: &Value) -> String {
    let rel = recorded_release(conniff_rg(), conniff_rel());
    let lengths: Vec<f64> = rel["media"][0]["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["length"].as_f64().unwrap() / 1000.0)
        .collect();
    let diffs: Vec<f64> = case["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .zip(&lengths)
        .map(|(t, l)| t["duration"].as_f64().unwrap() - l)
        .collect();
    assert_eq!(diffs.len(), 6);
    let lo = diffs.iter().copied().fold(f64::INFINITY, f64::min);
    let hi = diffs.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    format!(
        "Tier 1: 6 of 6 tracks aligned by position: track counts match and every length is within 10 s ({lo:+.1} to {hi:+.1} s); 0 agreed with the duration pass, 0 replaced"
    )
}

/// The Conniff tracks after the positional fallback, from the
/// recorded release: title, track artist credit, linear position and length,
/// each fact at the positional confidence 0.8.
pub fn conniff_positional_tracks() -> Vec<[(&'static str, Value); 4]> {
    let rel = recorded_release(conniff_rg(), conniff_rel());
    let f = |v: &str| json!({"value": v, "source": "musicbrainz", "confidence": 0.8});
    rel["media"][0]["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| {
            let credit = t["recording"]["artist-credit"].as_array().unwrap();
            let artist: String = credit
                .iter()
                .map(|c| {
                    format!(
                        "{}{}",
                        c["name"].as_str().unwrap(),
                        c["joinphrase"].as_str().unwrap_or("")
                    )
                })
                .collect();
            [
                ("canonical_title", f(t["title"].as_str().unwrap().trim())),
                ("canonical_artist", f(&artist)),
                (
                    "canonical_tracknumber",
                    f(&t["position"].as_i64().unwrap().to_string()),
                ),
                (
                    "canonical_duration_s",
                    json!(t["length"].as_f64().unwrap() / 1000.0),
                ),
            ]
        })
        .collect()
}

/// Positional-alignment deltas for one Conniff scenario: every track gains its canonical
/// facts (null in the Python app, which aligned 0 of 6 at 2 s), the unresolved
/// count drops 6 -> 0, and the positional trace line is inserted into the
/// notes at `at` (`old_notes` is the record after the scenario's other
/// deltas, which apply first).
pub fn conniff_positional_deltas(
    scenario: &str,
    steps: &[usize],
    case: &Value,
    old_notes: &[String],
    at: usize,
) -> Vec<Delta> {
    let mut out = Vec::new();
    for (i, facts) in conniff_positional_tracks().into_iter().enumerate() {
        for (k, v) in facts {
            out.push(set(
                scenario,
                steps,
                "positional alignment",
                &format!("/body/tracks/{i}/{k}"),
                Value::Null,
                v,
            ));
        }
    }
    out.push(set(
        scenario,
        steps,
        "positional alignment",
        "/body/unresolved_track_count",
        json!(6),
        json!(0),
    ));
    let mut new_notes = old_notes.to_vec();
    new_notes.insert(at, conniff_positional_note(case));
    out.push(set(
        scenario,
        steps,
        "positional alignment",
        "/body/notes",
        json!(old_notes),
        json!(new_notes),
    ));
    out
}
