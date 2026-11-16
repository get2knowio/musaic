//! Cascade behaviour the Python app golden cannot show by itself: degradation on
//! every kind of network failure, concurrent resolves coalescing,
//! and persistence through the real library database (idempotency: the
//! second resolve short-circuits at Tier 0a).

mod common;

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};

use common::deltas as d;
use common::env::{NOW_EPOCH_S, NoFingerprints, ReplayFingerprints, ReplayMb, World, album_record};
use musaic_resolve::db_store::DbStore;
use musaic_resolve::source::{
    AlbumRecord, AlbumSource, AnchorStore, FingerprintRow, FlacTrack, MusicBrainzSource, NetError,
    StoreError, StoredAnchor,
};
use musaic_resolve::{Env, Resolver};
use serde_json::{Value, json};

fn golden_case(name: &str) -> Value {
    musaic_testdata::golden("resolver/cascade.json")["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == name)
        .unwrap_or_else(|| panic!("{name}"))
        .clone()
}

/// Wraps a source and fails chosen request kinds at the network level.
struct Failing<'a> {
    inner: &'a dyn MusicBrainzSource,
    search: bool,
    browse: bool,
    cover: bool,
    calls: AtomicUsize,
}

impl<'a> Failing<'a> {
    fn new(inner: &'a dyn MusicBrainzSource) -> Self {
        Self {
            inner,
            search: false,
            browse: false,
            cover: false,
            calls: AtomicUsize::new(0),
        }
    }
}

fn down() -> NetError {
    NetError::Network("connection refused (test)".into())
}

impl MusicBrainzSource for Failing<'_> {
    fn search_release_groups(&self, a: &str, t: &str) -> Result<Vec<u8>, NetError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.search {
            Err(down())
        } else {
            self.inner.search_release_groups(a, t)
        }
    }
    fn browse_releases(&self, rg: &str) -> Result<Vec<u8>, NetError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.browse {
            Err(down())
        } else {
            self.inner.browse_releases(rg)
        }
    }
    fn browse_release_dates(&self, rg: &str) -> Result<Vec<u8>, NetError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.browse {
            Err(down())
        } else {
            self.inner.browse_release_dates(rg)
        }
    }
    fn cover_front_status(&self, rg: &str) -> Result<u16, NetError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.cover {
            Err(down())
        } else {
            self.inner.cover_front_status(rg)
        }
    }
    fn lookup_release_group(&self, rg: &str) -> Result<Vec<u8>, NetError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.browse {
            Err(down())
        } else {
            self.inner.lookup_release_group(rg)
        }
    }
}

fn notes(body: &Value) -> Vec<String> {
    body["notes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n.as_str().unwrap().to_owned())
        .collect()
}

// The search succeeds but the release listing the Tier 0b acceptance check
// check needs cannot be fetched. The hit is rejected with the reason in the
// trace, nothing is persisted, the cascade goes on to Tier 0c, and the caller
// gets a resolution, not an error.
#[test]
fn listing_failure_rejects_the_name_hit_and_falls_through() {
    musaic_testdata::require_private!("replay");
    let case = golden_case("conniff 0b by directory name");
    let world = World::from_case(&case);
    let replay = ReplayMb::new(&json!([]));
    let mut mb = Failing::new(&replay);
    mb.browse = true;
    let fps = ReplayFingerprints::new("rip-0001");
    let env = Env {
        albums: &world,
        store: &world,
        mb: &mb,
        fingerprints: &fps,
        now_epoch_s: NOW_EPOCH_S,
    };
    let r = Resolver::new()
        .resolve(&env, case["album"]["id"].as_str().unwrap(), "", false)
        .unwrap();
    let n = notes(&r.resolution.to_dict());
    assert!(
        n[0].starts_with(&format!(
            "Tier 0b: rejected \"The Ray Conniff Singers \u{2014} {}\"",
            d::conniff_mb_title()
        )) && n[0]
            .ends_with("release listing unavailable (network error: connection refused (test))"),
        "{n:?}"
    );
    // Tier 0c then anchors it by fingerprints (the expected anchor),
    // and Tier 1's tracklist fetch fails too: a partial result.
    assert_eq!(r.resolution.anchor_strategy, "fingerprint-vote");
    assert_eq!(
        n.last().unwrap(),
        "Tier 1: anchored but MB returned no tracklist"
    );
    // no tracklist (deliberate change; the Python app lists no track and counts 0):
    // every one of the six tracks is listed and unresolved.
    assert_eq!(r.resolution.unresolved_track_count, 6);
    assert_eq!(r.resolution.tracks.len(), 6);
    assert!(
        r.resolution
            .tracks
            .iter()
            .all(|t| t.canonical_title.is_none())
    );
}

// No network at all — search, listings, cover and every AcoustID
// lookup fail. The resolve still returns Ok, unanchored, with each failure in
// the trace and every track listed with its stored fingerprint fields.
#[test]
fn total_outage_degrades_to_an_unanchored_resolution() {
    musaic_testdata::require_private!("replay");
    let case = golden_case("conniff 0b by directory name");
    let world = World::from_case(&case);
    let replay = ReplayMb::new(&json!([]));
    let mut mb = Failing::new(&replay);
    mb.search = true;
    mb.browse = true;
    mb.cover = true;
    let mut fps = ReplayFingerprints::new("rip-0001");
    fps.fail = fps.files.iter().map(|f| f.0.clone()).collect();
    let env = Env {
        albums: &world,
        store: &world,
        mb: &mb,
        fingerprints: &fps,
        now_epoch_s: NOW_EPOCH_S,
    };
    let r = Resolver::new()
        .resolve(&env, case["album"]["id"].as_str().unwrap(), "", false)
        .unwrap();
    let body = r.resolution.to_dict();
    assert_eq!(
        notes(&body),
        vec![
            "Tier 0b: MusicBrainz search failed (network error: connection refused (test))",
            "Tier 0c: AcoustID lookup failed for 6 of 6 tracks (network error: lookup failed (test))",
            "Tier 0c: no anchor: no candidate release-group",
            "No anchor found via Tiers 0a-0c; AI escalation may be required",
        ]
    );
    assert_eq!(body["anchor"], Value::Null);
    assert_eq!(body["unresolved_track_count"], 6);
    // The per-track fingerprint fields still come from the store (Python app shape).
    let python = &golden_case("conniff cached anchor blocklisted")["results"][0]["body"];
    assert_eq!(body["tracks"], python["tracks"]);
    assert!(world.take_writes().is_empty());
}

// Failed fingerprint lookups: two of six fail; the four that answered still
// carry 4/6 = 67% >= 60% of the album to the expected group, so the anchor
// survives, and the trace says how many lookups were lost.
#[test]
fn partial_acoustid_failure_is_traced_and_can_still_anchor() {
    musaic_testdata::require_private!("replay");
    let case = golden_case("real 0a rip-0001");
    let mut case = case.clone();
    case["stored_anchor"] = Value::Null;
    let world = World::from_case(&case);
    let mb = ReplayMb::new(&json!([]));
    let mut fps = ReplayFingerprints::new("rip-0001");
    fps.fail = fps.files.iter().take(2).map(|f| f.0.clone()).collect();
    let env = Env {
        albums: &world,
        store: &world,
        mb: &mb,
        fingerprints: &fps,
        now_epoch_s: NOW_EPOCH_S,
    };
    let r = Resolver::new()
        .resolve(&env, case["album"]["id"].as_str().unwrap(), "", false)
        .unwrap();
    let n = notes(&r.resolution.to_dict());
    assert_eq!(
        n[0],
        "Tier 0c: AcoustID lookup failed for 2 of 6 tracks (network error: lookup failed (test))"
    );
    assert!(
        n[1].starts_with(&format!(
            "Tier 0c: 4/6 tracks on \"{}\" ({})",
            d::conniff_mb_title(),
            d::conniff_rg()
        )),
        "{n:?}"
    );
    assert_eq!(r.resolution.anchor.as_ref().unwrap().confidence, 0.88); // max(88, 66)
}

/// An anchor store whose writes fail.
struct ReadOnly<'a>(&'a World);

impl AnchorStore for ReadOnly<'_> {
    fn anchor(&self, id: &str) -> Result<Option<StoredAnchor>, StoreError> {
        self.0.anchor(id)
    }
    fn set_anchor(&self, _: &str, _: &str, _: Option<&str>, _: i64) -> Result<(), StoreError> {
        Err(StoreError("disk full (test)".into()))
    }
    fn blocked(&self, id: &str) -> Result<BTreeSet<String>, StoreError> {
        self.0.blocked(id)
    }
    fn fingerprints(&self, id: &str) -> Result<Vec<FingerprintRow>, StoreError> {
        self.0.fingerprints(id)
    }
}

// Like the Python app (try/except around set_album_mb_anchor), a failed
// anchor write does not fail the resolve and publishes no write event; only
// the resolver's own "new anchor" event remains.
#[test]
fn failed_anchor_write_is_not_an_error() {
    musaic_testdata::require_private!("replay");
    let case = golden_case("conniff 0b by directory name");
    let world = World::from_case(&case);
    let mb = ReplayMb::new(&json!([]));
    let store = ReadOnly(&world);
    let env = Env {
        albums: &world,
        store: &store,
        mb: &mb,
        fingerprints: &NoFingerprints,
        now_epoch_s: NOW_EPOCH_S,
    };
    let r = Resolver::new()
        .resolve(&env, case["album"]["id"].as_str().unwrap(), "", false)
        .unwrap();
    assert_eq!(r.resolution.anchor_strategy, "dir-search");
    assert_eq!(r.events.len(), 1);
    assert_eq!(r.events[0].source, "musicbrainz");
}

/// Counts requests across threads; each thread brings its own replay source.
struct Counting<'a> {
    inner: &'a ReplayMb,
    n: &'a AtomicUsize,
}

impl MusicBrainzSource for Counting<'_> {
    fn search_release_groups(&self, a: &str, t: &str) -> Result<Vec<u8>, NetError> {
        self.n.fetch_add(1, Ordering::SeqCst);
        std::thread::sleep(std::time::Duration::from_millis(20));
        self.inner.search_release_groups(a, t)
    }
    fn browse_releases(&self, rg: &str) -> Result<Vec<u8>, NetError> {
        self.n.fetch_add(1, Ordering::SeqCst);
        self.inner.browse_releases(rg)
    }
    fn browse_release_dates(&self, rg: &str) -> Result<Vec<u8>, NetError> {
        self.n.fetch_add(1, Ordering::SeqCst);
        self.inner.browse_release_dates(rg)
    }
    fn cover_front_status(&self, rg: &str) -> Result<u16, NetError> {
        self.n.fetch_add(1, Ordering::SeqCst);
        self.inner.cover_front_status(rg)
    }
    fn lookup_release_group(&self, rg: &str) -> Result<Vec<u8>, NetError> {
        self.n.fetch_add(1, Ordering::SeqCst);
        self.inner.lookup_release_group(rg)
    }
}

// Concurrency: eight threads resolving the same album at once
// coalesce onto one cascade (the Python app's per-album lock): one set of
// requests, one set of anchor writes, and every caller gets the same object.
#[test]
fn concurrent_resolves_of_one_album_coalesce() {
    musaic_testdata::require_private!("replay");
    let case = golden_case("conniff 0b by directory name");
    let world = World::from_case(&case);
    let resolver = Resolver::new();
    let requests = AtomicUsize::new(0);
    let barrier = Barrier::new(8);
    let id = case["album"]["id"].as_str().unwrap();
    let results: Vec<_> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..8)
            .map(|_| {
                s.spawn(|| {
                    let replay = ReplayMb::new(&json!([]));
                    let mb = Counting {
                        inner: &replay,
                        n: &requests,
                    };
                    let env = Env {
                        albums: &world,
                        store: &world,
                        mb: &mb,
                        fingerprints: &NoFingerprints,
                        now_epoch_s: NOW_EPOCH_S,
                    };
                    barrier.wait();
                    resolver.resolve(&env, id, "", false).unwrap()
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    // search + listing + cover: one cascade's worth.
    assert_eq!(requests.load(Ordering::SeqCst), 3);
    assert_eq!(world.take_writes().len(), 2);
    let computed: Vec<_> = results.iter().filter(|r| !r.from_memo).collect();
    assert_eq!(computed.len(), 1);
    assert_eq!(computed[0].events.len(), 3);
    for r in &results {
        assert!(Arc::ptr_eq(&r.resolution, &results[0].resolution));
    }
}

/// Album row from a golden case, its anchor column read from the database.
struct DbAlbums<'a> {
    world: &'a World,
    conn: &'a rusqlite::Connection,
    id: String,
}

impl AlbumSource for DbAlbums<'_> {
    fn album(&self, _: &str) -> Option<AlbumRecord> {
        let a = self.world.album.lock().unwrap().clone()?;
        let mut a = a;
        a["id"] = json!(self.id);
        let stored = musaic_db::mb_anchor::get_album_mb_anchor(self.conn, &self.id).unwrap();
        Some(album_record(
            &a,
            stored
                .map(|s| StoredAnchor {
                    release_group_mbid: s.release_group_mbid,
                    release_mbid: s.release_mbid,
                    score: s.score,
                    last_checked_at: s.last_checked_at,
                })
                .as_ref(),
        ))
    }
    fn filenames(&self, a: &AlbumRecord) -> Vec<String> {
        self.world.filenames(a)
    }
    fn tracks(&self, a: &AlbumRecord) -> Vec<FlacTrack> {
        self.world.tracks(a)
    }
}

// Idempotency, through the library database the Python app wrote
// (fixture copy): a Tier 0b anchor is persisted with the Python app's columns,
// the memo is dropped (as the bus's invalidate subscriber does on the
// published event), and the next resolve short-circuits at Tier 0a without
// searching. Blocking the group ("Wrong match") then keeps it from coming back.
#[test]
fn anchor_persists_in_the_database_and_short_circuits() {
    musaic_testdata::require_private!("replay");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.db");
    std::fs::copy(
        musaic_testdata::public("db/v16-synthetic-offline.db"),
        &path,
    )
    .unwrap();
    let conn = rusqlite::Connection::open(&path).unwrap();
    let id = "0c97baca60f74928".to_owned(); // "Mono Harbor - Gaps": a real row, no anchor yet
    assert!(
        musaic_db::mb_anchor::get_album_mb_anchor(&conn, &id)
            .unwrap()
            .is_none()
    );

    let case = golden_case("conniff 0b by directory name");
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
    let replay = ReplayMb::new(&json!([]));
    let resolver = Resolver::new();
    let env = Env {
        albums: &albums,
        store: &store,
        mb: &replay,
        fingerprints: &NoFingerprints,
        now_epoch_s: NOW_EPOCH_S,
    };

    let first = resolver.resolve(&env, &id, "", false).unwrap();
    assert_eq!(first.resolution.anchor_strategy, "dir-search");
    assert_eq!(first.events.len(), 3);
    let python_after = &case["results"][0]["store_after"];
    let row = musaic_db::mb_anchor::get_album_mb_anchor(&conn, &id)
        .unwrap()
        .unwrap();
    assert_eq!(
        row.release_group_mbid,
        python_after["release_group_mbid"].as_str().unwrap()
    );
    assert_eq!(
        row.release_mbid.as_deref(),
        python_after["release_mbid"].as_str()
    );
    assert_eq!(row.score, python_after["score"].as_i64());
    assert_eq!(row.last_checked_at.as_deref(), Some("2026-11-10T00:00:00Z"));
    replay.take_log();

    for e in &first.events {
        resolver.invalidate(Some(&e.album_id));
    }
    let second = resolver.resolve(&env, &id, "", false).unwrap();
    assert!(!second.from_memo);
    assert_eq!(second.resolution.anchor_strategy, "cache");
    assert_eq!(
        second.resolution.notes[0],
        "Tier 0a: anchored from cache (score 100)"
    );
    assert!(
        second.events.is_empty(),
        "release already known, anchor already set"
    );
    // Only Tier 1's requests, no search: the Python app's cover check and release
    // listing, plus the release-group lookup for the album fields (recorded
    // for this group).
    let browse_key = musaic_net::MusicBrainzClient::<musaic_net::ReplayTransport>::browse_request(
        &row.release_group_mbid,
    )
    .replay_key();
    let log: Vec<(String, String)> = replay
        .take_log()
        .into_iter()
        .map(|l| (l.method, l.key))
        .collect();
    assert_eq!(log.len(), 3, "{log:?}");
    assert!(log.contains(&("GET".to_owned(), browse_key)), "{log:?}");
    assert!(
        log.contains(&common::deltas::lookup_key(&row.release_group_mbid)),
        "{log:?}"
    );
    assert!(log.iter().any(|(m, _)| m == "HEAD"), "{log:?}");
    assert_eq!(second.resolution.tracks, first.resolution.tracks);

    musaic_db::mb_anchor::add_blocked_mb_release_group(
        &conn,
        &id,
        &row.release_group_mbid,
        "2026-11-10T00:00:01Z",
    )
    .unwrap();
    resolver.invalidate(Some(&id));
    let third = resolver.resolve(&env, &id, "", false).unwrap();
    assert!(third.resolution.anchor.is_none());
    assert_eq!(
        third.resolution.notes[0],
        "Blocklist active (1 release-group(s) rejected via Wrong match)"
    );
}
