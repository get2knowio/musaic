#![allow(dead_code)]

use std::path::{Path, PathBuf};

use musaic_db::Db;
use serde_json::Value;

pub fn golden(name: &str) -> Value {
    let p = musaic_testdata::public("golden/persistence").join(name);
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

/// A private copy of a fixture database as `<tmp>/library.db`, opened (and so
/// migrated to v17, with its pre-migration backup beside it).
pub fn open_copy(fixture: &str) -> (tempfile::TempDir, PathBuf, Db) {
    let t = tempfile::tempdir().unwrap();
    let dir = musaic_testdata::canonical(t.path());
    let p = dir.join("library.db");
    std::fs::copy(musaic_testdata::public("db").join(fixture), &p).unwrap();
    let (db, _) = Db::open(&p).unwrap();
    (t, dir, db)
}

pub fn contains(haystack: &[u8], needle: &str) -> bool {
    let n = needle.as_bytes();
    haystack.windows(n.len()).any(|w| w == n)
}

pub fn file_contains(p: &Path, needle: &str) -> bool {
    std::fs::read(p)
        .map(|b| contains(&b, needle))
        .unwrap_or(false)
}

pub fn settings_rows(conn: &rusqlite::Connection) -> serde_json::Map<String, Value> {
    let mut stmt = conn.prepare("SELECT key, value FROM settings").unwrap();
    stmt.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, Value::String(r.get(1)?)))
    })
    .unwrap()
    .map(Result::unwrap)
    .collect()
}

/// Every file under `dir` (recursively) whose bytes contain `needle`.
pub fn files_containing(dir: &Path, needle: &str) -> Vec<PathBuf> {
    let mut hits = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else if file_contains(&p, needle) {
                hits.push(p);
            }
        }
    }
    hits
}

/// Every file name under `dir` (non-recursive), sorted.
pub fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    v.sort();
    v
}

/// Schema SQL plus every table's rows (sorted), for whole-database comparisons.
pub fn dump(conn: &rusqlite::Connection) -> Vec<(String, Vec<String>)> {
    let mut stmt = conn
        .prepare("SELECT name, sql FROM sqlite_master WHERE name NOT LIKE 'sqlite_%' ORDER BY name")
        .unwrap();
    let objs: Vec<(String, Option<String>)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let mut out = Vec::new();
    for (name, sql) in objs {
        let mut rows = vec![format!("SQL {}", sql.clone().unwrap_or_default())];
        let is_table: bool = conn
            .query_row(
                "SELECT type = 'table' FROM sqlite_master WHERE name = ?1",
                [&name],
                |r| r.get(0),
            )
            .unwrap();
        if is_table {
            let mut st = conn.prepare(&format!("SELECT * FROM \"{name}\"")).unwrap();
            let n = st.column_count();
            let mut data: Vec<String> = st
                .query_map([], |r| {
                    Ok((0..n)
                        .map(|i| format!("{:?}", r.get_ref(i).unwrap()))
                        .collect::<Vec<_>>()
                        .join("|"))
                })
                .unwrap()
                .map(Result::unwrap)
                .collect();
            data.sort();
            rows.extend(data);
        }
        out.push((name, rows));
    }
    out
}
