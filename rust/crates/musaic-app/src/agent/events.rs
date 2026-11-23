//! Reading the delegated-work history for an agent: `events/list` and
//! `events/poll` (spec 005, contracts/events.md).
//!
//! The table `delegation_events` is the truth; the bus and the outbox publish
//! only wake a parked poll. A cursor is an opaque `(epoch, sequence)` pair; one
//! that does not decode, names another library epoch or sequences below the
//! prune watermark (or a `maxAgeMs` floor) is answered as `truncated` with a
//! fresh cursor, never as an error. An event whose work the caller can no
//! longer see is skipped and the cursor still advances past it. A poll holds
//! no work guard and no database connection while it waits.

use std::time::{Duration, Instant};

use musaic_commands::events::{
    EVENT_REQUESTED, EVENT_UPDATED, EventOccurrence, ListResult, MAX_WAITING_POLLS, NEXT_POLL_MS,
    PollParams, PollResult, WorkEventData, descriptors,
};
use musaic_commands::tools::work::{WorkKind, WorkState};
use musaic_commands::{Decision, PermissionKind, ToolError, ToolErrorCode};
use musaic_db::delegated_work as work;
use musaic_db::delegation_events as history;
use rusqlite::Connection;

use crate::App;
use crate::agent::{Principal, delegation, run};

const BATCH: i64 = 100;
const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

fn err(code: ToolErrorCode, message: &str) -> ToolError {
    ToolError::new(code, message)
}

fn database(error: impl std::fmt::Display) -> ToolError {
    musaic_log::error(
        "musaic.agent",
        &format!("event history read failed: {error}"),
    );
    err(
        ToolErrorCode::Internal,
        "the event history could not be read",
    )
}

// ---------------------------------------------------------------- cursor

fn b64url(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for i in 0..=chunk.len() {
            out.push(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize] as char);
        }
    }
    out
}

fn unb64url(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let mut acc = 0u32;
    let mut bits = 0;
    for c in text.bytes() {
        let v = ALPHABET.iter().position(|a| *a == c)? as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// The opaque cursor of `(epoch, sequence)`.
pub fn encode_cursor(epoch: &str, sequence: i64) -> String {
    b64url(format!("v1.{epoch}.{sequence}").as_bytes())
}

/// `(epoch, sequence)` of a cursor, or `None` when it is not one of ours.
pub fn decode_cursor(cursor: &str) -> Option<(String, i64)> {
    let text = String::from_utf8(unb64url(cursor)?).ok()?;
    let rest = text.strip_prefix("v1.")?;
    let (epoch, seq) = rest.rsplit_once('.')?;
    let seq: i64 = seq.parse().ok().filter(|s| *s >= 0)?;
    (!epoch.is_empty()).then(|| (epoch.to_string(), seq))
}

/// `evt_<first 8 hex of the epoch>_<sequence>`.
pub fn event_id(epoch: &str, sequence: i64) -> String {
    let short: String = epoch
        .chars()
        .filter(char::is_ascii_hexdigit)
        .take(8)
        .collect();
    format!("evt_{short}_{sequence}")
}

fn event_name(kind: &str) -> &'static str {
    if kind == "requested" {
        EVENT_REQUESTED
    } else {
        EVENT_UPDATED
    }
}

// ------------------------------------------------------------ operations

pub fn events_list(app: &App, principal: &Principal) -> Result<ListResult, ToolError> {
    check(app, principal)?;
    Ok(ListResult {
        events: descriptors(true),
    })
}

/// The same permission the work-list tool needs, and an unrevoked pairing.
fn check(app: &App, principal: &Principal) -> Result<(), ToolError> {
    match run::authorize(app, principal, &[PermissionKind::LibraryRead]) {
        Ok(Decision::Allow) => Ok(()),
        // A database that could not be read says nothing about the pairing;
        // answering Forbidden would end a host's stream as if it were revoked.
        Err(e) if e.code == ToolErrorCode::Internal => Err(e),
        _ => Err(err(
            ToolErrorCode::EventForbidden,
            "this pairing may not read delegated work",
        )),
    }
}

pub fn events_poll(
    app: &App,
    principal: &Principal,
    params: &PollParams,
) -> Result<PollResult, ToolError> {
    params.validate()?;
    check(app, principal)?;
    let args = params.arguments.clone().unwrap_or_default();
    let kinds: Vec<&str> = args
        .kinds
        .iter()
        .flatten()
        .map(|k| WorkKind::as_str(*k))
        .collect();
    let states: Vec<&str> = args
        .states
        .iter()
        .flatten()
        .map(|s| WorkState::as_str(*s))
        .collect();
    let wait = Duration::from_millis(u64::from(params.wait_ms.unwrap_or(0)));
    let deadline = Instant::now() + wait;
    let hub = &app.0.agent;
    let mut parked = None;
    {
        let was_active = hub.subscription_active(&principal.pairing_id);
        hub.note_poll(&principal.pairing_id);
        if !was_active {
            // The subscription just became active: refresh diagnostics.
            crate::agent::hosts::announce(app, &principal.pairing_id);
        }
    }
    loop {
        let seen = hub.events_generation();
        let page = read_page(app, principal, params, &kinds, &states)?;
        let nothing = page.events.is_empty() && !page.truncated;
        if !nothing || wait.is_zero() || app.is_shutting_down() {
            return Ok(page);
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Ok(page);
        }
        if parked.is_none() {
            parked = Some(
                hub.park_waiter(&principal.pairing_id, MAX_WAITING_POLLS)
                    .ok_or_else(|| {
                        err(
                            ToolErrorCode::EventLimit,
                            "this pairing already has the most waiting polls",
                        )
                    })?,
            );
        }
        hub.wait_events(seen, left);
        // A revocation wakes the loop; the next pass re-checks the pairing.
        check(app, principal)?;
    }
}

fn read_page(
    app: &App,
    principal: &Principal,
    params: &PollParams,
    kinds: &[&str],
    states: &[&str],
) -> Result<PollResult, ToolError> {
    let conn = musaic_db::open_connection(app.db_path()).map_err(database)?;
    // One snapshot: the head, the watermark and the rows agree.
    let tx = conn.unchecked_transaction().map_err(database)?;
    let epoch = work::epoch(&tx).map_err(database)?;
    let head = history::head(&tx, &epoch).map_err(database)?;
    let fresh = |truncated| PollResult {
        events: Vec::new(),
        cursor: encode_cursor(&epoch, head),
        truncated,
        has_more: false,
        next_poll_ms: NEXT_POLL_MS,
    };
    let Some(given) = params.cursor.as_deref() else {
        return Ok(fresh(false));
    };
    let Some((cursor_epoch, after)) = decode_cursor(given) else {
        return Ok(fresh(true));
    };
    let floor = match params.max_age_ms {
        Some(age) => {
            let cutoff =
                delegation::now_ms(app).saturating_sub(i64::try_from(age).unwrap_or(i64::MAX));
            history::age_floor(&tx, &epoch, cutoff).map_err(database)?
        }
        None => 0,
    };
    if cursor_epoch != epoch
        || after > head
        || after < history::pruned_through(&tx).map_err(database)?
        || after < floor
    {
        return Ok(fresh(true));
    }
    let limit = params.max_events() as usize;
    let wanted = params
        .name
        .strip_prefix("musaic.work.")
        .and_then(|n| n.split('.').next());
    let mut events: Vec<EventOccurrence> = Vec::new();
    let mut position = after;
    let mut has_more = false;
    'scan: loop {
        let rows = history::page(&tx, &epoch, position, kinds, states, BATCH).map_err(database)?;
        let exhausted = (rows.len() as i64) < BATCH;
        for row in rows {
            if events.len() == limit {
                has_more = true;
                break 'scan;
            }
            position = row.sequence;
            if wanted.is_some_and(|w| w != row.event_type) {
                continue;
            }
            if !visible_now(&tx, &row.work_id, &epoch, &principal.pairing_id)? {
                continue;
            }
            let (Some(kind), Some(state)) =
                (WorkKind::parse(&row.kind), WorkState::parse(&row.state))
            else {
                continue;
            };
            events.push(EventOccurrence {
                event_id: event_id(&epoch, row.sequence),
                name: event_name(&row.event_type).to_string(),
                timestamp: iso(row.committed_at),
                data: WorkEventData {
                    work_id: row.work_id,
                    kind,
                    state,
                    revision: row.revision,
                    library_epoch: epoch.clone(),
                },
                cursor: Some(encode_cursor(&epoch, row.sequence)),
            });
        }
        if exhausted {
            // Nothing is left up to the head read in this snapshot.
            position = position.max(head);
            break;
        }
    }
    Ok(PollResult {
        cursor: encode_cursor(&epoch, position),
        events,
        truncated: false,
        has_more,
        next_poll_ms: if has_more { 0 } else { NEXT_POLL_MS },
    })
}

/// The work item as it is now: still there, in this epoch, and one this
/// principal may see (open work, or held by it).
fn visible_now(
    conn: &Connection,
    work_id: &str,
    epoch: &str,
    principal: &str,
) -> Result<bool, ToolError> {
    Ok(work::get(conn, work_id)
        .map_err(database)?
        .is_some_and(|row| row.library_epoch == epoch && work::visible(&row, principal)))
}

fn iso(ms: i64) -> String {
    musaic_db::backup::iso_seconds(
        std::time::SystemTime::UNIX_EPOCH + Duration::from_millis(ms.max(0) as u64),
    )
}

/// Remove history older than seven days (the app's tick).
pub fn prune(app: &App, conn: &Connection) {
    let cutoff = delegation::now_ms(app).saturating_sub(history::RETENTION_MS);
    if let Err(e) = history::prune(conn, cutoff) {
        let _ = database(e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursors_round_trip_and_refuse_anything_else() {
        let c = encode_cursor("1234abcd-0000-4000-8000-000000000000", 42);
        assert_eq!(
            decode_cursor(&c),
            Some(("1234abcd-0000-4000-8000-000000000000".to_string(), 42))
        );
        for bad in [
            "",
            "!!",
            "c1",
            "djEuLi4",
            &b64url(b"v2.e.1"),
            &b64url(b"v1.e.x"),
            &b64url(b"v1..3"),
        ] {
            assert_eq!(decode_cursor(bad), None, "{bad}");
        }
        assert!(
            c.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        );
    }

    #[test]
    fn b64_matches_the_standard_alphabet_values() {
        assert_eq!(b64url(b"f"), "Zg");
        assert_eq!(b64url(b"fo"), "Zm8");
        assert_eq!(b64url(b"foo"), "Zm9v");
        assert_eq!(unb64url("Zm9vYg").unwrap(), b"foob");
    }

    #[test]
    fn event_ids_name_the_epoch_and_the_sequence() {
        assert_eq!(
            event_id("1a2b3c4d-0000-4000-8000-000000000000", 42),
            "evt_1a2b3c4d_42"
        );
    }
}
