//! `merge::move_track_rows` and `merge::merge_album_rows`: every table that
//! holds an album id, every key collision, every refusal (which must leave
//! the database exactly as it was), and the resume sequence.

use musaic_db::merge::{
    MergeRefusal, MergeReport, MergeRequest, merge_album_rows, merge_album_rows_tx, move_track_rows,
};
use musaic_db::renames::{ALBUM_ID_HOLDERS, FileRowsMoved, rename_album_directory};
use musaic_db::{Db, Error};
use rusqlite::types::ValueRef;
use rusqlite::{Connection, params};

const LIB: &str = "/music";
const OTHER_LIB: &str = "/elsewhere";
const NOW: &str = "2026-11-18T12:00:00Z";

struct T {
    _dir: tempfile::TempDir,
    db: Db,
}

impl T {
    fn new() -> T {
        let dir = tempfile::tempdir().unwrap();
        let (db, _) = Db::open_or_create(dir.path().join("library.db")).unwrap();
        for lib in [LIB, OTHER_LIB] {
            db.connection()
                .execute(
                    "INSERT INTO libraries (path, scanned_at) VALUES (?1, 'x')",
                    [lib],
                )
                .unwrap();
        }
        T { _dir: dir, db }
    }

    fn c(&self) -> &Connection {
        self.db.connection()
    }

    fn album(&self, id: &str) -> &Self {
        self.album_in(id, LIB)
    }

    fn album_in(&self, id: &str, lib: &str) -> &Self {
        self.c()
            .execute(
                "INSERT INTO albums (id, library_path, path, dir_name, track_count) \
                 VALUES (?1, ?2, ?3, ?1, 2)",
                params![id, lib, format!("{lib}/{id}")],
            )
            .unwrap();
        self
    }

    fn fp(&self, album: &str, file: &str, fingerprint: &str) -> &Self {
        self.c()
            .execute(
                "INSERT INTO track_fingerprints (album_id, filename, fingerprint, duration_s, \
                 last_computed_at) VALUES (?1, ?2, ?3, 1.0, 't')",
                params![album, file, fingerprint],
            )
            .unwrap();
        self
    }

    fn tag(&self, album: &str, file: &str, tag: &str, created: &str) -> &Self {
        self.c()
            .execute(
                "INSERT INTO track_tags (album_id, filename, tag, created_at) VALUES (?1, ?2, ?3, ?4)",
                params![album, file, tag, created],
            )
            .unwrap();
        self
    }

    fn reject(&self, album: &str, file: &str, sig: &str) -> &Self {
        self.c()
            .execute(
                "INSERT INTO fingerprint_rejects (album_id, filename, file_signature, \
                 fingerprint_impl, rejected_at) VALUES (?1, ?2, ?3, 'rust', 't')",
                params![album, file, sig],
            )
            .unwrap();
        self
    }

    fn block(&self, album: &str, rg: &str, at: &str) -> &Self {
        self.c()
            .execute(
                "INSERT INTO album_blocked_mb_release_groups (album_id, release_group_mbid, \
                 blocked_at) VALUES (?1, ?2, ?3)",
                params![album, rg, at],
            )
            .unwrap();
        self
    }

    /// A finding; `state` is "open", "ignored" or "resolved".
    fn finding(&self, check: &str, album: &str, summary: &str, state: &str) -> &Self {
        let (resolved, ignored) = match state {
            "open" => (None, None),
            "ignored" => (None, Some("t-ignored")),
            "resolved" => (Some("t-resolved"), None),
            _ => unreachable!(),
        };
        self.c()
            .execute(
                "INSERT INTO check_findings (check_id, album_id, summary, detail_json, \
                 first_seen_at, last_seen_at, resolved_at, ignored_at, ignored_reason) \
                 VALUES (?1, ?2, ?3, '{}', 't', 't', ?4, ?5, ?6)",
                params![
                    check,
                    album,
                    summary,
                    resolved,
                    ignored,
                    ignored.map(|_| "fine")
                ],
            )
            .unwrap();
        self
    }

    /// An old job, as schema v24 folded it: a history record and its
    /// request.
    fn job(&self, id: &str, album: &str, status: &str) -> &Self {
        let job = musaic_db::research_history::JobRow {
            album_path: format!("{LIB}/{album}"),
            ..musaic_db::research_history::JobRow::seed(id, album, "metadata", status, "t")
        };
        musaic_db::research_history::import_legacy_job(self.c(), &job).unwrap();
        // Ids and the fold's clock made stable, so two libraries built the
        // same way dump the same.
        self.c()
            .execute_batch(&format!(
                "UPDATE delegation_audit SET work_id = 'w-{id}'
                   WHERE work_id IN (SELECT id FROM delegated_work WHERE legacy_job_id = '{id}');
                 UPDATE delegation_outbox SET work_id = 'w-{id}'
                   WHERE work_id IN (SELECT id FROM delegated_work WHERE legacy_job_id = '{id}');
                 UPDATE delegated_work SET id = 'w-{id}', group_id = 'g-{id}'
                   WHERE legacy_job_id = '{id}';
                 UPDATE research_history SET migrated_at = 0 WHERE id = '{id}';"
            ))
            .unwrap();
        self
    }

    /// An assistant's proposal on `album`, valid until 2099.
    fn proposal(&self, id: &str, album: &str) -> &Self {
        self.c()
            .execute(
                "INSERT OR IGNORE INTO agent_pairings (id, name, scopes, secret_fingerprint, \
                 created_at) VALUES ('p', 'Assistant', '[]', 'f', 't')",
                [],
            )
            .unwrap();
        self.c()
            .execute(
                "INSERT INTO agent_proposals (id, pairing_id, album_id, base_revision, patch, \
                 digest, kinds, diff, created_at, expires_at) \
                 VALUES (?1, 'p', ?2, 'rev', '{}', 'd', '[]', '{}', '2026-11-18T11:00:00Z', \
                 '2099-01-01T00:00:00Z')",
                params![id, album],
            )
            .unwrap();
        self
    }

    /// A finished change record of an assistant on `album`; its after-image
    /// names the same album.
    fn change(&self, id: &str, album: &str) -> &Self {
        self.c()
            .execute(
                "INSERT INTO agent_changes (id, pairing_id, proposal_id, album_id, created_at, \
                 state, before, after) VALUES (?1, NULL, 'pr', ?2, 't', 'applied', '{}', ?3)",
                params![
                    id,
                    album,
                    format!(r#"{{"albumId":"{album}","folder":"{album}","files":{{}}}}"#)
                ],
            )
            .unwrap();
        self
    }

    fn input(&self, check: &str, album: &str) -> &Self {
        self.c()
            .execute(
                "INSERT INTO check_inputs (check_id, library_path, album_id, digest, checked_at) \
                 VALUES (?1, ?2, ?3, 'd', 't')",
                params![check, LIB, album],
            )
            .unwrap();
        self
    }

    fn run_scope(&self, scope: &str) -> &Self {
        self.c()
            .execute(
                "INSERT INTO check_runs (check_id, started_at, status, library_path, scope) \
                 VALUES ('c', 't', 'completed', ?1, ?2)",
                params![LIB, scope],
            )
            .unwrap();
        self
    }

    fn anchor(&self, album: &str, rg: &str, release: Option<&str>, score: i64) -> &Self {
        musaic_db::mb_anchor::set_album_mb_anchor(self.c(), album, rg, release, score, "t-anchor")
            .unwrap();
        self
    }

    fn set(&self, album: &str, column: &str, value: Option<&str>) -> &Self {
        self.c()
            .execute(
                &format!("UPDATE albums SET {column} = ?1 WHERE id = ?2"),
                params![value, album],
            )
            .unwrap();
        self
    }

    fn get(&self, sql: &str) -> Option<String> {
        self.c()
            .query_row(sql, [], |r| match r.get_ref(0)? {
                ValueRef::Null => Ok(None),
                ValueRef::Integer(n) => Ok(Some(n.to_string())),
                ValueRef::Real(f) => Ok(Some(f.to_string())),
                ValueRef::Text(t) => Ok(Some(String::from_utf8_lossy(t).into_owned())),
                ValueRef::Blob(_) => unreachable!(),
            })
            .unwrap()
    }

    fn rows(&self, sql: &str) -> Vec<String> {
        let mut stmt = self.c().prepare(sql).unwrap();
        let n = stmt.column_count();
        stmt.query_map([], |r| {
            Ok((0..n)
                .map(|i| value_text(r.get_ref(i).unwrap()))
                .collect::<Vec<_>>()
                .join("|"))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
    }

    /// Every row of every table, sorted: two equal dumps mean equal data.
    fn dump(&self) -> String {
        let tables: Vec<String> =
            self.rows("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name");
        let mut out = String::new();
        for t in tables {
            let t = t.trim_matches('\'');
            let mut rows = self.rows(&format!("SELECT * FROM \"{t}\""));
            rows.sort();
            out.push_str(&format!("[{t}]\n{}\n", rows.join("\n")));
        }
        out
    }

    fn assert_foreign_keys_hold(&self) {
        assert_eq!(
            self.rows("PRAGMA foreign_key_check"),
            Vec::<String>::new(),
            "foreign keys"
        );
    }

    /// `table|album_id` of every row that refers to an album no row has.
    fn dangling(&self) -> Vec<String> {
        let mut out = Vec::new();
        for table in ALBUM_ID_HOLDERS {
            out.extend(self.rows(&format!(
                "SELECT '{table}', album_id FROM {table} \
                 WHERE album_id != '' AND album_id NOT IN (SELECT id FROM albums) ORDER BY 2"
            )));
        }
        out
    }
}

fn value_text(v: ValueRef<'_>) -> String {
    match v {
        ValueRef::Null => "NULL".into(),
        ValueRef::Integer(n) => n.to_string(),
        ValueRef::Real(f) => f.to_string(),
        ValueRef::Text(t) => format!("'{}'", String::from_utf8_lossy(t)),
        ValueRef::Blob(b) => format!("blob{b:?}"),
    }
}

fn ids(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

fn req<'a>(from: &'a [String], into: &'a str) -> MergeRequest<'a> {
    MergeRequest {
        from_ids: from,
        into_id: into,
        emptied: &[],
        now: NOW,
    }
}

fn refusal(r: musaic_db::Result<impl std::fmt::Debug>) -> MergeRefusal {
    match r {
        Err(Error::MergeRefused(why)) => {
            assert!(!why.to_string().is_empty());
            why
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
}

/// Run `f`, which must refuse; the database must be exactly as before.
fn refused<R: std::fmt::Debug>(
    t: &T,
    f: impl FnOnce(&Connection) -> musaic_db::Result<R>,
) -> MergeRefusal {
    let before = t.dump();
    let why = refusal(f(t.c()));
    assert_eq!(t.dump(), before, "a refusal changed the database: {why}");
    why
}

// ---------------- move_track_rows ----------------

#[test]
fn moves_a_files_rows_to_another_album_under_a_new_name() {
    let t = T::new();
    t.album("x").album("t");
    t.fp("x", "01 a.flac", "FPX")
        .tag("x", "01 a.flac", "live", "c1")
        .tag("x", "01 a.flac", "fav", "c2")
        .reject("x", "02 b.flac", "sig");
    // Another file of the source stays where it is.
    t.fp("x", "03 c.flac", "FP3");
    let moved = move_track_rows(t.c(), "x", "01 a.flac", "t", "13 a.flac").unwrap();
    assert_eq!(
        moved,
        FileRowsMoved {
            track_tags: 2,
            track_fingerprints: 1,
            fingerprint_rejects: 0
        }
    );
    assert_eq!(
        t.rows("SELECT album_id, filename, fingerprint FROM track_fingerprints ORDER BY 1, 2"),
        ["'t'|'13 a.flac'|'FPX'", "'x'|'03 c.flac'|'FP3'"]
    );
    assert_eq!(
        t.rows("SELECT album_id, filename, tag, created_at FROM track_tags ORDER BY 3"),
        ["'t'|'13 a.flac'|'fav'|'c2'", "'t'|'13 a.flac'|'live'|'c1'"]
    );
    let moved = move_track_rows(t.c(), "x", "02 b.flac", "t", "14 b.flac").unwrap();
    assert_eq!(moved.fingerprint_rejects, 1);
    assert_eq!(
        t.rows("SELECT album_id, filename, file_signature FROM fingerprint_rejects"),
        ["'t'|'14 b.flac'|'sig'"]
    );
    t.assert_foreign_keys_hold();
}

#[test]
fn a_source_row_replaces_a_stale_target_row_and_tags_are_united() {
    let t = T::new();
    t.album("x").album("t");
    t.fp("x", "a.flac", "NEW").fp("t", "13.flac", "STALE");
    t.reject("x", "a.flac", "new-sig")
        .reject("t", "13.flac", "stale-sig");
    t.tag("x", "a.flac", "live", "from-x")
        .tag("x", "a.flac", "fav", "from-x")
        .tag("t", "13.flac", "live", "from-t")
        .tag("t", "13.flac", "keep", "from-t");
    move_track_rows(t.c(), "x", "a.flac", "t", "13.flac").unwrap();
    assert_eq!(
        t.rows("SELECT album_id, filename, fingerprint FROM track_fingerprints"),
        ["'t'|'13.flac'|'NEW'"]
    );
    assert_eq!(
        t.rows("SELECT album_id, filename, file_signature FROM fingerprint_rejects"),
        ["'t'|'13.flac'|'new-sig'"]
    );
    // The union; a tag both had keeps the target's stamp.
    assert_eq!(
        t.rows("SELECT album_id, filename, tag, created_at FROM track_tags ORDER BY 3"),
        [
            "'t'|'13.flac'|'fav'|'from-x'",
            "'t'|'13.flac'|'keep'|'from-t'",
            "'t'|'13.flac'|'live'|'from-t'"
        ]
    );
}

#[test]
fn without_a_source_row_the_targets_row_is_kept_and_a_second_call_is_a_no_op() {
    let t = T::new();
    t.album("x").album("t");
    // A fingerprint pass already fingerprinted the moved file in its new place.
    t.fp("t", "13.flac", "FRESH")
        .tag("x", "a.flac", "live", "c");
    let first = move_track_rows(t.c(), "x", "a.flac", "t", "13.flac").unwrap();
    assert_eq!(first.track_fingerprints, 0);
    assert_eq!(
        t.rows("SELECT fingerprint FROM track_fingerprints"),
        ["'FRESH'"]
    );
    let after = t.dump();
    let again = move_track_rows(t.c(), "x", "a.flac", "t", "13.flac").unwrap();
    assert!(again.is_empty());
    assert_eq!(t.dump(), after);
}

#[test]
fn a_source_album_with_no_row_moves_nothing() {
    let t = T::new();
    t.album("t").fp("t", "13.flac", "F");
    let before = t.dump();
    let moved = move_track_rows(t.c(), "gone", "a.flac", "t", "13.flac").unwrap();
    assert!(moved.is_empty());
    assert_eq!(t.dump(), before);
}

#[test]
fn a_missing_source_album_may_still_give_up_its_rows() {
    let t = T::new();
    t.album("x")
        .album("t")
        .set("x", "missing_since", Some("t-gone"));
    t.fp("x", "a.flac", "F");
    move_track_rows(t.c(), "x", "a.flac", "t", "13.flac").unwrap();
    assert_eq!(
        t.rows("SELECT album_id, filename FROM track_fingerprints"),
        ["'t'|'13.flac'"]
    );
}

#[test]
fn move_track_rows_refusals_change_nothing() {
    let t = T::new();
    t.album("x").album("t").album("m").album_in("o", OTHER_LIB);
    t.set("m", "missing_since", Some("t-gone"));
    t.fp("x", "a.flac", "F").fp("o", "a.flac", "F");
    assert!(matches!(
        refused(&t, |c| move_track_rows(c, "x", "a.flac", "x", "b.flac")),
        MergeRefusal::SameAlbum(_)
    ));
    for (fa, fname, ia, iname) in [
        ("", "a.flac", "t", "b.flac"),
        ("x", "", "t", "b.flac"),
        ("x", "a.flac", "", "b.flac"),
        ("x", "a.flac", "t", ""),
    ] {
        assert!(matches!(
            refused(&t, |c| move_track_rows(c, fa, fname, ia, iname)),
            MergeRefusal::Empty(_)
        ));
    }
    assert_eq!(
        refused(&t, |c| move_track_rows(c, "x", "a.flac", "nope", "b.flac")),
        MergeRefusal::UnknownAlbum("nope".into())
    );
    assert_eq!(
        refused(&t, |c| move_track_rows(c, "x", "a.flac", "m", "b.flac")),
        MergeRefusal::Missing {
            album_id: "m".into(),
            survivor: true
        }
    );
    assert!(matches!(
        refused(&t, |c| move_track_rows(c, "o", "a.flac", "t", "b.flac")),
        MergeRefusal::OtherLibrary { .. }
    ));
}

// ---------------- merge_album_rows: every table ----------------

/// Survivor `t`, members `x1`, `x2`, with rows in every table: some under
/// a from-album only, some under the survivor only, some colliding.
fn full_library() -> T {
    let t = T::new();
    t.album("t").album("x1").album("x2").album("bystander");
    // Per-file rows: already moved to t (the caller did that).
    t.fp("t", "01.flac", "F1").fp("t", "13.flac", "F13");
    t.tag("t", "13.flac", "live", "c");
    t.fp("bystander", "01.flac", "FB");
    // Blocklists: x1 and t share one group (different stamps), x2 has its own.
    t.block("x1", "rg-shared", "t-x1")
        .block("t", "rg-shared", "t-t")
        .block("x2", "rg-x2", "t-x2");
    // Findings: the same summary on every member, ignored on x2; one already
    // resolved on x1; the survivor's own; a library-level one.
    t.finding("no-cover", "t", "No cover", "open")
        .finding("no-cover", "x1", "No cover", "open")
        .finding("no-cover", "x2", "No cover", "ignored")
        .finding("track-gaps", "x1", "Gap at 3", "resolved")
        .finding("track-gaps", "x2", "Gap at 5", "open")
        .finding("dupes", "bystander", "Dupes", "ignored");
    t.c()
        .execute(
            "INSERT INTO check_findings (check_id, album_id, summary, first_seen_at, last_seen_at) \
             VALUES ('lib-wide', NULL, 'Library', 't', 't')",
            [],
        )
        .unwrap();
    // Jobs: finished ones on members and survivor.
    t.job("j1", "x1", "failed")
        .job("j2", "x2", "acted_on")
        .job("j3", "t", "acted_on")
        .job("j4", "bystander", "completed");
    // Inputs: per album, and a whole-library row.
    t.input("no-cover", "t")
        .input("no-cover", "x1")
        .input("no-cover", "x2")
        .input("track-gaps", "x2")
        .input("no-cover", "bystander")
        .input("dupes", "");
    t.run_scope("[\"x1\", \"x2\"]");
    // An assistant's proposals and change records.
    t.proposal("pr-x1", "x1")
        .proposal("pr-t", "t")
        .change("ch-x2", "x2")
        .change("ch-bystander", "bystander");
    // AI review stamps: x2 is the latest.
    t.set("t", "metadata_ai_acted_on_at", Some("2026-01-01T00:00:00Z"))
        .set("x1", "metadata_ai_acted_on_at", None)
        .set(
            "x2",
            "metadata_ai_acted_on_at",
            Some("2026-05-01T00:00:00Z"),
        );
    t
}

#[test]
fn folds_every_table_and_deletes_the_emptied_rows() {
    let t = full_library();
    let from = ids(&["x1", "x2"]);
    let report = merge_album_rows(t.c(), &req(&from, "t")).unwrap();
    assert_eq!(
        report,
        MergeReport {
            already_merged: false,
            albums_deleted: 2,
            album_blocked_mb_release_groups: 1,
            check_findings_resolved: 3,
            ai_jobs: 2,
            check_inputs: 4,
            agent_proposals: 1,
            agent_changes: 1,
            release_group: None,
            release: None,
            release_cleared: false,
            metadata_ai_acted_on_at: Some("2026-05-01T00:00:00Z".into()),
        }
    );
    // An assistant's rows follow the survivor; a member's proposal, made
    // against one disc, is expired; the survivor's own stays valid.
    assert_eq!(
        t.rows("SELECT id, album_id, expires_at FROM agent_proposals ORDER BY id"),
        [
            "'pr-t'|'t'|'2099-01-01T00:00:00Z'",
            "'pr-x1'|'t'|'2026-11-18T11:00:00Z'"
        ]
    );
    assert_eq!(
        t.rows(
            "SELECT id, album_id, json_extract(after, '$.albumId') FROM agent_changes ORDER BY id"
        ),
        ["'ch-bystander'|'bystander'|'bystander'", "'ch-x2'|'t'|'t'"]
    );
    assert_eq!(
        t.rows("SELECT id FROM albums ORDER BY id"),
        ["'bystander'", "'t'"]
    );
    // Per-file rows: untouched.
    assert_eq!(
        t.rows("SELECT album_id, filename FROM track_fingerprints ORDER BY 1, 2"),
        ["'bystander'|'01.flac'", "'t'|'01.flac'", "'t'|'13.flac'"]
    );
    assert_eq!(
        t.rows("SELECT album_id, filename, tag FROM track_tags"),
        ["'t'|'13.flac'|'live'"]
    );
    // Blocklist: the union, the survivor's stamp kept on the shared group.
    assert_eq!(
        t.rows(
            "SELECT album_id, release_group_mbid, blocked_at FROM album_blocked_mb_release_groups \
             ORDER BY 2"
        ),
        ["'t'|'rg-shared'|'t-t'", "'t'|'rg-x2'|'t-x2'"]
    );
    // Findings: the members' unresolved ones resolved in place, ignore kept
    // as recorded; the already resolved one keeps its stamp; the survivor's,
    // the bystander's and the library-level one untouched.
    assert_eq!(
        t.rows(
            "SELECT check_id, album_id, summary, resolved_at, ignored_at FROM check_findings \
             ORDER BY id"
        ),
        [
            "'no-cover'|'t'|'No cover'|NULL|NULL".to_string(),
            format!("'no-cover'|'x1'|'No cover'|'{NOW}'|NULL"),
            format!("'no-cover'|'x2'|'No cover'|'{NOW}'|'t-ignored'"),
            "'track-gaps'|'x1'|'Gap at 3'|'t-resolved'|NULL".to_string(),
            format!("'track-gaps'|'x2'|'Gap at 5'|'{NOW}'|NULL"),
            "'dupes'|'bystander'|'Dupes'|NULL|'t-ignored'".to_string(),
            "'lib-wide'|NULL|'Library'|NULL|NULL".to_string(),
        ]
    );
    // Jobs: repointed, display path kept.
    assert_eq!(
        t.rows("SELECT id, album_id, album_path FROM research_history ORDER BY id"),
        [
            format!("'j1'|'t'|'{LIB}/x1'"),
            format!("'j2'|'t'|'{LIB}/x2'"),
            format!("'j3'|'t'|'{LIB}/t'"),
            format!("'j4'|'bystander'|'{LIB}/bystander'"),
        ]
    );
    // Inputs: members' and survivor's gone; others kept.
    assert_eq!(
        t.rows("SELECT check_id, album_id FROM check_inputs ORDER BY 1, 2"),
        ["'dupes'|''", "'no-cover'|'bystander'"]
    );
    // Run scopes: history, untouched.
    assert_eq!(
        t.rows("SELECT scope FROM check_runs"),
        ["'[\"x1\", \"x2\"]'"]
    );
    assert_eq!(
        t.get("SELECT metadata_ai_acted_on_at FROM albums WHERE id = 't'"),
        Some("2026-05-01T00:00:00Z".into())
    );
    t.assert_foreign_keys_hold();
    // Nothing refers to a dead id but the findings kept as history.
    assert_eq!(
        t.dangling(),
        [
            "'check_findings'|'x1'",
            "'check_findings'|'x1'",
            "'check_findings'|'x2'",
            "'check_findings'|'x2'"
        ]
    );
    assert_eq!(
        t.rows(
            "SELECT COUNT(*) FROM check_findings WHERE album_id IN ('x1', 'x2') \
             AND resolved_at IS NULL"
        ),
        ["0"]
    );
}

#[test]
fn a_second_fold_is_a_no_op() {
    let t = full_library();
    let from = ids(&["x1", "x2"]);
    merge_album_rows(t.c(), &req(&from, "t")).unwrap();
    let after = t.dump();
    let again = merge_album_rows(t.c(), &req(&from, "t")).unwrap();
    assert_eq!(
        again,
        MergeReport {
            already_merged: true,
            ..MergeReport::default()
        }
    );
    assert_eq!(t.dump(), after);
}

#[test]
fn ai_review_stamp_takes_the_latest_including_the_survivors() {
    let t = T::new();
    t.album("t").album("x");
    t.set("t", "metadata_ai_acted_on_at", Some("2026-09-01T00:00:00Z"))
        .set("x", "metadata_ai_acted_on_at", Some("2026-02-01T00:00:00Z"));
    let from = ids(&["x"]);
    let r = merge_album_rows(t.c(), &req(&from, "t")).unwrap();
    assert_eq!(
        r.metadata_ai_acted_on_at.as_deref(),
        Some("2026-09-01T00:00:00Z")
    );
    let t = T::new();
    t.album("t").album("x");
    t.set("x", "metadata_ai_acted_on_at", Some("2026-02-01T00:00:00Z"));
    let r = merge_album_rows(t.c(), &req(&from, "t")).unwrap();
    assert_eq!(
        r.metadata_ai_acted_on_at.as_deref(),
        Some("2026-02-01T00:00:00Z")
    );
    assert_eq!(
        t.get("SELECT metadata_ai_acted_on_at FROM albums WHERE id = 't'"),
        Some("2026-02-01T00:00:00Z".into())
    );
}

// ---------------- anchors ----------------

fn anchor_of(t: &T) -> Vec<String> {
    t.rows(
        "SELECT mb_release_group_mbid, mb_release_mbid, mb_match_score, mb_last_checked_at \
         FROM albums WHERE id = 't'",
    )
}

#[test]
fn no_anchor_anywhere_clears_an_earlier_failed_lookup() {
    let t = T::new();
    t.album("t").album("x");
    t.set("t", "mb_last_checked_at", Some("t-failed-lookup"));
    let from = ids(&["x"]);
    let r = merge_album_rows(t.c(), &req(&from, "t")).unwrap();
    assert_eq!(
        (r.release_group, r.release, r.release_cleared),
        (None, None, false)
    );
    assert_eq!(anchor_of(&t), ["NULL|NULL|NULL|NULL"]);
}

#[test]
fn the_survivor_takes_the_one_anchor_a_member_has() {
    let t = T::new();
    t.album("t").album("x1").album("x2");
    t.anchor("x2", "rg", Some("rel"), 90);
    let from = ids(&["x1", "x2"]);
    let r = merge_album_rows(t.c(), &req(&from, "t")).unwrap();
    assert_eq!(r.release_group.as_deref(), Some("rg"));
    assert_eq!(r.release.as_deref(), Some("rel"));
    assert!(!r.release_cleared);
    // Last checked cleared: the next resolve re-confirms the group for the set.
    assert_eq!(anchor_of(&t), ["'rg'|'rel'|90|NULL"]);
}

#[test]
fn members_that_agree_keep_group_and_release_with_the_lowest_score() {
    let t = T::new();
    t.album("t").album("x");
    t.anchor("t", "rg", Some("rel"), 95)
        .anchor("x", "rg", Some("rel"), 80);
    let from = ids(&["x"]);
    merge_album_rows(t.c(), &req(&from, "t")).unwrap();
    assert_eq!(anchor_of(&t), ["'rg'|'rel'|80|NULL"]);
}

#[test]
fn members_naming_different_releases_keep_the_group_and_clear_the_release() {
    let t = T::new();
    t.album("t").album("x1").album("x2");
    t.anchor("t", "rg", Some("rel-a"), 95)
        .anchor("x1", "rg", None, 85)
        .anchor("x2", "rg", Some("rel-b"), 90);
    let from = ids(&["x1", "x2"]);
    let r = merge_album_rows(t.c(), &req(&from, "t")).unwrap();
    assert!(r.release_cleared);
    assert_eq!(r.release, None);
    assert_eq!(anchor_of(&t), ["'rg'|NULL|85|NULL"]);
}

#[test]
fn members_anchored_to_different_groups_are_refused() {
    let t = T::new();
    t.album("t").album("x");
    t.anchor("t", "rg-a", None, 90)
        .anchor("x", "rg-b", None, 90);
    let from = ids(&["x"]);
    assert_eq!(
        refused(&t, |c| merge_album_rows(c, &req(&from, "t"))),
        MergeRefusal::ReleaseGroupsDiffer {
            anchors: vec![("t".into(), "rg-a".into()), ("x".into(), "rg-b".into())]
        }
    );
}

#[test]
fn the_agreed_group_blocked_on_a_member_is_refused() {
    let t = T::new();
    t.album("t").album("x");
    t.anchor("t", "rg", None, 90).block("x", "rg", "t-block");
    let from = ids(&["x"]);
    assert_eq!(
        refused(&t, |c| merge_album_rows(c, &req(&from, "t"))),
        MergeRefusal::ReleaseGroupBlocked {
            release_group: "rg".into(),
            blocked_on: "x".into()
        }
    );
}

// ---------------- refusals ----------------

#[test]
fn malformed_requests_are_refused() {
    let t = T::new();
    t.album("t").album("x");
    let none: Vec<String> = vec![];
    assert_eq!(
        refused(&t, |c| merge_album_rows(c, &req(&none, "t"))),
        MergeRefusal::NoMembers
    );
    let empty = ids(&[""]);
    assert!(matches!(
        refused(&t, |c| merge_album_rows(c, &req(&empty, "t"))),
        MergeRefusal::Empty(_)
    ));
    let x = ids(&["x"]);
    assert!(matches!(
        refused(&t, |c| merge_album_rows(c, &req(&x, ""))),
        MergeRefusal::Empty(_)
    ));
    let twice = ids(&["x", "x"]);
    assert_eq!(
        refused(&t, |c| merge_album_rows(c, &req(&twice, "t"))),
        MergeRefusal::ListedTwice("x".into())
    );
    let with_into = ids(&["x", "t"]);
    assert_eq!(
        refused(&t, |c| merge_album_rows(c, &req(&with_into, "t"))),
        MergeRefusal::IntoAmongFrom("t".into())
    );
    let stranger = ids(&["t"]);
    assert_eq!(
        refused(&t, |c| merge_album_rows(
            c,
            &MergeRequest {
                emptied: &stranger,
                ..req(&x, "t")
            }
        )),
        MergeRefusal::EmptiedNotAMember("t".into())
    );
}

#[test]
fn unknown_ids_are_refused() {
    let t = T::new();
    t.album("t").album("x");
    let x = ids(&["x"]);
    assert_eq!(
        refused(&t, |c| merge_album_rows(c, &req(&x, "nope"))),
        MergeRefusal::UnknownAlbum("nope".into())
    );
    // One known, one not: no fold leaves that state.
    let mixed = ids(&["x", "nope"]);
    assert_eq!(
        refused(&t, |c| merge_album_rows(c, &req(&mixed, "t"))),
        MergeRefusal::UnknownAlbum("nope".into())
    );
    // An id whose album was removed some other way: rows still refer to it.
    t.finding("no-cover", "removed", "No cover", "open");
    let removed = ids(&["removed"]);
    assert_eq!(
        refused(&t, |c| merge_album_rows(c, &req(&removed, "t"))),
        MergeRefusal::UnknownAlbum("removed".into())
    );
    t.c().execute("DELETE FROM check_findings", []).unwrap();
    t.job("j", "removed-too", "failed");
    let removed = ids(&["removed-too"]);
    assert_eq!(
        refused(&t, |c| merge_album_rows(c, &req(&removed, "t"))),
        MergeRefusal::UnknownAlbum("removed-too".into())
    );
    t.input("c", "removed-three");
    let removed = ids(&["removed-three"]);
    assert_eq!(
        refused(&t, |c| merge_album_rows(c, &req(&removed, "t"))),
        MergeRefusal::UnknownAlbum("removed-three".into())
    );
    // An assistant's proposal or change record holds an id the same way.
    t.proposal("pr", "removed-four");
    let removed = ids(&["removed-four"]);
    assert_eq!(
        refused(&t, |c| merge_album_rows(c, &req(&removed, "t"))),
        MergeRefusal::UnknownAlbum("removed-four".into())
    );
    t.change("ch", "removed-five");
    let removed = ids(&["removed-five"]);
    assert_eq!(
        refused(&t, |c| merge_album_rows(c, &req(&removed, "t"))),
        MergeRefusal::UnknownAlbum("removed-five".into())
    );
}

/// A rename (and a scan adoption, which moves the row the same way) takes
/// an assistant's proposals and change records with the album: the change
/// record's column and its after-image both, and a record written before
/// whose after-image alone names the album (its own change renamed the
/// folder) too. Proposals are expired: they were made against the old row.
#[test]
fn a_rename_moves_an_assistants_rows_and_expires_its_proposals() {
    let mut t = T::new();
    t.album("a").album("other");
    t.proposal("pr-a", "a")
        .proposal("pr-other", "other")
        .change("ch-a", "a")
        .change("ch-other", "other");
    // A change made on "old" that renamed the folder to "a".
    t.change("ch-renamed", "old");
    t.c()
        .execute(
            "UPDATE agent_changes SET after = json_set(after, '$.albumId', 'a') \
             WHERE id = 'ch-renamed'",
            [],
        )
        .unwrap();
    // A change still running: no after-image yet.
    t.c()
        .execute(
            "INSERT INTO agent_changes (id, proposal_id, album_id, created_at, state, before) \
             VALUES ('ch-running', 'pr', 'a', 't', 'recorded', '{}')",
            [],
        )
        .unwrap();

    let moved = rename_album_directory(t.db.connection_mut(), "a", "b", "/music/b", "b").unwrap();
    assert_eq!((moved.agent_proposals, moved.agent_changes), (1, 3));
    assert_eq!(
        t.rows("SELECT id, album_id, expires_at FROM agent_proposals ORDER BY id"),
        [
            "'pr-a'|'b'|'2026-11-18T11:00:00Z'",
            "'pr-other'|'other'|'2099-01-01T00:00:00Z'"
        ]
    );
    assert_eq!(
        t.rows(
            "SELECT id, album_id, json_extract(after, '$.albumId'), json_valid(after) \
             FROM agent_changes ORDER BY id"
        ),
        [
            "'ch-a'|'b'|'b'|1",
            "'ch-other'|'other'|'other'|1",
            "'ch-renamed'|'old'|'b'|1",
            "'ch-running'|'b'|NULL|NULL"
        ]
    );
    // The rest of the after-image is untouched.
    assert_eq!(
        t.get("SELECT json_extract(after, '$.folder') FROM agent_changes WHERE id = 'ch-a'"),
        Some("a".into())
    );
}

/// Rows of an assistant left under an id no album has hold that id: a scan
/// must not adopt another folder onto it.
#[test]
fn an_id_an_assistant_still_names_is_in_use() {
    let t = T::new();
    t.proposal("pr", "gone-proposal")
        .change("ch", "gone-change");
    let in_use = |id: &str| musaic_db::renames::album_key_in_use(t.c(), id, "/music/new").unwrap();
    assert!(in_use("gone-proposal"));
    assert!(in_use("gone-change"));
    assert!(!in_use("free"));
}

/// Every table with an `album_id` column is in the one inventory of album
/// id holders, so a rename, an adoption and a merge all know about it.
#[test]
fn every_table_with_an_album_id_is_a_listed_holder() {
    let t = T::new();
    let tables = t.rows(
        "SELECT m.name FROM sqlite_master m, pragma_table_info(m.name) p \
         WHERE m.type = 'table' AND p.name = 'album_id' ORDER BY m.name",
    );
    let mut listed: Vec<String> = ALBUM_ID_HOLDERS.iter().map(|t| format!("'{t}'")).collect();
    listed.sort();
    assert_eq!(tables, listed);
}

#[test]
fn an_id_nothing_refers_to_is_reported_as_already_merged_and_nothing_changes() {
    let t = T::new();
    t.album("t");
    let before = t.dump();
    let ghost = ids(&["never-was"]);
    let r = merge_album_rows(t.c(), &req(&ghost, "t")).unwrap();
    assert!(r.already_merged);
    assert_eq!(t.dump(), before);
}

#[test]
fn a_member_in_another_library_is_refused() {
    let t = T::new();
    t.album("t").album_in("o", OTHER_LIB);
    let o = ids(&["o"]);
    assert_eq!(
        refused(&t, |c| merge_album_rows(c, &req(&o, "t"))),
        MergeRefusal::OtherLibrary {
            album_id: "o".into(),
            library: OTHER_LIB.into(),
            into_library: LIB.into()
        }
    );
}

#[test]
fn missing_members_are_refused_unless_emptied_by_this_merge() {
    let t = T::new();
    t.album("t").album("x").album("y");
    t.set("x", "missing_since", Some("t-gone"));
    let x = ids(&["x"]);
    assert_eq!(
        refused(&t, |c| merge_album_rows(c, &req(&x, "t"))),
        MergeRefusal::Missing {
            album_id: "x".into(),
            survivor: false
        }
    );
    // Vouching for another member does not cover this one.
    let xy = ids(&["x", "y"]);
    let y = ids(&["y"]);
    assert!(matches!(
        refused(&t, |c| merge_album_rows(
            c,
            &MergeRequest {
                emptied: &y,
                ..req(&xy, "t")
            }
        )),
        MergeRefusal::Missing { .. }
    ));
    // The survivor may never be Missing, whatever the caller says.
    t.set("t", "missing_since", Some("t-gone"));
    t.set("x", "missing_since", None);
    assert_eq!(
        refused(&t, |c| merge_album_rows(c, &req(&x, "t"))),
        MergeRefusal::Missing {
            album_id: "t".into(),
            survivor: true
        }
    );
    // The resume case: the caller emptied x, a scan then marked it Missing.
    t.set("t", "missing_since", None);
    t.set("x", "missing_since", Some("t-gone"));
    let r = merge_album_rows(
        t.c(),
        &MergeRequest {
            emptied: &x,
            ..req(&x, "t")
        },
    )
    .unwrap();
    assert_eq!(r.albums_deleted, 1);
    assert_eq!(
        t.get("SELECT missing_since FROM albums WHERE id = 't'"),
        None
    );
}

#[test]
fn file_rows_left_under_a_member_are_refused_per_table() {
    for (table, setup) in [
        ("track_fingerprints", 0),
        ("track_tags", 1),
        ("fingerprint_rejects", 2),
    ] {
        let t = T::new();
        t.album("t").album("x");
        match setup {
            0 => t.fp("x", "05.flac", "F"),
            1 => t.tag("x", "05.flac", "live", "c"),
            _ => t.reject("x", "05.flac", "s"),
        };
        let x = ids(&["x"]);
        assert_eq!(
            refused(&t, |c| merge_album_rows(c, &req(&x, "t"))),
            MergeRefusal::FileRowsLeft {
                album_id: "x".into(),
                rows: vec![(table, 1)]
            }
        );
        // Moving the file first lets the fold through.
        move_track_rows(t.c(), "x", "05.flac", "t", "17.flac").unwrap();
        merge_album_rows(t.c(), &req(&x, "t")).unwrap();
        t.assert_foreign_keys_hold();
        assert_eq!(t.dangling(), Vec::<String>::new());
    }
}

#[test]
fn an_active_job_on_any_member_is_refused_and_finished_ones_are_not() {
    for status in ["queued", "running", "completed"] {
        for on in ["x", "t"] {
            let t = T::new();
            t.album("t").album("x");
            t.job("j", on, status);
            let x = ids(&["x"]);
            // The refusal names the request's group in the old vocabulary
            // (an old running job was folded into a queued request).
            assert_eq!(
                refused(&t, |c| merge_album_rows(c, &req(&x, "t"))),
                MergeRefusal::ActiveJob {
                    album_id: on.into(),
                    job_id: "g-j".into(),
                    status: if status == "running" {
                        "queued"
                    } else {
                        status
                    }
                    .into()
                }
            );
        }
    }
    for status in ["failed", "acted_on"] {
        let t = T::new();
        t.album("t").album("x");
        t.job("j", "x", status);
        let x = ids(&["x"]);
        let r = merge_album_rows(t.c(), &req(&x, "t")).unwrap();
        assert_eq!(r.ai_jobs, 1);
        assert_eq!(t.rows("SELECT album_id FROM research_history"), ["'t'"]);
        assert_eq!(t.rows("SELECT album_id FROM delegated_work"), ["'t'"]);
    }
}

/// A native research request (no old job behind it) on `album`, moved to
/// `state` by hand (a held one gets the lease columns its state requires).
fn native_request(t: &T, album: &str, state: &str) {
    musaic_db::delegated_work::enqueue(
        t.c(),
        &[musaic_db::delegated_work::NewWork {
            id: "w-native",
            group_id: "g-native",
            kind: "metadata.identify",
            album_id: album,
            base_revision: "rev",
            input_json: "{}",
            input_digest: "input",
            max_attempts: musaic_db::delegated_work::DEFAULT_MAX_ATTEMPTS,
        }],
        1_000,
    )
    .unwrap();
    let held = matches!(state, "claimed" | "running");
    t.c()
        .execute(
            "UPDATE delegated_work SET state = ?1, principal_id = ?2, lease_token_hash = ?3, \
             lease_expires_at = ?4 WHERE id = 'w-native'",
            params![
                state,
                held.then_some("p"),
                held.then_some("h"),
                held.then_some(9_999_999_i64)
            ],
        )
        .unwrap();
}

#[test]
fn every_unresolved_request_state_refuses_and_finished_requests_follow_the_survivor() {
    for (state, status) in [
        ("queued", "queued"),
        ("claimed", "running"),
        ("running", "running"),
        ("awaiting_review", "completed"),
    ] {
        for on in ["x", "t"] {
            let t = T::new();
            t.album("t").album("x");
            native_request(&t, on, state);
            let x = ids(&["x"]);
            assert_eq!(
                refused(&t, |c| merge_album_rows(c, &req(&x, "t"))),
                MergeRefusal::ActiveJob {
                    album_id: on.into(),
                    job_id: "g-native".into(),
                    status: status.into(),
                },
                "{state} on {on}"
            );
        }
    }
    for state in ["applied", "discarded", "failed", "cancelled"] {
        let t = T::new();
        t.album("t").album("x");
        native_request(&t, "x", state);
        t.job("old", "x", "failed");
        let x = ids(&["x"]);
        let r = merge_album_rows(t.c(), &req(&x, "t")).unwrap();
        // One native request plus one old job (its record and request are
        // one job).
        assert_eq!(r.ai_jobs, 2, "{state}");
        assert_eq!(
            t.rows("SELECT id, album_id FROM delegated_work ORDER BY id"),
            ["'w-native'|'t'", "'w-old'|'t'"],
            "{state}"
        );
        assert_eq!(
            t.rows("SELECT id, album_id FROM research_history"),
            ["'old'|'t'"]
        );
        t.assert_foreign_keys_hold();
        assert_eq!(t.dangling(), Vec::<String>::new());
    }
}

#[test]
fn a_refusal_inside_the_callers_transaction_writes_nothing() {
    let t = full_library();
    t.fp("x1", "left.flac", "F");
    let before = t.dump();
    {
        let tx = musaic_db::immediate(t.c()).unwrap();
        let from = ids(&["x1", "x2"]);
        refusal(merge_album_rows_tx(&tx, &req(&from, "t")));
        tx.commit().unwrap();
    }
    assert_eq!(t.dump(), before);
}

// ---------------- the whole sequence, and resuming it ----------------

/// Three per-disc albums of one set: `d1` survives; `d2` and `d3` hold
/// their own files with fingerprints, tags and a reject.
fn set_of_three() -> T {
    let t = T::new();
    t.album("d1").album("d2").album("d3");
    t.fp("d1", "01.flac", "A1").fp("d1", "02.flac", "A2");
    t.fp("d2", "01.flac", "B1").fp("d2", "02.flac", "B2");
    t.tag("d2", "01.flac", "live", "c")
        .reject("d2", "03.flac", "s");
    t.fp("d3", "01.flac", "C1").tag("d3", "01.flac", "fav", "c");
    t.anchor("d1", "rg", Some("rel"), 90)
        .anchor("d3", "rg", Some("rel"), 70);
    t.block("d2", "rg-wrong", "t-b");
    t.finding("no-cover", "d2", "No cover", "ignored");
    t.job("j", "d3", "acted_on");
    t.input("no-cover", "d1").input("no-cover", "d2");
    t
}

const MOVES: [(&str, &str, &str); 6] = [
    ("d2", "01.flac", "03.flac"),
    ("d2", "02.flac", "04.flac"),
    ("d2", "03.flac", "05.flac"),
    ("d3", "01.flac", "06.flac"),
    ("d3", "02.flac", "07.flac"),
    ("d3", "03.flac", "08.flac"),
];

fn move_files(t: &T, which: &[(&str, &str, &str)]) {
    for (from, old, new) in which {
        move_track_rows(t.c(), from, old, "d1", new).unwrap();
    }
}

fn finish(t: &mut T) {
    let from = ids(&["d2", "d3"]);
    let r = merge_album_rows(t.c(), &req(&from, "d1")).unwrap();
    assert!(!r.already_merged);
    let again = merge_album_rows(t.c(), &req(&from, "d1")).unwrap();
    assert!(again.already_merged);
    rename_album_directory(
        t.db.connection_mut(),
        "d1",
        "set",
        &format!("{LIB}/set"),
        "set",
    )
    .unwrap();
}

#[test]
fn an_interrupted_merge_resumed_ends_where_an_uninterrupted_one_does() {
    let mut straight = set_of_three();
    move_files(&straight, &MOVES);
    finish(&mut straight);

    let mut resumed = set_of_three();
    let epoch = musaic_db::delegated_work::epoch(straight.c()).unwrap();
    // One epoch for both, the old job's request and its audit included.
    for table in [
        "library_epoch",
        "delegated_work",
        "delegation_audit",
        "delegation_outbox",
    ] {
        let column = if table == "library_epoch" {
            "epoch"
        } else {
            "library_epoch"
        };
        resumed
            .c()
            .execute(&format!("UPDATE {table} SET {column}=?1"), [&epoch])
            .unwrap();
    }
    move_files(&resumed, &MOVES[..2]);
    // Crash. Resume: every move again (the first two are no-ops now).
    move_files(&resumed, &MOVES);
    // A crash inside the fold's transaction, before its commit: nothing of
    // it is kept.
    {
        let tx = musaic_db::immediate(resumed.c()).unwrap();
        let from = ids(&["d2", "d3"]);
        merge_album_rows_tx(&tx, &req(&from, "d1")).unwrap();
        drop(tx);
    }
    assert_eq!(resumed.rows("SELECT COUNT(*) FROM albums"), ["3"]);
    move_files(&resumed, &MOVES);
    finish(&mut resumed);

    assert_eq!(resumed.dump(), straight.dump());
    straight.assert_foreign_keys_hold();
    assert_eq!(
        straight.rows("SELECT id, path FROM albums"),
        [format!("'set'|'{LIB}/set'")]
    );
    assert_eq!(
        straight.rows("SELECT album_id, filename, fingerprint FROM track_fingerprints ORDER BY 2"),
        [
            "'set'|'01.flac'|'A1'",
            "'set'|'02.flac'|'A2'",
            "'set'|'03.flac'|'B1'",
            "'set'|'04.flac'|'B2'",
            "'set'|'06.flac'|'C1'"
        ]
    );
    assert_eq!(
        straight.rows("SELECT album_id, filename, tag FROM track_tags ORDER BY 2"),
        ["'set'|'03.flac'|'live'", "'set'|'06.flac'|'fav'"]
    );
    assert_eq!(
        straight.rows("SELECT album_id, filename FROM fingerprint_rejects"),
        ["'set'|'05.flac'"]
    );
    assert_eq!(
        straight.rows("SELECT album_id, release_group_mbid FROM album_blocked_mb_release_groups"),
        ["'set'|'rg-wrong'"]
    );
    assert_eq!(
        straight.rows("SELECT album_id FROM research_history"),
        ["'set'"]
    );
    assert_eq!(straight.rows("SELECT COUNT(*) FROM check_inputs"), ["0"]);
    assert_eq!(
        straight.rows("SELECT mb_release_group_mbid, mb_release_mbid, mb_match_score FROM albums"),
        ["'rg'|'rel'|70"]
    );
    assert_eq!(straight.dangling(), ["'check_findings'|'d2'"]);
}

#[test]
fn a_scan_between_the_moves_and_the_fold_is_covered_by_emptied() {
    let t = set_of_three();
    move_files(&t, &MOVES);
    // A scan found d2 and d3 without a FLAC and marked them Missing.
    t.set("d2", "missing_since", Some("t-scan"))
        .set("d3", "missing_since", Some("t-scan"));
    let from = ids(&["d2", "d3"]);
    assert!(matches!(
        refused(&t, |c| merge_album_rows(c, &req(&from, "d1"))),
        MergeRefusal::Missing { .. }
    ));
    let r = merge_album_rows(
        t.c(),
        &MergeRequest {
            emptied: &from,
            ..req(&from, "d1")
        },
    )
    .unwrap();
    assert_eq!(r.albums_deleted, 2);
    t.assert_foreign_keys_hold();
}
