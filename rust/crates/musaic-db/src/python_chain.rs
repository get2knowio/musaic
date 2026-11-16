//! Databases older than v16: brought to v16
//! exactly as the Python app's `db.bootstrap()` at 6902c23 brings them
//! (musaic/db.py:580-622), then on through this build's v17 / v18 / v19 steps.
//!
//! The Python app's behaviour, ported as it is (not each historical version's
//! own code: what the Python app a user would last have run does to an old
//! file is the reference):
//!
//! 1. inside one transaction, every `SCHEMA_STATEMENTS` entry (all
//!    `CREATE ... IF NOT EXISTS`: tables the old file lacks are created with
//!    their *current* text and columns, existing ones are left alone);
//! 2. then each `MIGRATIONS[v]` for `found < v <= 16` in version order, each
//!    statement on its own; an error whose message contains "duplicate column
//!    name" is skipped (a column the step adds already came from step 1),
//!    any other error aborts and rolls the whole transaction back;
//! 3. `UPDATE schema_version SET version = 16`, commit.
//!
//! The only step that changes existing rows is v14 (musaic/db.py:522-540):
//! open findings of the five retired check ids get `resolved_at =
//! strftime('%Y-%m-%dT%H:%M:%fZ','now')`; ignored ones are left untouched
//! (a deliberate change: the Python app resolved them too). No step deletes a row or drops a
//! table or column. Recorded against the Python app in
//! testdata/golden/persistence/old-schemas/.

use rusqlite::Connection;

use crate::{PYTHON_SCHEMA_VERSION, Result};

/// The oldest schema version this build migrates: the Python app's chain starts
/// at `MIGRATIONS[2]`, i.e. from a v1 file. A version row below this (0 or
/// negative) is refused (`Error::TooOld`): the Python app treats 0 like a missing
/// row and inserts a second one, which no released build could have written.
pub const OLDEST_SCHEMA_VERSION: i64 = 1;

/// SQLite's message for an `ADD COLUMN` of a column that exists; the Python app
/// matches it case-insensitively (`"duplicate column name" in str(e).lower()`).
fn is_duplicate_column(e: &rusqlite::Error) -> bool {
    let msg = match e {
        rusqlite::Error::SqliteFailure(_, Some(m)) => m.clone(),
        other => other.to_string(),
    };
    msg.to_lowercase().contains("duplicate column name")
}

/// The Python app's v14 condition, and the one this build runs instead.
const V14_PYTHON_WHERE: &str = "WHERE resolved_at IS NULL";
const V14_KEEP_IGNORED_WHERE: &str = "WHERE resolved_at IS NULL AND ignored_at IS NULL";

/// The v14 step resolves only the open
/// findings of the five retired checks that the user had not ignored;
/// ignored ones are left untouched (the Python app stamped them resolved too).
/// The Python app's statement is kept verbatim in `schema_migrations` (generated)
/// and its WHERE clause narrowed here.
fn v14_keeping_ignored(stmt: &str) -> String {
    assert_eq!(
        stmt.matches(V14_PYTHON_WHERE).count(),
        1,
        "the Python app's v14 statement changed: {stmt}"
    );
    stmt.replace(V14_PYTHON_WHERE, V14_KEEP_IGNORED_WHERE)
}

/// Steps 1-3 above, in one transaction. The version is re-read inside it, so
/// a file another process already brought to v16 is left alone.
pub(crate) fn to_python_version(conn: &mut Connection) -> Result<()> {
    let tx = crate::immediate(conn)?;
    let current: i64 = tx.query_row("SELECT version FROM schema_version LIMIT 1", [], |r| {
        r.get(0)
    })?;
    if (OLDEST_SCHEMA_VERSION..PYTHON_SCHEMA_VERSION).contains(&current) {
        for stmt in crate::schema_v16::STATEMENTS {
            tx.execute(stmt, [])?;
        }
        for (version, stmts) in crate::schema_migrations::MIGRATIONS {
            if *version <= current || *version > PYTHON_SCHEMA_VERSION {
                continue;
            }
            for stmt in *stmts {
                let stmt = if *version == 14 {
                    v14_keeping_ignored(stmt)
                } else {
                    (*stmt).to_string()
                };
                match tx.execute(&stmt, []) {
                    Ok(_) => {}
                    Err(e) if is_duplicate_column(&e) => {}
                    Err(e) => return Err(e.into()),
                }
            }
        }
        tx.execute(
            "UPDATE schema_version SET version = ?1",
            [PYTHON_SCHEMA_VERSION],
        )?;
    }
    tx.commit()?;
    Ok(())
}
