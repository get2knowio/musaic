//! Desktop ownership of research requests, grouped review and compatibility jobs.
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use musaic_ai::{Py, ResolverEnv, synth};
use musaic_commands::auth::hex_encode;
use musaic_commands::tools::fix::{FixDiff, ProposedPatch};
use musaic_commands::tools::work::{Recommendation, WorkContext, WorkResult};
use musaic_db::delegated_work::{self as db, NewWork, WorkRow};
use musaic_db::research_history::{self, JobRow, LIST_LIMIT};
use musaic_events::{EventKind, Subscriber, Topic};
use musaic_services::research_request::NewJob;
use musaic_services::{OpError, agent_proposal, agent_read};
use rusqlite::Connection;
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::App;

pub(crate) fn millis(t: SystemTime) -> i64 {
    t.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(i64::MAX)
}
fn stamp(ms: i64) -> String {
    musaic_db::backup::iso_seconds(UNIX_EPOCH + Duration::from_millis(ms.max(0) as u64))
}
fn conflict(message: impl ToString) -> OpError {
    OpError::Conflict {
        message: message.to_string(),
        details: None,
    }
}
fn tool(e: musaic_commands::ToolError) -> OpError {
    conflict(e.message)
}
fn checked<T>(r: db::QueueResult<T>) -> Result<T, OpError> {
    r?.map_err(|_| conflict("The research item changed. Reopen its review and try again."))
}

fn input(
    app: &App,
    conn: &Connection,
    album: &str,
    hint: &str,
) -> Result<(String, String, String), OpError> {
    let (revision, context) = crate::research::context(
        app,
        conn,
        album,
        (!hint.is_empty()).then(|| hint.to_owned()),
    )
    .map_err(tool)?;
    let input = serde_json::to_string(&context).map_err(OpError::internal)?;
    let digest = crate::research::digest(&crate::agent::to_json_text(&(&revision, &input)));
    Ok((revision, input, digest))
}

pub(crate) fn hydrate(app: &App, conn: &Connection) -> Result<(), musaic_db::Error> {
    let Some(library) = app.library_dir() else {
        return Ok(());
    };
    let mut after = String::new();
    loop {
        let rows = db::legacy_candidates(conn, &library, &after)?;
        let Some(last) = rows.last() else {
            return Ok(());
        };
        after.clone_from(&last.id);
        for row in rows {
            let Some(legacy) = row.legacy_job_id.as_deref() else {
                continue;
            };
            let Some(job) = research_history::get(conn, legacy)? else {
                continue;
            };
            // An album that is still unreadable stays history until another
            // committed scan provides an authoritative snapshot.
            if row.base_revision.is_empty()
                && let Ok((revision, ctx, digest)) = input(app, conn, &row.album_id, &job.hint)
            {
                db::hydrate_legacy(conn, &row.id, &revision, &ctx, &digest, millis(app.now()))?;
            }
            if row.state == "awaiting_review"
                && row.result_id.is_none()
                && let Some(fresh) = db::get(conn, &row.id)?
                && !fresh.base_revision.is_empty()
            {
                crate::research::legacy::convert(conn, &fresh, &job, millis(app.now()))?;
            }
        }
    }
}

pub(crate) fn subscribe_legacy_hydration(app: &App) {
    let app_ref = Arc::downgrade(&app.0);
    app.bus().register(
        Topic::Kind(EventKind::LibraryScanCompleted),
        Subscriber::from_fn(move |_| {
            let Some(inner) = app_ref.upgrade() else {
                return Ok(());
            };
            let app = App(inner);
            let _guard = app.gate().begin_work()?;
            let conn = musaic_db::open_connection(app.db_path())?;
            hydrate(&app, &conn)?;
            crate::agent::delegation::publish_pending(&app, &conn);
            Ok(())
        })
        .name("hydrate_legacy_research")
        .priority(250),
    );
}

/// Re-derives the context of every queued request of `album_id` (its new id
/// after a rename, an adoption or a merge): the stored `base_revision` hashes
/// the album row with its old path, so without this every claim would be
/// refused as stale. The hint is the request's own. A held request is left to
/// the claim and completion rules (its result is checked against the album
/// as it is now). An album that cannot be read now keeps its old context.
/// A request migrated from an old job and not yet hydrated (empty
/// `base_revision`, input `{}`) is left to [`hydrate`]: its hint is in the
/// old job's history record, not in its input, and deriving it here would
/// drop that hint for good.
pub(crate) fn rederive_after_rename(
    app: &App,
    conn: &Connection,
    album_id: &str,
) -> Result<(), musaic_db::Error> {
    for row in db::queued_for_album(conn, album_id)? {
        if row.base_revision.is_empty() {
            continue;
        }
        let hint = serde_json::from_str::<WorkContext>(&row.input_json)
            .ok()
            .and_then(|c| c.hint)
            .unwrap_or_default();
        let Ok((revision, ctx, digest)) = input(app, conn, album_id, &hint) else {
            continue;
        };
        db::rederive_queued(conn, &row.id, &revision, &ctx, &digest, millis(app.now()))?;
    }
    Ok(())
}

/// Registers [`rederive_after_rename`] on `AlbumRenamed` (rule 1: the rename
/// path publishes, it never calls this).
pub(crate) fn subscribe_rename_rederive(app: &App) {
    let app_ref = Arc::downgrade(&app.0);
    app.bus().register(
        Topic::Kind(EventKind::AlbumRenamed),
        Subscriber::from_fn(move |event| {
            let musaic_events::Event::AlbumRenamed { new_album_id, .. } = event else {
                return Ok(());
            };
            let Some(inner) = app_ref.upgrade() else {
                return Ok(());
            };
            let app = App(inner);
            let _guard = app.gate().begin_work()?;
            let conn = musaic_db::open_connection(app.db_path())?;
            rederive_after_rename(&app, &conn, new_album_id)?;
            crate::agent::delegation::publish_pending(&app, &conn);
            Ok(())
        })
        .name("rederive_after_rename")
        .priority(250),
    );
}

/// The bulk run a request is made for.
#[derive(Clone, Debug)]
pub(crate) struct BatchRef {
    pub id: String,
    pub check_id: String,
    /// When the run started, ms.
    pub started_at: i64,
}

/// When [`enqueue`] publishes the request's outbox rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Publish {
    /// Right after the commit (every single request).
    Now,
    /// Later, by the caller (a bulk run publishes every 128 requests and at
    /// its end, so a large batch is a handful of relay events).
    Deferred,
}

/// What [`enqueue`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Enqueued {
    /// The card (group) the request belongs to.
    pub group_id: String,
    /// `false` when an unresolved request already covered it.
    pub created: bool,
}

/// Makes the research request `job` asks for, after the local resolver: a
/// result the resolver can give is kept with it for review. One transaction
/// per call: the request, its audit, outbox and event-history rows (and, for
/// a bulk run, the batch row on its first request). An unresolved request
/// for the same album and input is returned instead of a new one; in a bulk
/// run (`batch`), any unresolved request of the same album and kind is, and it
/// keeps its own batch membership.
pub(crate) fn enqueue(
    app: &App,
    conn: &mut Connection,
    job: &NewJob<'_>,
    batch: Option<&BatchRef>,
    publish: Publish,
) -> Result<Enqueued, OpError> {
    if job.hint.len() > 2000 {
        return Err(OpError::Rejected {
            message: "Use a hint of at most 2000 bytes.".into(),
            details: None,
        });
    }
    let current =
        agent_read::cached_row(conn, app.library_dir().as_deref(), job.album_id).map_err(tool)?;
    if current.missing_since.is_some() {
        return Err(conflict("The album is missing."));
    }
    if batch.is_some()
        && let Some(existing) = db::unresolved_for_albums(conn, &[job.album_id])?
            .into_iter()
            .find(|u| {
                let metadata = u.kind.starts_with("metadata.");
                match job.job_type {
                    "metadata" => metadata,
                    "cover" => u.kind == "cover.research",
                    _ => true,
                }
            })
    {
        return Ok(Enqueued {
            group_id: existing.group_id,
            created: false,
        });
    }
    let env = crate::ai::AppEnv::new(app);
    let resolution = env
        .resolve(conn, job.album_id, job.hint)
        .map_err(OpError::internal)?;
    let wants_metadata = job.job_type != "cover";
    let wants_cover = job.job_type != "metadata";
    let metadata = (wants_metadata && synth::resolver_settles_metadata(&resolution)).then(|| {
        let album = Py::from(&current.json);
        let tracks = env.read_tracks(&current.album.path);
        synth::synthesize_metadata(&album, &tracks, &resolution)
    });
    let cover = (wants_cover && synth::cover_from_resolver(&resolution))
        .then(|| synth::synthesize_cover(&resolution));
    let (base_revision, input_json, input_digest) = input(app, conn, job.album_id, job.hint)?;
    let metadata_kind = if resolution.anchor.is_some() {
        "metadata.correct"
    } else {
        "metadata.identify"
    };
    let kinds = [
        wants_metadata.then_some(metadata_kind),
        wants_cover.then_some("cover.research"),
    ];
    let group = (app.0.ai.ids)();
    let children: Vec<_> = kinds
        .into_iter()
        .flatten()
        .map(|kind| (crate::agent::delegation::uuid(), kind))
        .collect();
    let requests: Vec<_> = children
        .iter()
        .map(|(id, kind)| NewWork {
            id,
            group_id: &group,
            kind,
            album_id: job.album_id,
            base_revision: &base_revision,
            input_json: &input_json,
            input_digest: &input_digest,
            max_attempts: db::DEFAULT_MAX_ATTEMPTS,
        })
        .collect();
    let now = millis(app.now());
    let tx = musaic_db::immediate(conn)?;
    let work = match batch {
        Some(b) => db::enqueue_batched_in(
            &tx,
            &requests,
            &db::NewBatch {
                id: &b.id,
                check_id: &b.check_id,
                started_at: b.started_at,
            },
            now,
        )?,
        None => db::enqueue_in(&tx, &requests, now)?,
    };
    let first = work
        .first()
        .ok_or_else(|| OpError::internal("no research request created"))?;
    let created = work
        .iter()
        .any(|row| children.iter().any(|(id, _)| *id == row.id));
    if metadata.is_some() || cover.is_some() {
        let retrieved_at = stamp(now);
        for row in &work {
            crate::research::local::retain(
                &tx,
                row,
                metadata.as_ref(),
                cover.as_ref(),
                &retrieved_at,
                now,
            )?;
        }
    }
    let group_id = first.group_id.clone();
    tx.commit().map_err(musaic_db::Error::from)?;
    if publish == Publish::Now {
        crate::agent::delegation::publish_pending(app, conn);
    }
    Ok(Enqueued { group_id, created })
}

fn group(conn: &Connection, rows: &[WorkRow]) -> Result<JobRow, musaic_db::Error> {
    let first = &rows[0];
    let ctx = serde_json::from_str::<WorkContext>(&first.input_json).ok();
    let any = |s: &str| rows.iter().any(|r| r.state == s);
    let status = if any("awaiting_review") {
        "completed"
    } else if any("running") || any("claimed") {
        "running"
    } else if any("queued") {
        "queued"
    } else if any("failed") || any("cancelled") {
        "failed"
    } else {
        "acted_on"
    };
    let meta = rows.iter().any(|r| r.kind.starts_with("metadata."));
    let cover = rows.iter().any(|r| r.kind == "cover.research");
    let updated = rows
        .iter()
        .map(|r| r.updated_at)
        .max()
        .unwrap_or(first.created_at);
    let mut job = JobRow {
        id: first.group_id.clone(),
        album_id: first.album_id.clone(),
        album_title: ctx.as_ref().map(|c| c.title.clone()).unwrap_or_default(),
        album_path: String::new(),
        hint: ctx.and_then(|c| c.hint).unwrap_or_default(),
        job_type: match (meta, cover) {
            (true, true) => "metadata+cover",
            (true, false) => "metadata",
            _ => "cover",
        }
        .into(),
        created_at: stamp(first.created_at),
        started_at: None,
        status: status.into(),
        result_json: Some(r#"{"delegated":true}"#.into()),
        completed_at: matches!(status, "completed" | "failed" | "acted_on").then(|| stamp(updated)),
        acted_on_at: (status == "acted_on").then(|| stamp(updated)),
        acted_on_action: (status == "acted_on").then(|| {
            if any("applied") {
                "applied"
            } else {
                "discarded"
            }
            .into()
        }),
        error: rows.iter().find_map(|r| r.last_error.clone()),
        attach_cover: false,
    };
    if let Some(id) = rows.iter().find_map(|r| r.legacy_job_id.as_deref())
        && let Some(history) = research_history::get(conn, id)?
    {
        if job.album_title.is_empty() {
            job.album_title = history.album_title;
        }
        if job.hint.is_empty() {
            job.hint = history.hint;
        }
        job.created_at = history.created_at;
        job.started_at = history.started_at;
        if history.status == "acted_on" {
            job.completed_at = history.completed_at;
            job.acted_on_at = history.acted_on_at;
        }
    }
    Ok(job)
}
pub(crate) fn group_job(conn: &Connection, id: &str) -> Result<Option<JobRow>, musaic_db::Error> {
    let rows = musaic_db::research_jobs::group(conn, id)?;
    (!rows.is_empty()).then(|| group(conn, &rows)).transpose()
}
pub(crate) fn jobs(
    conn: &Connection,
    statuses: Option<&[String]>,
) -> Result<Vec<JobRow>, musaic_db::Error> {
    let mut groups: BTreeMap<String, Vec<WorkRow>> = BTreeMap::new();
    for r in musaic_db::research_jobs::latest(conn, statuses, LIST_LIMIT)? {
        groups.entry(r.group_id.clone()).or_default().push(r);
    }
    let mut all = research_history::list_history_only(conn, statuses, LIST_LIMIT)?;
    for rows in groups.values() {
        all.push(group(conn, rows)?);
    }
    all.sort_by(|a, b| {
        b.created_at
            .cmp(&a.created_at)
            .then_with(|| a.id.cmp(&b.id))
    });
    all.truncate(LIST_LIMIT as usize);
    Ok(all)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewChild {
    pub id: String,
    pub kind: String,
    pub state: String,
    pub revision: i64,
    pub validation: Option<String>,
    pub digest: Option<String>,
    pub result: Option<WorkResult>,
    pub note: Option<String>,
    pub historical_result: Option<String>,
    pub change_id: Option<String>,
    pub apply_outcome: Option<musaic_commands::tools::operation::ApplyResult>,
}
pub(crate) fn children(
    app: &App,
    conn: &Connection,
    group: &str,
) -> Result<Vec<ReviewChild>, OpError> {
    let mut children = Vec::new();
    for row in musaic_db::research_jobs::group(conn, group)? {
        let result = row
            .result_id
            .as_deref()
            .map(|id| db::result(conn, id))
            .transpose()?
            .flatten();
        let historical_result =
            if result.is_none() && row.last_error.as_deref() == Some("legacy_result_unconverted") {
                row.legacy_job_id
                    .as_deref()
                    .map(|id| research_history::get(conn, id))
                    .transpose()?
                    .flatten()
                    .and_then(|j| j.result_json)
            } else {
                None
            };
        let change = row
            .result_id
            .as_deref()
            .map(|id| musaic_db::agent::change_of_proposal(conn, id))
            .transpose()?
            .flatten();
        let change_id = change
            .as_ref()
            .filter(|c| crate::agent::undo::research_change_undoable(app, conn, c))
            .map(|c| c.id.clone());
        let apply_outcome = change
            .as_ref()
            .and_then(|c| c.outcome.as_deref())
            .map(serde_json::from_str)
            .transpose()
            .map_err(OpError::internal)?;
        children.push(ReviewChild {
            change_id,
            apply_outcome,
            historical_result,
            id: row.id,
            kind: row.kind,
            state: row.state,
            revision: row.revision,
            digest: result.as_ref().map(|r| r.digest.clone()),
            validation: result.as_ref().map(|r| r.validation.clone()),
            result: result
                .map(|r| serde_json::from_str(&r.result_json))
                .transpose()
                .map_err(OpError::internal)?,
            note: row.last_error,
        });
    }
    Ok(children)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Preview {
    pub digest: String,
    pub revision: i64,
    pub diff: FixDiff,
    #[serde(skip)]
    pub cover_thumbnail: Option<Vec<u8>>,
}

struct Prepared {
    album: musaic_services::albums::AlbumRow,
    patch: ProposedPatch,
    cover_bytes: Option<Vec<u8>>,
    preview: Preview,
}
pub(crate) fn review_digest(plan: &agent_proposal::ProposalPlan, cover: Option<&[u8]>) -> String {
    let cover_digest = cover.map(|bytes| hex_encode(&Sha256::digest(bytes)));
    crate::research::digest(&crate::agent::to_json_text(&(
        &plan.digest,
        &plan.diff,
        cover_digest,
    )))
}

fn prepare(
    app: &App,
    conn: &mut Connection,
    work: &WorkRow,
    digest: &str,
) -> Result<Prepared, OpError> {
    let result = work
        .result_id
        .as_deref()
        .map(|id| db::result(conn, id))
        .transpose()?
        .flatten()
        .ok_or_else(|| conflict("No result is ready to review."))?;
    if work.state != "awaiting_review"
        || work.review_reserved
        || result.digest != digest
        || work.library_epoch != db::epoch(conn)?
    {
        return Err(conflict("The reviewed proposal has changed."));
    }
    if crate::research::digest(&result.result_json) != digest {
        return Err(conflict(
            "The stored proposal no longer matches the selected result.",
        ));
    }
    let body: WorkResult = serde_json::from_str(&result.result_json).map_err(OpError::internal)?;
    crate::research::current(app, conn, work).map_err(tool)?;
    // A verified metadata result remains usable offline. Cover bytes are always
    // fetched under the public image policy and bound to this exact preview.
    if result.validation != "verified"
        && !matches!(body.recommendation, Recommendation::Cover(_))
        && crate::research::verify_result(app, conn, work, &body).map_err(tool)? != "verified"
    {
        return Err(conflict(
            "Verification is pending. Reopen the preview when connected.",
        ));
    }
    let revision = &work.base_revision;
    let patch = crate::research::patch(work, &body).map_err(tool)?;
    let row =
        agent_read::cached_row(conn, app.library_dir().as_deref(), &work.album_id).map_err(tool)?;
    let plan = app
        .with_apply_env(conn, |env| {
            agent_proposal::plan(env, &row, revision, &patch)
        })
        .map_err(tool)?;
    let (cover_bytes, cover_thumbnail) = if let Recommendation::Cover(c) = &body.recommendation {
        let image = musaic_net::preview::fetch_preview(&app.0.transport, &c.url)
            .map_err(|_| conflict("The cover is unavailable."))?;
        let (width, height, thumb) = musaic_tags::cover::review_thumbnail(&image.bytes)
            .ok_or_else(|| conflict("The cover could not be verified."))?;
        if c.width.is_some_and(|w| w != width) || c.height.is_some_and(|h| h != height) {
            return Err(conflict("The cover dimensions changed."));
        }
        (Some(image.bytes), Some(thumb))
    } else {
        (None, None)
    };
    let digest = review_digest(&plan, cover_bytes.as_deref());
    let revision = if result.validation != "verified" {
        let updated = checked(db::set_verification(
            conn,
            &work.id,
            &result.digest,
            "verified",
            millis(app.now()),
        ))?;
        let revision = updated.revision;
        crate::agent::delegation::publish_pending(app, conn);
        revision
    } else {
        work.revision
    };
    Ok(Prepared {
        album: row,
        patch,
        cover_bytes,
        preview: Preview {
            digest,
            revision,
            diff: plan.diff,
            cover_thumbnail,
        },
    })
}
pub(crate) fn preview(app: &App, id: &str, digest: &str) -> Result<Preview, OpError> {
    let _guard = app.gate().begin_work()?;
    let mut conn = musaic_db::open_connection(app.db_path())?;
    let work = db::get(&conn, id)?.ok_or_else(|| conflict("The research item is unavailable."))?;
    Ok(prepare(app, &mut conn, &work, digest)?.preview)
}

pub(crate) fn decide(
    app: &App,
    id: &str,
    revision: i64,
    digest: Option<&str>,
    action: &str,
    preview: Option<&str>,
) -> Result<(), OpError> {
    let _guard = app.gate().begin(musaic_services::gate::WorkKind::Apply)?;
    let mut conn = musaic_db::open_connection(app.db_path())?;
    let work = db::get(&conn, id)?.ok_or_else(|| conflict("The research item is unavailable."))?;
    let now = || millis(app.now());
    match action {
        "cancel" => {
            checked(db::cancel_from_desktop(&conn, id, revision, now()))?;
        }
        "discard" => {
            checked(db::discard(&conn, id, digest, revision, now()))?;
            crate::agent::delegation::publish_pending(app, &conn);
            mark_reviewed(app, &conn, &work);
            return Ok(());
        }
        "retry" => {
            crate::research::current(app, &conn, &work).map_err(|_| {
                conflict("The album has changed. Open it and request new research.")
            })?;
            checked(db::retry(&conn, id, revision, now()))?;
        }
        "apply" => {
            let digest = digest.ok_or_else(|| conflict("Choose a research result."))?;
            let prepared = prepare(app, &mut conn, &work, digest)?;
            if Some(prepared.preview.digest.as_str()) != preview {
                return Err(conflict("Review the current changes first."));
            }
            let reserved = checked(db::reserve_review(&conn, id, digest, revision, now()))?;
            let applied = crate::agent::run::apply_research(
                app,
                &mut conn,
                prepared.album,
                prepared.patch,
                &prepared.preview.digest,
                prepared.cover_bytes,
                &reserved,
            );
            let disposition = if applied.is_ok() { "applied" } else { "failed" };
            checked(db::finish_review_with_error(
                &conn,
                id,
                reserved.revision,
                disposition,
                applied.as_ref().err().map(|e| e.message.as_str()),
                now(),
            ))?;
            crate::agent::delegation::publish_pending(app, &conn);
            if applied.is_ok() {
                mark_reviewed(app, &conn, &work);
            }
            return applied.map_err(tool);
        }
        _ => {
            return Err(OpError::Rejected {
                details: None,
                message: "Unknown research decision.".into(),
            });
        }
    }
    crate::agent::delegation::publish_pending(app, &conn);
    Ok(())
}

/// The album panel's "Reviewed" row: a metadata proposal the user applied
/// or discarded stamps the album's `metadata_ai_acted_on_at`, and
/// `AlbumTagsChanged` of that field refreshes the album caches (rule 1). A
/// cover proposal stamps nothing. The decision is already committed, so a
/// stamp that fails is logged, never returned.
fn mark_reviewed(app: &App, conn: &Connection, work: &WorkRow) {
    if !work.kind.starts_with("metadata.") {
        return;
    }
    if let Err(e) =
        musaic_db::albums::mark_metadata_ai_acted_on(conn, &work.album_id, &app.now_iso())
    {
        app.log().error(
            "musaic.research",
            &format!(
                "Couldn't stamp metadata_ai_acted_on_at for album {}: {e}",
                work.album_id
            ),
        );
        return;
    }
    app.bus().publish(&musaic_events::Event::AlbumTagsChanged {
        album_id: work.album_id.clone(),
        fields: vec!["metadata_ai_acted_on_at".into()],
    });
}

/// One delegated work item as the Review page lists it (spec 005): its state,
/// who holds it and how recently they showed life, and whether anyone is
/// listening for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DelegatedItem {
    pub work_id: String,
    /// The job card the item belongs to (what the job routes answer to).
    pub group_id: String,
    /// The bulk run that made it; `None` for a request made on its own.
    pub batch_id: Option<String>,
    pub album_id: String,
    /// "Artist - Title" when the album is known; Missing albums are listed.
    pub album_label: Option<String>,
    pub kind: String,
    pub state: String,
    pub revision: i64,
    pub holder: Option<String>,
    pub last_heartbeat_at: Option<String>,
    pub lease_expires_at: Option<String>,
    pub attempts: i64,
    pub max_attempts: i64,
    pub last_error: Option<String>,
    /// Queued, and no pairing has an active subscription to hear about it.
    pub waiting_for_agent: bool,
}

/// The six listed states, read from the database only: it writes nothing and
/// touches no file, so it is the same for an album whose folder is gone.
pub(crate) fn delegated(app: &App) -> Result<Vec<DelegatedItem>, OpError> {
    let conn = musaic_db::open_connection(app.db_path())?;
    delegated_items(app, &conn, db::desktop_items(&conn)?)
}

/// The desktop's view of `items` (labels, holder, listening).
pub(crate) fn delegated_items(
    app: &App,
    conn: &Connection,
    items: Vec<db::DesktopItem>,
) -> Result<Vec<DelegatedItem>, OpError> {
    let listening = db_listening(app, conn)?;
    let lib = app.library_dir();
    let mut out = Vec::new();
    for item in items {
        let row = item.row;
        let album_label = crate::ops::agent::album_label(conn, lib.as_deref(), &row.album_id);
        out.push(DelegatedItem {
            waiting_for_agent: row.state == "queued" && !listening,
            holder: item.holder,
            last_heartbeat_at: item.last_heartbeat_at.map(stamp),
            lease_expires_at: row.lease_expires_at.map(stamp),
            last_error: row.last_error.map(|e| {
                e.chars()
                    .filter(|c| !c.is_control())
                    .take(200)
                    .collect::<String>()
            }),
            work_id: row.id,
            group_id: row.group_id,
            batch_id: row.batch_id,
            album_id: row.album_id,
            album_label,
            kind: row.kind,
            state: row.state,
            revision: row.revision,
            attempts: row.attempts,
            max_attempts: row.max_attempts,
        });
    }
    Ok(out)
}

/// Whether any active pairing has a subscription (a parked or recent poll).
fn db_listening(app: &App, conn: &Connection) -> Result<bool, OpError> {
    Ok(musaic_db::agent::list_pairings(conn)?
        .iter()
        .any(|p| p.revoked_at.is_none() && app.0.agent.subscription_active(&p.id)))
}
