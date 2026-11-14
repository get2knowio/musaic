//! The check-fix flow a host runs around [`crate::check_fixes`]
//! (`musaic/api/health.py`): assembling the [`FixEnv`] from the host's
//! parts (the check fixes' sources, a host override or the production
//! [`AppFixSources`]; cover downloads; the trash directory and its
//! collision stamp), and the fix route's fall-through to the AI queue
//! (needs-AI-job → `jobs.enqueue(...)`, or the conflict answer). This lives
//! here rather than in musaic-api's `routes/fixes.rs` so a non-HTTP face
//! fixes findings the same way.
//!
//! The host opens the connection, reads the library directory and the
//! resolver's parts, and says whether a model backend exists (the AI
//! provider is the host's; without one, a fix that would need a model
//! answers with the not-available message instead of queueing a job); it also takes the restore gate's work guard
//! and, once the fix has returned and the guard is released, notifies the
//! jobs stream when a job was enqueued (the Python app did not notify).

use std::path::Path;

use musaic_checks::CheckEnv;
use musaic_events::Bus;
use musaic_scan::TrackOrder;
use rusqlite::Connection;

use crate::ai_jobs::JobError;
use crate::apply::HttpError;
use crate::check_fixes::{self, FixEnv, FixOutcome, FixResult, FixSources};
use crate::production::{AppFixSources, Fingerprinting, NetCovers, Parts, SharedTransport};

/// `datetime.now().strftime("%Y-%m-%dT%H-%M-%S")` for a trash name
/// collision, from the host clock's `_now_iso()`. The Python app used local
/// time; this build has no time-zone database and stamps UTC on purpose.
pub fn trash_stamp(now_iso: &str) -> String {
    // YYYY-MM-DDTHH:MM:SSZ
    now_iso.trim_end_matches('Z').replace(':', "-")
}

/// What [`with_fix_env`] builds a [`FixEnv`] from.
pub struct FixHost<'a> {
    /// A connection the host opened for this fix.
    pub conn: Connection,
    pub bus: &'a Bus,
    pub checks: &'a CheckEnv,
    /// The host's override of the fixes' outside world (tests), else the
    /// production sources over `parts`.
    pub fix_sources: Option<&'a dyn FixSources>,
    /// `config.library_dir` as stored.
    pub library_dir: Option<String>,
    /// The resolver's parts with the AcoustID key as it is now.
    pub parts: &'a Parts,
    pub fingerprinting: &'a Fingerprinting,
    pub transport: SharedTransport,
    pub config_dir: &'a Path,
    /// `_now_iso()` from the host clock (also the trash stamp's source).
    pub now_iso: &'a dyn Fn() -> String,
    pub track_order: TrackOrder,
    /// `Some(message)` when no model backend exists.
    pub model_unavailable: Option<String>,
}

/// Run `f` with the [`FixEnv`] assembled from `host`.
pub fn with_fix_env<T>(host: FixHost<'_>, f: impl FnOnce(&mut FixEnv<'_>) -> T) -> T {
    let FixHost {
        mut conn,
        bus,
        checks,
        fix_sources,
        library_dir: lib,
        parts,
        fingerprinting,
        transport,
        config_dir,
        now_iso,
        track_order,
        model_unavailable,
    } = host;
    let app_sources = AppFixSources {
        parts,
        library_dir: lib.as_deref(),
        fingerprinting,
    };
    let sources: &dyn FixSources = match fix_sources {
        Some(s) => s,
        None => &app_sources,
    };
    let covers = NetCovers { transport };
    let trash_root = musaic_fsops::trash::trash_dir(config_dir);
    let now = || now_iso();
    let stamp = || trash_stamp(&now_iso());
    let mut env = FixEnv {
        conn: &mut conn,
        bus,
        checks,
        sources,
        covers: &covers,
        library_dir: lib.as_deref(),
        trash_root: &trash_root,
        now: &now,
        trash_stamp: &stamp,
        track_order,
        model_unavailable: model_unavailable.as_deref(),
    };
    f(&mut env)
}

/// Why [`fix_finding`] failed: the fix itself refused (the host maps it as
/// it maps every service refusal), or the AI job could not be written.
#[derive(Debug)]
pub enum FixFindingError {
    Fix(HttpError),
    Job(JobError),
}

/// `POST /api/checks/{check_id}/findings/{finding_id}/fix`: run the fix;
/// where it falls through to AI ([`FixOutcome::NeedsAiJob`]) enqueue the
/// job as the Python app's `jobs.enqueue(...)` inside `fix()` (checks/*.py):
/// `{ok: true, message, job_id}`, or, when the album already has an active
/// job, `{ok: false, message: "Already has an active job (<status>)",
/// job_id: <existing>}`. Returns the answer and whether a job was
/// enqueued (the host then notifies the jobs stream). `ids` makes
/// a new job id; `now_iso` is read once, when the job is written.
pub fn fix_finding(
    env: &mut FixEnv<'_>,
    check_id: &str,
    finding_id: i64,
    hint: &str,
    ids: &dyn Fn() -> String,
    now_iso: &dyn Fn() -> String,
) -> Result<(FixResult, bool), FixFindingError> {
    let mut enqueued = false;
    let r = match check_fixes::fix_finding(env, check_id, finding_id, hint)
        .map_err(FixFindingError::Fix)?
    {
        FixOutcome::Done(r) => r,
        // checks/*.py: `jobs.enqueue(...)`, or the JobConflictError answer.
        FixOutcome::NeedsAiJob(req) => {
            match crate::ai_jobs::enqueue_for_fix(env.conn, &req, ids, &now_iso())
                .map_err(FixFindingError::Job)?
            {
                Ok(job_id) => {
                    enqueued = true;
                    FixResult {
                        ok: true,
                        message: req.message,
                        job_id: Some(job_id),
                    }
                }
                Err((existing, status)) => FixResult {
                    ok: false,
                    message: format!("Already has an active job ({status})"),
                    job_id: Some(existing),
                },
            }
        }
    };
    Ok((r, enqueued))
}
