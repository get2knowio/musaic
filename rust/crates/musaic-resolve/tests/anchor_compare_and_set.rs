//! The resolver's anchor write is a compare-and-set on what it read: a
//! user's "Wrong match" (anchor cleared and the release group blocked), or a
//! cleared anchor, made while a background resolve was waiting on
//! MusicBrainz always wins; with nothing changed the write goes through.

use musaic_resolve::db_store::DbStore;
use musaic_resolve::source::AnchorStore;
use rusqlite::Connection;

const RG: &str = "11111111-2222-3333-4444-555555555555";
const OTHER: &str = "99999999-2222-3333-4444-555555555555";

fn db() -> (tempfile::TempDir, Connection) {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("library.db");
    musaic_db::Db::open_or_create(&p).unwrap();
    let c = musaic_db::open_connection(&p).unwrap();
    c.execute("INSERT INTO libraries(path) VALUES ('/lib')", [])
        .unwrap();
    c.execute(
        "INSERT INTO albums(id, library_path, path, dir_name) VALUES ('a', '/lib', '/lib/a', 'a')",
        [],
    )
    .unwrap();
    musaic_db::mb_anchor::set_album_mb_anchor(&c, "a", RG, None, 90, "2001-01-01T00:00:00Z")
        .unwrap();
    (d, c)
}

fn anchor(c: &Connection) -> Option<(String, Option<String>)> {
    musaic_db::mb_anchor::get_album_mb_anchor(c, "a")
        .unwrap()
        .map(|a| (a.release_group_mbid, a.last_checked_at))
}

#[test]
fn an_unchanged_anchor_is_refreshed() {
    let (_d, c) = db();
    let store = DbStore::new(&c, "2026-11-16T00:00:00Z".into());
    store.anchor("a").unwrap();
    store.set_anchor("a", RG, None, 95).unwrap();
    assert_eq!(
        anchor(&c),
        Some((RG.into(), Some("2026-11-16T00:00:00Z".into())))
    );
}

#[test]
fn a_wrong_match_made_meanwhile_wins() {
    let (_d, c) = db();
    let store = DbStore::new(&c, "2026-11-16T00:00:00Z".into());
    store.anchor("a").unwrap();
    // The user: "Wrong match" (clear and block) while the resolve waits.
    musaic_db::mb_anchor::clear_album_mb_anchor(&c, "a").unwrap();
    musaic_db::mb_anchor::add_blocked_mb_release_group(&c, "a", RG, "2026-11-16T00:00:00Z")
        .unwrap();
    assert!(store.set_anchor("a", RG, None, 95).is_err());
    assert_eq!(anchor(&c), None);
}

#[test]
fn a_cleared_or_changed_anchor_is_not_overwritten() {
    let (_d, c) = db();
    let store = DbStore::new(&c, "2026-11-16T00:00:00Z".into());
    store.anchor("a").unwrap();
    musaic_db::mb_anchor::clear_album_mb_anchor(&c, "a").unwrap();
    assert!(store.set_anchor("a", RG, None, 95).is_err());
    assert_eq!(anchor(&c), None);
    // The user picked another release group meanwhile.
    let store = DbStore::new(&c, "2026-11-16T00:00:00Z".into());
    store.anchor("a").unwrap();
    musaic_db::mb_anchor::set_album_mb_anchor(&c, "a", OTHER, None, 100, "2026-11-15T00:00:00Z")
        .unwrap();
    assert!(store.set_anchor("a", RG, None, 95).is_err());
    assert_eq!(anchor(&c).unwrap().0, OTHER);
}

#[test]
fn a_blocked_release_group_is_never_written() {
    let (_d, c) = db();
    musaic_db::mb_anchor::clear_album_mb_anchor(&c, "a").unwrap();
    musaic_db::mb_anchor::add_blocked_mb_release_group(&c, "a", RG, "2026-11-16T00:00:00Z")
        .unwrap();
    let store = DbStore::new(&c, "2026-11-16T00:00:00Z".into());
    store.anchor("a").unwrap();
    assert!(store.set_anchor("a", RG, None, 95).is_err());
    store.set_anchor("a", OTHER, None, 95).unwrap();
    assert_eq!(anchor(&c).unwrap().0, OTHER);
}
