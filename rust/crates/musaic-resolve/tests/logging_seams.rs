//! Log lines the Python app writes on the resolver's failure paths
//! (`musaic.mb_lookup`, `musaic.services.resolver`; server log follow-up: the
//! lower crates log through `musaic-log`). Every failure here degrades the resolution, as before; these tests pin only the log
//! line each one now leaves. Inputs are the Python app golden's
//! "conniff 0b by directory name" case and the recorded replay set.
//!
//! Deliberate differences, said once: the Python app's `log.exception(msg)`
//! (ERROR plus a traceback) is ERROR `msg: <error text>` here, and an
//! interpolated exception (`%s` of `e`) is this build's `NetError` text.

mod common;

use std::collections::BTreeSet;

use common::env::{FRESH, NOW_EPOCH_S, ReplayFingerprints, ReplayMb, World};
use musaic_log::{Level, Record, capture};
use musaic_resolve::find_release::{FindOptions, find_release};
use musaic_resolve::source::{
    AlbumSource, AnchorStore, FingerprintRow, MusicBrainzSource, NetError, StoreError, StoredAnchor,
};
use musaic_resolve::{Env, Resolved, Resolver};
use serde_json::{Value, json};

const CASE: &str = "conniff 0b by directory name";

fn golden_case(name: &str) -> Value {
    musaic_testdata::golden("resolver/cascade.json")["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == name)
        .unwrap_or_else(|| panic!("{name}"))
        .clone()
}

/// The replay set with chosen request kinds failing at the network level.
struct Failing<'a> {
    inner: &'a dyn MusicBrainzSource,
    search: bool,
    browse: bool,
}

fn down() -> NetError {
    NetError::Network("connection refused (test)".into())
}

impl MusicBrainzSource for Failing<'_> {
    fn search_release_groups(&self, a: &str, t: &str) -> Result<Vec<u8>, NetError> {
        if self.search {
            Err(down())
        } else {
            self.inner.search_release_groups(a, t)
        }
    }
    fn browse_releases(&self, rg: &str) -> Result<Vec<u8>, NetError> {
        if self.browse {
            Err(down())
        } else {
            self.inner.browse_releases(rg)
        }
    }
    fn browse_release_dates(&self, rg: &str) -> Result<Vec<u8>, NetError> {
        self.inner.browse_release_dates(rg)
    }
    fn cover_front_status(&self, rg: &str) -> Result<u16, NetError> {
        self.inner.cover_front_status(rg)
    }
    fn lookup_release_group(&self, rg: &str) -> Result<Vec<u8>, NetError> {
        if self.browse {
            Err(down())
        } else {
            self.inner.lookup_release_group(rg)
        }
    }
}

/// The world's store, with every anchor write refused.
struct ReadOnly<'a>(&'a World);

impl AnchorStore for ReadOnly<'_> {
    fn anchor(&self, id: &str) -> Result<Option<StoredAnchor>, StoreError> {
        self.0.anchor(id)
    }
    fn set_anchor(&self, _: &str, _: &str, _: Option<&str>, _: i64) -> Result<(), StoreError> {
        Err(StoreError("database is locked (test)".into()))
    }
    fn blocked(&self, id: &str) -> Result<BTreeSet<String>, StoreError> {
        self.0.blocked(id)
    }
    fn fingerprints(&self, id: &str) -> Result<Vec<FingerprintRow>, StoreError> {
        self.0.fingerprints(id)
    }
}

fn resolve(
    world: &World,
    store: &dyn AnchorStore,
    mb: &dyn MusicBrainzSource,
) -> (Resolved, Vec<Record>) {
    let case = golden_case(CASE);
    let fps = ReplayFingerprints::new("rip-0001");
    let env = Env {
        albums: world,
        store,
        mb,
        fingerprints: &fps,
        now_epoch_s: NOW_EPOCH_S,
    };
    capture(|| {
        Resolver::new()
            .resolve(&env, case["album"]["id"].as_str().unwrap(), "", false)
            .unwrap()
    })
}

fn album_id() -> String {
    golden_case(CASE)["album"]["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn find<'a>(lines: &'a [Record], prefix: &str) -> Vec<&'a Record> {
    lines
        .iter()
        .filter(|r| r.message.starts_with(prefix))
        .collect()
}

fn one(lines: &[Record], level: Level, target: &str, prefix: &str) -> Record {
    let hits = find(lines, prefix);
    assert_eq!(hits.len(), 1, "{prefix}: {lines:?}");
    assert_eq!((hits[0].level, hits[0].target.as_str()), (level, target));
    hits[0].clone()
}

// Python app: musaic/mb_lookup.py:170 `log.warning("MusicBrainz search failed: %s", e)`.
// Tier 0b's search is down; the cascade goes on.
#[test]
fn search_network_failure_is_logged() {
    musaic_testdata::require_private!("replay");
    let world = World::from_case(&golden_case(CASE));
    let replay = ReplayMb::new(&json!([]));
    let mb = Failing {
        inner: &replay,
        search: true,
        browse: false,
    };
    let (_, lines) = resolve(&world, &world, &mb);
    let r = one(
        &lines,
        Level::Warning,
        "musaic.mb_lookup",
        "MusicBrainz search failed: ",
    );
    assert_eq!(
        r.message,
        "MusicBrainz search failed: connection refused (test)"
    );
}

// Python app: musaic/mb_lookup.py:302
// `log.exception("Failed to cache MB anchor for album %s", album_id)`.
// Tier 0b's confident hit cannot be stored: anchored anyway.
#[test]
fn tier_0b_anchor_write_failure_is_logged() {
    musaic_testdata::require_private!("replay");
    let world = World::from_case(&golden_case(CASE));
    let replay = ReplayMb::new(&json!([]));
    let (r, lines) = resolve(&world, &ReadOnly(&world), &replay);
    assert_eq!(r.resolution.anchor_strategy, "dir-search");
    let rec = one(
        &lines,
        Level::Error,
        "musaic.mb_lookup",
        "Failed to cache MB anchor for album ",
    );
    assert_eq!(
        rec.message,
        format!(
            "Failed to cache MB anchor for album {}: store: database is locked (test)",
            album_id()
        )
    );
}

// Python app: musaic/mb_lookup.py:432
// `log.exception("Failed to backfill release_mbid for album %s", album_id)`.
// A fresh stored anchor without a release: Tier 1 picks one and the
// backfill write fails. The anchor is the one a normal run stores (derived
// from that run, not typed in).
#[test]
fn release_backfill_failure_is_logged() {
    musaic_testdata::require_private!("replay");
    let world = World::from_case(&golden_case(CASE));
    let replay = ReplayMb::new(&json!([]));
    let _ = resolve(&world, &world, &replay);
    let written = world.take_writes();
    let rg = written[0]["release_group_mbid"]
        .as_str()
        .unwrap()
        .to_owned();
    *world.store.lock().unwrap() = Some(StoredAnchor {
        release_group_mbid: rg,
        release_mbid: None,
        score: written[0]["score"].as_i64(),
        last_checked_at: Some(FRESH.to_owned()),
    });
    let replay = ReplayMb::new(&json!([]));
    let (r, lines) = resolve(&world, &ReadOnly(&world), &replay);
    assert_eq!(r.resolution.anchor_strategy, "cache");
    one(
        &lines,
        Level::Error,
        "musaic.mb_lookup",
        &format!(
            "Failed to backfill release_mbid for album {}: store: ",
            album_id()
        ),
    );
}

// Python app: musaic/mb_lookup.py:454
// `log.warning("MB tracklist fetch (cached path) failed for %s: %s", mbid, e)`.
// Release listings are down: Tier 0b's hit fails the acceptance check,
// Tier 0c anchors by fingerprints and stores it (fresh), and Tier 1's
// listing fails, which the Python app's `find_release_cached` logs on its
// cached path.
#[test]
fn tier_1_listing_failure_on_a_fresh_anchor_is_logged() {
    musaic_testdata::require_private!("replay");
    let world = World::from_case(&golden_case(CASE));
    let replay = ReplayMb::new(&json!([]));
    let mb = Failing {
        inner: &replay,
        search: false,
        browse: true,
    };
    let (r, lines) = resolve(&world, &world, &mb);
    assert_eq!(r.resolution.anchor_strategy, "fingerprint-vote");
    let anchor = r.resolution.anchor.as_ref().unwrap().value.clone();
    let rec = one(
        &lines,
        Level::Warning,
        "musaic.mb_lookup",
        "MB tracklist fetch (cached path) failed for ",
    );
    assert_eq!(
        rec.message,
        format!(
            "MB tracklist fetch (cached path) failed for {anchor}: network error: connection refused (test)"
        )
    );
    assert!(find(&lines, "Couldn't persist").is_empty());
}

// Python app: musaic/services/resolver.py:579
// `_log.exception("Couldn't persist fingerprint-derived anchor for %s", album["id"])`
// and, with nothing stored, Tier 1 is the Python app's plain `find_release`
// path: musaic/mb_lookup.py:283
// `log.warning("MB tracklist fetch failed for %s: %s", mbid, e)`.
#[test]
fn tier_0c_anchor_write_failure_is_logged() {
    musaic_testdata::require_private!("replay");
    let world = World::from_case(&golden_case(CASE));
    let replay = ReplayMb::new(&json!([]));
    let mb = Failing {
        inner: &replay,
        search: false,
        browse: true,
    };
    let (r, lines) = resolve(&world, &ReadOnly(&world), &mb);
    assert_eq!(r.resolution.anchor_strategy, "fingerprint-vote");
    let anchor = r.resolution.anchor.as_ref().unwrap().value.clone();
    one(
        &lines,
        Level::Error,
        "musaic.services.resolver",
        &format!(
            "Couldn't persist fingerprint-derived anchor for {}: store: ",
            album_id()
        ),
    );
    one(
        &lines,
        Level::Warning,
        "musaic.mb_lookup",
        &format!("MB tracklist fetch failed for {anchor}: "),
    );
}

// Python app: musaic/mb_lookup.py:283
// `log.warning("MB tracklist fetch failed for %s: %s", mbid, e)`, from
// `find_release(..., fetch_tracklist=True)` itself (the check-fix callers'
// path).
#[test]
fn find_release_tracklist_failure_is_logged() {
    musaic_testdata::require_private!("replay");
    let world = World::from_case(&golden_case(CASE));
    let album = world.album(&album_id()).unwrap();
    let replay = ReplayMb::new(&json!([]));
    let mb = Failing {
        inner: &replay,
        search: false,
        browse: true,
    };
    let blocked = BTreeSet::new();
    let (m, lines) = capture(|| {
        find_release(
            &album,
            FindOptions {
                hint: "",
                blocked: &blocked,
                fetch_cover: false,
                fetch_tracklist: true,
            },
            &mb,
        )
    });
    let mbid = m.mbid.clone().unwrap();
    let rec = one(
        &lines,
        Level::Warning,
        "musaic.mb_lookup",
        "MB tracklist fetch failed for ",
    );
    assert_eq!(
        rec.message,
        format!("MB tracklist fetch failed for {mbid}: network error: connection refused (test)")
    );
}
