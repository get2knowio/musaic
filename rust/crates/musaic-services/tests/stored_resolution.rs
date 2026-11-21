//! The agent's stored-only resolution read (`stored_album_resolution`)
//! reports what the resolver memoized for the album's current content, read
//! through the production album source: never an entry memoized before the
//! album changed. The transport is an in-process stub; nothing touches the
//! network.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use musaic_db::Db;
use musaic_events::Bus;
use musaic_net::{Request, Response, Transport, TransportError};
use musaic_resolve::Resolver;
use musaic_services::fingerprint::Chromaprint;
use musaic_services::library_ops::stored_album_resolution;
use musaic_services::production::SharedTransport;
use musaic_services::sources::ResolveParts;
use rusqlite::params;

const ALBUM_ID: &str = "0123456789abcdef";

/// MusicBrainz finds nothing: a complete answer, so the resolve is memoized.
#[derive(Default)]
struct NothingFound(AtomicUsize);

impl Transport for NothingFound {
    fn send(&self, _request: &Request) -> Result<Response, TransportError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(Response {
            status: 200,
            headers: vec![("content-type".into(), "application/json".into())],
            body: br#"{"created":"2026-01-01T00:00:00.000Z","count":0,"offset":0,"release-groups":[]}"#
                .to_vec(),
        })
    }
}

#[test]
fn the_stored_read_never_reports_a_resolution_of_older_content() {
    let td = tempfile::tempdir().unwrap();
    let base = musaic_scan::py_resolve(td.path());
    let lib = base.join("library");
    let album_dir = lib.join("Test Artist - Test Album [FLAC]");
    std::fs::create_dir_all(&album_dir).unwrap();
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
    conn.execute(
        "INSERT INTO albums (id, library_path, path, dir_name, dir_artist, dir_album, title, artist, track_count) \
         VALUES (?1, ?2, ?3, 'Test Artist - Test Album [FLAC]', 'Test Artist', 'Test Album', 'Test Album', 'Test Artist', 1)",
        params![ALBUM_ID, lib_s, album_dir.to_string_lossy()],
    )
    .unwrap();

    let t = Arc::new(NothingFound::default());
    let resolver = Arc::new(Resolver::new());
    let parts = ResolveParts {
        resolver: Arc::clone(&resolver),
        transport: Arc::clone(&t) as SharedTransport,
        acoustid_key: None,
        fingerprinter: Arc::new(Chromaprint),
        bus: Bus::new(),
        now_iso: Arc::new(|| "2026-01-01T00:00:00Z".to_owned()),
        now_epoch: Arc::new(|| 1_767_225_600),
    };
    let stored = || stored_album_resolution(&conn, Some(&lib_s), &resolver, ALBUM_ID).unwrap();

    assert!(stored().is_none(), "never resolved");
    let first = parts
        .resolve(&conn, Some(&lib_s), ALBUM_ID, "", false)
        .unwrap();
    assert!(t.0.load(Ordering::SeqCst) > 0, "the resolve asked");
    let held = stored().expect("memoized");
    assert!(Arc::ptr_eq(&held, &first));

    // The album's content changes (its row now says two tracks), so its
    // content hash does; nothing invalidated the memo, which still holds
    // the first entry.
    conn.execute(
        "UPDATE albums SET track_count = 2 WHERE id = ?1",
        [ALBUM_ID],
    )
    .unwrap();
    let asked = t.0.load(Ordering::SeqCst);
    assert!(
        stored().is_none(),
        "the resolution of the old content is not reported"
    );
    assert_eq!(
        t.0.load(Ordering::SeqCst),
        asked,
        "the stored read asks nothing"
    );

    let second = parts
        .resolve(&conn, Some(&lib_s), ALBUM_ID, "", false)
        .unwrap();
    assert!(
        !Arc::ptr_eq(&second, &first),
        "a new content is a new resolve"
    );
    let held = stored().expect("memoized");
    assert!(Arc::ptr_eq(&held, &second));
}
