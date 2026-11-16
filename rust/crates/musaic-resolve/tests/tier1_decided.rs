//! Deliberate changes from the Python app that its golden cannot show by
//! itself: a stale anchor is refreshed by id (never replaced by a name
//! search); album fields are filled for every anchor (the release-group
//! lookup path, tested on a CONSTRUCTED response); a hint skips the memo; and
//! Tier 1 aligns against the best-fit release (alignment figures on the
//! recorded listings).
//! Scenario-level deltas against the Python app live in cascade_python_parity.rs.

mod common;

use std::collections::BTreeSet;

use base64::Engine as _;
use common::deltas::{self as d};
use common::env::{FRESH, NOW_EPOCH_S, NoFingerprints, ReplayMb, World, flac_track};
use musaic_resolve::db_store::DbStore;
use musaic_resolve::find_release::{best_fit_release, parse_listing, parse_tracklist};
use musaic_resolve::resolution::align;
use musaic_resolve::source::{AlbumRecord, AlbumSource, FlacTrack, MusicBrainzSource, NetError};
use musaic_resolve::{Env, Resolver};
use serde_json::{Value, json};

const STALE: &str = "2000-01-01T00:00:00Z";

fn golden_case(name: &str) -> Value {
    musaic_testdata::golden("resolver/cascade.json")["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == name)
        .unwrap_or_else(|| panic!("{name}"))
        .clone()
}

fn notes(v: &Value) -> Vec<String> {
    v["notes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n.as_str().unwrap().to_owned())
        .collect()
}

fn env<'a>(world: &'a World, mb: &'a dyn MusicBrainzSource) -> Env<'a> {
    Env {
        albums: world,
        store: world,
        mb,
        fingerprints: &NoFingerprints,
        now_epoch_s: NOW_EPOCH_S,
    }
}

/// A Conniff case ("real 0a rip-0001": the real album's tracks) with the
/// stored anchor replaced.
fn conniff_with_anchor(anchor: Value, blocked: &[&str]) -> Value {
    let mut case = golden_case("real 0a rip-0001");
    case["stored_anchor"] = anchor;
    case["blocked"] = json!(blocked);
    case
}

/// Fails the release browse; everything else goes to the replay set.
struct NoBrowse<'a>(&'a ReplayMb);

impl MusicBrainzSource for NoBrowse<'_> {
    fn search_release_groups(&self, a: &str, t: &str) -> Result<Vec<u8>, NetError> {
        self.0.search_release_groups(a, t)
    }
    fn browse_releases(&self, _: &str) -> Result<Vec<u8>, NetError> {
        Err(NetError::Http(503))
    }
    fn browse_release_dates(&self, rg: &str) -> Result<Vec<u8>, NetError> {
        self.0.browse_release_dates(rg)
    }
    fn cover_front_status(&self, rg: &str) -> Result<u16, NetError> {
        self.0.cover_front_status(rg)
    }
    fn lookup_release_group(&self, rg: &str) -> Result<Vec<u8>, NetError> {
        self.0.lookup_release_group(rg)
    }
}

// A stale stored anchor whose release listing is recorded is
// refreshed by id: no search, same group, its own score kept, the best-fit
// release written, `last_checked_at` renewed (the store stamps it), one
// AlbumMbAnchorSet for the write, and the refresh in the trace.
#[test]
fn stale_anchor_is_refreshed_by_id_and_keeps_its_score() {
    musaic_testdata::require_private!("replay");
    let case = conniff_with_anchor(
        json!({"release_group_mbid": d::conniff_rg(), "release_mbid": null, "score": 92, "last_checked_at": STALE}),
        &[],
    );
    let world = World::from_case(&case);
    let mb = ReplayMb::new(&json!([]));
    let r = Resolver::new()
        .resolve(
            &env(&world, &mb),
            case["album"]["id"].as_str().unwrap(),
            "",
            false,
        )
        .unwrap();
    let body = r.resolution.to_dict();
    assert_eq!(body["anchor"]["value"], d::conniff_rg());
    assert_eq!(body["anchor_strategy"], "cache");
    let id = case["album"]["id"].as_str().unwrap();
    assert_eq!(
        Value::from(world.take_writes()),
        json!([{"album_id": id, "release_group_mbid": d::conniff_rg(), "release_mbid": d::conniff_rel(), "score": 92}])
    );
    assert_eq!(world.store_json()["last_checked_at"], FRESH);
    assert_eq!(r.events.len(), 1);
    assert!(
        notes(&body).contains(&format!(
            "Tier 1: stale anchor refreshed by release-group id ({})",
            d::conniff_rg()
        )),
        "{body}"
    );
    let keys: BTreeSet<_> = mb
        .take_log()
        .into_iter()
        .map(|l| (l.method, l.key))
        .collect();
    assert!(!keys.contains(&d::search_key("Ray Conniff", d::conniff_album_title())));
    assert!(keys.contains(&d::browse_key(d::conniff_rg())));
}

// When the refetch fails the stale anchor is kept as it is — no
// write (so `last_checked_at` stays old and the next resolve tries again), no
// event, the failure in the trace — and, with no tracklist, every track is
// unresolved.
#[test]
fn failed_refresh_keeps_the_stale_anchor_untouched() {
    musaic_testdata::require_private!("replay");
    let anchor = json!({"release_group_mbid": d::conniff_rg(), "release_mbid": null, "score": 92, "last_checked_at": STALE});
    let case = conniff_with_anchor(anchor.clone(), &[]);
    let world = World::from_case(&case);
    let replay = ReplayMb::new(&json!([]));
    let mb = NoBrowse(&replay);
    let r = Resolver::new()
        .resolve(
            &env(&world, &mb),
            case["album"]["id"].as_str().unwrap(),
            "",
            false,
        )
        .unwrap();
    let body = r.resolution.to_dict();
    assert_eq!(body["anchor"]["value"], d::conniff_rg());
    assert!(world.take_writes().is_empty());
    assert_eq!(world.store_json(), anchor);
    assert!(r.events.is_empty());
    let n = notes(&body);
    assert!(
        n.contains(
            &"Tier 1: stale anchor kept, not refreshed: release listing unavailable (HTTP 503)"
                .to_owned()
        ),
        "{n:?}"
    );
    assert_eq!(body["unresolved_track_count"], 6);
    assert_eq!(body["tracks"].as_array().unwrap().len(), 6);
}

// A listing that names no release does not renew the anchor either
// (the group may have been merged away); the golden's CONSTRUCTED empty
// listing for 5555 ("cached anchor, empty release listing") made stale.
#[test]
fn empty_listing_does_not_renew_a_stale_anchor() {
    musaic_testdata::require_private!("replay");
    let mut case = golden_case("cached anchor, empty release listing");
    case["stored_anchor"]["last_checked_at"] = json!(STALE);
    let before = case["stored_anchor"].clone();
    let world = World::from_case(&case);
    let mb = ReplayMb::new(&case["overlay"]);
    let r = Resolver::new()
        .resolve(&env(&world, &mb), "medium-1", "", false)
        .unwrap();
    assert!(world.take_writes().is_empty());
    assert_eq!(world.store_json(), before);
    assert!(notes(&r.resolution.to_dict()).contains(
        &"Tier 1: stale anchor kept, not refreshed: MusicBrainz lists no release for release-group 55555555-5555-5555-5555-555555555555".to_owned()
    ));
    assert_eq!(r.resolution.unresolved_track_count, 3);
}

// The stored anchor is never replaced by a name search. The Python app's
// scenario has a search answer (5555, score 95) waiting in the overlay; this
// build never asks it and keeps 6666.
#[test]
fn stale_anchor_is_never_replaced_by_a_name_search() {
    musaic_testdata::require_private!("replay");
    let case = golden_case("stale anchor, fresh search finds another group");
    let world = World::from_case(&case);
    let mb = ReplayMb::new(&case["overlay"]);
    let r = Resolver::new()
        .resolve(&env(&world, &mb), "medium-1", "", false)
        .unwrap();
    assert_eq!(
        r.resolution.anchor.as_ref().unwrap().value,
        case["stored_anchor"]["release_group_mbid"]
            .as_str()
            .unwrap()
    );
    assert_eq!(world.store_json(), case["stored_anchor"]);
    let keys: BTreeSet<_> = mb
        .take_log()
        .into_iter()
        .map(|l| (l.method, l.key))
        .collect();
    assert!(
        !keys.contains(&d::search_key("Ana Artist", "Album One")),
        "{keys:?}"
    );
}

// Stale anchor and the Python app's blocklisted-cache rule: a stale stored anchor that is
// blocklisted is skipped at Tier 0a (as in the Python app), the search runs with
// the blocklist (which removes the only hit), and Tier 1 never refetches or
// renews the blocked group.
#[test]
fn blocklisted_stale_anchor_is_neither_used_nor_refreshed() {
    musaic_testdata::require_private!("replay");
    let anchor = json!({"release_group_mbid": d::conniff_rg(), "release_mbid": null, "score": 92, "last_checked_at": STALE});
    let case = conniff_with_anchor(anchor.clone(), &[d::conniff_rg()]);
    let world = World::from_case(&case);
    let mb = ReplayMb::new(&json!([]));
    let r = Resolver::new()
        .resolve(
            &env(&world, &mb),
            case["album"]["id"].as_str().unwrap(),
            "",
            false,
        )
        .unwrap();
    assert!(r.resolution.anchor.is_none());
    assert!(world.take_writes().is_empty());
    assert_eq!(world.store_json(), anchor);
    let keys: BTreeSet<_> = mb
        .take_log()
        .into_iter()
        .map(|l| (l.method, l.key))
        .collect();
    assert!(!keys.contains(&d::browse_key(d::conniff_rg())), "{keys:?}");
    assert!(!keys.contains(&d::lookup_key(d::conniff_rg())), "{keys:?}");
}

/// Album row from the golden case with id and anchor column from the database.
struct DbAlbums<'a> {
    world: &'a World,
    conn: &'a rusqlite::Connection,
    id: String,
}

impl AlbumSource for DbAlbums<'_> {
    fn album(&self, _: &str) -> Option<AlbumRecord> {
        let mut a = self.world.album.lock().unwrap().clone()?;
        a["id"] = json!(self.id);
        let rg = musaic_db::mb_anchor::get_album_mb_anchor(self.conn, &self.id)
            .unwrap()
            .map(|s| s.release_group_mbid);
        let mut rec = common::env::album_record(&a, None);
        rec.mb_release_group_mbid = rg;
        Some(rec)
    }
    fn filenames(&self, a: &AlbumRecord) -> Vec<String> {
        self.world.filenames(a)
    }
    fn tracks(&self, a: &AlbumRecord) -> Vec<FlacTrack> {
        self.world.tracks(a)
    }
}

// Stale anchor refreshed by id through the library database (fixture copy): the refresh renews
// mb_last_checked_at and keeps group and score; a second resolve right after
// (memo dropped, as the bus does on the event) is fresh and writes nothing.
#[test]
fn refresh_renews_last_checked_at_in_the_database() {
    musaic_testdata::require_private!("replay");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.db");
    std::fs::copy(
        musaic_testdata::public("db/v16-synthetic-offline.db"),
        &path,
    )
    .unwrap();
    let conn = rusqlite::Connection::open(&path).unwrap();
    let id = "0c97baca60f74928".to_owned(); // "Mono Harbor - Gaps": a real row
    musaic_db::mb_anchor::set_album_mb_anchor(&conn, &id, d::conniff_rg(), None, 92, STALE)
        .unwrap();

    let case = golden_case("real 0a rip-0001");
    let world = World::from_case(&case);
    let albums = DbAlbums {
        world: &world,
        conn: &conn,
        id: id.clone(),
    };
    let store = DbStore {
        conn: &conn,
        now_iso: "2026-11-10T00:00:00Z".into(),
        read: Default::default(),
    };
    let mb = ReplayMb::new(&json!([]));
    let e = Env {
        albums: &albums,
        store: &store,
        mb: &mb,
        fingerprints: &NoFingerprints,
        now_epoch_s: NOW_EPOCH_S,
    };
    let resolver = Resolver::new();
    let first = resolver.resolve(&e, &id, "", false).unwrap();
    let row = musaic_db::mb_anchor::get_album_mb_anchor(&conn, &id)
        .unwrap()
        .unwrap();
    assert_eq!(row.release_group_mbid, d::conniff_rg());
    assert_eq!(row.score, Some(92));
    assert_eq!(row.release_mbid.as_deref(), Some(d::conniff_rel()));
    assert_eq!(row.last_checked_at.as_deref(), Some("2026-11-10T00:00:00Z"));
    assert_eq!(first.events.len(), 1);

    resolver.invalidate(Some(&id));
    let second = resolver.resolve(&e, &id, "", false).unwrap();
    assert!(second.events.is_empty());
    assert!(
        !notes(&second.resolution.to_dict())
            .iter()
            .any(|n| n.contains("stale"))
    );
}

/// A CONSTRUCTED release-group lookup body for the Conniff group. Not a
/// recording: title, first-release-date and artist credit are copied from the
/// RECORDED search hit for that group; the tags are invented (the recorded
/// lookup of this group carries one tag at count 1, so no genre;
/// this overlay replaces it under the same key to put a genre at count >= 2
/// on a cached anchor). The request is
/// `GET https://musicbrainz.org/ws/2/release-group/<conniff_rg>?fmt=json&inc=artist-credits+tags`.
fn constructed_lookup_overlay() -> (Value, Value) {
    let body = ReplayMb::new(&json!([]))
        .search_release_groups("Ray Conniff", d::conniff_album_title())
        .unwrap();
    let hit = serde_json::from_slice::<Value>(&body).unwrap()["release-groups"][0].clone();
    assert_eq!(hit["id"], d::conniff_rg());
    let lookup = json!({
        "id": d::conniff_rg(),
        "title": hit["title"],
        "first-release-date": hit["first-release-date"],
        "artist-credit": hit["artist-credit"],
        "tags": [{"count": 1, "name": "pop"}, {"count": 3, "name": "constructed-genre"}],
    });
    let overlay = json!([{
        "key": d::lookup_key(d::conniff_rg()).1,
        "status": 200,
        "headers": {"Content-Type": "application/json"},
        "content_b64": base64::engine::general_purpose::STANDARD
            .encode(serde_json::to_vec(&lookup).unwrap()),
        "note": "CONSTRUCTED (album fields): overrides the recording (tags changed)",
    }]);
    (overlay, hit)
}

// A fresh cached anchor (Tier 0a) gets all five album fields from
// the release-group lookup when it answers — including the genre, which no
// release listing carries — and no fallback note. CONSTRUCTED response.
#[test]
fn release_group_lookup_fills_every_album_field() {
    musaic_testdata::require_private!("replay");
    let (overlay, hit) = constructed_lookup_overlay();
    let case = golden_case("real 0a rip-0001");
    let world = World::from_case(&case);
    let mb = ReplayMb::new(&overlay);
    let r = Resolver::new()
        .resolve(
            &env(&world, &mb),
            case["album"]["id"].as_str().unwrap(),
            "",
            false,
        )
        .unwrap();
    let body = r.resolution.to_dict();
    let credit = &hit["artist-credit"][0];
    assert_eq!(
        body["canonical_title"],
        d::fact(hit["title"].as_str().unwrap())
    );
    assert_eq!(
        body["canonical_album_artist"],
        d::fact(credit["name"].as_str().unwrap())
    );
    assert_eq!(
        body["canonical_album_artist_sort"],
        d::fact(credit["artist"]["sort-name"].as_str().unwrap())
    );
    assert_eq!(
        body["canonical_year"],
        d::fact(&hit["first-release-date"].as_str().unwrap()[..4])
    );
    assert_eq!(
        body["canonical_genre"],
        json!({"value": "constructed-genre", "source": "musicbrainz", "confidence": 0.55})
    );
    assert_eq!(
        notes(&body),
        vec![
            "Tier 0a: anchored from cache (score 100)".to_owned(),
            d::conniff_positional_note(&case) // positional alignment
        ]
    );
    assert!(mb.take_log().iter().any(|l| l.served == "constructed"));
}

// A lookup answer that is not JSON falls back to the listing, with
// the parse failure in the trace. CONSTRUCTED response.
#[test]
fn unreadable_lookup_falls_back_to_the_listing() {
    musaic_testdata::require_private!("replay");
    let overlay = json!([{
        "key": d::lookup_key(d::conniff_rg()).1, "status": 200, "headers": {},
        "content_b64": base64::engine::general_purpose::STANDARD.encode(b"<html>"),
        "note": "CONSTRUCTED (album fields)",
    }]);
    let case = golden_case("real 0a rip-0001");
    let world = World::from_case(&case);
    let mb = ReplayMb::new(&overlay);
    let r = Resolver::new()
        .resolve(
            &env(&world, &mb),
            case["album"]["id"].as_str().unwrap(),
            "",
            false,
        )
        .unwrap();
    let body = r.resolution.to_dict();
    let n = notes(&body);
    assert!(
        n[1].starts_with("Tier 1: release-group lookup failed (parse error: ")
            && n[1].ends_with("); album fields taken from the release listing"),
        "{n:?}"
    );
    for (k, v) in d::listing_fields(d::conniff_rg(), d::conniff_rel()) {
        assert_eq!(body[k], v, "{k}");
    }
    assert_eq!(body["canonical_genre"], Value::Null);
}

// A non-empty hint skips the memo for the read and its result
// replaces the memo entry; a whitespace-only hint is no hint; `force` and
// `invalidate` behave as before.
#[test]
fn hint_skips_the_memo_and_replaces_the_entry() {
    musaic_testdata::require_private!("replay");
    let case = golden_case("conniff 0b by directory name");
    let world = World::from_case(&case);
    let mb = ReplayMb::new(&json!([]));
    let e = env(&world, &mb);
    let id = case["album"]["id"].as_str().unwrap();
    let resolver = Resolver::new();

    let plain = resolver.resolve(&e, id, "", false).unwrap();
    assert!(!plain.from_memo);
    mb.take_log();

    let hinted = resolver.resolve(&e, id, "ray conniff", false).unwrap();
    assert!(
        !hinted.from_memo,
        "a hint must not be answered from the memo"
    );
    assert!(!std::sync::Arc::ptr_eq(
        &plain.resolution,
        &hinted.resolution
    ));
    assert!(
        !mb.take_log().is_empty(),
        "the hinted resolve asked MusicBrainz"
    );
    assert_eq!(
        hinted.resolution.notes.last().unwrap(),
        "Hint applied to dir-search reranking: 'ray conniff'"
    );

    // Stored, replacing the un-hinted entry: the badge shows the last answer.
    let after = resolver.resolve(&e, id, "", false).unwrap();
    assert!(after.from_memo);
    assert!(std::sync::Arc::ptr_eq(
        &after.resolution,
        &hinted.resolution
    ));

    // Whitespace only: Python's strip() leaves nothing, so no hint.
    let blank = resolver.resolve(&e, id, " \t ", false).unwrap();
    assert!(blank.from_memo);
    assert!(mb.take_log().is_empty());

    let forced = resolver.resolve(&e, id, "", true).unwrap();
    assert!(!forced.from_memo);
    let memo = resolver.resolve(&e, id, "", false).unwrap();
    assert!(memo.from_memo && std::sync::Arc::ptr_eq(&memo.resolution, &forced.resolution));
    resolver.invalidate(Some(id));
    assert!(!resolver.resolve(&e, id, "", false).unwrap().from_memo);
}

fn flac_of(case: &Value) -> Vec<FlacTrack> {
    case["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .map(flac_track)
        .collect()
}

fn aligned(flac: &[FlacTrack], tracks: &[musaic_resolve::find_release::MbTrack]) -> usize {
    align(flac, tracks).iter().filter(|h| h.is_some()).count()
}

// The best-fit release on the recorded listings of the five real albums: which release
// is chosen, and how many tracks align against it versus the Python app's
// earliest release. The "before" figures are the Python app's own (golden:
// track count minus unresolved_track_count). Finding: the decision changes
// the pick only on real-album-5 (same 10/10) and aligns no album better;
// rip-0001 (Conniff) stays at 0/6 because both of its releases carry the
// same lengths, 2.7-8.1 s shorter than the rip. No album gets worse.
#[test]
fn best_fit_release_on_the_real_albums() {
    musaic_testdata::require_private!("replay");
    let mut changed = Vec::new();
    for (scenario, rg, want_rel) in d::real_albums() {
        let case = golden_case(scenario);
        let flac = flac_of(&case);
        let python_body = &case["results"][0]["body"];
        let before = flac.len() - python_body["unresolved_track_count"].as_u64().unwrap() as usize;
        let body = ReplayMb::new(&json!([])).browse_releases(rg).unwrap();
        let (earliest, earliest_tracks) = parse_tracklist(&body).unwrap();
        assert_eq!(aligned(&flac, &earliest_tracks), before, "{scenario}");
        let listing = parse_listing(&body).unwrap();
        let best = best_fit_release(&listing, &flac).unwrap();
        assert_eq!(best.id, want_rel, "{scenario}");
        let after = aligned(&flac, &best.tracks);
        assert!(after >= before, "{scenario}: worse ({before} -> {after})");
        assert_eq!(after, before, "{scenario}: alignment figure changed");
        if best.id != earliest {
            changed.push((scenario, earliest, best.id.clone()));
        }
    }
    assert_eq!(
        changed,
        vec![(
            d::COMPILATION_CASE,
            d::compilation_python_rel().to_owned(),
            d::compilation_fit_rel().to_owned(),
        )]
    );
}

/// A CONSTRUCTED release browse: one release per entry, each with a single
/// medium of `(title, length_ms)` tracks.
fn constructed_listing(releases: &[(&str, &str, &[i64])]) -> Vec<u8> {
    let rels: Vec<Value> = releases
        .iter()
        .map(|(id, date, lens)| {
            json!({
                "id": id, "date": date, "title": "T",
                "media": [{"tracks": lens.iter().enumerate()
                    .map(|(i, l)| json!({"title": format!("t{}", i + 1), "length": l}))
                    .collect::<Vec<_>>()}]
            })
        })
        .collect();
    serde_json::to_vec(&json!({"releases": rels})).unwrap()
}

fn flac(durations: &[f64]) -> Vec<FlacTrack> {
    durations
        .iter()
        .enumerate()
        .map(|(i, d)| {
            flac_track(&json!({"filename": format!("{:02}.flac", i + 1),
                                          "tracknumber": (i + 1).to_string(), "duration": d}))
        })
        .collect()
}

// The fit order on CONSTRUCTED listings, one rule per case.
#[test]
fn best_fit_order() {
    let album = flac(&[200.0, 180.0, 240.0]);
    let pick = |rels: &[(&str, &str, &[i64])], f: &[FlacTrack]| {
        best_fit_release(&parse_listing(&constructed_listing(rels)).unwrap(), f)
            .map(|r| r.id.clone())
    };
    // The earliest release is 5 s off on every track; a later one fits:
    // chosen, and all three tracks align where none did.
    let rels: &[(&str, &str, &[i64])] = &[
        ("a-early", "1970", &[205_000, 185_000, 245_000]),
        ("b-later", "1980", &[200_500, 179_500, 240_000]),
    ];
    assert_eq!(pick(rels, &album).as_deref(), Some("b-later"));
    let l = parse_listing(&constructed_listing(rels)).unwrap();
    assert_eq!(aligned(&album, &l.usable[0].tracks), 0);
    assert_eq!(
        aligned(&album, &best_fit_release(&l, &album).unwrap().tracks),
        3
    );
    // 1. Track count first: an exact count beats closer durations with a bonus track.
    assert_eq!(
        pick(
            &[
                ("a", "1990", &[200_000, 180_000, 240_000, 100_000]),
                ("b", "1990", &[203_000, 183_000, 243_000])
            ],
            &album
        )
        .as_deref(),
        Some("b")
    );
    // 2. Fewer unpaired tracks before duration: a missing length counts as unpaired.
    assert_eq!(
        pick(
            &[
                ("a", "1990", &[200_000, 0, 240_000]),
                ("b", "1990", &[209_000, 189_000, 249_000])
            ],
            &album
        )
        .as_deref(),
        Some("b")
    );
    // 4. Equal fit: earliest date (the Python app's order), undated last.
    assert_eq!(
        pick(
            &[
                ("a", "", &[200_000, 180_000, 240_000]),
                ("b", "1999", &[200_000, 180_000, 240_000])
            ],
            &album
        )
        .as_deref(),
        Some("b")
    );
    // 5. Equal fit and date: smallest MBID, whatever the listing order.
    assert_eq!(
        pick(
            &[
                ("z", "1990", &[200_000, 180_000, 240_000]),
                ("m", "1990", &[200_000, 180_000, 240_000])
            ],
            &album
        )
        .as_deref(),
        Some("m")
    );
    // No durations on the album: nothing pairs, so the Python app's earliest wins.
    assert_eq!(
        pick(
            &[("b", "1990", &[1, 2, 3]), ("a", "1980", &[4, 5, 6])],
            &flac(&[0.0, 0.0, 0.0])
        )
        .as_deref(),
        Some("a")
    );
    // Unusable releases (a track without a title) are never candidates.
    let body = br#"{"releases":[{"id":"x","date":"1900","media":[{"tracks":[{"title":"","length":200000}]}]}]}"#;
    assert!(best_fit_release(&parse_listing(body).unwrap(), &album).is_none());
}

// The best-fit release on the synthetic albums: the golden's constructed one-release
// listing for "Album One" (3/3 before and after), and the recorded Night Mix
// listing (two releases, neither a 4-track one) as a report of what Tier 1
// would do had it been anchored (it is not: the Tier 0b acceptance check rejects it).
#[test]
fn best_fit_on_synthetic_albums() {
    musaic_testdata::require_private!("replay");
    let case = golden_case("cached anchor without score");
    let flac = flac_of(&case);
    let rg = "55555555-5555-5555-5555-555555555555";
    let body = ReplayMb::new(&case["overlay"]).browse_releases(rg).unwrap();
    let (_, earliest) = parse_tracklist(&body).unwrap();
    let best = best_fit_release(&parse_listing(&body).unwrap(), &flac)
        .unwrap()
        .clone();
    assert_eq!(
        (aligned(&flac, &earliest), aligned(&flac, &best.tracks)),
        (3, 3)
    );

    let case = golden_case("night mix 0b by name (negative: no anchor without a duration check)");
    let flac = flac_of(&case);
    let rg = d::night_mix_rg();
    let body = ReplayMb::new(&json!([])).browse_releases(rg).unwrap();
    let (_, earliest) = parse_tracklist(&body).unwrap();
    let before = aligned(&flac, &earliest);
    assert_eq!(
        before,
        flac.len()
            - case["results"][0]["body"]["unresolved_track_count"]
                .as_u64()
                .unwrap() as usize
    );
    let best = best_fit_release(&parse_listing(&body).unwrap(), &flac)
        .unwrap()
        .clone();
    let after = aligned(&flac, &best.tracks);
    assert!(after >= before, "{before} -> {after}");
    eprintln!(
        "night mix: {before} -> {after} of {} ({})",
        flac.len(),
        best.id
    );
}
