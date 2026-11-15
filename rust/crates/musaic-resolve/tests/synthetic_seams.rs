//! Around the cascade, on the synthetic world of `tests/synthetic/mod.rs`:
//! the log line each failure path leaves, the check-fix name search,
//! concurrent resolves of one album, and the title similarity the
//! fingerprint tie-break uses (hand-computed `difflib` ratios).

mod synthetic;

use std::collections::BTreeSet;
use std::sync::Arc;

use musaic_log::{Level, Record, capture};
use musaic_resolve::find_release::{FindOptions, FixAnchor, find_release_for_fix};
use musaic_resolve::similarity::{norm_title, ratio, title_similarity_hundredths};
use musaic_resolve::source::{AnchorStore, FingerprintRow, StoreError, StoredAnchor};
use musaic_resolve::{Env, Resolved, Resolver};
use serde_json::json;
use synthetic::*;

/// The world's store with every anchor write refused.
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

fn resolve_logged(
    world: &World,
    store: &dyn AnchorStore,
    net: &Net,
    prints: &dyn musaic_resolve::source::FingerprintSource,
) -> (Resolved, Vec<Record>) {
    let mb = SynthMb(net);
    let env = Env {
        albums: world,
        store,
        mb: &mb,
        fingerprints: prints,
        now_epoch_s: NOW_EPOCH_S,
    };
    capture(|| Resolver::new().resolve(&env, ALBUM_ID, "", false).unwrap())
}

/// The one record starting with `prefix`, at `level` on `target`.
fn one(lines: &[Record], level: Level, target: &str, prefix: &str) -> Record {
    let hits: Vec<&Record> = lines
        .iter()
        .filter(|r| r.message.starts_with(prefix))
        .collect();
    assert_eq!(hits.len(), 1, "{prefix}: {lines:?}");
    assert_eq!((hits[0].level, hits[0].target.as_str()), (level, target));
    hits[0].clone()
}

fn album_hit(score: i64) -> serde_json::Value {
    search_hit(
        RG_ALBUM,
        json!(score),
        ALBUM,
        main_credit(),
        Some("1987"),
        &[],
    )
}

fn browse_request(rg: &str) -> musaic_net::Request {
    musaic_net::MusicBrainzClient::<musaic_net::ReplayTransport>::browse_request(rg)
}

// ---------------------------------------------------------------------------
// Log lines.

/// A search with no answer at all logs a WARNING on `musaic.mb_lookup`; an
/// HTTP error status does not (it is only traced).
#[test]
fn only_a_network_failure_of_the_search_is_logged() {
    let world = World::new();
    let (_, lines) = resolve_logged(&world, &world, &Net::new(), &NoPrints);
    one(
        &lines,
        Level::Warning,
        "musaic.mb_lookup",
        "MusicBrainz search failed: ",
    );

    let net = Net::new();
    net.status(
        &musaic_net::MusicBrainzClient::<musaic_net::ReplayTransport>::search_request(
            ARTIST, ALBUM,
        ),
        503,
    );
    let (_, lines) = resolve_logged(&world, &world, &net, &NoPrints);
    assert!(
        lines
            .iter()
            .all(|r| !r.message.starts_with("MusicBrainz search failed")),
        "{lines:?}"
    );
}

/// A refused Tier 0b anchor write is an ERROR naming the album, not a
/// failed resolve: the anchor still stands for this resolution.
#[test]
fn refused_name_search_anchor_write_is_logged() {
    let world = World::new();
    let net = Net::new();
    net.search(ARTIST, ALBUM, &search_body(vec![album_hit(100)]));
    net.browse(RG_ALBUM, &album_listing());
    net.cover_ok(RG_ALBUM);
    let (r, lines) = resolve_logged(&world, &ReadOnly(&world), &net, &NoPrints);
    let rec = one(
        &lines,
        Level::Error,
        "musaic.mb_lookup",
        "Failed to cache MB anchor for album ",
    );
    assert_eq!(
        rec.message,
        format!("Failed to cache MB anchor for album {ALBUM_ID}: store: database is locked (test)")
    );
    assert_eq!(r.resolution.anchor.as_ref().unwrap().value, RG_ALBUM);
    assert!(world.writes().is_empty());
}

/// A refused fingerprint-anchor write is an ERROR on the resolver's logger.
#[test]
fn refused_fingerprint_anchor_write_is_logged() {
    let world = World::new();
    let net = Net::new();
    net.search(ARTIST, ALBUM, &search_body(vec![]));
    let group = ac_group(RG_ALBUM, ALBUM, &[(REL_ALBUM, 5, &[5])]);
    for (i, (_, fp, dur)) in album_prints().into_iter().enumerate() {
        net.acoustid(
            fp,
            dur,
            &ac_body(vec![ac_result(
                ACOUSTID_IDS[i],
                0.9,
                &[(RECORDINGS[i], vec![group.clone()])],
            )]),
        );
    }
    let prints = SynthPrints::new(&net, &album_prints());
    let (r, lines) = resolve_logged(&world, &ReadOnly(&world), &net, &prints);
    one(
        &lines,
        Level::Error,
        "musaic.services.resolver",
        &format!("Couldn't persist fingerprint-derived anchor for {ALBUM_ID}: "),
    );
    assert_eq!(r.resolution.anchor.as_ref().unwrap().value, RG_ALBUM);
}

/// A refused release backfill (fresh stored anchor, best-fitting release
/// differs from the stored one) is an ERROR naming the album.
#[test]
fn refused_release_backfill_is_logged() {
    let world = World::new().stored(RG_ALBUM, None, 93, FRESH);
    let net = Net::new();
    net.browse(RG_ALBUM, &album_listing());
    net.cover_ok(RG_ALBUM);
    let (_, lines) = resolve_logged(&world, &ReadOnly(&world), &net, &NoPrints);
    one(
        &lines,
        Level::Error,
        "musaic.mb_lookup",
        &format!("Failed to backfill release_mbid for album {ALBUM_ID}: "),
    );
}

/// A failed tracklist fetch is a WARNING; for a fresh stored anchor it names
/// the cached path, for any other anchor it does not.
#[test]
fn failed_tracklist_fetch_is_logged_by_path() {
    let net = Net::new();
    net.status(&browse_request(RG_ALBUM), 502);
    net.cover_ok(RG_ALBUM);

    let fresh = World::new().stored(RG_ALBUM, Some(REL_ALBUM), 93, FRESH);
    let (_, lines) = resolve_logged(&fresh, &fresh, &net, &NoPrints);
    let rec = one(
        &lines,
        Level::Warning,
        "musaic.mb_lookup",
        "MB tracklist fetch (cached path) failed for ",
    );
    assert_eq!(
        rec.message,
        format!("MB tracklist fetch (cached path) failed for {RG_ALBUM}: HTTP 502")
    );

    let stale = World::new().stored(RG_ALBUM, Some(REL_ALBUM), 93, STALE);
    let (_, lines) = resolve_logged(&stale, &stale, &net, &NoPrints);
    one(
        &lines,
        Level::Warning,
        "musaic.mb_lookup",
        &format!("MB tracklist fetch failed for {RG_ALBUM}: HTTP 502"),
    );
}

// ---------------------------------------------------------------------------
// The check-fix name search.

fn fix_search(net: &Net) -> musaic_resolve::find_release::FixSearch {
    let blocked = BTreeSet::new();
    find_release_for_fix(
        &album_record(),
        FindOptions {
            hint: "",
            blocked: &blocked,
            fetch_cover: true,
            fetch_tracklist: true,
        },
        &SynthMb(net),
    )
}

/// The check fixes save a name-search anchor only when the hit is confident
/// AND its group passes the track-count / duration check; the match itself
/// (cover, tracklist) is returned either way. The listing fetched for the
/// tracklist is reused by the check (one request).
#[test]
fn check_fix_search_saves_only_a_checked_confident_hit() {
    // Confident, five tracks within 0.03 %: saved, with the earliest
    // release's id from the tracklist fetch.
    let net = Net::new();
    net.search(ARTIST, ALBUM, &search_body(vec![album_hit(95)]));
    net.browse(RG_ALBUM, &album_listing());
    net.cover_ok(RG_ALBUM);
    let fix = fix_search(&net);
    assert_eq!(
        fix.anchor,
        FixAnchor::Save {
            album_id: ALBUM_ID.to_owned(),
            release_group: RG_ALBUM.to_owned(),
            release: Some(REL_ALBUM.to_owned()),
            score: 95,
        }
    );
    assert!(fix.matched.cover_url_verified);
    assert_eq!(fix.matched.tracklist.len(), 5);
    assert_eq!(
        net.log()
            .iter()
            .filter(|l| l.contains("inc=media%2Brecordings%2Bartist-credits"))
            .count(),
        1
    );

    // Confident but a one-track release: rejected with the check's reason.
    let net = Net::new();
    net.search(ARTIST, ALBUM, &search_body(vec![album_hit(95)]));
    net.browse(
        RG_ALBUM,
        &browse_body(vec![release(
            REL_ALBUM,
            ALBUM,
            Some("1987"),
            main_credit(),
            &[&[("Tide Glass", Some(1_000_000), RECORDINGS[0])]],
        )]),
    );
    net.cover_ok(RG_ALBUM);
    let fix = fix_search(&net);
    assert_eq!(
        fix.anchor,
        FixAnchor::Rejected("no release within 2 tracks of the album (closest 4)".to_owned())
    );
    assert_eq!(fix.matched.mbid.as_deref(), Some(RG_ALBUM));

    // Below 88: never saved, not even checked.
    let net = Net::new();
    net.search(ARTIST, ALBUM, &search_body(vec![album_hit(80)]));
    net.browse(RG_ALBUM, &album_listing());
    net.cover_ok(RG_ALBUM);
    assert_eq!(fix_search(&net).anchor, FixAnchor::NotConfident);
}

// ---------------------------------------------------------------------------
// Concurrency.

/// Two resolves of the same album at the same time compute it once: the
/// second waits for the first and is served from the memo (one search,
/// one shared result).
#[test]
fn concurrent_resolves_of_one_album_compute_once() {
    let world = World::new();
    let net = Net::new();
    net.search(ARTIST, ALBUM, &search_body(vec![album_hit(80)]));
    net.browse(RG_ALBUM, &album_listing());
    net.cover_ok(RG_ALBUM);
    let resolver = Resolver::new();
    let results: Vec<Resolved> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..4)
            .map(|_| s.spawn(|| resolve(&resolver, &world, &net, &NoPrints, "")))
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert_eq!(results.iter().filter(|r| !r.from_memo).count(), 1);
    assert!(
        results
            .iter()
            .all(|r| Arc::ptr_eq(&r.resolution, &results[0].resolution))
    );
    assert_eq!(
        net.log()
            .iter()
            .filter(|l| l.contains("/ws/2/release-group/?query="))
            .count(),
        1
    );
}

// ---------------------------------------------------------------------------
// Title similarity (the fingerprint vote's third tie-break).

/// `difflib.SequenceMatcher(None, a, b).ratio()` = 2 * M / (len(a) +
/// len(b)), M the size of the Ratcliff/Obershelp matching blocks (longest
/// common block, then recursively left and right of it).
#[test]
fn ratio_follows_the_matching_block_rule() {
    // "bcd" is the longest block; nothing to its left in b, "e" vs "" right.
    assert_eq!(ratio("abcd", "bcde"), 2.0 * 3.0 / 8.0);
    // Longest block "ab", then "cd" right of it: M = 4 of 9 characters.
    assert_eq!(ratio("abxcd", "abcd"), 2.0 * 4.0 / 9.0);
    // Identical strings: 1; nothing in common: 0.
    assert_eq!(ratio("harbour", "harbour"), 1.0);
    assert_eq!(ratio("abc", "xyz"), 0.0);
    // Blocks never cross: in "ab" vs "ba" the longest block is "a" (the
    // first found), and nothing remains on the matching side of it.
    assert_eq!(ratio("ab", "ba"), 0.5);
}

/// The "popular element" heuristic: with len(b) >= 200, an element of b
/// occurring more than len(b) / 100 + 1 times is dropped from the index.
/// a = "aaaaax", b = "x" + 199 * "a": "a" is popular, so the only block
/// found is "x" (M = 1, ratio 2 / 206) instead of "aaaaa" (M = 5). With one
/// "a" fewer (len(b) = 199) the heuristic is off and M = 5 (10 / 205).
#[test]
fn popular_elements_of_long_sequences_are_ignored() {
    let a = format!("{}x", "a".repeat(5));
    let long = format!("x{}", "a".repeat(199));
    assert_eq!(ratio(&a, &long), 2.0 / 206.0);
    let shorter = format!("x{}", "a".repeat(198));
    assert_eq!(ratio(&a, &shorter), 10.0 / 205.0);
}

/// Titles are lower-cased, decomposed, and every run outside [a-z0-9]
/// becomes one space before comparing; the ratio is rounded to hundredths.
#[test]
fn titles_compare_after_normalisation() {
    assert_eq!(norm_title("  PAPER--Harbours!! "), "paper harbours");
    assert_eq!(
        title_similarity_hundredths("Paper Harbours", "paper, harbours"),
        100
    );
    // "paper harbours" (14 characters) is a prefix of "paper harbours live"
    // (19): M = 14, ratio = 28 / 33 = 0.848..., rounded 0.85.
    assert_eq!(
        title_similarity_hundredths("Paper Harbours", "Paper Harbours (Live)"),
        85
    );
    assert!(
        title_similarity_hundredths("Paper Harbours", "Night Ferries")
            < title_similarity_hundredths("Paper Harbours", "Paper Harbours (Live)")
    );
}
