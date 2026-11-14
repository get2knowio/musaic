//! Deliberate change: duplicate-tracks
//! only compares fingerprints produced by the same implementation. There is no
//! Python app output for it (the Python app has one implementation); the expected
//! outcome follows from the rule. The fingerprints are real ones from the Python-recorded
//! golden (the identical "Static Bloom" pair), so with one implementation they
//! cluster exactly as testdata/golden/checks/cases.json `dupes-basic` shows.

mod common;

use std::sync::Arc;

use common::{Sandbox, env, registry, setup};
use musaic_checks::{NoResolver, ScopeKind, run_check};
use musaic_db::findings::all_findings;
use serde_json::json;

fn bloom() -> String {
    let g = common::golden("cases.json");
    let case = g["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == "dupes-basic")
        .unwrap();
    case["fingerprints"][0]["fingerprint"]
        .as_str()
        .unwrap()
        .to_string()
}

fn count_after(impl_b: &str) -> usize {
    let (sb, db) = Sandbox::new();
    let conn = db.connection();
    let fp = bloom();
    setup(
        &sb,
        conn,
        &json!({
            "albums": [{"id": "a1", "library_path": "<LIB>", "path": "<LIB>/D", "dir_name": "D"}],
            "fingerprints": [
                {"album_id": "a1", "filename": "a.flac", "fingerprint": fp, "duration_s": 10.0, "acoustid_recording_mbid": null},
                {"album_id": "a1", "filename": "b.flac", "fingerprint": fp, "duration_s": 10.0, "acoustid_recording_mbid": null},
            ],
        }),
    );
    conn.execute(
        "UPDATE track_fingerprints SET fingerprint_impl = ?1 WHERE filename = 'b.flac'",
        [impl_b],
    )
    .unwrap();
    let env = env(registry(None, &Default::default()), Arc::new(NoResolver));
    run_check(
        conn,
        &env,
        "duplicate-tracks",
        &sb.lib_s,
        ScopeKind::All,
        None,
    )
    .unwrap();
    all_findings(conn).unwrap().len()
}

/// Same implementation -> one duplicate finding; mixed -> none.
#[test]
fn only_same_implementation_fingerprints_are_compared() {
    assert_eq!(count_after(musaic_db::FINGERPRINT_IMPL_FPCALC), 1);
    assert_eq!(count_after(musaic_db::FINGERPRINT_IMPL_RUST), 0);
}
