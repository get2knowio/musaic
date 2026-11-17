//! A request cancelled while the app closes is not a network failure: the
//! callers that map `musaic-net` errors leave it out of what they remember,
//! count and log. Every transport here is an in-process stub that ends the
//! request as cancelled, as a stopped paced transport does; nothing touches
//! the network.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use musaic_db::Db;
use musaic_events::Bus;
use musaic_log::{Level, capture};
use musaic_net::{
    MusicBrainzClient, RecordingLookup, Request, Response, Transport, TransportError,
};
use musaic_resolve::Resolver;
use musaic_resolve::find_release::{FindOptions, find_release_for_fix};
use musaic_resolve::source::{AlbumRecord, MusicBrainzSource, NetError};
use musaic_services::fingerprint::Chromaprint;
use musaic_services::production::{ChecksResolution, SharedTransport};
use musaic_services::recording_canonical::{RecordingCanonicalCache, failure_reason};
use musaic_services::sources::{NetMusicBrainz, ResolveParts};
use rusqlite::params;

/// Ends every request as cancelled and counts them.
#[derive(Default)]
struct Cancelled(AtomicUsize);

impl Transport for Cancelled {
    fn send(&self, _request: &Request) -> Result<Response, TransportError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(TransportError::Stopped)
    }
}

fn quiet(lines: &[musaic_log::Record]) {
    let loud: Vec<_> = lines.iter().filter(|l| l.level != Level::Debug).collect();
    assert!(loud.is_empty(), "{loud:?}");
}

const MBID: &str = "6b3d5f0e-4a57-4c0e-9d0f-0a3b2c1d4e5f";

/// The duplicate-tracks recording lookup: a cancel has its own reason (not
/// "couldn't reach MusicBrainz"), is not kept in the cache, logs nothing
/// above DEBUG, and the next request asks again.
#[test]
fn a_cancelled_recording_lookup_has_its_own_reason_and_is_not_cached() {
    let t = Cancelled::default();
    let mb = MusicBrainzClient::new(&t);
    let cache = RecordingCanonicalCache::new();
    let (got, lines) = capture(|| cache.get_or_lookup(&mb, MBID));
    let RecordingLookup::Failed(f) = got else {
        panic!("{got:?}")
    };
    assert_eq!(f, musaic_net::LookupFailure::Cancelled);
    quiet(&lines);
    assert_eq!(cache.len(), 0);
    let _ = cache.get_or_lookup(&mb, MBID);
    assert_eq!(t.0.load(Ordering::SeqCst), 2, "asked again, not served");

    let (code, text) = failure_reason(f);
    assert_eq!(code, "cancelled");
    let lower = text.to_lowercase();
    assert!(
        !lower.contains("reach") && !lower.contains("network"),
        "{text}"
    );
}

/// The resolver's MusicBrainz source: a cancelled request is
/// `NetError::Cancelled` (no "network error" text), the resolution is
/// marked incomplete, and so is a cancelled cover check.
#[test]
fn the_musicbrainz_source_reports_a_cancel_as_cancelled_and_incomplete() {
    let mb = NetMusicBrainz::new(Arc::new(Cancelled::default()));
    let e = mb.search_release_groups("A", "B").unwrap_err();
    assert_eq!(e, NetError::Cancelled);
    assert!(!e.to_string().contains("network"), "{e}");
    assert!(mb.failed.load(Ordering::SeqCst));

    let mb = NetMusicBrainz::new(Arc::new(Cancelled::default()));
    assert_eq!(mb.cover_front_status("rg"), Err(NetError::Cancelled));
    assert!(mb.failed.load(Ordering::SeqCst));
}

fn record() -> AlbumRecord {
    AlbumRecord {
        id: "0123456789abcdef".into(),
        dir_name: Some("Test Artist - Test Album [FLAC]".into()),
        dir_artist: Some("Test Artist".into()),
        dir_album: Some("Test Album".into()),
        title: Some("Test Album".into()),
        artist: Some("Test Artist".into()),
        ..AlbumRecord::default()
    }
}

/// The name search: a cancelled search says it was cancelled, keeps the
/// failure for the caller, and logs nothing above DEBUG.
#[test]
fn a_cancelled_name_search_is_reported_without_a_warning() {
    let mb = NetMusicBrainz::new(Arc::new(Cancelled::default()));
    let opts = FindOptions {
        hint: "",
        blocked: &Default::default(),
        fetch_cover: false,
        fetch_tracklist: false,
    };
    let (m, lines) = capture(|| find_release_for_fix(&record(), opts, &mb).matched);
    quiet(&lines);
    assert_eq!(m.failure, Some(NetError::Cancelled));
    assert_eq!(m.source, "MusicBrainz (cancelled)");
    let text = format!("{} {}", m.notes, m.source).to_lowercase();
    assert!(!text.contains("network"), "{text}");
}

/// The checks' view of the resolver: an album whose lookup was cancelled
/// comes back with `network_failed` (the check then calls
/// `could_not_evaluate`, keeping its findings and recording no inputs) and
/// the resolution is not memoised, so the next call asks again.
#[test]
fn a_resolution_cut_short_by_a_cancel_is_incomplete_and_not_remembered() {
    let td = tempfile::tempdir().unwrap();
    let base = musaic_scan::py_resolve(td.path());
    let lib = base.join("library");
    std::fs::create_dir(&lib).unwrap();
    let db = base.join("library.db");
    std::fs::copy(musaic_testdata::public("db/v16-empty.db"), &db).unwrap();
    drop(Db::open(&db).unwrap());
    let conn = rusqlite::Connection::open(&db).unwrap();
    let lib_s = lib.to_string_lossy().into_owned();
    conn.execute(
        "INSERT INTO libraries (path, scanned_at) VALUES (?1, '2001-01-01T00:00:00Z')",
        [&lib_s],
    )
    .unwrap();
    let r = record();
    conn.execute(
        "INSERT INTO albums (id, library_path, path, dir_name, dir_artist, dir_album, title, artist, track_count) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 1)",
        params![
            r.id,
            lib_s,
            lib.join("album").to_string_lossy(),
            r.dir_name,
            r.dir_artist,
            r.dir_album,
            r.title,
            r.artist
        ],
    )
    .unwrap();

    let t = Arc::new(Cancelled::default());
    let parts = ResolveParts {
        resolver: Arc::new(Resolver::new()),
        transport: Arc::clone(&t) as SharedTransport,
        acoustid_key: None,
        fingerprinter: Arc::new(Chromaprint),
        bus: Bus::new(),
        now_iso: Arc::new(|| "2026-01-01T00:00:00Z".to_owned()),
        now_epoch: Arc::new(|| 1_767_225_600),
    };
    let source = ChecksResolution {
        parts: Arc::new(move || parts.clone()),
        library_dir: Arc::new(move || Some(lib_s.clone())),
    };
    use musaic_checks::resolution::ResolutionSource as _;
    let first = source.resolve(&conn, &r.id).unwrap();
    assert!(first.network_failed);
    let asked = t.0.load(Ordering::SeqCst);
    assert!(asked > 0);
    let again = source.resolve(&conn, &r.id).unwrap();
    assert!(again.network_failed);
    assert!(
        t.0.load(Ordering::SeqCst) > asked,
        "asked again, not served"
    );
}
