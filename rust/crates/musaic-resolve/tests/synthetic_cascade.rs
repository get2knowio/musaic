//! The resolver cascade end to end on a synthetic world (see
//! `tests/synthetic/mod.rs`): an invented five-track album ("Lantern Coast -
//! Paper Harbours"), MusicBrainz / AcoustID / Cover Art Archive answers built
//! here, served through the real request builders by replay key.
//!
//! Each expected value follows from the inputs by the rule named in the
//! comment above the assertion; none was copied from a run.

mod synthetic;

use std::sync::Arc;

use musaic_resolve::resolution::UNANCHORED_NOTE;
use musaic_resolve::source::{FingerprintRow, FlacTrack};
use musaic_resolve::{AlbumResolution, Env, Resolver};
use serde_json::{Value, json};
use synthetic::*;

fn cover_url(rg: &str) -> String {
    format!("https://coverartarchive.org/release-group/{rg}/front")
}

fn notes(r: &AlbumResolution) -> Vec<String> {
    r.notes.clone()
}

fn has_note(r: &AlbumResolution, want: &str) -> bool {
    r.notes.iter().any(|n| n == want)
}

fn anchor(r: &AlbumResolution) -> Option<(&str, &str, f64)> {
    r.anchor
        .as_ref()
        .map(|a| (a.value.as_str(), a.source.as_str(), a.confidence))
}

fn fact(f: &Option<musaic_resolve::FieldFact>) -> Option<(&str, &str, f64)> {
    f.as_ref()
        .map(|a| (a.value.as_str(), a.source.as_str(), a.confidence))
}

/// Track numbers the resolution gives each file, in file order.
fn numbers(r: &AlbumResolution) -> Vec<Option<String>> {
    r.tracks
        .iter()
        .map(|t| t.canonical_tracknumber.as_ref().map(|f| f.value.clone()))
        .collect()
}

fn titles(r: &AlbumResolution) -> Vec<Option<String>> {
    r.tracks
        .iter()
        .map(|t| t.canonical_title.as_ref().map(|f| f.value.clone()))
        .collect()
}

fn mb_titles() -> Vec<Option<String>> {
    TRACKS.iter().map(|t| Some(t.3.to_owned())).collect()
}

fn one_to_five() -> Vec<Option<String>> {
    (1..=5).map(|n| Some(n.to_string())).collect()
}

/// Requests whose URL contains every one of `needles`.
fn count(net: &Net, needles: &[&str]) -> usize {
    net.log()
        .iter()
        .filter(|l| needles.iter().all(|n| l.contains(n)))
        .count()
}

const SEARCH: &str = "/ws/2/release-group/?query=";
const LISTING: &str = "inc=media%2Brecordings%2Bartist-credits";

/// The search hit for the studio album at `score`, with its date and tags.
fn album_hit(score: i64) -> Value {
    search_hit(
        RG_ALBUM,
        json!(score),
        ALBUM,
        main_credit(),
        Some("1987-03-02"),
        &[("dream pop", 4), ("shoegaze", 4), ("tin whistle", 1)],
    )
}

/// The release-group lookup of the studio album.
fn album_group() -> Value {
    group_body(
        RG_ALBUM,
        ALBUM,
        main_credit(),
        "1987-03-02",
        &[("tin whistle", 1), ("dream pop", 3)],
    )
}

/// Everything Tier 1 asks about the studio album, answered.
fn album_tier_1(net: &Net) {
    net.browse(RG_ALBUM, &album_listing());
    net.cover_ok(RG_ALBUM);
    net.group(RG_ALBUM, &album_group());
}

/// A five-track album's lengths shifted by `offsets` (ms).
fn shifted_medium(offsets_ms: [i64; 5]) -> Vec<TrackSpec<'static>> {
    album_medium()
        .into_iter()
        .zip(offsets_ms)
        .map(|((t, len, rec), o)| (t, len.map(|l| l + o), rec))
        .collect()
}

// ---------------------------------------------------------------------------
// Entry and Tier 0a.

/// An album id the store does not know: one trace note, nothing asked.
#[test]
fn album_not_in_the_cache_asks_for_a_rescan() {
    let world = World::new();
    *world.album.lock().unwrap() = None;
    let net = Net::new();
    let r = resolve_once(&world, &net, &NoPrints).resolution;
    assert_eq!(notes(&r), ["Album not in cache; rescan first"]);
    assert!(r.anchor.is_none());
    assert!(net.log().is_empty());
}

/// Tier 0a: a fresh, unblocked stored anchor is used as it is (source
/// `cache`, confidence = stored score / 100); no name search and no
/// fingerprint lookup is made; nothing is written back (fresh, and the
/// best-fitting release is the stored one) and, the album already having an
/// anchor, no anchor event is published. Tier 1 asks the release-group
/// lookup (the anchor did not come from a search) for the album fields.
#[test]
fn cached_anchor_is_used_without_searching() {
    let world = World::new().stored(RG_ALBUM, Some(REL_ALBUM), 93, FRESH);
    let net = Net::new();
    album_tier_1(&net);
    let prints = SynthPrints::new(&net, &album_prints());
    let resolved = resolve_once(&world, &net, &prints);
    let r = &resolved.resolution;

    assert_eq!(anchor(r), Some((RG_ALBUM, "cache", 0.93)));
    assert_eq!(r.anchor_strategy, "cache");
    assert_eq!(r.notes[0], "Tier 0a: anchored from cache (score 93)");
    assert_eq!(count(&net, &[SEARCH]), 0);
    assert_eq!(prints.calls(), 0);
    assert!(world.writes().is_empty());
    assert!(resolved.events.is_empty());

    // Album fields from the release-group lookup: title, credited name,
    // the first credit's sort-name, the first four digits of
    // first-release-date; genre = the tag with the highest count when that
    // count is at least 2 ("dream pop", 3), at confidence 0.55.
    assert_eq!(fact(&r.canonical_title), Some((ALBUM, "musicbrainz", 0.95)));
    assert_eq!(
        fact(&r.canonical_album_artist),
        Some((ARTIST, "musicbrainz", 0.95))
    );
    assert_eq!(
        fact(&r.canonical_album_artist_sort),
        Some((ARTIST_SORT, "musicbrainz", 0.95))
    );
    assert_eq!(fact(&r.canonical_year), Some(("1987", "musicbrainz", 0.95)));
    assert_eq!(
        fact(&r.canonical_genre),
        Some(("dream pop", "musicbrainz", 0.55))
    );
    // The front cover answered 200: the CAA release-group URL is offered.
    let cover = cover_url(RG_ALBUM);
    assert_eq!(
        fact(&r.cover_url),
        Some((cover.as_str(), "musicbrainz", 0.95))
    );

    // Every FLAC is within 2 s of the MusicBrainz track at its TRACKNUMBER.
    assert_eq!(numbers(r), one_to_five());
    assert_eq!(titles(r), mb_titles());
    assert_eq!(r.unresolved_track_count, 0);
    for (t, (_, _, _, _, ms)) in r.tracks.iter().zip(TRACKS) {
        assert_eq!(t.canonical_duration_s, Some(ms as f64 / 1000.0));
        assert_eq!(t.canonical_title.as_ref().unwrap().confidence, 0.95);
        assert_eq!(t.canonical_artist.as_ref().unwrap().value, ARTIST);
    }
}

/// A blocklisted stored anchor is skipped at Tier 0a (with the blocklist
/// note) and filtered out of the name search, whose next hit anchors.
#[test]
fn blocklisted_stored_anchor_is_skipped() {
    let world = World::new()
        .stored(RG_OTHER, Some(REL_OTHER), 100, FRESH)
        .block(RG_OTHER);
    let net = Net::new();
    net.search(
        ARTIST,
        ALBUM,
        &search_body(vec![
            search_hit(
                RG_OTHER,
                json!(100),
                ALBUM,
                main_credit(),
                Some("1987"),
                &[],
            ),
            album_hit(95),
        ]),
    );
    album_tier_1(&net);
    let r = resolve_once(&world, &net, &NoPrints).resolution;

    assert!(has_note(
        &r,
        "Blocklist active (1 release-group(s) rejected via Wrong match)"
    ));
    assert_eq!(anchor(&r), Some((RG_ALBUM, "musicbrainz", 0.95)));
    assert_eq!(count(&net, &[RG_OTHER]), 0, "{:?}", net.log());
    // 95 >= 88: persisted at Tier 0b (no release yet), then Tier 1 backfills
    // the release it aligned against.
    assert_eq!(
        world.writes(),
        [
            (RG_ALBUM.to_owned(), None, 95),
            (RG_ALBUM.to_owned(), Some(REL_ALBUM.to_owned()), 95),
        ]
    );
}

// ---------------------------------------------------------------------------
// Tier 0b: name + duration search.

/// A confident hit (score >= 88) whose release agrees on track count (5 =
/// 5) and total duration (999.7 s against 1000.0 s, 0.03 %) anchors as
/// `dir-search` and is persisted; Tier 1 reuses the hit's album fields
/// (no release-group lookup), browses the group once (the check and the
/// tracklist share it), follows the cover redirects to a 200 and offers the
/// CAA URL. Fingerprints are never asked.
#[test]
fn confident_name_search_hit_is_checked_persisted_and_refined() {
    let world = World::new();
    let net = Net::new();
    net.search(ARTIST, ALBUM, &search_body(vec![album_hit(100)]));
    net.browse(RG_ALBUM, &album_listing());
    net.cover_via_redirects(RG_ALBUM, REL_ALBUM);
    let prints = SynthPrints::new(&net, &album_prints());
    let resolved = resolve_once(&world, &net, &prints);
    let r = &resolved.resolution;

    assert_eq!(anchor(r), Some((RG_ALBUM, "musicbrainz", 1.0)));
    assert_eq!(r.anchor_strategy, "dir-search");
    let want = format!("Tier 0b: {}", matched_note(ARTIST, ALBUM, 100, RG_ALBUM));
    assert!(has_note(r, &want), "{:?}", r.notes);
    assert_eq!(prints.calls(), 0);
    assert_eq!(
        world.writes(),
        [
            (RG_ALBUM.to_owned(), None, 100),
            (RG_ALBUM.to_owned(), Some(REL_ALBUM.to_owned()), 100),
        ]
    );
    // Two anchor writes (the hit, then the release id Tier 1 learns) and the
    // first-anchor notice are still one event for the resolve.
    assert_eq!(resolved.events.len(), 1, "{:?}", resolved.events);
    assert!(
        resolved
            .events
            .iter()
            .all(|e| e.album_id == ALBUM_ID && e.source == "musicbrainz")
    );
    assert_eq!(count(&net, &[LISTING]), 1, "{:?}", net.log());
    assert_eq!(count(&net, &[&format!("release-group/{RG_ALBUM}?")]), 0);

    // Fields of the search hit. Genre: "dream pop" and "shoegaze" tie at 4,
    // the first listed wins.
    assert_eq!(fact(&r.canonical_title), Some((ALBUM, "musicbrainz", 0.95)));
    assert_eq!(
        fact(&r.canonical_album_artist_sort),
        Some((ARTIST_SORT, "musicbrainz", 0.95))
    );
    assert_eq!(fact(&r.canonical_year), Some(("1987", "musicbrainz", 0.95)));
    assert_eq!(
        fact(&r.canonical_genre),
        Some(("dream pop", "musicbrainz", 0.55))
    );
    assert_eq!(r.cover_url.as_ref().unwrap().value, cover_url(RG_ALBUM));
    assert_eq!(numbers(r), one_to_five());
    assert_eq!(r.unresolved_track_count, 0);
}

/// A hit at 70 <= score < 88 anchors (confidence = score / 100) but is not
/// persisted, so Tier 1 has no stored anchor to renew or backfill either.
#[test]
fn medium_score_hit_anchors_without_persisting() {
    let world = World::new();
    let net = Net::new();
    net.search(ARTIST, ALBUM, &search_body(vec![album_hit(75)]));
    net.browse(RG_ALBUM, &album_listing());
    net.cover_ok(RG_ALBUM);
    let r = resolve_once(&world, &net, &NoPrints).resolution;
    assert_eq!(anchor(&r), Some((RG_ALBUM, "musicbrainz", 0.75)));
    assert!(world.writes().is_empty());
    assert_eq!(numbers(&r), one_to_five());
}

/// A hit below 70 is no anchor; it is not even checked (no release
/// listing), and without fingerprints the album ends unanchored with every
/// track unresolved.
#[test]
fn hit_below_seventy_is_not_an_anchor() {
    let world = World::new();
    let net = Net::new();
    net.search(ARTIST, ALBUM, &search_body(vec![album_hit(69)]));
    let r = resolve_once(&world, &net, &NoPrints).resolution;
    assert!(r.anchor.is_none());
    assert_eq!(r.anchor_strategy, "none");
    assert_eq!(notes(&r), [UNANCHORED_NOTE]);
    assert_eq!(r.unresolved_track_count, 5);
    assert_eq!(r.tracks.len(), 5);
    assert!(r.tracks.iter().all(|t| t.canonical_title.is_none()));
    assert_eq!(count(&net, &[LISTING]), 0);
    assert!(world.writes().is_empty());
}

/// The name-search check: no release within 2 tracks (an 8-track release
/// against 5 files: gap 3) rejects the hit with the closest gap in the
/// trace; nothing is written and the cascade moves on.
#[test]
fn name_search_hit_with_another_track_count_is_rejected() {
    let world = World::new();
    let net = Net::new();
    net.search(ARTIST, ALBUM, &search_body(vec![album_hit(100)]));
    let mut eight = album_medium();
    eight.extend([
        ("Quay Lights", Some(60_000), DOUBLE_RECORDINGS[0]),
        ("Ropewalk", Some(60_000), DOUBLE_RECORDINGS[1]),
        ("Last Ferry", Some(60_000), DOUBLE_RECORDINGS[2]),
    ]);
    net.browse(
        RG_ALBUM,
        &browse_body(vec![release(
            REL_ALBUM_DELUXE,
            ALBUM,
            Some("1987-03-02"),
            main_credit(),
            &[&eight],
        )]),
    );
    let r = resolve_once(&world, &net, &NoPrints).resolution;
    let want = format!(
        "Tier 0b: rejected \"{ARTIST} \u{2014} {ALBUM}\" (score 100/100, MBID {RG_ALBUM}): \
         no release within 2 tracks of the album (closest 3)"
    );
    assert!(has_note(&r, &want), "{:?}", r.notes);
    assert!(r.anchor.is_none());
    assert!(world.writes().is_empty());
}

/// The track-count tolerance is inclusive: a 7-track release (gap 2) whose
/// total is 1020.0 s (2 % from 1000.0 s) passes the check.
#[test]
fn track_gap_of_two_is_within_tolerance() {
    let world = World::new();
    let net = Net::new();
    net.search(ARTIST, ALBUM, &search_body(vec![album_hit(100)]));
    let seven: Vec<TrackSpec<'_>> = vec![
        ("Tide Glass", Some(150_000), RECORDINGS[0]),
        ("Salt Ladder", Some(150_000), RECORDINGS[1]),
        ("Low Lanterns", Some(150_000), RECORDINGS[2]),
        ("Gull Static", Some(150_000), RECORDINGS[3]),
        ("Harbour Paper", Some(150_000), RECORDINGS[4]),
        ("Quay Lights", Some(150_000), BONUS_RECORDINGS[0]),
        ("Ropewalk", Some(120_000), BONUS_RECORDINGS[1]),
    ];
    net.browse(
        RG_ALBUM,
        &browse_body(vec![release(
            REL_ALBUM_DELUXE,
            ALBUM,
            Some("1987-03-02"),
            main_credit(),
            &[&seven],
        )]),
    );
    net.cover_ok(RG_ALBUM);
    let r = resolve_once(&world, &net, &NoPrints).resolution;
    assert_eq!(anchor(&r), Some((RG_ALBUM, "musicbrainz", 1.0)));
}

/// The duration check: a 5-track release lasting 1050.0 s against the
/// album's 1000.0 s is 5 % off, beyond 3 %: rejected, with the closest
/// relative difference to one decimal.
#[test]
fn name_search_hit_with_another_duration_is_rejected() {
    let world = World::new();
    let net = Net::new();
    net.search(ARTIST, ALBUM, &search_body(vec![album_hit(100)]));
    // Each length is the FLAC's own plus 10 s: 1000 + 5 * 10 = 1050 s.
    let long: Vec<TrackSpec<'_>> = TRACKS
        .iter()
        .zip(RECORDINGS)
        .map(|((_, _, d, t, _), rec)| (*t, Some((d * 1000.0).round() as i64 + 10_000), rec))
        .collect();
    net.browse(
        RG_ALBUM,
        &browse_body(vec![release(
            REL_ALBUM,
            ALBUM,
            Some("1987-03-02"),
            main_credit(),
            &[&long],
        )]),
    );
    let r = resolve_once(&world, &net, &NoPrints).resolution;
    let want = format!(
        "Tier 0b: rejected \"{ARTIST} \u{2014} {ALBUM}\" (score 100/100, MBID {RG_ALBUM}): \
         no release within 3% of the album's total duration (closest 5.0%)"
    );
    assert!(has_note(&r, &want), "{:?}", r.notes);
    assert!(r.anchor.is_none());
}

/// The duration tolerance is inclusive: exactly 3 % (1030.0 s) passes.
#[test]
fn duration_difference_of_exactly_three_percent_is_within_tolerance() {
    let world = World::new();
    let net = Net::new();
    net.search(ARTIST, ALBUM, &search_body(vec![album_hit(100)]));
    let long: Vec<TrackSpec<'_>> = TRACKS
        .iter()
        .zip(RECORDINGS)
        .map(|((_, _, d, t, _), rec)| (*t, Some((d * 1000.0).round() as i64 + 6_000), rec))
        .collect();
    net.browse(
        RG_ALBUM,
        &browse_body(vec![release(
            REL_ALBUM,
            ALBUM,
            Some("1987-03-02"),
            main_credit(),
            &[&long],
        )]),
    );
    net.cover_ok(RG_ALBUM);
    let r = resolve_once(&world, &net, &NoPrints).resolution;
    assert_eq!(anchor(&r), Some((RG_ALBUM, "musicbrainz", 1.0)));
}

/// A failed search is traced with its failure and the cascade continues.
#[test]
fn failed_search_is_traced_and_the_cascade_continues() {
    let world = World::new();
    let net = Net::new();
    net.status(
        &musaic_net::MusicBrainzClient::<musaic_net::ReplayTransport>::search_request(
            ARTIST, ALBUM,
        ),
        503,
    );
    let r = resolve_once(&world, &net, &NoPrints).resolution;
    assert_eq!(
        notes(&r),
        [
            "Tier 0b: MusicBrainz search failed (HTTP 503)",
            UNANCHORED_NOTE
        ]
    );

    // No answer at all is a network error.
    let r = resolve_once(&world, &Net::new(), &NoPrints).resolution;
    assert!(
        r.notes[0].starts_with("Tier 0b: MusicBrainz search failed (network error: "),
        "{:?}",
        r.notes
    );
}

/// No artist anywhere (directory, ALBUMARTIST, ARTIST): no search is made.
#[test]
fn no_search_without_an_artist() {
    let mut album = album_record();
    album.dir_artist = None;
    album.album_artist = None;
    album.artist = None;
    let world = World::with(album, album_tracks());
    let net = Net::new();
    let r = resolve_once(&world, &net, &NoPrints).resolution;
    assert!(net.log().is_empty());
    assert_eq!(notes(&r), [UNANCHORED_NOTE]);
}

/// A compilation is searched as "Various Artists", whatever the directory
/// says.
#[test]
fn compilation_is_searched_as_various_artists() {
    let mut album = album_record();
    album.is_compilation = true;
    let world = World::with(album, album_tracks());
    let net = Net::new();
    net.search("Various Artists", ALBUM, &search_body(vec![]));
    let r = resolve_once(&world, &net, &NoPrints).resolution;
    // The search was answered (no failure note), with no hit.
    assert_eq!(notes(&r), [UNANCHORED_NOTE]);
    assert_eq!(count(&net, &["Various+Artists"]), 1, "{:?}", net.log());
}

/// No `first-release-date` on the hit: the year is the earliest four-digit
/// `date` of the group's releases (one extra listing without `inc`);
/// undated releases are ignored.
#[test]
fn year_falls_back_to_the_earliest_listed_release() {
    let world = World::new();
    let net = Net::new();
    let hit = search_hit(RG_ALBUM, json!(100), ALBUM, main_credit(), None, &[]);
    net.search(ARTIST, ALBUM, &search_body(vec![hit]));
    net.release_dates(
        RG_ALBUM,
        &json!({"release-count": 3, "release-offset": 0, "releases": [
            {"id": REL_ALBUM, "date": "1991-05-01"},
            {"id": REL_ALBUM_DELUXE, "date": "1987-03-02"},
            {"id": REL_OTHER, "date": ""},
        ]}),
    );
    net.browse(RG_ALBUM, &album_listing());
    net.cover_ok(RG_ALBUM);
    let r = resolve_once(&world, &net, &NoPrints).resolution;
    assert_eq!(fact(&r.canonical_year), Some(("1987", "musicbrainz", 0.95)));
    // The hit has no tags: no genre.
    assert!(r.canonical_genre.is_none());
}

/// The search answer's score may be a string; it reads as the number.
#[test]
fn string_scores_are_read_as_numbers() {
    let world = World::new();
    let net = Net::new();
    let hit = search_hit(
        RG_ALBUM,
        json!("91"),
        ALBUM,
        main_credit(),
        Some("1987"),
        &[],
    );
    net.search(ARTIST, ALBUM, &search_body(vec![hit]));
    net.browse(RG_ALBUM, &album_listing());
    net.cover_ok(RG_ALBUM);
    let r = resolve_once(&world, &net, &NoPrints).resolution;
    assert_eq!(anchor(&r), Some((RG_ALBUM, "musicbrainz", 0.91)));
}

/// Two hits: the studio album (score 80) first, a live album (75) second.
/// Neither is confident, so nothing is persisted and Tier 0b runs on every
/// resolve.
fn hint_world() -> (World, Net) {
    let net = Net::new();
    let live = format!("{ALBUM} (Live at the Quay)");
    net.search(
        ARTIST,
        ALBUM,
        &search_body(vec![
            album_hit(80),
            search_hit(RG_LIVE, json!(75), &live, main_credit(), Some("1989"), &[]),
        ]),
    );
    album_tier_1(&net);
    net.browse(
        RG_LIVE,
        &browse_body(vec![release(
            REL_LIVE,
            &live,
            Some("1989-01-01"),
            main_credit(),
            &[&album_medium()],
        )]),
    );
    net.cover_missing(RG_LIVE);
    (World::new(), net)
}

/// Hint biasing: among the hits whose `title + " " + credit names`
/// (lower-cased) contains the stripped, lower-cased hint, the highest score
/// wins; with no such hit the first hit wins. The trace names the hint as
/// given. A hinted resolve never reads the memo, and its answer replaces the
/// memo entry, so a later plain read returns what was last asked.
#[test]
fn hint_reranks_the_search_and_bypasses_the_memo() {
    let (world, net) = hint_world();
    let resolver = Resolver::new();

    let plain = resolve(&resolver, &world, &net, &NoPrints, "");
    assert_eq!(anchor(&plain.resolution).unwrap().0, RG_ALBUM);

    let hinted = resolve(&resolver, &world, &net, &NoPrints, " LIVE ");
    assert!(!hinted.from_memo);
    assert_eq!(
        anchor(&hinted.resolution),
        Some((RG_LIVE, "musicbrainz", 0.75))
    );
    assert!(has_note(
        &hinted.resolution,
        "Hint applied to dir-search reranking: ' LIVE '"
    ));
    // No cover for the live album: no cover URL.
    assert!(hinted.resolution.cover_url.is_none());

    let again = resolve(&resolver, &world, &net, &NoPrints, "");
    assert!(again.from_memo);
    assert!(Arc::ptr_eq(&again.resolution, &hinted.resolution));

    // A hint no hit contains: the first hit, and the note still names it.
    let unmatched = resolve(&resolver, &world, &net, &NoPrints, "acoustic");
    assert_eq!(anchor(&unmatched.resolution).unwrap().0, RG_ALBUM);
    assert!(has_note(
        &unmatched.resolution,
        "Hint applied to dir-search reranking: 'acoustic'"
    ));
    assert!(world.writes().is_empty());
}

/// A hint that is only whitespace is no hint: the memo answers.
#[test]
fn blank_hint_reads_the_memo() {
    let (world, net) = hint_world();
    let resolver = Resolver::new();
    let first = resolve(&resolver, &world, &net, &NoPrints, "");
    let blank = resolve(&resolver, &world, &net, &NoPrints, "   ");
    assert!(blank.from_memo);
    assert!(Arc::ptr_eq(&first.resolution, &blank.resolution));
}

// ---------------------------------------------------------------------------
// Tier 0c: fingerprint vote.

/// The search finds nothing, so Tier 0c decides.
fn vote_net() -> Net {
    let net = Net::new();
    net.search(ARTIST, ALBUM, &search_body(vec![]));
    net
}

/// Track `i`'s answer: one result (score `score`) whose recording sits on
/// every listed group.
fn on(i: usize, score: f64, groups: Vec<Value>) -> Value {
    ac_body(vec![ac_result(
        ACOUSTID_IDS[i],
        score,
        &[(RECORDINGS[i], groups)],
    )])
}

fn empty_answer() -> Value {
    ac_body(vec![])
}

/// The studio album as AcoustID describes it: one 5-track release.
fn album_ac_group() -> Value {
    ac_group(RG_ALBUM, ALBUM, &[(REL_ALBUM, 5, &[5])])
}

/// Serve `answers[i]` for the i-th file's fingerprint.
fn serve_prints(net: &Net, answers: Vec<Value>) {
    for ((_, fp, dur), body) in album_prints().into_iter().zip(answers) {
        net.acoustid(fp, dur, &body);
    }
}

/// Every track on the studio album: accepted; score = max(88, int(5/5 *
/// 100)) = 100; source `fingerprint+mb`; the anchor is written with no
/// release, then Tier 1 backfills the release it aligned against. The trace
/// line gives on-tracks, the title, matched tracks, the track-count gap and
/// the number of tied candidates.
#[test]
fn every_track_on_one_release_group_anchors_by_fingerprint() {
    let world = World::new();
    let net = vote_net();
    serve_prints(
        &net,
        (0..5)
            .map(|i| on(i, 0.95, vec![album_ac_group()]))
            .collect(),
    );
    album_tier_1(&net);
    let prints = SynthPrints::new(&net, &album_prints());
    let resolved = resolve_once(&world, &net, &prints);
    let r = &resolved.resolution;

    assert_eq!(anchor(r), Some((RG_ALBUM, "fingerprint+mb", 1.0)));
    assert_eq!(r.anchor_strategy, "fingerprint-vote");
    let want = format!(
        "Tier 0c: 5/5 tracks on \"{ALBUM}\" ({RG_ALBUM}); 5 of 5 matched by AcoustID; \
         track-count gap 0; 1 tied"
    );
    assert!(has_note(r, &want), "{:?}", r.notes);
    assert_eq!(
        world.writes(),
        [
            (RG_ALBUM.to_owned(), None, 100),
            (RG_ALBUM.to_owned(), Some(REL_ALBUM.to_owned()), 100),
        ]
    );
    assert!(resolved.events.iter().any(|e| e.source == "fingerprint+mb"));
    // Not anchored by a search: the album fields come from the group lookup.
    assert_eq!(count(&net, &[&format!("release-group/{RG_ALBUM}?")]), 1);
    assert_eq!(fact(&r.canonical_title), Some((ALBUM, "musicbrainz", 0.95)));
    assert_eq!(numbers(r), one_to_five());
}

/// Coverage exactly 60 % (3 of 5) is accepted, and the anchor score has a
/// floor of 88: max(88, int(0.6 * 100)) = 88.
#[test]
fn three_of_five_tracks_anchor_at_the_floor_score() {
    let world = World::new();
    let net = vote_net();
    let mut answers: Vec<Value> = (0..3).map(|i| on(i, 0.9, vec![album_ac_group()])).collect();
    answers.extend([empty_answer(), empty_answer()]);
    serve_prints(&net, answers);
    album_tier_1(&net);
    let r = resolve_once(&world, &net, &SynthPrints::new(&net, &album_prints())).resolution;
    assert_eq!(anchor(&r), Some((RG_ALBUM, "fingerprint+mb", 0.88)));
    let want = format!(
        "Tier 0c: 3/5 tracks on \"{ALBUM}\" ({RG_ALBUM}); 3 of 5 matched by AcoustID; \
         track-count gap 0; 1 tied"
    );
    assert!(has_note(&r, &want), "{:?}", r.notes);
    assert_eq!(world.writes()[0], (RG_ALBUM.to_owned(), None, 88));
}

/// 2 of 5 (40 %) is below the 60 % coverage: no anchor, the reason in the
/// trace after the winner's numbers; nothing written.
#[test]
fn two_of_five_tracks_are_too_few() {
    let world = World::new();
    let net = vote_net();
    let mut answers: Vec<Value> = (0..2).map(|i| on(i, 0.9, vec![album_ac_group()])).collect();
    answers.extend([empty_answer(), empty_answer(), empty_answer()]);
    serve_prints(&net, answers);
    let r = resolve_once(&world, &net, &SynthPrints::new(&net, &album_prints())).resolution;
    assert!(r.anchor.is_none());
    let want = format!(
        "Tier 0c: no anchor: 2/5 tracks on \"{ALBUM}\" ({RG_ALBUM}); 2 of 5 matched by \
         AcoustID; track-count gap 0; 1 tied: coverage below 0.6"
    );
    assert!(has_note(&r, &want), "{:?}", r.notes);
    assert!(world.writes().is_empty());
    assert_eq!(r.unresolved_track_count, 5);
}

/// Results scoring below 0.5 are ignored: three tracks whose only result
/// scores 0.45 neither count on the group nor as matched, leaving 2 of 5.
#[test]
fn low_scoring_results_do_not_count() {
    let world = World::new();
    let net = vote_net();
    let answers: Vec<Value> = (0..5)
        .map(|i| on(i, if i < 2 { 0.9 } else { 0.45 }, vec![album_ac_group()]))
        .collect();
    serve_prints(&net, answers);
    let r = resolve_once(&world, &net, &SynthPrints::new(&net, &album_prints())).resolution;
    assert!(r.anchor.is_none());
    assert!(
        r.notes
            .iter()
            .any(|n| n.contains("2/5 tracks on") && n.contains("2 of 5 matched by AcoustID")),
        "{:?}",
        r.notes
    );
}

/// A failed lookup still counts in the album's track count (it dilutes the
/// coverage) and is traced with the count of failures and the first error:
/// 3 matched + 2 failed (HTTP 503) = 3/5, accepted; 2 matched + 3 failed =
/// 2/5, rejected.
#[test]
fn failed_lookups_dilute_the_coverage_and_are_traced() {
    let fail = |net: &Net, i: usize| {
        let (_, fp, dur) = album_prints()[i];
        net.status(
            &musaic_net::AcoustIdClient::<musaic_net::ReplayTransport>::lookup_request(
                "any", fp, dur,
            ),
            503,
        );
    };

    let world = World::new();
    let net = vote_net();
    serve_prints(
        &net,
        (0..3).map(|i| on(i, 0.9, vec![album_ac_group()])).collect(),
    );
    fail(&net, 3);
    fail(&net, 4);
    album_tier_1(&net);
    let r = resolve_once(&world, &net, &SynthPrints::new(&net, &album_prints())).resolution;
    assert!(has_note(
        &r,
        "Tier 0c: AcoustID lookup failed for 2 of 5 tracks (HTTP 503)"
    ));
    assert_eq!(anchor(&r), Some((RG_ALBUM, "fingerprint+mb", 0.88)));

    let world = World::new();
    let net = vote_net();
    serve_prints(
        &net,
        (0..2).map(|i| on(i, 0.9, vec![album_ac_group()])).collect(),
    );
    for i in 2..5 {
        fail(&net, i);
    }
    let r = resolve_once(&world, &net, &SynthPrints::new(&net, &album_prints())).resolution;
    assert!(has_note(
        &r,
        "Tier 0c: AcoustID lookup failed for 3 of 5 tracks (HTTP 503)"
    ));
    assert!(r.anchor.is_none());
}

/// A one-track album never anchors by fingerprint: 1/1 is full coverage and
/// gap 0, but the winner must carry at least 2 tracks.
#[test]
fn single_track_album_needs_two_tracks_on_the_winner() {
    let mut album = album_record();
    album.track_count = Some(1);
    album.total_seconds = Some(TRACKS[0].2);
    let world = World::with(album, vec![flac(TRACKS[0].0, "1", TRACKS[0].2)]);
    let net = vote_net();
    let single = ac_group(RG_FIFTH, "Tide Glass", &[(REL_FIFTH, 1, &[1])]);
    let (_, fp, dur) = album_prints()[0];
    net.acoustid(fp, dur, &on(0, 0.98, vec![single]));
    let prints = SynthPrints::new(&net, &album_prints()[..1]);
    let r = resolve_once(&world, &net, &prints).resolution;
    assert!(r.anchor.is_none());
    let want = format!(
        "Tier 0c: no anchor: 1/1 tracks on \"Tide Glass\" ({RG_FIFTH}); 1 of 1 matched by \
         AcoustID; track-count gap 0; 1 tied: fewer than 2 tracks on the winner"
    );
    assert!(has_note(&r, &want), "{:?}", r.notes);
}

/// The winner needs a release within 2 tracks of the album: every track on
/// a group whose only release has 9 tracks (gap 4) is rejected.
#[test]
fn winner_without_a_release_of_about_the_right_size_is_rejected() {
    let world = World::new();
    let net = vote_net();
    let nine = ac_group(RG_OTHER, "Night Ferries", &[(REL_OTHER, 9, &[9])]);
    serve_prints(
        &net,
        (0..5).map(|i| on(i, 0.9, vec![nine.clone()])).collect(),
    );
    let r = resolve_once(&world, &net, &SynthPrints::new(&net, &album_prints())).resolution;
    assert!(r.anchor.is_none());
    let want = format!(
        "Tier 0c: no anchor: 5/5 tracks on \"Night Ferries\" ({RG_OTHER}); 5 of 5 matched by \
         AcoustID; track-count gap 4; 1 tied: no release within 2 tracks of the album"
    );
    assert!(has_note(&r, &want), "{:?}", r.notes);
}

/// A "(disc N of M)" directory is compared with per-disc counts as well as
/// whole releases; any other directory with whole releases only. Three
/// files on a 7-track release of discs 4 + 3: gap 0 for "(disc 2 of 2)",
/// gap 4 for a plain directory.
#[test]
fn disc_directories_compare_disc_track_counts() {
    let double = ac_group(RG_DOUBLE, ALBUM, &[(REL_DOUBLE, 7, &[4, 3])]);
    let prints3 = &album_prints()[..3];
    for (dir, want_anchor) in [
        ("Lantern Coast - Paper Harbours (disc 2 of 2) [FLAC]", true),
        ("Lantern Coast - Paper Harbours [FLAC]", false),
    ] {
        let mut album = album_record();
        album.dir_name = Some(dir.to_owned());
        album.track_count = Some(3);
        album.total_seconds = Some(553.7);
        let tracks = album_tracks()[..3].to_vec();
        let world = World::with(album, tracks);
        let net = vote_net();
        for (i, (_, fp, dur)) in prints3.iter().enumerate() {
            net.acoustid(fp, *dur, &on(i, 0.9, vec![double.clone()]));
        }
        let r = resolve_once(&world, &net, &SynthPrints::new(&net, prints3)).resolution;
        assert_eq!(
            anchor(&r).map(|a| a.0),
            want_anchor.then_some(RG_DOUBLE),
            "{dir}: {:?}",
            r.notes
        );
        if !want_anchor {
            assert!(
                r.notes.iter().any(|n| n.contains("track-count gap 4")),
                "{:?}",
                r.notes
            );
        }
    }
}

/// Tie-break 1, the closest track count: every track sits on a 20-track
/// compilation (listed first) and on the 5-track studio album; both carry 5
/// tracks, the album's gap is 0 against 15. Two candidates tie on tracks.
#[test]
fn tie_on_tracks_goes_to_the_closest_track_count() {
    let world = World::new();
    let net = vote_net();
    let comp = ac_group(
        RG_COMPILATION,
        "Harbour Lights: Collected",
        &[(REL_COMPILATION, 20, &[20])],
    );
    serve_prints(
        &net,
        (0..5)
            .map(|i| on(i, 0.9, vec![comp.clone(), album_ac_group()]))
            .collect(),
    );
    album_tier_1(&net);
    let r = resolve_once(&world, &net, &SynthPrints::new(&net, &album_prints())).resolution;
    assert_eq!(anchor(&r).map(|a| a.0), Some(RG_ALBUM));
    assert!(
        r.notes
            .iter()
            .any(|n| n.ends_with("track-count gap 0; 2 tied")),
        "{:?}",
        r.notes
    );
}

/// Tie-break 2, title similarity to the album's own title: two 5-track
/// groups, "Night Ferries" listed first, "Paper Harbours" (the album's
/// directory title) second; the second wins.
#[test]
fn tie_on_count_goes_to_the_closest_title() {
    let world = World::new();
    let net = vote_net();
    let other = ac_group(RG_OTHER, "Night Ferries", &[(REL_OTHER, 5, &[5])]);
    serve_prints(
        &net,
        (0..5)
            .map(|i| on(i, 0.9, vec![other.clone(), album_ac_group()]))
            .collect(),
    );
    album_tier_1(&net);
    let r = resolve_once(&world, &net, &SynthPrints::new(&net, &album_prints())).resolution;
    assert_eq!(anchor(&r).map(|a| a.0), Some(RG_ALBUM));
}

/// Tie-break 3, the summed best score: two groups with the same title and
/// count; each track's recording on the fifth group scores 0.9, on the
/// studio album 0.95 (sums 4.5 against 4.75); the studio album wins
/// although the other is discovered first.
#[test]
fn tie_on_title_goes_to_the_higher_summed_score() {
    let world = World::new();
    let net = vote_net();
    let fifth = ac_group(RG_FIFTH, ALBUM, &[(REL_FIFTH, 5, &[5])]);
    let answers = (0..5)
        .map(|i| {
            ac_body(vec![
                ac_result(
                    ACOUSTID_IDS[i],
                    0.9,
                    &[(RECORDINGS[i], vec![fifth.clone()])],
                ),
                ac_result(
                    BONUS_RECORDINGS[0],
                    0.95,
                    &[(RECORDINGS[i], vec![album_ac_group()])],
                ),
            ])
        })
        .collect();
    serve_prints(&net, answers);
    album_tier_1(&net);
    let r = resolve_once(&world, &net, &SynthPrints::new(&net, &album_prints())).resolution;
    assert_eq!(anchor(&r).map(|a| a.0), Some(RG_ALBUM));
}

/// A complete tie keeps the first candidate in discovery order; a
/// blocklisted candidate is skipped (with the blocklist note), so the other
/// wins.
#[test]
fn full_tie_keeps_the_first_and_the_blocklist_skips_it() {
    let fifth = ac_group(RG_FIFTH, ALBUM, &[(REL_FIFTH, 5, &[5])]);
    let answers = || -> Vec<Value> {
        (0..5)
            .map(|i| on(i, 0.9, vec![fifth.clone(), album_ac_group()]))
            .collect()
    };

    let world = World::new();
    let net = vote_net();
    serve_prints(&net, answers());
    let r = resolve_once(&world, &net, &SynthPrints::new(&net, &album_prints())).resolution;
    assert_eq!(anchor(&r).map(|a| a.0), Some(RG_FIFTH));

    let world = World::new().block(RG_FIFTH);
    let net = vote_net();
    serve_prints(&net, answers());
    album_tier_1(&net);
    let r = resolve_once(&world, &net, &SynthPrints::new(&net, &album_prints())).resolution;
    assert_eq!(anchor(&r).map(|a| a.0), Some(RG_ALBUM));
    assert!(has_note(
        &r,
        "Blocklist active (1 release-group(s) rejected via Wrong match)"
    ));
    // One non-blocklisted candidate carries 5 tracks.
    assert!(
        r.notes.iter().any(|n| n.ends_with("; 1 tied")),
        "{:?}",
        r.notes
    );
}

/// No result anywhere: no candidate release-group.
#[test]
fn no_recordings_means_no_candidate() {
    let world = World::new();
    let net = vote_net();
    serve_prints(&net, (0..5).map(|_| empty_answer()).collect());
    let r = resolve_once(&world, &net, &SynthPrints::new(&net, &album_prints())).resolution;
    assert_eq!(
        notes(&r),
        [
            "Tier 0c: no anchor: no candidate release-group",
            UNANCHORED_NOTE
        ]
    );
}

// ---------------------------------------------------------------------------
// Tier 1: refine from the anchor.

/// A fresh cached anchor with the given Tier 1 answers.
fn cached(release: Option<&str>) -> World {
    World::new().stored(RG_ALBUM, release, 93, FRESH)
}

/// Alignment fallback by unique duration: the files tagged 4 and 3 hold the
/// third and fourth recordings (tags swapped). The tagged position differs
/// by more than 2 s, so each takes the only untaken MusicBrainz track within
/// 2 s of its duration; a file with no TRACKNUMBER does the same.
#[test]
fn unique_duration_fallback_fixes_swapped_and_missing_track_numbers() {
    let mut tracks = album_tracks();
    tracks[1] = flac(TRACKS[1].0, "", TRACKS[1].2);
    tracks[2] = flac(TRACKS[2].0, "4", TRACKS[2].2);
    tracks[3] = flac(TRACKS[3].0, "3", TRACKS[3].2);
    let world = World::with(album_record(), tracks).stored(RG_ALBUM, Some(REL_ALBUM), 93, FRESH);
    let net = Net::new();
    album_tier_1(&net);
    let r = resolve_once(&world, &net, &NoPrints).resolution;
    assert_eq!(numbers(&r), one_to_five());
    assert_eq!(titles(&r), mb_titles());
    assert!(
        r.tracks
            .iter()
            .all(|t| t.canonical_title.as_ref().unwrap().confidence == 0.95)
    );
    assert_eq!(r.unresolved_track_count, 0);
}

/// A file more than 2 s from every MusicBrainz track stays unaligned; the
/// positional fallback is refused because that file is 27.6 s off (beyond
/// 10 s), and the trace says which track and by how much.
#[test]
fn track_far_from_every_length_stays_unresolved() {
    let mut tracks = album_tracks();
    tracks[4] = flac(TRACKS[4].0, "5", 229.6);
    let world = World::with(album_record(), tracks).stored(RG_ALBUM, Some(REL_ALBUM), 93, FRESH);
    let net = Net::new();
    album_tier_1(&net);
    let r = resolve_once(&world, &net, &NoPrints).resolution;
    assert_eq!(r.unresolved_track_count, 1);
    let mut want = one_to_five();
    want[4] = None;
    assert_eq!(numbers(&r), want);
    // 229.6 - 202.0 = +27.6 s.
    assert!(
        r.notes
            .iter()
            .any(|n| n.ends_with("track 5 differs by +27.6 s, beyond 10 s")),
        "{:?}",
        r.notes
    );
}

/// The positional fallback: every file is 4.0-6.0 s longer than its
/// MusicBrainz track (a pressing that runs uniformly long), so neither the
/// 2 s pass nor the unique-duration fallback aligns anything; counts match,
/// every difference is within 10 s and all point the same way, so all five
/// align by position at confidence 0.8.
#[test]
fn uniformly_long_pressing_aligns_by_position() {
    let offsets = [4.0, 5.0, 6.0, 4.5, 5.5];
    let tracks: Vec<FlacTrack> = TRACKS
        .iter()
        .zip(offsets)
        .map(|((f, n, _, _, ms), o)| flac(f, n, *ms as f64 / 1000.0 + o))
        .collect();
    let world = World::with(album_record(), tracks).stored(RG_ALBUM, Some(REL_ALBUM), 93, FRESH);
    let net = Net::new();
    album_tier_1(&net);
    let r = resolve_once(&world, &net, &NoPrints).resolution;
    assert_eq!(numbers(&r), one_to_five());
    assert_eq!(titles(&r), mb_titles());
    assert!(
        r.tracks
            .iter()
            .all(|t| t.canonical_title.as_ref().unwrap().confidence == 0.8)
    );
    assert_eq!(r.unresolved_track_count, 0);
    assert!(
        r.notes.iter().any(
            |n| n.starts_with("Tier 1: 5 of 5 tracks aligned by position")
                && n.ends_with(
                    "track counts match and every length is within 10 s (+4.0 to +6.0 s); \
                 0 agreed with the duration pass, 0 replaced"
                )
        ),
        "{:?}",
        r.notes
    );
}

/// Differences beyond 2 s that point both ways (+5, -5, +5, -5, +5) refuse
/// the positional fallback: nothing aligns.
#[test]
fn mixed_direction_differences_refuse_the_positional_fallback() {
    let offsets = [5.0, -5.0, 5.0, -5.0, 5.0];
    let tracks: Vec<FlacTrack> = TRACKS
        .iter()
        .zip(offsets)
        .map(|((f, n, _, _, ms), o)| flac(f, n, *ms as f64 / 1000.0 + o))
        .collect();
    let world = World::with(album_record(), tracks).stored(RG_ALBUM, Some(REL_ALBUM), 93, FRESH);
    let net = Net::new();
    album_tier_1(&net);
    let r = resolve_once(&world, &net, &NoPrints).resolution;
    assert_eq!(r.unresolved_track_count, 5);
    assert!(
        r.notes
            .iter()
            .any(|n| n.ends_with("lengths differ in both directions (-5.0 to +5.0 s)")),
        "{:?}",
        r.notes
    );
}

/// A "(disc 2 of 2)" folder is compared positionally with medium 2 of the
/// chosen release only, and its files are numbered within that medium.
#[test]
fn disc_folder_aligns_by_position_against_its_medium() {
    let disc1: Vec<TrackSpec<'_>> = vec![
        ("Seawall Hymn", Some(150_000), DOUBLE_RECORDINGS[0]),
        ("Pilot Light", Some(160_000), DOUBLE_RECORDINGS[1]),
        ("Undertow", Some(170_000), DOUBLE_RECORDINGS[2]),
        ("Signal Flags", Some(180_000), DOUBLE_RECORDINGS[3]),
    ];
    let disc2: Vec<TrackSpec<'_>> = vec![
        ("Harbour Master", Some(190_000), DOUBLE_RECORDINGS[4]),
        ("Slipway", Some(210_000), DOUBLE_RECORDINGS[5]),
        ("Lamp Room", Some(230_000), DOUBLE_RECORDINGS[6]),
    ];
    let mut album = album_record();
    album.dir_name = Some("Lantern Coast - Paper Harbours (disc 2 of 2) [FLAC]".to_owned());
    album.track_count = Some(3);
    album.total_seconds = Some(639.5);
    album.disc_number = Some(2);
    album.disc_total = Some(2);
    // 3.0, 3.5 and 3.0 s longer than disc 2's tracks; far from disc 1's.
    let tracks = vec![
        flac("01 - Harbour Master.flac", "1", 193.0),
        flac("02 - Slipway.flac", "2", 213.5),
        flac("03 - Lamp Room.flac", "3", 233.0),
    ];
    let world = World::with(album, tracks).stored(RG_DOUBLE, Some(REL_DOUBLE), 95, FRESH);
    let net = Net::new();
    net.browse(
        RG_DOUBLE,
        &browse_body(vec![release(
            REL_DOUBLE,
            ALBUM,
            Some("1993-09-09"),
            main_credit(),
            &[&disc1, &disc2],
        )]),
    );
    net.cover_missing(RG_DOUBLE);
    net.group(
        RG_DOUBLE,
        &group_body(RG_DOUBLE, ALBUM, main_credit(), "1993-09-09", &[]),
    );
    let r = resolve_once(&world, &net, &NoPrints).resolution;
    assert_eq!(
        numbers(&r),
        [Some("1".into()), Some("2".into()), Some("3".into())]
    );
    assert_eq!(
        titles(&r),
        [
            Some("Harbour Master".into()),
            Some("Slipway".into()),
            Some("Lamp Room".into())
        ]
    );
    assert!(
        r.notes.iter().any(|n| n
            .starts_with("Tier 1: 3 of 3 tracks aligned by position against medium 2")
            && n.contains("(+3.0 to +3.5 s)")),
        "{:?}",
        r.notes
    );
    // A 404 from the Cover Art Archive: no cover offered.
    assert!(r.cover_url.is_none());
}

/// The release aligned against is the best fit, not the earliest: an
/// earlier 7-track deluxe release loses to the 5-track one on the
/// track-count gap (2 against 0). A fresh anchor with no stored release
/// gets that release backfilled, keeping its score.
#[test]
fn best_fitting_release_is_chosen_and_backfilled() {
    let mut deluxe = album_medium();
    deluxe.extend([
        ("Quay Lights", Some(150_000), BONUS_RECORDINGS[0]),
        ("Ropewalk", Some(120_000), BONUS_RECORDINGS[1]),
    ]);
    let net = Net::new();
    net.browse(
        RG_ALBUM,
        &browse_body(vec![
            release(
                REL_ALBUM_DELUXE,
                &format!("{ALBUM} (Expanded Edition)"),
                Some("1987-01-01"),
                main_credit(),
                &[&deluxe],
            ),
            release(
                REL_ALBUM,
                ALBUM,
                Some("1990-06-01"),
                main_credit(),
                &[&album_medium()],
            ),
        ]),
    );
    net.cover_ok(RG_ALBUM);
    net.group(RG_ALBUM, &album_group());
    let world = cached(None);
    let r = resolve_once(&world, &net, &NoPrints).resolution;
    assert_eq!(
        world.writes(),
        [(RG_ALBUM.to_owned(), Some(REL_ALBUM.to_owned()), 93)]
    );
    assert_eq!(numbers(&r), one_to_five());
}

/// Between releases of the same size, the smaller summed length difference
/// wins over an earlier date: the earlier release's tracks are each 1.5 s
/// longer (still aligned within 2 s), the later one matches the album.
#[test]
fn closer_lengths_beat_an_earlier_date() {
    let net = Net::new();
    net.browse(
        RG_ALBUM,
        &browse_body(vec![
            release(
                REL_ALBUM_DELUXE,
                ALBUM,
                Some("1987-01-01"),
                main_credit(),
                &[&shifted_medium([1500; 5])],
            ),
            release(
                REL_ALBUM,
                ALBUM,
                Some("1990-06-01"),
                main_credit(),
                &[&album_medium()],
            ),
        ]),
    );
    net.cover_ok(RG_ALBUM);
    net.group(RG_ALBUM, &album_group());
    let world = cached(Some(REL_ALBUM_DELUXE));
    resolve_once(&world, &net, &NoPrints);
    // Fresh anchor, the chosen release differs from the stored one:
    // backfilled.
    assert_eq!(
        world.writes(),
        [(RG_ALBUM.to_owned(), Some(REL_ALBUM.to_owned()), 93)]
    );
}

/// A stale stored anchor is refreshed by its own release-group id, never by
/// a name search: written back with the same group and the stored score,
/// and the best-fitting release (which here differs from the stored one).
#[test]
fn stale_anchor_is_refreshed_by_id_and_keeps_its_score() {
    let world = World::new().stored(RG_ALBUM, Some(REL_ALBUM_DELUXE), 91, STALE);
    let net = Net::new();
    album_tier_1(&net);
    let r = resolve_once(&world, &net, &NoPrints).resolution;
    assert_eq!(anchor(&r), Some((RG_ALBUM, "cache", 0.91)));
    assert_eq!(
        world.writes(),
        [(RG_ALBUM.to_owned(), Some(REL_ALBUM.to_owned()), 91)]
    );
    assert!(has_note(
        &r,
        &format!("Tier 1: stale anchor refreshed by release-group id ({RG_ALBUM})")
    ));
    assert_eq!(count(&net, &[SEARCH]), 0);
}

/// A stale anchor whose listing fails is kept untouched (no write, still
/// stale) with the reason; without a tracklist every file is unresolved
/// and listed with its fingerprint fields only.
#[test]
fn stale_anchor_with_a_failed_listing_is_kept() {
    let world = World::new().stored(RG_ALBUM, Some(REL_ALBUM), 91, STALE);
    let net = Net::new();
    net.status(
        &musaic_net::MusicBrainzClient::<musaic_net::ReplayTransport>::browse_request(RG_ALBUM),
        503,
    );
    net.cover_ok(RG_ALBUM);
    net.group(RG_ALBUM, &album_group());
    let r = resolve_once(&world, &net, &NoPrints).resolution;
    assert!(world.writes().is_empty());
    assert_eq!(
        world.stored_anchor().unwrap().last_checked_at.as_deref(),
        Some(STALE)
    );
    assert!(has_note(
        &r,
        "Tier 1: stale anchor kept, not refreshed: release listing unavailable (HTTP 503)"
    ));
    assert!(has_note(
        &r,
        "Tier 1: anchored but MB returned no tracklist"
    ));
    assert_eq!(r.unresolved_track_count, 5);
    assert_eq!(r.tracks.len(), 5);
    // The album fields still come from the release-group lookup.
    assert_eq!(fact(&r.canonical_title), Some((ALBUM, "musicbrainz", 0.95)));
}

/// A stale anchor whose listing names no release is kept, with that reason.
#[test]
fn stale_anchor_with_an_empty_listing_is_kept() {
    let world = World::new().stored(RG_ALBUM, Some(REL_ALBUM), 91, STALE);
    let net = Net::new();
    net.browse(RG_ALBUM, &browse_body(vec![]));
    net.cover_ok(RG_ALBUM);
    net.group(RG_ALBUM, &album_group());
    let r = resolve_once(&world, &net, &NoPrints).resolution;
    assert!(world.writes().is_empty());
    assert!(has_note(
        &r,
        &format!(
            "Tier 1: stale anchor kept, not refreshed: MusicBrainz lists no release for \
             release-group {RG_ALBUM}"
        )
    ));
    assert_eq!(r.unresolved_track_count, 5);
}

/// A fresh anchor whose listing fails: every file unresolved, nothing
/// written, and each track still carries its fingerprint fields (a row with
/// a recording, a row without, no row).
#[test]
fn fresh_anchor_without_a_tracklist_counts_every_track_unresolved() {
    let mut world = cached(Some(REL_ALBUM));
    world.fingerprints = vec![
        FingerprintRow {
            filename: TRACKS[0].0.to_owned(),
            recording_mbid: Some(RECORDINGS[0].to_owned()),
            score: Some(0.97),
        },
        FingerprintRow {
            filename: TRACKS[1].0.to_owned(),
            recording_mbid: None,
            score: None,
        },
    ];
    let net = Net::new();
    net.status(
        &musaic_net::MusicBrainzClient::<musaic_net::ReplayTransport>::browse_request(RG_ALBUM),
        500,
    );
    net.cover_ok(RG_ALBUM);
    net.group(RG_ALBUM, &album_group());
    let r = resolve_once(&world, &net, &NoPrints).resolution;
    assert!(world.writes().is_empty());
    assert_eq!(r.unresolved_track_count, 5);
    assert!(r.tracks.iter().all(|t| t.canonical_title.is_none()));
    let t = &r.tracks;
    assert_eq!(t[0].recording_mbid.as_deref(), Some(RECORDINGS[0]));
    assert_eq!(t[0].fingerprint_score, Some(0.97));
    assert!(t[0].fingerprinted);
    assert_eq!(t[1].recording_mbid, None);
    assert!(t[1].fingerprinted);
    assert!(!t[2].fingerprinted);
}

/// The release-group lookup fails: title, artist and sort-name come from
/// the chosen (best-fitting) release, the year from the earliest listed
/// release, no genre (releases carry no tags); the trace says so.
#[test]
fn failed_group_lookup_takes_album_fields_from_the_listing() {
    let guest = vec![
        credit(ARTIST, ARTIST_ID, ARTIST_SORT, " & "),
        credit("Marlow Vane", GUEST_ARTIST_ID, "Vane, Marlow", ""),
    ];
    let mut deluxe = album_medium();
    deluxe.extend([
        ("Quay Lights", Some(150_000), BONUS_RECORDINGS[0]),
        ("Ropewalk", Some(120_000), BONUS_RECORDINGS[1]),
    ]);
    let net = Net::new();
    net.browse(
        RG_ALBUM,
        &browse_body(vec![
            release(
                REL_ALBUM_DELUXE,
                "Paper Harbours (Expanded Edition)",
                Some("1987-01-01"),
                main_credit(),
                &[&deluxe],
            ),
            release(
                REL_ALBUM,
                ALBUM,
                Some("1990-06-01"),
                guest,
                &[&album_medium()],
            ),
        ]),
    );
    net.cover_ok(RG_ALBUM);
    net.status(
        &musaic_net::MusicBrainzClient::<musaic_net::ReplayTransport>::release_group_request(
            RG_ALBUM,
        ),
        404,
    );
    let world = cached(Some(REL_ALBUM));
    let r = resolve_once(&world, &net, &NoPrints).resolution;
    assert!(has_note(
        &r,
        "Tier 1: release-group lookup failed (HTTP 404); album fields taken from the release listing"
    ));
    assert_eq!(fact(&r.canonical_title), Some((ALBUM, "musicbrainz", 0.95)));
    // Credited names joined with their join phrases; sort-name of the first.
    assert_eq!(
        r.canonical_album_artist.as_ref().unwrap().value,
        "Lantern Coast & Marlow Vane"
    );
    assert_eq!(
        r.canonical_album_artist_sort.as_ref().unwrap().value,
        ARTIST_SORT
    );
    assert_eq!(r.canonical_year.as_ref().unwrap().value, "1987");
    assert!(r.canonical_genre.is_none());
}

/// Lookup and listing both fail: no album fields at all, and the trace says
/// that too.
#[test]
fn failed_group_lookup_and_listing_leave_no_album_fields() {
    let world = cached(Some(REL_ALBUM));
    let net = Net::new();
    net.cover_ok(RG_ALBUM);
    let r = resolve_once(&world, &net, &NoPrints).resolution;
    assert!(
        r.notes.iter().any(|n| n
            .starts_with("Tier 1: release-group lookup failed (network error: ")
            && n.ends_with("); no album fields")),
        "{:?}",
        r.notes
    );
    assert!(r.canonical_title.is_none());
    assert!(r.canonical_year.is_none());
    assert_eq!(r.unresolved_track_count, 5);
}

// ---------------------------------------------------------------------------
// Memoisation.

/// One resolution per (album, content): a second read is served from the
/// memo (same object, no request); a changed file list (content hash) is a
/// new question; `invalidate` for this album (not for another) and `force`
/// recompute.
#[test]
fn resolutions_are_memoised_by_album_content() {
    let world = cached(Some(REL_ALBUM));
    let net = Net::new();
    album_tier_1(&net);
    let resolver = Resolver::new();
    let first = resolve(&resolver, &world, &net, &NoPrints, "");
    assert!(!first.from_memo);
    let asked = net.log().len();

    let second = resolve(&resolver, &world, &net, &NoPrints, "");
    assert!(second.from_memo);
    assert!(Arc::ptr_eq(&first.resolution, &second.resolution));
    assert_eq!(net.log().len(), asked);

    resolver.invalidate(Some("some-other-album"));
    assert!(resolve(&resolver, &world, &net, &NoPrints, "").from_memo);

    resolver.invalidate(Some(ALBUM_ID));
    let third = resolve(&resolver, &world, &net, &NoPrints, "");
    assert!(!third.from_memo);
    assert!(net.log().len() > asked);

    let mb = SynthMb(&net);
    let env = Env {
        albums: &world,
        store: &world,
        mb: &mb,
        fingerprints: &NoPrints,
        now_epoch_s: NOW_EPOCH_S,
    };
    let forced = resolver.resolve(&env, ALBUM_ID, "", true).unwrap();
    assert!(!forced.from_memo);

    // Renaming a file changes the content hash.
    world.tracks.lock().unwrap()[0].filename = "01 - Tide Glass (remaster).flac".to_owned();
    let renamed = resolve(&resolver, &world, &net, &NoPrints, "");
    assert!(!renamed.from_memo);
    assert_eq!(
        renamed.resolution.tracks[0].filename,
        "01 - Tide Glass (remaster).flac"
    );
}

/// An anchor written by the Python app (`mb_last_checked_at` as
/// `YYYY-MM-DDTHH:MM:SSZ`, months old) looks stale exactly once: the first
/// resolve refreshes it by id and stamps it with the app's own timestamp in
/// the same format, which the staleness rule reads as fresh, so the next
/// resolves neither refresh nor write it. It turns stale again only after
/// the 90-day refresh period. Real anchor store (the library database), no
/// network (the MusicBrainz answers are invented).
#[test]
fn an_anchor_from_the_python_app_is_refreshed_once_then_stays_fresh() {
    use musaic_resolve::db_store::DbStore;

    let t = tempfile::tempdir().unwrap();
    let path = t.path().join("library.db");
    std::fs::copy(musaic_testdata::public("db/v16-empty.db"), &path).unwrap();
    let (db, _) = musaic_db::Db::open(&path).unwrap();
    let conn = db.connection();
    conn.execute(
        "INSERT INTO libraries (path, scanned_at) VALUES ('/invented/lib', '')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO albums (id, library_path, path, dir_name) VALUES (?1, '/invented/lib', ?2, ?3)",
        rusqlite::params![ALBUM_ID, format!("/invented/lib/{DIR_NAME}"), DIR_NAME],
    )
    .unwrap();
    // Written by the Python app's `_now_iso()` about four months earlier.
    let python_checked = "2026-07-01T12:34:56Z";
    musaic_db::mb_anchor::set_album_mb_anchor(
        conn,
        ALBUM_ID,
        RG_ALBUM,
        Some(REL_ALBUM),
        93,
        python_checked,
    )
    .unwrap();

    let net = Net::new();
    album_tier_1(&net);
    let world = World::new().stored(RG_ALBUM, Some(REL_ALBUM), 93, python_checked);
    let mb = SynthMb(&net);
    let resolve_at = |epoch: i64| {
        let store = DbStore {
            conn,
            now_iso: musaic_db::backup::iso_seconds(
                std::time::UNIX_EPOCH + std::time::Duration::from_secs(epoch as u64),
            ),
            read: Default::default(),
        };
        let env = Env {
            albums: &world,
            store: &store,
            mb: &mb,
            fingerprints: &NoPrints,
            now_epoch_s: epoch,
        };
        // A fresh resolver each time: no memo, every resolve reads the store.
        Resolver::new()
            .resolve(&env, ALBUM_ID, "", false)
            .unwrap()
            .resolution
    };
    let stored = || {
        musaic_db::mb_anchor::get_album_mb_anchor(conn, ALBUM_ID)
            .unwrap()
            .unwrap()
    };
    let refreshed = format!("Tier 1: stale anchor refreshed by release-group id ({RG_ALBUM})");
    let is_stale_note = |r: &AlbumResolution| r.notes.iter().any(|n| n.contains("stale anchor"));

    let first = resolve_at(NOW_EPOCH_S);
    assert!(has_note(&first, &refreshed), "{:?}", first.notes);
    let after_first = stored();
    assert_eq!(
        after_first.last_checked_at.as_deref(),
        Some("2026-11-10T00:00:00Z")
    );
    assert_eq!(after_first.release_group_mbid, RG_ALBUM);
    assert_eq!(after_first.score, Some(93));

    // Later resolves within the refresh period: no stale note, no write.
    for days in [0, 1, 30, 89] {
        let r = resolve_at(NOW_EPOCH_S + days * 86_400);
        assert!(!is_stale_note(&r), "day {days}: {:?}", r.notes);
        assert_eq!(stored(), after_first, "day {days}: not written again");
        assert_eq!(anchor(&r), Some((RG_ALBUM, "cache", 0.93)), "day {days}");
    }
    // Past 90 days it is due again, by design.
    let r = resolve_at(NOW_EPOCH_S + 91 * 86_400);
    assert!(has_note(&r, &refreshed), "{:?}", r.notes);
}
