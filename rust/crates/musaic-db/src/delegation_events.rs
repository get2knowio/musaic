//! The retained history of delegated-work changes that agents read through
//! `events/poll` (spec 005). A row is written by `delegated_work::record_change`
//! in the transaction that changes the work; nothing here updates a row, and
//! pruning never touches `delegated_work` or the audit (FR-021).

use rusqlite::{Connection, OptionalExtension, params};

use crate::Result;

/// How long a row is kept: seven days, in milliseconds.
pub const RETENTION_MS: i64 = 7 * 24 * 60 * 60 * 1000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventRow {
    pub sequence: i64,
    pub library_epoch: String,
    pub work_id: String,
    /// `requested` or `updated`.
    pub event_type: String,
    pub kind: String,
    pub state: String,
    pub revision: i64,
    pub committed_at: i64,
}

/// Write the event for `work_id` when it is new or its state differs from the
/// state its latest event reported. The row mirrors the stored work row, so
/// the caller records after updating it.
pub(crate) fn record(
    conn: &Connection,
    sequence: i64,
    epoch: &str,
    work_id: &str,
    now: i64,
) -> Result<()> {
    let current: Option<(String, String, i64)> = conn
        .query_row(
            "SELECT kind,state,revision FROM delegated_work WHERE id=?1",
            [work_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((kind, state, revision)) = current else {
        return Ok(());
    };
    let previous: Option<String> = conn
        .query_row(
            "SELECT state FROM delegation_events WHERE work_id=?1 ORDER BY sequence DESC LIMIT 1",
            [work_id],
            |r| r.get(0),
        )
        .optional()?;
    if previous.as_deref() == Some(state.as_str()) {
        return Ok(());
    }
    let event_type = if state == "queued" {
        "requested"
    } else {
        "updated"
    };
    conn.execute(
        "INSERT INTO delegation_events
           (sequence,library_epoch,work_id,event_type,kind,state,revision,committed_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
        params![
            sequence, epoch, work_id, event_type, kind, state, revision, now
        ],
    )?;
    Ok(())
}

/// The largest audit sequence of `epoch` (0 when it has none): the position a
/// fresh cursor starts at.
pub fn head(conn: &Connection, epoch: &str) -> Result<i64> {
    // The audit is never pruned and `library_epoch` has no index: walking the
    // rowid down stops at the newest row, which belongs to the current epoch,
    // where `MAX(sequence) ... WHERE` would read the whole table on every poll.
    Ok(conn
        .query_row(
            "SELECT sequence FROM delegation_audit WHERE library_epoch=?1
             ORDER BY sequence DESC LIMIT 1",
            [epoch],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(0))
}

/// The highest sequence ever pruned or removed by an epoch change.
pub fn pruned_through(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT pruned_through FROM delegation_event_state WHERE singleton=1",
        [],
        |r| r.get(0),
    )?)
}

/// The oldest retained row of `epoch`, if any.
pub fn oldest(conn: &Connection, epoch: &str) -> Result<Option<EventRow>> {
    let mut rows = page(conn, epoch, 0, &[], &[], 1)?;
    Ok(rows.pop())
}

/// The highest sequence among rows older than `cutoff_ms` (0 when none): the
/// floor a `maxAgeMs` request raises a cursor to.
pub fn age_floor(conn: &Connection, epoch: &str, cutoff_ms: i64) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COALESCE(MAX(sequence),0) FROM delegation_events
         WHERE library_epoch=?1 AND committed_at<?2",
        params![epoch, cutoff_ms],
        |r| r.get(0),
    )?)
}

/// Rows of `epoch` after `after`, by sequence, optionally limited to some
/// kinds and states (an empty slice means all).
pub fn page(
    conn: &Connection,
    epoch: &str,
    after: i64,
    kinds: &[&str],
    states: &[&str],
    limit: i64,
) -> Result<Vec<EventRow>> {
    let mut sql = String::from(
        "SELECT sequence,library_epoch,work_id,event_type,kind,state,revision,committed_at
         FROM delegation_events WHERE library_epoch=?1 AND sequence>?2",
    );
    let mut values: Vec<rusqlite::types::Value> = vec![epoch.to_owned().into(), after.into()];
    for (column, list) in [("kind", kinds), ("state", states)] {
        if !list.is_empty() {
            let marks = list
                .iter()
                .map(|v| {
                    values.push((*v).to_owned().into());
                    format!("?{}", values.len())
                })
                .collect::<Vec<_>>()
                .join(",");
            sql.push_str(&format!(" AND {column} IN ({marks})"));
        }
    }
    values.push(limit.into());
    sql.push_str(&format!(" ORDER BY sequence LIMIT ?{}", values.len()));
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(values), |r| {
        Ok(EventRow {
            sequence: r.get(0)?,
            library_epoch: r.get(1)?,
            work_id: r.get(2)?,
            event_type: r.get(3)?,
            kind: r.get(4)?,
            state: r.get(5)?,
            revision: r.get(6)?,
            committed_at: r.get(7)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Delete rows committed before `before_ms` and advance the watermark in the
/// same transaction. Returns how many rows went.
pub fn prune(conn: &Connection, before_ms: i64) -> Result<usize> {
    let tx = crate::immediate(conn)?;
    let top: Option<i64> = tx.query_row(
        "SELECT MAX(sequence) FROM delegation_events WHERE committed_at<?1",
        [before_ms],
        |r| r.get(0),
    )?;
    let Some(top) = top else {
        return Ok(0);
    };
    let n = tx.execute(
        "DELETE FROM delegation_events WHERE committed_at<?1",
        [before_ms],
    )?;
    raise_watermark(&tx, top)?;
    tx.commit()?;
    Ok(n)
}

fn raise_watermark(conn: &Connection, through: i64) -> Result<()> {
    conn.execute(
        "UPDATE delegation_event_state SET pruned_through=MAX(pruned_through,?1) WHERE singleton=1",
        [through],
    )?;
    Ok(())
}

/// Remove the rows of every epoch but `current` (in the rotation transaction).
pub(crate) fn drop_other_epochs(conn: &Connection, current: &str) -> Result<()> {
    let top: Option<i64> = conn.query_row(
        "SELECT MAX(sequence) FROM delegation_events WHERE library_epoch<>?1",
        [current],
        |r| r.get(0),
    )?;
    if let Some(top) = top {
        conn.execute(
            "DELETE FROM delegation_events WHERE library_epoch<>?1",
            [current],
        )?;
        raise_watermark(conn, top)?;
    }
    Ok(())
}
