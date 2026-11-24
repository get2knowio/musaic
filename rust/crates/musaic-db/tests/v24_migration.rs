//! The v23 -> v24 step (research history), on every committed fixture that
//! holds old jobs: the Python app's `old-schemas/v04…v16.db` and the
//! synthetic `db/v20-jobs.db`, `v22-jobs.db`, `v23-jobs.db`. Each is copied
//! to a temp dir before it is opened.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use musaic_db::{Db, Error, Opened, SCHEMA_VERSION, backup, research_history};
use rusqlite::Connection;
use rusqlite::types::ValueRef;

const HISTORY_COLUMNS: &str = "id, album_id, album_path, COALESCE(album_title, ''), job_type, \
     COALESCE(hint, ''), status, acted_on_action, error, result_json, created_at, started_at, \
     completed_at, acted_on_at, COALESCE(attach_cover, 1)";

fn fixtures() -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = (4..=16)
        .map(|v| {
            let name = format!("v{v:02}.db");
            let p = musaic_testdata::public(format!("golden/persistence/old-schemas/{name}"));
            (name, p)
        })
        .collect();
    for name in ["v20-jobs.db", "v22-jobs.db", "v23-jobs.db"] {
        out.push((name.into(), musaic_testdata::public(format!("db/{name}"))));
    }
    out
}

fn copy(src: &Path, dir: &Path, name: &str) -> PathBuf {
    let dest = dir.join(name);
    std::fs::copy(src, &dest).unwrap();
    dest
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

fn rows(c: &Connection, sql: &str) -> Vec<String> {
    let mut stmt = c.prepare(sql).unwrap();
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

fn one(c: &Connection, sql: &str) -> i64 {
    c.query_row(sql, [], |r| r.get(0)).unwrap()
}

fn version(c: &Connection) -> i64 {
    one(c, "SELECT version FROM schema_version")
}

fn has_table(c: &Connection, name: &str) -> bool {
    c.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
        [name],
        |r| r.get(0),
    )
    .unwrap()
}

/// The schema and every row of every table, sorted.
fn dump(c: &Connection) -> String {
    let mut out = rows(
        c,
        "SELECT type, name, sql FROM sqlite_master WHERE name NOT LIKE 'sqlite_%' ORDER BY name",
    )
    .join("\n");
    for t in rows(
        c,
        "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
    ) {
        let t = t.trim_matches('\'');
        let mut r = rows(c, &format!("SELECT * FROM \"{t}\""));
        r.sort();
        out.push_str(&format!("\n[{t}]\n{}", r.join("\n")));
    }
    out
}

/// The requests an old job is folded into: `(kind, state)`, as the v22
/// mapping makes them.
fn expected_requests(job_type: &str, status: &str, action: &str) -> Vec<(String, String)> {
    let kinds: &[&str] = match job_type {
        "metadata" => &["metadata.identify"],
        "metadata+cover" => &["cover.research", "metadata.identify"],
        "cover" => &["cover.research"],
        _ => &[],
    };
    let state = match (status, action) {
        ("queued" | "running", _) => "queued",
        ("completed", _) => "awaiting_review",
        ("failed", _) => "failed",
        ("acted_on", "'applied'") => "applied",
        ("acted_on", _) => "discarded",
        _ => "failed",
    };
    kinds
        .iter()
        .map(|k| (k.to_string(), state.to_string()))
        .collect()
}

/// A copy of `src` brought to v23 by this build's own steps (an older file)
/// or left as it is (a v23 file): what the v24 step starts from.
fn at_v23(src: &Path, dir: &Path) -> Connection {
    let p = copy(src, dir, "before.db");
    let mut c = Connection::open(&p).unwrap();
    musaic_db::configure_connection(&c).unwrap();
    musaic_db::migrate::upgrade_to(&mut c, 23).unwrap();
    assert_eq!(version(&c), 23);
    c
}

#[test]
fn every_fixture_keeps_each_old_job_as_history_and_its_requests() {
    for (name, src) in fixtures() {
        let dir = tempfile::tempdir().unwrap();
        let before = at_v23(&src, dir.path());
        let jobs = rows(
            &before,
            &format!("SELECT {HISTORY_COLUMNS} FROM ai_jobs ORDER BY id"),
        );
        assert!(!jobs.is_empty(), "{name}: no old jobs");
        let requests_before = rows(
            &before,
            "SELECT id, group_id, kind, album_id, state, base_revision, created_at, updated_at, \
             last_error, legacy_job_id FROM delegated_work ORDER BY id",
        );
        let albums_before = rows(&before, "SELECT * FROM albums ORDER BY id");
        // Read from a copy: a fixture is never opened in place.
        let original_version = {
            let raw = Connection::open(copy(&src, dir.path(), "probe.db")).unwrap();
            version(&raw)
        };

        let live = copy(&src, dir.path(), "library.db");
        let (db, opened) = Db::open(&live).unwrap();
        assert!(
            matches!(opened, Opened::Migrated { from, .. } if from == original_version),
            "{name}: {opened:?}"
        );
        let c = db.connection();
        assert_eq!(version(c), 24, "{name}");
        assert!(!has_table(c, "ai_jobs"), "{name}");

        // Title, hint, times and outcome as they were, for every status and
        // type, unreadable results and jobs of gone albums included.
        assert_eq!(
            rows(
                c,
                &format!("SELECT {HISTORY_COLUMNS} FROM research_history ORDER BY id")
            ),
            jobs,
            "{name}: history"
        );
        // No album row is read away or deleted.
        assert_eq!(
            rows(c, "SELECT * FROM albums ORDER BY id"),
            albums_before,
            "{name}"
        );
        // Requests that existed before (v22 and v23 files) are untouched: a
        // request already waiting stays waiting, under the same id. An older
        // file got its requests from the v22 step in each copy, under fresh
        // ids, so only the rest is compared there.
        let requests_after = rows(
            c,
            "SELECT id, group_id, kind, album_id, state, base_revision, created_at, updated_at, \
             last_error, legacy_job_id FROM delegated_work ORDER BY id",
        );
        if original_version >= 22 {
            assert_eq!(requests_after, requests_before, "{name}: requests");
        } else {
            let without_ids = |v: &[String]| {
                let mut out: Vec<String> = v
                    .iter()
                    .map(|r| r.splitn(3, '|').nth(2).unwrap().to_string())
                    .collect();
                out.sort();
                out
            };
            assert_eq!(
                without_ids(&requests_after),
                without_ids(&requests_before),
                "{name}: requests"
            );
        }
        // Each old job has the requests its type and status map to.
        for job in rows(
            c,
            "SELECT id, job_type, status, acted_on_action FROM research_history",
        ) {
            let f: Vec<&str> = job.split('|').collect();
            let id = f[0].trim_matches('\'');
            let mut got: Vec<(String, String)> = rows(
                c,
                &format!(
                    "SELECT kind, state FROM delegated_work WHERE legacy_job_id = '{id}' \
                     ORDER BY kind"
                ),
            )
            .into_iter()
            .map(|r| {
                let (k, s) = r.split_once('|').unwrap();
                (k.trim_matches('\'').into(), s.trim_matches('\'').into())
            })
            .collect();
            got.sort();
            assert_eq!(
                got,
                expected_requests(f[1].trim_matches('\''), f[2].trim_matches('\''), f[3]),
                "{name}: {job}"
            );
        }
        for key in [
            "ai_provider_id",
            "ai_model_id",
            "ai_max_parallel",
            "ai_queue_paused",
        ] {
            assert_eq!(
                one(
                    c,
                    &format!("SELECT COUNT(*) FROM settings WHERE key = '{key}'")
                ),
                0,
                "{name}: {key}"
            );
        }
        assert!(rows(c, "PRAGMA foreign_key_check").is_empty(), "{name}");
    }
}

#[test]
fn the_synthetic_fixtures_keep_odd_rows_and_history_only_records() {
    for name in ["v20-jobs.db", "v22-jobs.db", "v23-jobs.db"] {
        let dir = tempfile::tempdir().unwrap();
        let live = copy(
            &musaic_testdata::public(format!("db/{name}")),
            dir.path(),
            "library.db",
        );
        let (db, _) = Db::open(&live).unwrap();
        let c = db.connection();
        assert_eq!(
            one(c, "SELECT COUNT(*) FROM research_history"),
            10,
            "{name}"
        );
        // A result that is not JSON is kept as stored.
        let a10 = research_history::get(c, "0000000000000a10")
            .unwrap()
            .unwrap();
        assert_eq!(a10.result_json.as_deref(), Some("not json {"), "{name}");
        // A job whose album is gone is kept, and still has its request.
        let a09 = research_history::get(c, "0000000000000a09")
            .unwrap()
            .unwrap();
        assert_eq!(a09.album_id, "a0000000000000ff");
        assert_eq!(
            one(
                c,
                "SELECT COUNT(*) FROM albums WHERE id = 'a0000000000000ff'"
            ),
            0
        );
        assert_eq!(
            one(
                c,
                "SELECT COUNT(*) FROM delegated_work WHERE legacy_job_id = '0000000000000a09'"
            ),
            1
        );
        // `lyrics` is history-only: no request, never listed to an agent.
        let only: Vec<String> = research_history::list_history_only(c, None, 100)
            .unwrap()
            .into_iter()
            .map(|j| j.id)
            .collect();
        assert_eq!(only, ["0000000000000a08"], "{name}");
        // The unsupported old status becomes a failed request saying so.
        assert_eq!(
            rows(
                c,
                "SELECT state, last_error FROM delegated_work \
                 WHERE legacy_job_id = '0000000000000a07'"
            ),
            ["'failed'|'legacy_state_unsupported'"],
            "{name}"
        );
        // The queued job's request is waiting.
        assert_eq!(
            rows(
                c,
                "SELECT state FROM delegated_work WHERE legacy_job_id = '0000000000000a01'"
            ),
            ["'queued'"],
            "{name}"
        );
    }
}

#[test]
fn a_second_open_changes_nothing() {
    for (name, src) in fixtures() {
        let dir = tempfile::tempdir().unwrap();
        let live = copy(&src, dir.path(), "library.db");
        let (db, _) = Db::open(&live).unwrap();
        let first = dump(db.connection());
        drop(db);
        let (db, opened) = Db::open(&live).unwrap();
        assert_eq!(opened, Opened::Current, "{name}");
        assert_eq!(dump(db.connection()), first, "{name}");
    }
}

#[test]
fn an_interrupted_step_commits_nothing_and_the_next_open_completes_it() {
    let dir = tempfile::tempdir().unwrap();
    let live = copy(
        &musaic_testdata::public("db/v23-jobs.db"),
        dir.path(),
        "library.db",
    );
    let pristine = {
        let c = Connection::open(&live).unwrap();
        c.execute_batch(
            "CREATE TRIGGER migration_interrupt BEFORE UPDATE ON schema_version
             WHEN NEW.version=24 BEGIN SELECT RAISE(ABORT,'interrupted upgrade'); END;",
        )
        .unwrap();
        dump(&c)
    };
    let err = Db::open(&live).err().unwrap();
    assert!(err.to_string().contains("interrupted upgrade"), "{err}");
    {
        let c = Connection::open(&live).unwrap();
        assert_eq!(version(&c), 23);
        assert!(has_table(&c, "ai_jobs"));
        assert!(!has_table(&c, "research_history"));
        assert_eq!(dump(&c), pristine);
        c.execute_batch("DROP TRIGGER migration_interrupt;")
            .unwrap();
    }
    let (db, opened) = Db::open(&live).unwrap();
    assert_eq!(
        opened,
        Opened::Migrated {
            from: 23,
            backup: None
        }
    );
    let c = db.connection();
    assert_eq!(version(c), 24);
    assert_eq!(one(c, "SELECT COUNT(*) FROM research_history"), 10);
    assert!(!has_table(c, "ai_jobs"));
}

/// A build one schema step behind refuses a file this build wrote: the file
/// is made one version newer than this build's constant, which is what a v24
/// file is to a build whose constant is 23.
#[test]
fn a_file_newer_than_the_build_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let live = copy(
        &musaic_testdata::public("db/v23-jobs.db"),
        dir.path(),
        "library.db",
    );
    drop(Db::open(&live).unwrap());
    let newer = SCHEMA_VERSION + 1;
    Connection::open(&live)
        .unwrap()
        .execute("UPDATE schema_version SET version = ?1", [newer])
        .unwrap();
    let before = std::fs::read(&live).unwrap();
    match Db::open(&live) {
        Err(Error::NewerSchema { found, supported }) => {
            assert_eq!((found, supported), (newer, SCHEMA_VERSION));
        }
        other => panic!("expected NewerSchema, got {:?}", other.map(|_| ())),
    }
    assert_eq!(std::fs::read(&live).unwrap(), before);
    assert!(matches!(
        backup::validate_upload(&live),
        Err(backup::RestoreError::Newer { .. })
    ));
}

#[test]
fn a_restored_v20_backup_migrates_through_v24() {
    let t = tempfile::tempdir().unwrap();
    let cfg = musaic_testdata::canonical(t.path());
    let db_path = cfg.join("library.db");
    std::fs::copy(musaic_testdata::public("db/v16-empty.db"), &db_path).unwrap();
    let (db, _) = Db::open(&db_path).unwrap();
    let upload = copy(
        &musaic_testdata::public("db/v20-jobs.db"),
        &cfg,
        "upload-v20.db",
    );
    let info = backup::validate_upload(&upload).unwrap();
    assert_eq!(info.schema_version, 20);
    // Counted from the old job table before the restore.
    assert_eq!(info.counts["ai_jobs"], 10);
    let (db, info) =
        backup::restore_from(db, &upload, "20260106-000000Z", SystemTime::now()).unwrap();
    assert!(matches!(info.opened, Opened::Migrated { from: 20, .. }));
    let c = db.connection();
    assert_eq!(version(c), SCHEMA_VERSION);
    assert!(!has_table(c, "ai_jobs"));
    assert_eq!(one(c, "SELECT COUNT(*) FROM research_history"), 10);
    // Eleven requests (a combined job makes two, `lyrics` none) plus the
    // one history-only record.
    assert_eq!(backup::gather_counts(c)["ai_jobs"], 12);
}
