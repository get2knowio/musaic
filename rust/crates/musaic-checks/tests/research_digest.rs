//! The research part of an album's inputs digest (schema v24): a change a
//! check can read (a request made, its state, its result) makes the album's
//! checks stale; a lease renewal or a progress report, which only bump the
//! request's revision and update time, does not. An assistant renews about
//! once a minute, so counting those would rerun every check of every album
//! being researched, network checks included, over and over.

mod common;

use common::{Sandbox, setup};
use musaic_checks::inputs::album_digests;
use musaic_db::delegated_work::{self, NewWork};
use serde_json::json;

const ALBUM: &str = "a000000000000001";

fn digest(conn: &rusqlite::Connection, sb: &Sandbox) -> String {
    album_digests(conn, &sb.lib_s, None).unwrap()[ALBUM].clone()
}

#[test]
fn a_renewal_leaves_the_digest_and_a_state_change_moves_it() {
    let (sb, db) = Sandbox::new();
    setup(
        &sb,
        db.connection(),
        &json!({ "albums": [{
            "id": ALBUM, "library_path": "<LIB>", "path": format!("<LIB>/{ALBUM}"),
            "dir_name": ALBUM, "title": "T", "artist": "A", "album_artist": "A",
        }] }),
    );
    let conn = db.connection();
    let without = digest(conn, &sb);

    delegated_work::enqueue(
        conn,
        &[NewWork {
            id: "00000000-0000-4000-8000-000000000001",
            group_id: "00000000-0000-4000-8000-000000000002",
            kind: "cover.research",
            album_id: ALBUM,
            base_revision: "r1",
            input_json: "{}",
            input_digest: "d1",
            max_attempts: 3,
        }],
        1_000,
    )
    .unwrap();
    let queued = digest(conn, &sb);
    assert_ne!(queued, without, "a new request is an input");

    // What a renewal or a progress report changes on the request row.
    conn.execute(
        "UPDATE delegated_work SET revision = revision + 1, updated_at = updated_at + 60000",
        [],
    )
    .unwrap();
    assert_eq!(digest(conn, &sb), queued, "a heartbeat is not an input");

    conn.execute("UPDATE delegated_work SET state = 'failed'", [])
        .unwrap();
    assert_ne!(digest(conn, &sb), queued, "a state change is an input");
}
