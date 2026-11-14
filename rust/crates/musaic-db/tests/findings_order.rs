//! Deliberate change: findings are listed by
//! `last_seen_at DESC, id ASC`. The Python app (`musaic/checks/runner.py:477`)
//! orders by `last_seen_at DESC` alone, at one-second resolution, and leaves
//! ties to SQLite. The rows below share one timestamp and are inserted so
//! that id order, album order and summary order all disagree.
//!
//! Limit of this test: with the bundled SQLite the query returns the same order
//! without the tiebreaker (removing `id ASC` does not fail it), because the
//! sorter happens to keep scan order. It pins the stated order; it does not
//! prove the old query was unstable.

use musaic_db::findings::{FindingFilter, list_findings};
use musaic_db::{Db, open_connection};

#[test]
fn ties_on_last_seen_are_broken_by_id() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.db");
    let (db, _) = Db::open_or_create(&path).unwrap();
    drop(db);
    let conn = open_connection(&path).unwrap();
    let ts = "2026-01-01T00:00:00+00:00";
    // (check, album, summary) in insertion (= id) order.
    let rows = [
        ("track-gaps", Some("zz"), "m"),
        ("no-cover-art", Some("mm"), "z"),
        ("track-gaps", Some("aa"), "a"),
        ("track-gaps", None, "q"),
        ("no-cover-art", Some("aa"), "b"),
        ("track-gaps", Some("mm"), "c"),
    ];
    for (check, album, summary) in rows {
        conn.execute(
            "INSERT INTO check_findings \
             (check_id, album_id, summary, first_seen_at, last_seen_at) \
             VALUES (?1, ?2, ?3, ?4, ?4)",
            rusqlite::params![check, album, summary, ts],
        )
        .unwrap();
    }
    // One newer row: last_seen_at still dominates.
    conn.execute(
        "INSERT INTO check_findings \
         (check_id, album_id, summary, first_seen_at, last_seen_at) \
         VALUES ('track-gaps', 'bb', 'newer', ?1, '2026-01-01T00:00:01+00:00')",
        [ts],
    )
    .unwrap();

    let ids = |f: &FindingFilter<'_>| -> Vec<i64> {
        list_findings(&conn, f)
            .unwrap()
            .iter()
            .map(|r| r.id)
            .collect()
    };
    assert_eq!(ids(&FindingFilter::default()), [7, 1, 2, 3, 4, 5, 6]);
    assert_eq!(
        ids(&FindingFilter {
            check_id: Some("track-gaps"),
            ..Default::default()
        }),
        [7, 1, 3, 4, 6]
    );
    assert_eq!(
        ids(&FindingFilter {
            album_id: Some("aa"),
            ..Default::default()
        }),
        [3, 5]
    );
}
