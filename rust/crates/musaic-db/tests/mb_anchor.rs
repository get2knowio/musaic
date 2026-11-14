//! Persistence helpers against a database the Python app wrote
//! (testdata/db/v16-synthetic-offline.db, copied first). Column semantics follow
//! musaic/cache.py; the values compared are the fixture's own rows.

use musaic_db::mb_anchor::{
    add_blocked_mb_release_group, clear_album_mb_anchor, get_album_mb_anchor,
    get_blocked_mb_release_groups, get_track_fingerprints, set_album_mb_anchor,
};
use rusqlite::Connection;

fn fixture_copy() -> (tempfile::TempDir, Connection) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("library.db");
    std::fs::copy(musaic_testdata::public("db/v16-synthetic-offline.db"), &db).unwrap();
    let conn = Connection::open(&db).unwrap();
    (dir, conn)
}

const GAPS: &str = "0c97baca60f74928"; // "Mono Harbor - Gaps [FLAC]"

// No anchor -> None; set writes all four columns; clear resets them;
// an unknown album is neither found nor updated.
#[test]
fn anchor_round_trip() {
    let (_d, conn) = fixture_copy();
    assert_eq!(get_album_mb_anchor(&conn, GAPS).unwrap(), None);
    assert!(set_album_mb_anchor(&conn, GAPS, "rg-1", None, 100, "2026-11-10T00:00:00Z").unwrap());
    let a = get_album_mb_anchor(&conn, GAPS).unwrap().unwrap();
    assert_eq!(a.release_group_mbid, "rg-1");
    assert_eq!(a.release_mbid, None);
    assert_eq!(a.score, Some(100));
    assert_eq!(a.last_checked_at.as_deref(), Some("2026-11-10T00:00:00Z"));
    assert!(
        set_album_mb_anchor(
            &conn,
            GAPS,
            "rg-1",
            Some("rel-1"),
            92,
            "2026-11-11T00:00:00Z"
        )
        .unwrap()
    );
    assert_eq!(
        get_album_mb_anchor(&conn, GAPS)
            .unwrap()
            .unwrap()
            .release_mbid
            .as_deref(),
        Some("rel-1")
    );
    assert!(clear_album_mb_anchor(&conn, GAPS).unwrap());
    assert_eq!(get_album_mb_anchor(&conn, GAPS).unwrap(), None);
    let raw: (Option<String>, Option<i64>, Option<String>) = conn
        .query_row(
            "SELECT mb_release_mbid, mb_match_score, mb_last_checked_at FROM albums WHERE id = ?1",
            [GAPS],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(raw, (None, None, None));
    assert!(!set_album_mb_anchor(&conn, "nope", "rg", None, 1, "t").unwrap());
    assert_eq!(get_album_mb_anchor(&conn, "nope").unwrap(), None);
}

// The "Wrong match" blocklist is per album, idempotent,
// ignores an empty id, and is removed with the album (FK cascade).
#[test]
fn blocklist() {
    let (_d, conn) = fixture_copy();
    conn.pragma_update(None, "foreign_keys", "ON").unwrap();
    assert!(
        get_blocked_mb_release_groups(&conn, GAPS)
            .unwrap()
            .is_empty()
    );
    assert!(add_blocked_mb_release_group(&conn, GAPS, "rg-b", "2026-11-10T00:00:00Z").unwrap());
    assert!(!add_blocked_mb_release_group(&conn, GAPS, "rg-b", "2026-11-10T00:00:01Z").unwrap());
    assert!(!add_blocked_mb_release_group(&conn, GAPS, "", "2026-11-10T00:00:01Z").unwrap());
    assert!(add_blocked_mb_release_group(&conn, GAPS, "rg-a", "2026-11-10T00:00:02Z").unwrap());
    let got: Vec<String> = get_blocked_mb_release_groups(&conn, GAPS)
        .unwrap()
        .into_iter()
        .collect();
    assert_eq!(got, ["rg-a", "rg-b"]);
    assert!(
        get_blocked_mb_release_groups(&conn, "1f796a3255bbb8dc")
            .unwrap()
            .is_empty()
    );
    musaic_db::albums::delete_album(&conn, GAPS).unwrap();
    assert!(
        get_blocked_mb_release_groups(&conn, GAPS)
            .unwrap()
            .is_empty()
    );
}

// Fingerprint rows as the resolver reads them, ordered by filename,
// equal to what the Python app stored (compared with a direct query).
#[test]
fn fingerprint_rows() {
    let (_d, conn) = fixture_copy();
    let mut total = 0;
    let ids: Vec<String> = conn
        .prepare("SELECT id FROM albums")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    for id in ids {
        let rows = get_track_fingerprints(&conn, &id).unwrap();
        let mut names: Vec<String> = rows.iter().map(|r| r.filename.clone()).collect();
        let sorted = {
            let mut s = names.clone();
            s.sort();
            s
        };
        assert_eq!(names, sorted, "{id}");
        names.dedup();
        assert_eq!(names.len(), rows.len());
        let direct: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM track_fingerprints WHERE album_id = ?1",
                [&id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(rows.len() as i64, direct);
        total += rows.len();
    }
    let all: i64 = conn
        .query_row("SELECT COUNT(*) FROM track_fingerprints", [], |r| r.get(0))
        .unwrap();
    assert!(all > 0);
    assert_eq!(total as i64, all);
}
