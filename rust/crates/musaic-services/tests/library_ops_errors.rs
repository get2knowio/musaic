//! Album lookups report a database error instead of reading it as "no such
//! album". A connection with no schema fails every query, which stands in for
//! a database that cannot be read.

use musaic_events::Bus;
use musaic_scan::TrackOrder;
use musaic_services::apply::HttpError;
use musaic_services::{library_ops, sources, track_tags};
use rusqlite::Connection;

/// The error a handler would answer with: the status of an `HttpError`, 500
/// for anything else.
#[derive(Debug)]
struct Answer(u16);

impl From<HttpError> for Answer {
    fn from(e: HttpError) -> Self {
        Answer(e.status)
    }
}
impl From<musaic_db::Error> for Answer {
    fn from(_: musaic_db::Error) -> Self {
        Answer(500)
    }
}
impl From<rusqlite::Error> for Answer {
    fn from(_: rusqlite::Error) -> Self {
        Answer(500)
    }
}

fn unreadable_db() -> Connection {
    Connection::open_in_memory().unwrap()
}

#[test]
fn cached_album_returns_a_database_error() {
    let lib = tempfile::tempdir().unwrap();
    let conn = unreadable_db();
    let lib = lib.path().to_str().unwrap();
    assert!(sources::cached_album(&conn, Some(lib), "abc").is_err());
    // No configured library is "not cached", not an error.
    assert!(sources::cached_album(&conn, None, "abc").unwrap().is_none());
}

#[test]
fn album_lookup_answers_500_not_a_live_walk_when_the_database_fails() {
    // An empty library directory: the live walk would answer 404.
    let lib = tempfile::tempdir().unwrap();
    let lib = lib.path().to_str().unwrap();
    let conn = unreadable_db();
    let err = library_ops::resolve_album_dict(&conn, Some(lib), "abc", TrackOrder::default())
        .unwrap_err();
    assert_eq!(err.status, 500, "{}", err.detail);
    // The tag routes share the same lookup.
    let err =
        track_tags::resolve_album_dict(&conn, Some(lib), "abc", TrackOrder::default()).unwrap_err();
    assert_eq!(err.status, 500, "{}", err.detail);
}

#[test]
fn removing_an_album_answers_500_not_404_when_the_database_fails() {
    let lib = tempfile::tempdir().unwrap();
    let lib = lib.path().to_str().unwrap();
    let bus = Bus::new();
    let r = library_ops::delete_album::<Answer>(unreadable_db(), &bus, Some(lib), "abc");
    assert_eq!(r.unwrap_err().0, 500);
}

#[test]
fn snapshot_returns_a_database_error() {
    let lib = tempfile::tempdir().unwrap();
    let conn = unreadable_db();
    assert!(library_ops::snapshot(&conn, lib.path().to_str().unwrap()).is_err());
}
