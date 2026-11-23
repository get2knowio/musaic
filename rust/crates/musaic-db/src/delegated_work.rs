//! The delegated work queue. Decisions, audit, replay and notification references
//! commit together. The app owns clock policy, authentication and work context.

use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::{Result, immediate};

const LEASE_MS: i64 = 5 * 60 * 1000;
const UNRESOLVED: &str = "('queued','claimed','running','awaiting_review')";
pub const DEFAULT_MAX_ATTEMPTS: i64 = 3;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct WorkRow {
    pub id: String,
    pub group_id: String,
    pub kind: String,
    pub album_id: String,
    pub library_epoch: String,
    pub base_revision: String,
    pub state: String,
    pub revision: i64,
    pub input_json: String,
    pub input_digest: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub available_at: i64,
    pub principal_id: Option<String>,
    #[serde(skip)]
    pub lease_token_hash: Option<String>,
    pub lease_expires_at: Option<i64>,
    pub claim_generation: i64,
    pub attempts: i64,
    pub max_attempts: i64,
    pub last_error: Option<String>,
    pub result_id: Option<String>,
    pub legacy_job_id: Option<String>,
    pub review_reserved: bool,
}

pub(crate) fn work(row: &Row<'_>) -> rusqlite::Result<WorkRow> {
    Ok(WorkRow {
        id: row.get("id")?,
        group_id: row.get("group_id")?,
        kind: row.get("kind")?,
        album_id: row.get("album_id")?,
        library_epoch: row.get("library_epoch")?,
        base_revision: row.get("base_revision")?,
        state: row.get("state")?,
        revision: row.get("revision")?,
        input_json: row.get("input_json")?,
        input_digest: row.get("input_digest")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
        available_at: row.get("available_at")?,
        principal_id: row.get("principal_id")?,
        lease_token_hash: row.get("lease_token_hash")?,
        lease_expires_at: row.get("lease_expires_at")?,
        claim_generation: row.get("claim_generation")?,
        attempts: row.get("attempts")?,
        max_attempts: row.get("max_attempts")?,
        last_error: row.get("last_error")?,
        result_id: row.get("result_id")?,
        legacy_job_id: row.get("legacy_job_id")?,
        review_reserved: row.get("review_reserved")?,
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResultRow {
    pub id: String,
    pub work_id: String,
    pub input_digest: String,
    pub digest: String,
    pub result_json: String,
    pub validation: String,
    pub created_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    NotFound,
    AlreadyClaimed,
    LeaseExpired,
    StaleGeneration,
    StaleContext,
    InvalidResult,
    PolicyDenied,
    RateLimited,
    IdempotencyConflict,
}

pub type QueueResult<T> = Result<std::result::Result<T, Refusal>>;

pub struct NewWork<'a> {
    pub id: &'a str,
    pub group_id: &'a str,
    pub kind: &'a str,
    pub album_id: &'a str,
    pub base_revision: &'a str,
    pub input_json: &'a str,
    pub input_digest: &'a str,
    pub max_attempts: i64,
}

pub struct List<'a> {
    pub principal_id: &'a str,
    pub state: Option<&'a str>,
    pub kind: Option<&'a str>,
    pub after: Option<&'a str>,
    pub limit: i64,
}

pub fn epoch(conn: &Connection) -> Result<String> {
    Ok(conn.query_row(
        "SELECT epoch FROM library_epoch WHERE singleton=1",
        [],
        |r| r.get(0),
    )?)
}

fn uuid(conn: &Connection) -> Result<String> {
    Ok(conn.query_row(
        "SELECT lower(hex(randomblob(4)) || '-' || hex(randomblob(2)) ||          '-4' || substr(hex(randomblob(2)),2) || '-' ||          substr('89ab', (random() & 3) + 1, 1) || substr(hex(randomblob(2)),2) || '-' ||          hex(randomblob(6)))",
        [],
        |r| r.get(0),
    )?)
}

pub(crate) fn migrate_legacy(tx: &Transaction<'_>) -> Result<()> {
    let epoch = uuid(tx)?;
    tx.execute(
        "INSERT INTO library_epoch(singleton, epoch) VALUES (1, ?1)",
        [&epoch],
    )?;
    let mut stmt = tx.prepare(
        "SELECT id, album_id, job_type, status, acted_on_action, error,
                COALESCE(CAST(unixepoch(created_at, 'subsec') * 1000 AS INTEGER),0),
                COALESCE(CAST(unixepoch(COALESCE(acted_on_at,completed_at,started_at,created_at),
                          'subsec') * 1000 AS INTEGER),0)
         FROM ai_jobs ORDER BY created_at, id",
    )?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let legacy: String = row.get(0)?;
        let album: String = row.get(1)?;
        let job_type: String = row.get(2)?;
        let kinds: &[&str] = match job_type.as_str() {
            "metadata" => &["metadata.identify"],
            "metadata+cover" => &["metadata.identify", "cover.research"],
            "cover" => &["cover.research"],
            _ => continue,
        };
        let action: Option<String> = row.get(4)?;
        let error: Option<String> = row.get(5)?;
        let status: String = row.get(3)?;
        let (state, error) = match (status.as_str(), action.as_deref()) {
            ("queued" | "running", _) => ("queued", None),
            ("completed", _) => ("awaiting_review", Some("legacy_result_unconverted")),
            ("failed", _) => ("failed", error.as_deref()),
            ("acted_on", Some("applied")) => ("applied", None),
            ("acted_on", _) => ("discarded", None),
            _ => ("failed", Some("legacy_state_unsupported")),
        };
        let created: i64 = row.get(6)?;
        let updated: i64 = row.get(7)?;
        let group = uuid(tx)?;
        for kind in kinds {
            let id = uuid(tx)?;
            // Hydrated by the app from current album data before delegation.
            // Raw legacy context stays in the unmodified ai_jobs row.
            let digest = format!("legacy:{legacy}:{kind}");
            tx.execute(
                "INSERT INTO delegated_work
                     (id,group_id,kind,album_id,library_epoch,base_revision,state,input_json,
                      input_digest,created_at,updated_at,available_at,max_attempts,last_error,legacy_job_id)
                 VALUES (?1,?2,?3,?4,?5,'',?6,'{}',?7,?8,?9,?9,?10,?11,?12)",
                params![id,group,kind,album,epoch,state,digest,created,updated,DEFAULT_MAX_ATTEMPTS,error,legacy],
            )?;
            record_change(tx, &epoch, Some(&id), None, "migrated", updated)?;
        }
    }
    Ok(())
}

pub fn get(conn: &Connection, id: &str) -> Result<Option<WorkRow>> {
    Ok(conn
        .query_row("SELECT * FROM delegated_work WHERE id=?1", [id], work)
        .optional()?)
}

pub fn visible(row: &WorkRow, principal: &str) -> bool {
    (!row.base_revision.is_empty() && row.state == "queued")
        || row.principal_id.as_deref() == Some(principal)
}

pub fn list(conn: &Connection, filter: List<'_>) -> Result<Vec<WorkRow>> {
    let mut stmt = conn.prepare(
        "SELECT * FROM delegated_work
         WHERE library_epoch = (SELECT epoch FROM library_epoch WHERE singleton=1)
           AND ((state='queued' AND base_revision <> '') OR principal_id=?1)
           AND (?2 IS NULL OR state=?2) AND (?3 IS NULL OR kind=?3) AND (?4 IS NULL OR id>?4)
         ORDER BY id LIMIT ?5",
    )?;
    let rows = stmt.query_map(
        params![
            filter.principal_id,
            filter.state,
            filter.kind,
            filter.after,
            filter.limit.clamp(1, 101)
        ],
        work,
    )?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn desktop_list(conn: &Connection) -> Result<Vec<WorkRow>> {
    let mut stmt = conn.prepare("SELECT * FROM delegated_work ORDER BY created_at DESC, id")?;
    let rows = stmt.query_map([], work)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Pending migration candidates for the present album snapshots of the
/// currently configured library. Page by work id instead of loading history.
pub fn legacy_candidates(conn: &Connection, library: &str, after: &str) -> Result<Vec<WorkRow>> {
    let mut stmt = conn.prepare(
        "SELECT w.* FROM delegated_work AS w
         WHERE w.library_epoch=(SELECT epoch FROM library_epoch WHERE singleton=1)
           AND w.id>?2 AND w.legacy_job_id IS NOT NULL
           AND ((w.base_revision='' AND w.state IN ('queued','awaiting_review','failed'))
                OR (w.state='awaiting_review' AND w.result_id IS NULL
                    AND w.last_error='legacy_result_unconverted'))
           AND EXISTS (SELECT 1 FROM albums AS a
                       WHERE a.id=w.album_id AND a.library_path=?1 AND a.missing_since IS NULL)
         ORDER BY w.id LIMIT 64",
    )?;
    let rows = stmt.query_map(params![library, after], work)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn result(conn: &Connection, id: &str) -> Result<Option<ResultRow>> {
    Ok(conn
        .query_row(
            "SELECT id, work_id, input_digest, digest, result_json, validation, created_at
         FROM work_results WHERE id=?1",
            [id],
            |r| {
                Ok(ResultRow {
                    id: r.get(0)?,
                    work_id: r.get(1)?,
                    input_digest: r.get(2)?,
                    digest: r.get(3)?,
                    result_json: r.get(4)?,
                    validation: r.get(5)?,
                    created_at: r.get(6)?,
                })
            },
        )
        .optional()?)
}

/// A group id remains usable if the requested children were combined into
/// another review after the caller originally opened it.
pub fn resolve_group(conn: &Connection, id: &str) -> Result<String> {
    Ok(conn
        .query_row(
            "SELECT group_id FROM work_group_aliases WHERE old_group_id=?1",
            [id],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or_else(|| id.to_owned()))
}

pub fn enqueue(conn: &Connection, work: &[NewWork<'_>], now: i64) -> Result<Vec<WorkRow>> {
    let tx = immediate(conn)?;
    let rows = enqueue_in(&tx, work, now)?;
    tx.commit()?;
    Ok(rows)
}

pub fn enqueue_in(tx: &Transaction<'_>, items: &[NewWork<'_>], now: i64) -> Result<Vec<WorkRow>> {
    let epoch = epoch(tx)?;
    let find = format!(
        "SELECT * FROM delegated_work WHERE library_epoch=?1 AND album_id=?2 AND kind=?3
          AND state IN {UNRESOLVED} AND (input_digest=?4 OR base_revision='')
         ORDER BY created_at, id LIMIT 1",
    );
    let mut rows: Vec<Option<WorkRow>> = items
        .iter()
        .map(|item| {
            Ok(tx
                .query_row(
                    &find,
                    params![epoch, item.album_id, item.kind, item.input_digest],
                    work,
                )
                .optional()?)
        })
        .collect::<Result<_>>()?;
    // When a combined request overlaps older single-kind requests, keep
    // their work IDs and leases and consolidate them under the oldest card.
    let group = rows
        .iter()
        .flatten()
        .min_by(|a, b| (a.created_at, &a.id).cmp(&(b.created_at, &b.id)))
        .map(|row| row.group_id.clone());
    for (item, row) in items.iter().zip(&mut rows) {
        let group = group.as_deref().unwrap_or(item.group_id);
        if let Some(existing) = row {
            if existing.group_id != group {
                let old_group = std::mem::replace(&mut existing.group_id, group.to_owned());
                tx.execute(
                    "UPDATE delegated_work SET group_id=?2 WHERE id=?1",
                    params![existing.id, group],
                )?;
                record_change(tx, &epoch, Some(&existing.id), None, "grouped", now)?;
                let remains: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM delegated_work WHERE group_id=?1)",
                    [&old_group],
                    |r| r.get(0),
                )?;
                if !remains {
                    tx.execute(
                        "UPDATE work_group_aliases SET group_id=?2 WHERE group_id=?1",
                        params![old_group, group],
                    )?;
                    tx.execute(
                        "INSERT INTO work_group_aliases(old_group_id,group_id) VALUES (?1,?2)",
                        params![old_group, group],
                    )?;
                }
            }
            continue;
        }
        let inserted = tx.query_row(
            "INSERT INTO delegated_work
               (id,group_id,kind,album_id,library_epoch,base_revision,state,input_json,input_digest,
                created_at,updated_at,available_at,max_attempts)
             VALUES (?1,?2,?3,?4,?5,?6,'queued',?7,?8,?9,?9,?9,?10) RETURNING *",
            params![
                item.id,
                group,
                item.kind,
                item.album_id,
                epoch,
                item.base_revision,
                item.input_json,
                item.input_digest,
                now,
                item.max_attempts
            ],
            work,
        )?;
        record_change(tx, &epoch, Some(item.id), None, "queued", now)?;
        *row = Some(inserted);
    }
    Ok(rows.into_iter().flatten().collect())
}

pub fn hydrate_legacy(
    conn: &Connection,
    id: &str,
    base_revision: &str,
    input_json: &str,
    input_digest: &str,
    now: i64,
) -> Result<bool> {
    let tx = immediate(conn)?;
    let updated = tx.execute(
        "UPDATE OR IGNORE delegated_work SET base_revision=?2, input_json=?3, input_digest=?4,
         updated_at=?5, revision=revision+1 WHERE id=?1 AND base_revision='' AND legacy_job_id IS NOT NULL
         AND state IN ('queued','awaiting_review','failed')
         AND library_epoch=(SELECT epoch FROM library_epoch WHERE singleton=1)",
        params![id,base_revision,input_json,input_digest,now],
    )? != 0;
    if updated {
        record_change(&tx, &epoch(&tx)?, Some(id), None, "legacy_hydrated", now)?;
    }
    tx.commit()?;
    Ok(updated)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutboxRow {
    pub sequence: i64,
    pub library_epoch: String,
    pub work_id: Option<String>,
    pub operation_id: Option<String>,
}

pub fn record_change(
    conn: &Connection,
    epoch: &str,
    work_id: Option<&str>,
    operation_id: Option<&str>,
    transition: &str,
    now: i64,
) -> Result<()> {
    let sequence: i64 = conn.query_row(
        "INSERT INTO delegation_audit(library_epoch,work_id,operation_id,transition,recorded_at)
         VALUES (?1,?2,?3,?4,?5) RETURNING sequence",
        params![epoch, work_id, operation_id, transition, now],
        |r| r.get(0),
    )?;
    conn.execute(
        "INSERT INTO delegation_outbox(sequence,library_epoch,work_id,operation_id) VALUES (?1,?2,?3,?4)",
        params![sequence,epoch,work_id,operation_id],
    )?;
    Ok(())
}

pub fn outbox(conn: &Connection, limit: i64) -> Result<Vec<OutboxRow>> {
    let mut stmt = conn.prepare(
        "SELECT sequence,library_epoch,work_id,operation_id FROM delegation_outbox
         WHERE library_epoch=(SELECT epoch FROM library_epoch WHERE singleton=1)
         ORDER BY sequence LIMIT ?1",
    )?;
    let rows = stmt.query_map([limit], |r| {
        Ok(OutboxRow {
            sequence: r.get(0)?,
            library_epoch: r.get(1)?,
            work_id: r.get(2)?,
            operation_id: r.get(3)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn ack_outbox(conn: &Connection, row_epoch: &str, through: i64) -> Result<()> {
    conn.execute(
        "DELETE FROM delegation_outbox WHERE library_epoch=?1 AND sequence<=?2",
        params![row_epoch, through],
    )?;
    Ok(())
}

pub struct Claim<'a> {
    pub work_id: &'a str,
    pub principal_id: &'a str,
    pub idempotency_key: &'a str,
    pub request_digest: &'a str,
    pub lease_token_hash: &'a str,
    pub now: i64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct ClaimRow {
    pub work_id: String,
    pub claim_generation: i64,
    pub lease_expires_at: i64,
    pub input_digest: String,
}

#[derive(Clone, Copy)]
pub struct Lease<'a> {
    pub work_id: &'a str,
    pub principal_id: &'a str,
    pub claim_generation: i64,
    pub lease_token_hash: &'a str,
    pub now: i64,
}

pub struct Progress<'a> {
    pub sequence: i64,
    pub stage: &'a str,
    pub completed: Option<i64>,
    pub total: Option<i64>,
    pub message: Option<&'a str>,
}

pub struct Completion<'a> {
    pub key: &'a str,
    pub request_digest: &'a str,
    pub input_digest: &'a str,
    pub id: &'a str,
    pub digest: &'a str,
    pub result_json: &'a str,
    pub validation: &'a str,
}

fn replay<T: DeserializeOwned>(
    conn: &Connection,
    principal: &str,
    operation: &str,
    key: &str,
    digest: &str,
) -> QueueResult<Option<T>> {
    let row: Option<(String, String)> = conn
        .query_row(
            "SELECT request_digest,response_json FROM work_replays
         WHERE library_epoch=(SELECT epoch FROM library_epoch WHERE singleton=1)
           AND principal_id=?1 AND operation=?2 AND replay_key=?3",
            params![principal, operation, key],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    match row {
        Some((saved, _)) if saved != digest => Ok(Err(Refusal::IdempotencyConflict)),
        Some((_, response)) => Ok(Ok(Some(serde_json::from_str(&response).map_err(
            |error| {
                rusqlite::Error::FromSqlConversionFailure(
                    1,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            },
        )?))),
        None => Ok(Ok(None)),
    }
}

fn save_replay(
    tx: &Transaction<'_>,
    principal: &str,
    operation: &str,
    key: &str,
    digest: &str,
    response: &impl Serialize,
) -> Result<()> {
    let response = serde_json::to_string(response)
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
    tx.execute(
        "INSERT INTO work_replays(library_epoch,principal_id,operation,replay_key,request_digest,response_json)
         VALUES ((SELECT epoch FROM library_epoch WHERE singleton=1),?1,?2,?3,?4,?5)",
        params![principal,operation,key,digest,response],
    )?;
    Ok(())
}

pub fn claim_replay(
    conn: &Connection,
    principal: &str,
    key: &str,
    request_digest: &str,
) -> QueueResult<Option<ClaimRow>> {
    replay(conn, principal, "claim", key, request_digest)
}

pub fn claim(conn: &Connection, claim: Claim<'_>) -> QueueResult<ClaimRow> {
    claim_with_context(conn, claim, |_, _| Ok(true))
}

pub fn claim_with_context(
    conn: &Connection,
    claim: Claim<'_>,
    context_current: impl FnOnce(&Connection, &WorkRow) -> Result<bool>,
) -> QueueResult<ClaimRow> {
    let tx = immediate(conn)?;
    match claim_replay(
        &tx,
        claim.principal_id,
        claim.idempotency_key,
        claim.request_digest,
    )? {
        Ok(Some(row)) => return Ok(Ok(row)),
        Err(refusal) => return Ok(Err(refusal)),
        Ok(None) => {}
    }
    expire_in(&tx, Some(claim.work_id), claim.now)?;
    let Some(row) = get(&tx, claim.work_id)? else {
        return Ok(Err(Refusal::NotFound));
    };
    let refusal = match row.state.as_str() {
        "claimed" | "running" => Some(Refusal::AlreadyClaimed),
        "queued" if row.library_epoch != epoch(&tx)? || row.base_revision.is_empty() => {
            Some(Refusal::StaleContext)
        }
        "queued" if row.available_at > claim.now => Some(Refusal::RateLimited),
        "queued" => None,
        _ if row.principal_id.as_deref() == Some(claim.principal_id) => Some(Refusal::PolicyDenied),
        _ => Some(Refusal::NotFound),
    };
    if let Some(refusal) = refusal {
        tx.commit()?;
        return Ok(Err(refusal));
    }
    if !context_current(&tx, &row)? {
        tx.commit()?;
        return Ok(Err(Refusal::StaleContext));
    }
    let generation = row.claim_generation + 1;
    let expires = claim.now.saturating_add(LEASE_MS);
    tx.execute(
        "UPDATE delegated_work SET state='claimed', revision=revision+1, updated_at=?2,
            principal_id=?3, lease_token_hash=?4, lease_expires_at=?5, claim_generation=?6,
            attempts=attempts+1, last_error=NULL WHERE id=?1",
        params![
            row.id,
            claim.now,
            claim.principal_id,
            claim.lease_token_hash,
            expires,
            generation
        ],
    )?;
    tx.execute(
        "INSERT INTO work_attempts(work_id,generation,principal_id,lease_token_hash,started_at,expires_at)
         VALUES (?1,?2,?3,?4,?5,?6)",
        params![row.id,generation,claim.principal_id,claim.lease_token_hash,claim.now,expires],
    )?;
    record_change(
        &tx,
        &row.library_epoch,
        Some(&row.id),
        None,
        "claimed",
        claim.now,
    )?;
    let reply = ClaimRow {
        work_id: row.id,
        claim_generation: generation,
        lease_expires_at: expires,
        input_digest: row.input_digest,
    };
    save_replay(
        &tx,
        claim.principal_id,
        "claim",
        claim.idempotency_key,
        claim.request_digest,
        &reply,
    )?;
    tx.commit()?;
    Ok(Ok(reply))
}

fn leased(conn: &Connection, lease: Lease<'_>) -> QueueResult<WorkRow> {
    let Some(row) = get(conn, lease.work_id)? else {
        return Ok(Err(Refusal::NotFound));
    };
    let active = matches!(row.state.as_str(), "claimed" | "running");
    if !active || row.claim_generation != lease.claim_generation {
        let previous: Option<Option<String>> = conn
            .query_row(
                "SELECT terminal_reason FROM work_attempts
             WHERE work_id=?1 AND generation=?2 AND principal_id=?3 AND lease_token_hash=?4",
                params![
                    lease.work_id,
                    lease.claim_generation,
                    lease.principal_id,
                    lease.lease_token_hash
                ],
                |r| r.get(0),
            )
            .optional()?;
        let Some(reason) = previous else {
            // A current owner with a current token but a wrong generation is
            // fenced even if they confused two attempt numbers.
            if row.principal_id.as_deref() == Some(lease.principal_id)
                && row.lease_token_hash.as_deref() == Some(lease.lease_token_hash)
            {
                return Ok(Err(Refusal::StaleGeneration));
            }
            return Ok(Err(Refusal::NotFound));
        };
        if row.library_epoch != epoch(conn)? || reason.as_deref() == Some("stale_context") {
            return Ok(Err(Refusal::StaleContext));
        }
        if reason.as_deref() == Some("expired")
            && (!active && row.claim_generation == lease.claim_generation + 1)
        {
            return Ok(Err(Refusal::LeaseExpired));
        }
        return Ok(Err(Refusal::StaleGeneration));
    }
    if row.principal_id.as_deref() != Some(lease.principal_id)
        || row.lease_token_hash.as_deref() != Some(lease.lease_token_hash)
    {
        return Ok(Err(Refusal::NotFound));
    }
    if row.library_epoch != epoch(conn)? {
        return Ok(Err(Refusal::StaleContext));
    }
    if row
        .lease_expires_at
        .is_none_or(|expires| expires <= lease.now)
    {
        return Ok(Err(Refusal::LeaseExpired));
    }
    Ok(Ok(row))
}

pub fn check_lease(conn: &Connection, lease: Lease<'_>) -> QueueResult<WorkRow> {
    leased(conn, lease)
}

pub fn renew(conn: &Connection, lease: Lease<'_>) -> QueueResult<i64> {
    let tx = immediate(conn)?;
    let row = match leased(&tx, lease)? {
        Ok(row) => row,
        Err(refusal) => return Ok(Err(refusal)),
    };
    let expires = lease.now.saturating_add(LEASE_MS);
    tx.execute(
        "UPDATE delegated_work SET lease_expires_at=?2,updated_at=?3,revision=revision+1 WHERE id=?1",
        params![row.id,expires,lease.now],
    )?;
    tx.execute(
        "UPDATE work_attempts SET expires_at=?3 WHERE work_id=?1 AND generation=?2",
        params![row.id, lease.claim_generation, expires],
    )?;
    record_change(
        &tx,
        &row.library_epoch,
        Some(&row.id),
        None,
        "renewed",
        lease.now,
    )?;
    tx.commit()?;
    Ok(Ok(expires))
}

pub fn progress(
    conn: &Connection,
    lease: Lease<'_>,
    progress: Progress<'_>,
) -> QueueResult<WorkRow> {
    let tx = immediate(conn)?;
    let row = match leased(&tx, lease)? {
        Ok(row) => row,
        Err(refusal) => return Ok(Err(refusal)),
    };
    let n = tx.execute(
        "UPDATE work_attempts SET progress_sequence=?3,stage=?4,completed=?5,total=?6,message=?7
         WHERE work_id=?1 AND generation=?2 AND progress_sequence<?3",
        params![
            row.id,
            lease.claim_generation,
            progress.sequence,
            progress.stage,
            progress.completed,
            progress.total,
            progress.message
        ],
    )?;
    if n == 0 {
        return Ok(Ok(row));
    }
    let updated = tx.query_row(
        "UPDATE delegated_work SET state='running',revision=revision+1,updated_at=?2
         WHERE id=?1 RETURNING *",
        params![row.id, lease.now],
        work,
    )?;
    record_change(
        &tx,
        &row.library_epoch,
        Some(&row.id),
        None,
        "progress",
        lease.now,
    )?;
    tx.commit()?;
    Ok(Ok(updated))
}

pub fn completion_replay(
    conn: &Connection,
    principal: &str,
    key: &str,
    request_digest: &str,
) -> QueueResult<Option<WorkRow>> {
    replay(conn, principal, "complete", key, request_digest)
}

pub fn complete(
    conn: &Connection,
    lease: Lease<'_>,
    completion: Completion<'_>,
) -> QueueResult<WorkRow> {
    complete_with_context(conn, lease, completion, |_, _| Ok(true))
}

pub fn complete_with_context(
    conn: &Connection,
    lease: Lease<'_>,
    completion: Completion<'_>,
    context_current: impl FnOnce(&Connection, &WorkRow) -> Result<bool>,
) -> QueueResult<WorkRow> {
    let tx = immediate(conn)?;
    match completion_replay(
        &tx,
        lease.principal_id,
        completion.key,
        completion.request_digest,
    )? {
        Ok(Some(row)) => return Ok(Ok(row)),
        Err(refusal) => return Ok(Err(refusal)),
        Ok(None) => {}
    }
    let row = match leased(&tx, lease)? {
        Ok(row) => row,
        Err(refusal) => return Ok(Err(refusal)),
    };
    if row.input_digest != completion.input_digest || !context_current(&tx, &row)? {
        return Ok(Err(Refusal::StaleContext));
    }
    if !valid_completion(&completion) {
        return Ok(Err(Refusal::InvalidResult));
    }
    insert_result(&tx, &row, &completion, lease.now)?;
    finish_attempt(&tx, &row, "completed", lease.now)?;
    let updated = tx.query_row(
        "UPDATE delegated_work SET state='awaiting_review',revision=revision+1,updated_at=?2,
            lease_token_hash=NULL,lease_expires_at=NULL,result_id=?3 WHERE id=?1 RETURNING *",
        params![row.id, lease.now, completion.id],
        work,
    )?;
    record_change(
        &tx,
        &row.library_epoch,
        Some(&row.id),
        None,
        "awaiting_review",
        lease.now,
    )?;
    save_replay(
        &tx,
        lease.principal_id,
        "complete",
        completion.key,
        completion.request_digest,
        &updated,
    )?;
    tx.commit()?;
    Ok(Ok(updated))
}

fn valid_completion(completion: &Completion<'_>) -> bool {
    completion.result_json.len() <= 262144
        && matches!(completion.validation, "pending" | "verified" | "rejected")
        && serde_json::from_str::<serde_json::Value>(completion.result_json).is_ok()
}

fn insert_result(
    tx: &Transaction<'_>,
    row: &WorkRow,
    completion: &Completion<'_>,
    now: i64,
) -> Result<()> {
    tx.execute(
        "INSERT INTO work_results(id,work_id,input_digest,digest,result_json,validation,created_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7)",
        params![
            completion.id,
            row.id,
            completion.input_digest,
            completion.digest,
            completion.result_json,
            completion.validation,
            now
        ],
    )?;
    Ok(())
}

/// A deterministic resolver result is part of the same transaction that
/// makes the sibling delegated work visible.
pub fn store_local_result(
    tx: &Transaction<'_>,
    row: &WorkRow,
    completion: Completion<'_>,
    now: i64,
) -> Result<WorkRow> {
    if row.state != "queued"
        || row.attempts != 0
        || row.legacy_job_id.is_some()
        || row.input_digest != completion.input_digest
        || !valid_completion(&completion)
    {
        return Ok(row.clone());
    }
    insert_result(tx, row, &completion, now)?;
    let updated = tx.query_row(
        "UPDATE delegated_work SET state='awaiting_review',result_id=?2,
            revision=revision+1,updated_at=?3,last_error=NULL WHERE id=?1 RETURNING *",
        params![row.id, completion.id, now],
        work,
    )?;
    record_change(
        tx,
        &row.library_epoch,
        Some(&row.id),
        None,
        "local_result",
        now,
    )?;
    Ok(updated)
}

pub fn store_legacy_result(
    conn: &Connection,
    id: &str,
    completion: Completion<'_>,
    now: i64,
) -> QueueResult<WorkRow> {
    let tx = immediate(conn)?;
    let row = match current_review(&tx, id)? {
        Ok(row) => row,
        Err(refusal) => return Ok(Err(refusal)),
    };
    if row.legacy_job_id.is_none() || row.result_id.is_some() || row.review_reserved {
        return Ok(Err(Refusal::PolicyDenied));
    }
    if row.base_revision.is_empty() || row.input_digest != completion.input_digest {
        return Ok(Err(Refusal::StaleContext));
    }
    if !valid_completion(&completion) {
        return Ok(Err(Refusal::InvalidResult));
    }
    insert_result(&tx, &row, &completion, now)?;
    let row = tx.query_row(
        "UPDATE delegated_work SET result_id=?2,revision=revision+1,updated_at=?3,last_error=NULL
           WHERE id=?1 RETURNING *",
        params![id, completion.id, now],
        work,
    )?;
    record_change(
        &tx,
        &row.library_epoch,
        Some(id),
        None,
        "legacy_result_converted",
        now,
    )?;
    tx.commit()?;
    Ok(Ok(row))
}

fn finish_attempt(tx: &Transaction<'_>, row: &WorkRow, reason: &str, now: i64) -> Result<()> {
    tx.execute(
        "UPDATE work_attempts SET finished_at=?3,terminal_reason=?4
         WHERE work_id=?1 AND generation=?2 AND finished_at IS NULL",
        params![row.id, row.claim_generation, now, reason],
    )?;
    Ok(())
}

fn close_lease(
    tx: &Transaction<'_>,
    row: &WorkRow,
    reason: &str,
    error: Option<&str>,
    state: &str,
    interval: i64,
    now: i64,
) -> Result<WorkRow> {
    finish_attempt(tx, row, reason, now)?;
    let updated = tx.query_row(
        "UPDATE delegated_work SET state=?2,revision=revision+1,updated_at=?3,available_at=?4,
            principal_id=CASE WHEN ?2='queued' THEN NULL ELSE principal_id END,
            lease_token_hash=NULL,lease_expires_at=NULL,last_error=?5,
            claim_generation=claim_generation+CASE WHEN ?6='expired' THEN 1 ELSE 0 END
         WHERE id=?1 RETURNING *",
        params![
            row.id,
            state,
            now,
            now.saturating_add(interval),
            error,
            reason
        ],
        work,
    )?;
    record_change(tx, &row.library_epoch, Some(&row.id), None, reason, now)?;
    Ok(updated)
}

pub fn fail(
    conn: &Connection,
    lease: Lease<'_>,
    code: &str,
    message: &str,
    retryable: bool,
) -> QueueResult<WorkRow> {
    let tx = immediate(conn)?;
    let row = match leased(&tx, lease)? {
        Ok(row) => row,
        Err(refusal) => return Ok(Err(refusal)),
    };
    let (state, backoff) = if retryable && row.attempts < row.max_attempts {
        ("queued", 1000 * (1 << row.attempts.clamp(0, 6)))
    } else {
        ("failed", 0)
    };
    let error = format!("{code}: {message}");
    let updated = close_lease(
        &tx,
        &row,
        "attempt_failed",
        Some(&error),
        state,
        backoff,
        lease.now,
    )?;
    tx.commit()?;
    Ok(Ok(updated))
}

pub fn release(conn: &Connection, lease: Lease<'_>, reason: &str) -> QueueResult<WorkRow> {
    let tx = immediate(conn)?;
    let row = match leased(&tx, lease)? {
        Ok(row) => row,
        Err(refusal) => return Ok(Err(refusal)),
    };
    let state = if row.attempts < row.max_attempts {
        "queued"
    } else {
        "failed"
    };
    let updated = close_lease(&tx, &row, "released", Some(reason), state, 0, lease.now)?;
    tx.commit()?;
    Ok(Ok(updated))
}

fn expire_in(tx: &Transaction<'_>, id: Option<&str>, now: i64) -> Result<Vec<String>> {
    let mut stmt = tx.prepare(
        "SELECT * FROM delegated_work WHERE lease_expires_at <= ?1 AND (?2 IS NULL OR id=?2)",
    )?;
    let rows = stmt
        .query_map(params![now, id], work)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut changed = Vec::with_capacity(rows.len());
    for row in rows {
        let state = if row.attempts < row.max_attempts {
            "queued"
        } else {
            "failed"
        };
        close_lease(tx, &row, "expired", Some("lease_expired"), state, 0, now)?;
        changed.push(row.id);
    }
    Ok(changed)
}

pub fn expire(conn: &Connection, now: i64) -> Result<Vec<String>> {
    let tx = immediate(conn)?;
    let changed = expire_in(&tx, None, now)?;
    tx.commit()?;
    Ok(changed)
}

pub fn cancel(
    conn: &Connection,
    id: &str,
    principal: &str,
    expected_revision: i64,
    now: i64,
) -> QueueResult<WorkRow> {
    cancel_for(conn, id, Some(principal), expected_revision, now)
}

pub fn cancel_from_desktop(
    conn: &Connection,
    id: &str,
    expected_revision: i64,
    now: i64,
) -> QueueResult<WorkRow> {
    cancel_for(conn, id, None, expected_revision, now)
}

fn cancel_for(
    conn: &Connection,
    id: &str,
    principal: Option<&str>,
    expected_revision: i64,
    now: i64,
) -> QueueResult<WorkRow> {
    let tx = immediate(conn)?;
    let Some(row) = get(&tx, id)?.filter(|row| principal.is_none_or(|id| visible(row, id))) else {
        return Ok(Err(Refusal::NotFound));
    };
    if row.revision != expected_revision || row.library_epoch != epoch(&tx)? {
        return Ok(Err(Refusal::StaleContext));
    }
    if !matches!(row.state.as_str(), "queued" | "claimed" | "running") {
        return Ok(Err(Refusal::PolicyDenied));
    }
    let updated = close_lease(&tx, &row, "cancelled", None, "cancelled", 0, now)?;
    tx.commit()?;
    Ok(Ok(updated))
}

fn current_review(conn: &Connection, id: &str) -> QueueResult<WorkRow> {
    let Some(row) = get(conn, id)? else {
        return Ok(Err(Refusal::NotFound));
    };
    if row.library_epoch != epoch(conn)? {
        return Ok(Err(Refusal::StaleContext));
    }
    if row.state != "awaiting_review" {
        return Ok(Err(Refusal::PolicyDenied));
    }
    Ok(Ok(row))
}

pub fn set_verification(
    conn: &Connection,
    id: &str,
    result_digest: &str,
    validation: &str,
    now: i64,
) -> QueueResult<WorkRow> {
    let tx = immediate(conn)?;
    let row = match current_review(&tx, id)? {
        Ok(row) => row,
        Err(refusal) => return Ok(Err(refusal)),
    };
    if row.review_reserved {
        return Ok(Err(Refusal::PolicyDenied));
    }
    let n = tx.execute(
        "UPDATE work_results SET validation=?3 WHERE id=?1 AND digest=?2 AND validation<>?3",
        params![row.result_id, result_digest, validation],
    )?;
    if n == 0 {
        return Ok(
            match row
                .result_id
                .as_deref()
                .map(|id| result(&tx, id))
                .transpose()?
                .flatten()
            {
                Some(result) if result.digest == result_digest => Ok(row),
                _ => Err(Refusal::StaleContext),
            },
        );
    }
    let row = tx.query_row(
        "UPDATE delegated_work SET revision=revision+1,updated_at=?2 WHERE id=?1 RETURNING *",
        params![id, now],
        work,
    )?;
    record_change(&tx, &row.library_epoch, Some(id), None, validation, now)?;
    tx.commit()?;
    Ok(Ok(row))
}

pub fn reserve_review(
    conn: &Connection,
    id: &str,
    result_digest: &str,
    expected_revision: i64,
    now: i64,
) -> QueueResult<WorkRow> {
    let tx = immediate(conn)?;
    let row = match current_review(&tx, id)? {
        Ok(row) => row,
        Err(refusal) => return Ok(Err(refusal)),
    };
    if row.revision != expected_revision || row.review_reserved {
        return Ok(Err(Refusal::StaleContext));
    }
    let Some(saved) = row
        .result_id
        .as_deref()
        .map(|id| result(&tx, id))
        .transpose()?
        .flatten()
    else {
        return Ok(Err(Refusal::InvalidResult));
    };
    if saved.digest != result_digest {
        return Ok(Err(Refusal::StaleContext));
    }
    if saved.validation != "verified" {
        return Ok(Err(Refusal::InvalidResult));
    }
    let row = tx.query_row(
        "UPDATE delegated_work SET review_reserved=1,revision=revision+1,updated_at=?2
         WHERE id=?1 RETURNING *",
        params![id, now],
        work,
    )?;
    record_change(
        &tx,
        &row.library_epoch,
        Some(id),
        None,
        "review_reserved",
        now,
    )?;
    tx.commit()?;
    Ok(Ok(row))
}

pub fn finish_review(
    conn: &Connection,
    id: &str,
    expected_revision: i64,
    disposition: &str,
    now: i64,
) -> QueueResult<WorkRow> {
    finish_review_with_error(conn, id, expected_revision, disposition, None, now)
}

pub fn finish_review_with_error(
    conn: &Connection,
    id: &str,
    expected_revision: i64,
    disposition: &str,
    error: Option<&str>,
    now: i64,
) -> QueueResult<WorkRow> {
    let tx = immediate(conn)?;
    let row = match current_review(&tx, id)? {
        Ok(row) => row,
        Err(refusal) => return Ok(Err(refusal)),
    };
    if row.revision != expected_revision || !row.review_reserved {
        return Ok(Err(Refusal::StaleContext));
    }
    if !matches!(disposition, "applied" | "failed") {
        return Ok(Err(Refusal::PolicyDenied));
    }
    let row = tx.query_row(
        "UPDATE delegated_work SET review_reserved=0,state=?2,revision=revision+1,updated_at=?3,
            last_error=?4 WHERE id=?1 RETURNING *",
        params![
            id,
            disposition,
            now,
            (disposition == "failed").then(|| error.unwrap_or("review_apply_failed"))
        ],
        work,
    )?;
    record_change(&tx, &row.library_epoch, Some(id), None, disposition, now)?;
    tx.commit()?;
    Ok(Ok(row))
}

pub fn discard(
    conn: &Connection,
    id: &str,
    result_digest: Option<&str>,
    expected_revision: i64,
    now: i64,
) -> QueueResult<WorkRow> {
    let tx = immediate(conn)?;
    let row = match current_review(&tx, id)? {
        Ok(row) => row,
        Err(refusal) => return Ok(Err(refusal)),
    };
    if row.revision != expected_revision || row.review_reserved {
        return Ok(Err(Refusal::StaleContext));
    }
    let saved = row
        .result_id
        .as_deref()
        .map(|id| result(&tx, id))
        .transpose()?
        .flatten();
    if saved.as_ref().map(|r| r.digest.as_str()) != result_digest {
        return Ok(Err(Refusal::StaleContext));
    }
    let row = tx.query_row(
        "UPDATE delegated_work SET state='discarded',revision=revision+1,updated_at=?2
         WHERE id=?1 RETURNING *",
        params![id, now],
        work,
    )?;
    record_change(&tx, &row.library_epoch, Some(id), None, "discarded", now)?;
    tx.commit()?;
    Ok(Ok(row))
}

pub fn retry(
    conn: &Connection,
    id: &str,
    expected_revision: i64,
    now: i64,
) -> QueueResult<WorkRow> {
    let tx = immediate(conn)?;
    let Some(row) = get(&tx, id)? else {
        return Ok(Err(Refusal::NotFound));
    };
    if row.revision != expected_revision
        || row.library_epoch != epoch(&tx)?
        || row.base_revision.is_empty()
    {
        return Ok(Err(Refusal::StaleContext));
    }
    if !matches!(row.state.as_str(), "cancelled" | "failed") {
        return Ok(Err(Refusal::PolicyDenied));
    }
    let sql = format!(
        "UPDATE delegated_work AS candidate SET state='queued',revision=revision+1,
          updated_at=?2,available_at=?2,attempts=0,last_error=NULL,result_id=NULL,principal_id=NULL
         WHERE id=?1 AND NOT EXISTS (
             SELECT 1 FROM delegated_work AS active WHERE active.library_epoch=candidate.library_epoch
               AND active.album_id=candidate.album_id AND active.kind=candidate.kind
               AND active.input_digest=candidate.input_digest AND active.state IN {UNRESOLVED})
         RETURNING *",
    );
    let Some(row) = tx.query_row(&sql, params![id, now], work).optional()? else {
        return Ok(Err(Refusal::AlreadyClaimed));
    };
    record_change(&tx, &row.library_epoch, Some(id), None, "retried", now)?;
    tx.commit()?;
    Ok(Ok(row))
}

pub fn recover_reserved(conn: &Connection, now: i64) -> Result<()> {
    let tx = immediate(conn)?;
    let reserved = {
        let mut stmt = tx.prepare("SELECT * FROM delegated_work WHERE review_reserved=1")?;
        stmt.query_map([], work)?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    for row in reserved {
        let change = row
            .result_id
            .as_deref()
            .map(|id| crate::agent::change_of_proposal(&tx, id))
            .transpose()?
            .flatten();
        // The apply may already have committed its final file outcomes before
        // the desktop stopped. Never repeat those writes during recovery.
        let applied = change.is_some_and(|change| change.state == "applied");
        let state = if applied { "applied" } else { "failed" };
        tx.execute(
            "UPDATE delegated_work SET state=?2,review_reserved=0,revision=revision+1,
               updated_at=?3,last_error=?4 WHERE id=?1",
            params![
                row.id,
                state,
                now,
                (!applied).then_some("review_interrupted_check_backup_and_refresh")
            ],
        )?;
        record_change(
            &tx,
            &row.library_epoch,
            Some(&row.id),
            None,
            if applied {
                "applied"
            } else {
                "review_interrupted"
            },
            now,
        )?;
    }
    tx.commit()?;
    Ok(())
}

pub fn active_external_count(conn: &Connection, now: i64) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM delegated_work WHERE lease_expires_at>?1
         AND library_epoch=(SELECT epoch FROM library_epoch WHERE singleton=1)",
        [now],
        |r| r.get(0),
    )?)
}

fn invalidate(
    tx: &Transaction<'_>,
    active_epoch: &str,
    album: Option<&str>,
    now: i64,
) -> Result<()> {
    let rows = {
        let mut stmt = tx.prepare(&format!(
            "SELECT * FROM delegated_work WHERE library_epoch=?1 AND state IN {UNRESOLVED}
             AND (?2 IS NULL OR album_id=?2)",
        ))?;
        stmt.query_map(params![active_epoch, album], work)?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    for row in rows {
        finish_attempt(tx, &row, "stale_context", now)?;
        tx.execute(
            "UPDATE delegated_work SET state='cancelled', review_reserved=0,revision=revision+1,
               updated_at=?2, lease_token_hash=NULL,lease_expires_at=NULL,last_error='stale_context'
             WHERE id=?1",
            params![row.id, now],
        )?;
        record_change(tx, active_epoch, Some(&row.id), None, "stale_context", now)?;
    }
    Ok(())
}

pub fn invalidate_album(conn: &Connection, album_id: &str, now: i64) -> Result<()> {
    let tx = immediate(conn)?;
    invalidate(&tx, &epoch(&tx)?, Some(album_id), now)?;
    tx.commit()?;
    Ok(())
}

fn rotate_in(tx: &Transaction<'_>, root: Option<&str>, now: i64) -> Result<String> {
    let old = epoch(tx)?;
    invalidate(tx, &old, None, now)?;
    crate::agent_operations::invalidate_epoch(tx, &old, now)?;
    let new = uuid(tx)?;
    tx.execute(
        "UPDATE library_epoch SET epoch=?1,root=?2,root_bound=1 WHERE singleton=1",
        params![new, root],
    )?;
    // A restored snapshot can contain unsent events. None may leave the
    // outbox as fresh work from the current collection.
    tx.execute(
        "DELETE FROM delegation_outbox WHERE library_epoch<>?1",
        [&new],
    )?;
    record_change(tx, &new, None, None, "epoch_changed", now)?;
    Ok(new)
}

pub fn rotate_epoch(conn: &Connection, root: Option<&str>, now: i64) -> Result<String> {
    let tx = immediate(conn)?;
    let new = rotate_in(&tx, root, now)?;
    tx.commit()?;
    Ok(new)
}

pub fn rotate_restored_epoch(conn: &Connection, now: i64) -> Result<String> {
    let tx = immediate(conn)?;
    let root: Option<String> = tx.query_row(
        "SELECT root FROM library_epoch WHERE singleton=1",
        [],
        |row| row.get(0),
    )?;
    let new = rotate_in(&tx, root.as_deref(), now)?;
    tx.commit()?;
    Ok(new)
}

pub fn sync_root(conn: &Connection, root: Option<&str>, now: i64) -> Result<bool> {
    let tx = immediate(conn)?;
    let (bound, current): (bool, Option<String>) = tx.query_row(
        "SELECT root_bound, root FROM library_epoch WHERE singleton=1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let same_root = match (current.as_deref(), root) {
        (Some(old), Some(new)) => same_file::is_same_file(old, new).unwrap_or_else(|_| {
            // When the volume is absent, retain an unchanged configured root.
            // Equality here does not establish that any file exists.
            old == new
        }),
        (None, None) => true,
        _ => false,
    };
    let changed = bound && !same_root;
    if changed {
        rotate_in(&tx, root, now)?;
    } else {
        tx.execute(
            "UPDATE library_epoch SET root=?1,root_bound=1 WHERE singleton=1",
            [root],
        )?;
    }
    tx.commit()?;
    Ok(changed)
}
