//! SQL of the agent bridge (v21): pairings, policies, proposals, approval
//! cards, before-images and the activity log. No secret is stored here: a
//! pairing keeps the SHA-256 fingerprint of its secret, which lives in the OS
//! keyring. Timestamps are ISO-8601 UTC text, so they compare as strings.

use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::Result;

/// A pairing never needs more history than this per pairing in the activity log.
pub const ACTIVITY_KEEP: i64 = 100;

// ---------------------------------------------------------------- pairings

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingRow {
    pub id: String,
    pub name: String,
    /// A JSON array of permission kinds.
    pub scopes: String,
    pub secret_fingerprint: String,
    pub created_at: String,
    pub last_used_at: Option<String>,
    pub revoked_at: Option<String>,
}

const PAIRING_COLS: &str =
    "id, name, scopes, secret_fingerprint, created_at, last_used_at, revoked_at";

fn pairing(r: &Row<'_>) -> rusqlite::Result<PairingRow> {
    Ok(PairingRow {
        id: r.get(0)?,
        name: r.get(1)?,
        scopes: r.get(2)?,
        secret_fingerprint: r.get(3)?,
        created_at: r.get(4)?,
        last_used_at: r.get(5)?,
        revoked_at: r.get(6)?,
    })
}

/// Insert a pairing unless `max_active` unrevoked ones exist already (checked
/// in the same transaction). Returns whether it was inserted.
pub fn insert_pairing(conn: &Connection, row: &PairingRow, max_active: i64) -> Result<bool> {
    let tx = crate::immediate(conn)?;
    let active: i64 = tx.query_row(
        "SELECT COUNT(*) FROM agent_pairings WHERE revoked_at IS NULL",
        [],
        |r| r.get(0),
    )?;
    if active >= max_active {
        return Ok(false);
    }
    tx.execute(
        "INSERT INTO agent_pairings(id, name, scopes, secret_fingerprint, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            row.id,
            row.name,
            row.scopes,
            row.secret_fingerprint,
            row.created_at
        ],
    )?;
    tx.commit()?;
    Ok(true)
}

/// Every pairing, oldest first.
pub fn list_pairings(conn: &Connection) -> Result<Vec<PairingRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {PAIRING_COLS} FROM agent_pairings ORDER BY created_at, id"
    ))?;
    let rows = stmt.query_map([], pairing)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn get_pairing(conn: &Connection, id: &str) -> Result<Option<PairingRow>> {
    Ok(conn
        .query_row(
            &format!("SELECT {PAIRING_COLS} FROM agent_pairings WHERE id = ?1"),
            [id],
            pairing,
        )
        .optional()?)
}

/// Revoke once; a revoked row is never reactivated. Returns whether this call
/// revoked it.
pub fn revoke_pairing(conn: &Connection, id: &str, at: &str) -> Result<bool> {
    let n = conn.execute(
        "UPDATE agent_pairings SET revoked_at = ?2 WHERE id = ?1 AND revoked_at IS NULL",
        params![id, at],
    )?;
    Ok(n == 1)
}

pub fn count_active_pairings(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM agent_pairings WHERE revoked_at IS NULL",
        [],
        |r| r.get(0),
    )?)
}

pub fn touch_pairing(conn: &Connection, id: &str, at: &str) -> Result<()> {
    conn.execute(
        "UPDATE agent_pairings SET last_used_at = ?2 WHERE id = ?1",
        params![id, at],
    )?;
    Ok(())
}

// ---------------------------------------------------------------- policies

/// The stored `(kind, decision)` pairs of a pairing; a kind with no row has
/// its default.
pub fn policies(conn: &Connection, pairing_id: &str) -> Result<Vec<(String, String)>> {
    let mut stmt = conn
        .prepare("SELECT kind, decision FROM agent_policies WHERE pairing_id = ?1 ORDER BY kind")?;
    let rows = stmt.query_map([pairing_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Set some kinds' decisions in one transaction; other kinds are untouched.
pub fn set_policies(conn: &Connection, pairing_id: &str, set: &[(String, String)]) -> Result<()> {
    let tx = crate::immediate(conn)?;
    for (kind, decision) in set {
        tx.execute(
            "INSERT INTO agent_policies(pairing_id, kind, decision) VALUES (?1, ?2, ?3) \
             ON CONFLICT(pairing_id, kind) DO UPDATE SET decision = excluded.decision",
            params![pairing_id, kind, decision],
        )?;
    }
    tx.commit()?;
    Ok(())
}

// --------------------------------------------------------------- proposals

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposalRow {
    pub id: String,
    pub pairing_id: String,
    pub album_id: String,
    pub base_revision: String,
    pub patch: String,
    pub digest: String,
    pub kinds: String,
    pub diff: String,
    pub created_at: String,
    pub expires_at: String,
}

const PROPOSAL_COLS: &str =
    "id, pairing_id, album_id, base_revision, patch, digest, kinds, diff, created_at, expires_at";

fn proposal(r: &Row<'_>) -> rusqlite::Result<ProposalRow> {
    Ok(ProposalRow {
        id: r.get(0)?,
        pairing_id: r.get(1)?,
        album_id: r.get(2)?,
        base_revision: r.get(3)?,
        patch: r.get(4)?,
        digest: r.get(5)?,
        kinds: r.get(6)?,
        diff: r.get(7)?,
        created_at: r.get(8)?,
        expires_at: r.get(9)?,
    })
}

/// Proposals are immutable: there is no update.
pub fn insert_proposal(conn: &Connection, p: &ProposalRow) -> Result<()> {
    conn.execute(
        &format!(
            "INSERT INTO agent_proposals({PROPOSAL_COLS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)"
        ),
        params![
            p.id,
            p.pairing_id,
            p.album_id,
            p.base_revision,
            p.patch,
            p.digest,
            p.kinds,
            p.diff,
            p.created_at,
            p.expires_at
        ],
    )?;
    Ok(())
}

pub fn get_proposal(conn: &Connection, id: &str) -> Result<Option<ProposalRow>> {
    Ok(conn
        .query_row(
            &format!("SELECT {PROPOSAL_COLS} FROM agent_proposals WHERE id = ?1"),
            [id],
            proposal,
        )
        .optional()?)
}

/// Drop proposals created before `cutoff`; returns how many.
pub fn prune_proposals(conn: &Connection, cutoff: &str) -> Result<usize> {
    Ok(conn.execute(
        "DELETE FROM agent_proposals WHERE created_at < ?1",
        [cutoff],
    )?)
}

// --------------------------------------------------------------- approvals

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalRow {
    pub id: String,
    pub pairing_id: String,
    pub action: String,
    pub proposal_id: Option<String>,
    pub request: Option<String>,
    pub request_digest: String,
    pub album_revision: Option<String>,
    pub kinds: String,
    pub state: String,
    pub reason: Option<String>,
    pub created_at: String,
    pub expires_at: String,
    pub decided_at: Option<String>,
    pub operation_id: Option<String>,
}

const APPROVAL_COLS: &str = "id, pairing_id, action, proposal_id, request, request_digest, \
    album_revision, kinds, state, reason, created_at, expires_at, decided_at, operation_id";

fn approval(r: &Row<'_>) -> rusqlite::Result<ApprovalRow> {
    Ok(ApprovalRow {
        id: r.get(0)?,
        pairing_id: r.get(1)?,
        action: r.get(2)?,
        proposal_id: r.get(3)?,
        request: r.get(4)?,
        request_digest: r.get(5)?,
        album_revision: r.get(6)?,
        kinds: r.get(7)?,
        state: r.get(8)?,
        reason: r.get(9)?,
        created_at: r.get(10)?,
        expires_at: r.get(11)?,
        decided_at: r.get(12)?,
        operation_id: r.get(13)?,
    })
}

/// Insert a card; its state is always `pending` (the row's own `state` is ignored).
pub fn insert_approval(conn: &Connection, a: &ApprovalRow) -> Result<()> {
    conn.execute(
        &format!(
            "INSERT INTO agent_approvals({APPROVAL_COLS}) \
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,'pending',NULL,?9,?10,NULL,?11)"
        ),
        params![
            a.id,
            a.pairing_id,
            a.action,
            a.proposal_id,
            a.request,
            a.request_digest,
            a.album_revision,
            a.kinds,
            a.created_at,
            a.expires_at,
            a.operation_id
        ],
    )?;
    Ok(())
}

pub fn get_approval(conn: &Connection, id: &str) -> Result<Option<ApprovalRow>> {
    Ok(conn
        .query_row(
            &format!("SELECT {APPROVAL_COLS} FROM agent_approvals WHERE id = ?1"),
            [id],
            approval,
        )
        .optional()?)
}

/// Cards in `state` (`None`: every card), newest first.
pub fn list_approvals(conn: &Connection, state: Option<&str>) -> Result<Vec<ApprovalRow>> {
    let sql = format!(
        "SELECT {APPROVAL_COLS} FROM agent_approvals \
         WHERE (?1 IS NULL OR state = ?1) ORDER BY created_at DESC, id DESC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([state], approval)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Move a card out of `pending` to `state` (`approved`, `denied` or
/// `expired`), exactly once. Returns whether this call changed it.
pub fn decide_approval(
    conn: &Connection,
    id: &str,
    state: &str,
    reason: Option<&str>,
    at: &str,
) -> Result<bool> {
    debug_assert!(matches!(state, "approved" | "denied" | "expired"));
    let n = conn.execute(
        "UPDATE agent_approvals SET state = ?2, reason = ?3, decided_at = ?4 \
         WHERE id = ?1 AND state = 'pending'",
        params![id, state, reason, at],
    )?;
    Ok(n == 1)
}

pub fn set_approval_operation(conn: &Connection, id: &str, operation_id: &str) -> Result<()> {
    conn.execute(
        "UPDATE agent_approvals SET operation_id = ?2 WHERE id = ?1",
        params![id, operation_id],
    )?;
    Ok(())
}

fn expire_where(
    conn: &Connection,
    filter: &str,
    bind: &[&dyn rusqlite::ToSql],
    reason: &str,
    at: &str,
) -> Result<Vec<String>> {
    let tx = crate::immediate(conn)?;
    let ids: Vec<String> = {
        let mut stmt = tx.prepare(&format!(
            "SELECT id FROM agent_approvals WHERE state = 'pending' {filter} ORDER BY created_at, id"
        ))?;
        let rows = stmt.query_map(bind, |r| r.get(0))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    for id in &ids {
        tx.execute(
            "UPDATE agent_approvals SET state = 'expired', reason = ?2, decided_at = ?3 \
             WHERE id = ?1 AND state = 'pending'",
            params![id, reason, at],
        )?;
    }
    tx.commit()?;
    Ok(ids)
}

/// Every pending card becomes `expired` with `reason` (an app restart ends the
/// operation handles a card waits on). Returns the ids.
pub fn expire_all_pending(conn: &Connection, reason: &str, at: &str) -> Result<Vec<String>> {
    expire_where(conn, "", &[], reason, at)
}

/// Pending cards whose expiry is at or before `now` become `expired` ("timed
/// out"). Returns the ids.
pub fn expire_due(conn: &Connection, now: &str) -> Result<Vec<String>> {
    expire_where(conn, "AND expires_at <= ?1", &[&now], "timed out", now)
}

// ----------------------------------------------------------------- changes

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeRow {
    pub id: String,
    pub pairing_id: String,
    pub proposal_id: String,
    pub approval_id: Option<String>,
    pub album_id: String,
    pub created_at: String,
    pub state: String,
    pub before: String,
    pub outcome: Option<String>,
    /// What the change left on disk (written once it finished), for the
    /// desktop's Undo to compare with the files now.
    pub after: Option<String>,
    /// For an undo's own record: the change it undid.
    pub undoes: Option<String>,
}

const CHANGE_COLS: &str = "id, pairing_id, proposal_id, approval_id, album_id, created_at, state, \
     before, outcome, after, undoes";

fn change(r: &Row<'_>) -> rusqlite::Result<ChangeRow> {
    Ok(ChangeRow {
        id: r.get(0)?,
        pairing_id: r.get(1)?,
        proposal_id: r.get(2)?,
        approval_id: r.get(3)?,
        album_id: r.get(4)?,
        created_at: r.get(5)?,
        state: r.get(6)?,
        before: r.get(7)?,
        outcome: r.get(8)?,
        after: r.get(9)?,
        undoes: r.get(10)?,
    })
}

/// Record a before-image with state `recorded`.
pub fn insert_change(conn: &Connection, c: &ChangeRow) -> Result<()> {
    conn.execute(
        &format!(
            "INSERT INTO agent_changes({CHANGE_COLS}) \
             VALUES (?1,?2,?3,?4,?5,?6,'recorded',?7,NULL,NULL,?8)"
        ),
        params![
            c.id,
            c.pairing_id,
            c.proposal_id,
            c.approval_id,
            c.album_id,
            c.created_at,
            c.before,
            c.undoes
        ],
    )?;
    Ok(())
}

pub fn get_change(conn: &Connection, id: &str) -> Result<Option<ChangeRow>> {
    Ok(conn
        .query_row(
            &format!("SELECT {CHANGE_COLS} FROM agent_changes WHERE id = ?1"),
            [id],
            change,
        )
        .optional()?)
}

pub fn list_changes(conn: &Connection) -> Result<Vec<ChangeRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {CHANGE_COLS} FROM agent_changes ORDER BY created_at, id"
    ))?;
    let rows = stmt.query_map([], change)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Move a change from `recorded` to `applied`, `partial` or `failed` with the
/// outcome reported to the agent.
pub fn finish_change(conn: &Connection, id: &str, state: &str, outcome: &str) -> Result<bool> {
    debug_assert!(matches!(state, "applied" | "partial" | "failed"));
    let n = conn.execute(
        "UPDATE agent_changes SET state = ?2, outcome = ?3 WHERE id = ?1 AND state = 'recorded'",
        params![id, state, outcome],
    )?;
    Ok(n == 1)
}

/// A pairing's changes (every pairing's when `None`), newest first, at most
/// `limit` (undo records included).
pub fn list_changes_of(
    conn: &Connection,
    pairing_id: Option<&str>,
    limit: u32,
) -> Result<Vec<ChangeRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {CHANGE_COLS} FROM agent_changes WHERE (?1 IS NULL OR pairing_id = ?1) \
         ORDER BY created_at DESC, rowid DESC LIMIT ?2"
    ))?;
    let rows = stmt.query_map(params![pairing_id, limit], change)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Store what a change left on disk (once, after it finished).
pub fn set_change_after(conn: &Connection, id: &str, after: &str) -> Result<bool> {
    let n = conn.execute(
        "UPDATE agent_changes SET after = ?2 WHERE id = ?1 AND after IS NULL",
        params![id, after],
    )?;
    Ok(n == 1)
}

/// Mark an `applied` or `partial` change `undone` (once).
pub fn mark_change_undone(conn: &Connection, id: &str) -> Result<bool> {
    let n = conn.execute(
        "UPDATE agent_changes SET state = 'undone' WHERE id = ?1 AND state IN ('applied','partial')",
        [id],
    )?;
    Ok(n == 1)
}

/// Delete changes created before `cutoff`; returns their ids so the caller can
/// remove the matching `agent-changes/<id>/` directories.
pub fn prune_changes(conn: &Connection, cutoff: &str) -> Result<Vec<String>> {
    let tx = crate::immediate(conn)?;
    let ids: Vec<String> = {
        let mut stmt = tx.prepare("SELECT id FROM agent_changes WHERE created_at < ?1")?;
        let rows = stmt.query_map([cutoff], |r| r.get(0))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    tx.execute("DELETE FROM agent_changes WHERE created_at < ?1", [cutoff])?;
    tx.commit()?;
    Ok(ids)
}

// ---------------------------------------------------------------- activity

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityRow {
    pub at: String,
    pub tool: String,
    pub outcome: String,
}

/// Record one call, then drop everything but the pairing's newest
/// [`ACTIVITY_KEEP`] rows.
pub fn insert_activity(
    conn: &Connection,
    pairing_id: &str,
    at: &str,
    tool: &str,
    outcome: &str,
) -> Result<()> {
    let tx = crate::immediate(conn)?;
    tx.execute(
        "INSERT INTO agent_activity(pairing_id, at, tool, outcome) VALUES (?1, ?2, ?3, ?4)",
        params![pairing_id, at, tool, outcome],
    )?;
    tx.execute(
        "DELETE FROM agent_activity WHERE pairing_id = ?1 AND id NOT IN \
         (SELECT id FROM agent_activity WHERE pairing_id = ?1 ORDER BY id DESC LIMIT ?2)",
        params![pairing_id, ACTIVITY_KEEP],
    )?;
    tx.commit()?;
    Ok(())
}

/// A pairing's recorded calls, newest first.
pub fn list_activity(conn: &Connection, pairing_id: &str) -> Result<Vec<ActivityRow>> {
    let mut stmt = conn.prepare(
        "SELECT at, tool, outcome FROM agent_activity WHERE pairing_id = ?1 ORDER BY id DESC",
    )?;
    let rows = stmt.query_map([pairing_id], |r| {
        Ok(ActivityRow {
            at: r.get(0)?,
            tool: r.get(1)?,
            outcome: r.get(2)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Db;

    fn db() -> (tempfile::TempDir, Db) {
        let dir = tempfile::tempdir().unwrap();
        let (db, _) = Db::open_or_create(dir.path().join("library.db")).unwrap();
        (dir, db)
    }

    fn pair(id: &str, at: &str) -> PairingRow {
        PairingRow {
            id: id.into(),
            name: format!("name {id}"),
            scopes: r#"["library.read"]"#.into(),
            secret_fingerprint: "f".repeat(64),
            created_at: at.into(),
            last_used_at: None,
            revoked_at: None,
        }
    }

    fn card(id: &str, at: &str, expires: &str) -> ApprovalRow {
        ApprovalRow {
            id: id.into(),
            pairing_id: "p_1".into(),
            action: "fix_apply".into(),
            proposal_id: Some("pr_1".into()),
            request: None,
            request_digest: "d".into(),
            album_revision: Some("r".into()),
            kinds: r#"["tags.write"]"#.into(),
            state: "ignored".into(),
            reason: None,
            created_at: at.into(),
            expires_at: expires.into(),
            decided_at: None,
            operation_id: None,
        }
    }

    #[test]
    fn pairings_insert_cap_revoke_once_and_touch() {
        let (_d, db) = db();
        let c = db.connection();
        assert!(insert_pairing(c, &pair("p_1", "2026-01-01T00:00:00Z"), 2).unwrap());
        assert!(insert_pairing(c, &pair("p_2", "2026-01-02T00:00:00Z"), 2).unwrap());
        assert!(!insert_pairing(c, &pair("p_3", "2026-01-03T00:00:00Z"), 2).unwrap());
        assert_eq!(count_active_pairings(c).unwrap(), 2);
        assert!(revoke_pairing(c, "p_1", "2026-02-01T00:00:00Z").unwrap());
        assert!(!revoke_pairing(c, "p_1", "2026-03-01T00:00:00Z").unwrap());
        assert_eq!(
            get_pairing(c, "p_1")
                .unwrap()
                .unwrap()
                .revoked_at
                .as_deref(),
            Some("2026-02-01T00:00:00Z"),
            "revoked once, never changed"
        );
        // A revoked row frees a slot.
        assert!(insert_pairing(c, &pair("p_3", "2026-01-03T00:00:00Z"), 2).unwrap());
        touch_pairing(c, "p_2", "2026-04-01T00:00:00Z").unwrap();
        let all = list_pairings(c).unwrap();
        assert_eq!(
            all.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(),
            ["p_1", "p_2", "p_3"]
        );
        assert_eq!(all[1].last_used_at.as_deref(), Some("2026-04-01T00:00:00Z"));
        assert!(get_pairing(c, "nope").unwrap().is_none());
    }

    #[test]
    fn policies_are_partial_and_overwritten() {
        let (_d, db) = db();
        let c = db.connection();
        insert_pairing(c, &pair("p_1", "2026-01-01T00:00:00Z"), 20).unwrap();
        assert!(policies(c, "p_1").unwrap().is_empty());
        set_policies(
            c,
            "p_1",
            &[
                ("tags.write".into(), "allow".into()),
                ("compute.scan".into(), "refuse".into()),
            ],
        )
        .unwrap();
        set_policies(c, "p_1", &[("tags.write".into(), "ask".into())]).unwrap();
        assert_eq!(
            policies(c, "p_1").unwrap(),
            [
                ("compute.scan".to_string(), "refuse".to_string()),
                ("tags.write".to_string(), "ask".to_string())
            ]
        );
        assert!(set_policies(c, "p_1", &[("tags.write".into(), "maybe".into())]).is_err());
    }

    #[test]
    fn proposals_round_trip_and_prune() {
        let (_d, db) = db();
        let c = db.connection();
        insert_pairing(c, &pair("p_1", "2026-01-01T00:00:00Z"), 20).unwrap();
        let mk = |id: &str, at: &str| ProposalRow {
            id: id.into(),
            pairing_id: "p_1".into(),
            album_id: "0123456789abcdef".into(),
            base_revision: "r".into(),
            patch: "{}".into(),
            digest: "d".into(),
            kinds: "[]".into(),
            diff: "{}".into(),
            created_at: at.into(),
            expires_at: "2099-01-01T00:00:00Z".into(),
        };
        insert_proposal(c, &mk("pr_old", "2026-01-01T00:00:00Z")).unwrap();
        insert_proposal(c, &mk("pr_new", "2026-01-09T00:00:00Z")).unwrap();
        assert!(
            insert_proposal(c, &mk("pr_new", "2026-01-09T00:00:00Z")).is_err(),
            "ids are unique"
        );
        assert_eq!(
            get_proposal(c, "pr_old").unwrap().unwrap(),
            mk("pr_old", "2026-01-01T00:00:00Z")
        );
        assert_eq!(prune_proposals(c, "2026-01-08T00:00:00Z").unwrap(), 1);
        assert!(get_proposal(c, "pr_old").unwrap().is_none());
        assert!(get_proposal(c, "pr_new").unwrap().is_some());
    }

    #[test]
    fn a_card_leaves_pending_exactly_once() {
        let (_d, db) = db();
        let c = db.connection();
        insert_pairing(c, &pair("p_1", "2026-01-01T00:00:00Z"), 20).unwrap();
        insert_approval(
            c,
            &card("ap_1", "2026-01-01T00:00:00Z", "2026-01-01T00:30:00Z"),
        )
        .unwrap();
        assert_eq!(get_approval(c, "ap_1").unwrap().unwrap().state, "pending");
        assert!(decide_approval(c, "ap_1", "approved", None, "2026-01-01T00:01:00Z").unwrap());
        assert!(!decide_approval(c, "ap_1", "denied", None, "2026-01-01T00:02:00Z").unwrap());
        let a = get_approval(c, "ap_1").unwrap().unwrap();
        assert_eq!(
            (a.state.as_str(), a.decided_at.as_deref()),
            ("approved", Some("2026-01-01T00:01:00Z"))
        );
        assert!(decide_approval(c, "ap_none", "denied", None, "x").is_ok_and(|b| !b));
    }

    #[test]
    fn expiry_by_time_and_at_restart() {
        let (_d, db) = db();
        let c = db.connection();
        insert_pairing(c, &pair("p_1", "2026-01-01T00:00:00Z"), 20).unwrap();
        insert_approval(
            c,
            &card("ap_a", "2026-01-01T00:00:00Z", "2026-01-01T00:30:00Z"),
        )
        .unwrap();
        insert_approval(
            c,
            &card("ap_b", "2026-01-01T00:10:00Z", "2026-01-01T00:40:00Z"),
        )
        .unwrap();
        insert_approval(
            c,
            &card("ap_c", "2026-01-01T00:20:00Z", "2026-01-01T00:50:00Z"),
        )
        .unwrap();
        decide_approval(c, "ap_c", "denied", Some("by user"), "2026-01-01T00:21:00Z").unwrap();
        assert!(expire_due(c, "2026-01-01T00:29:59Z").unwrap().is_empty());
        assert_eq!(expire_due(c, "2026-01-01T00:30:00Z").unwrap(), ["ap_a"]);
        assert_eq!(
            get_approval(c, "ap_a").unwrap().unwrap().reason.as_deref(),
            Some("timed out")
        );
        assert_eq!(
            expire_all_pending(c, "app restarted", "2026-01-02T00:00:00Z").unwrap(),
            ["ap_b"]
        );
        // A decided card keeps its state.
        assert_eq!(get_approval(c, "ap_c").unwrap().unwrap().state, "denied");
        assert!(list_approvals(c, Some("pending")).unwrap().is_empty());
        assert_eq!(list_approvals(c, None).unwrap().len(), 3);
        assert_eq!(list_approvals(c, Some("expired")).unwrap().len(), 2);
        set_approval_operation(c, "ap_b", "op_9").unwrap();
        assert_eq!(
            get_approval(c, "ap_b")
                .unwrap()
                .unwrap()
                .operation_id
                .as_deref(),
            Some("op_9")
        );
    }

    #[test]
    fn changes_are_recorded_finished_once_and_pruned() {
        let (_d, db) = db();
        let c = db.connection();
        insert_pairing(c, &pair("p_1", "2026-01-01T00:00:00Z"), 20).unwrap();
        let mk = |id: &str, at: &str| ChangeRow {
            id: id.into(),
            pairing_id: "p_1".into(),
            proposal_id: "pr_1".into(),
            approval_id: None,
            album_id: "0123456789abcdef".into(),
            created_at: at.into(),
            state: "ignored".into(),
            before: r#"{"files":{}}"#.into(),
            outcome: None,
            after: None,
            undoes: None,
        };
        insert_change(c, &mk("ch_old", "2026-01-01T00:00:00Z")).unwrap();
        insert_change(c, &mk("ch_new", "2026-06-01T00:00:00Z")).unwrap();
        assert_eq!(get_change(c, "ch_new").unwrap().unwrap().state, "recorded");
        assert!(finish_change(c, "ch_new", "applied", "{}").unwrap());
        assert!(
            !finish_change(c, "ch_new", "failed", "{}").unwrap(),
            "finishes once"
        );
        assert_eq!(get_change(c, "ch_new").unwrap().unwrap().state, "applied");
        assert!(set_change_after(c, "ch_new", "{}").unwrap());
        assert!(
            !set_change_after(c, "ch_new", "{}").unwrap(),
            "written once"
        );
        assert_eq!(
            list_changes_of(c, Some("p_1"), 10)
                .unwrap()
                .iter()
                .map(|r| r.id.as_str())
                .collect::<Vec<_>>(),
            ["ch_new", "ch_old"],
            "newest first"
        );
        assert!(
            !mark_change_undone(c, "ch_old").unwrap(),
            "only an applied or partial change"
        );
        assert!(mark_change_undone(c, "ch_new").unwrap());
        assert!(!mark_change_undone(c, "ch_new").unwrap(), "undone once");
        assert_eq!(get_change(c, "ch_new").unwrap().unwrap().state, "undone");
        assert_eq!(
            prune_changes(c, "2026-03-01T00:00:00Z").unwrap(),
            ["ch_old"]
        );
        assert_eq!(list_changes(c).unwrap().len(), 1);
    }

    #[test]
    fn activity_keeps_the_last_hundred_per_pairing_newest_first() {
        let (_d, db) = db();
        let c = db.connection();
        for i in 0..120 {
            insert_activity(
                c,
                "p_1",
                &format!("2026-01-01T00:{:02}:{:02}Z", i / 60, i % 60),
                "musaic_library_list",
                "ok",
            )
            .unwrap();
        }
        insert_activity(c, "p_2", "2026-01-01T01:00:00Z", "exchange", "ok").unwrap();
        let rows = list_activity(c, "p_1").unwrap();
        assert_eq!(rows.len(), 100);
        assert_eq!(rows[0].at, "2026-01-01T00:01:59Z");
        assert_eq!(rows[99].at, "2026-01-01T00:00:20Z");
        assert_eq!(list_activity(c, "p_2").unwrap().len(), 1);
    }
}
