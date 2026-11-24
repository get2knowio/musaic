//! The checks a research request's routes make before they touch the store
//! (`musaic/api/jobs.py`'s validation): the job type, the album, and the
//! statuses of a bulk delete. This lives here rather than in a face so every
//! face applies the same rules. Every refusal is an [`OpError`] whose message
//! is the Python app's detail text.

use musaic_db::research_history::JOB_TYPES;
use rusqlite::Connection;

use crate::albums::AlbumRow;
use crate::error::{OpError, Subject};

/// `jobs.STATUSES`: the statuses `DELETE /api/ai/jobs?status=` accepts.
pub const STATUSES: [&str; 5] = ["queued", "running", "completed", "failed", "acted_on"];

/// What a research request is made for (`POST /api/ai/jobs`, a check fix).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewJob<'a> {
    pub album_id: &'a str,
    pub album_path: &'a str,
    pub album_title: &'a str,
    /// One of [`JOB_TYPES`].
    pub job_type: &'a str,
    pub hint: &'a str,
    pub attach_cover: bool,
}

fn repr(s: &str) -> String {
    musaic_fsops::py_repr(s)
}

/// `str(list_of_str)`.
fn list_repr(items: &[String]) -> String {
    format!(
        "[{}]",
        items.iter().map(|s| repr(s)).collect::<Vec<_>>().join(", ")
    )
}

fn rejected(message: String) -> OpError {
    OpError::Rejected {
        message,
        details: None,
    }
}

fn conflict(message: String) -> OpError {
    OpError::Conflict {
        message,
        details: None,
    }
}

/// `POST /api/ai/jobs`' first check, before any database access: the job
/// type must be one of [`JOB_TYPES`] (`Rejected`).
pub fn check_job_type(job_type: &str) -> Result<(), OpError> {
    if !JOB_TYPES.contains(&job_type) {
        return Err(rejected(format!("Unknown job_type {}", repr(job_type))));
    }
    Ok(())
}

/// The album a request is made for: cached in the configured library, or
/// `NotFound`.
pub fn album_for_job(
    conn: &Connection,
    library_dir: Option<&str>,
    album_id: &str,
) -> Result<AlbumRow, OpError> {
    match crate::sources::cached_album(conn, library_dir, album_id)? {
        Some(row) => Ok(row),
        None => Err(OpError::NotFound {
            details: None,
            what: Subject::Album,
            id: album_id.to_string(),
            message: format!("Album {} not in cache; rescan first", repr(album_id)),
        }),
    }
}

/// `DELETE /api/ai/jobs?status=a,b`'s statuses: each trimmed, blank entries
/// dropped; `Rejected` when none is left or one is unknown, `Conflict` when
/// `running` is named. No database access.
pub fn bulk_delete_statuses(status: &[String]) -> Result<Vec<String>, OpError> {
    let statuses: Vec<String> = status
        .iter()
        .map(|s| musaic_core::pycompat::strip(s).to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if statuses.is_empty() {
        return Err(rejected("status query param is required".to_string()));
    }
    let invalid: Vec<String> = statuses
        .iter()
        .filter(|s| !STATUSES.contains(&s.as_str()))
        .cloned()
        .collect();
    if !invalid.is_empty() {
        return Err(rejected(format!("Unknown status: {}", list_repr(&invalid))));
    }
    if statuses.iter().any(|s| s == "running") {
        return Err(conflict(
            "Bulk delete cannot include 'running' jobs. Wait for them to finish.".to_string(),
        ));
    }
    Ok(statuses)
}
