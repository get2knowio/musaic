//! `no-cover-art` (port of `checks/no_cover_art.py`): no embedded cover and
//! no sidecar, unless cover research (an old job or a native request) failed
//! or ended without a cover in the last 30 days. Detection only; the fix
//! needs MusicBrainz/CAA, the apply pipeline and research requests.

use crate::base::{
    Check, CheckCtx, CheckError, CheckMeta, CheckScope, Cost, Emit, Finding, FixKind, Granularity,
    Severity,
};
use crate::pyjson::dict;

pub struct NoCoverArt;

/// `_RECENT_ATTEMPT_DAYS`.
pub const RECENT_ATTEMPT_DAYS: u32 = 30;

pub static META: CheckMeta = CheckMeta {
    id: "no-cover-art",
    label: "No cover art",
    description: "Albums with no cover: none embedded in the files and no cover.jpg, folder.jpg or front.jpg in the \
folder. Fix looks for one on MusicBrainz and the Cover Art Archive first, and asks AI only when they \
have none. An album whose lookup failed recently is skipped for a while, so a bulk run does not keep \
retrying it.",
    category: "cover",
    cost: Cost::Cheap,
    runs_during_scan: true,
    fix_kind: FixKind::AiJob,
    bulk_action_label: Some("Try MusicBrainz on all"),
};

/// The Python app's query, with one deliberate change: the window is
/// compared as points in time (`julianday` of both sides; the cut-off,
/// `?2`, comes from the check's clock). The Python app compared the
/// `created_at` text (ISO-8601 with a `T`) with SQLite's `datetime()` text (a
/// space), so on the cut-off date `'T' > ' '` counted a job up to a day older
/// than the window as recent.
///
/// A recent attempt is either kind of research record (schema v24):
///
/// * an old job's history record (`research_history`, the Python rule on
///   the old data): a cover job that failed, or was acted on with no
///   `cover.cover_url` in its stored result;
/// * a native `cover.research` request (`legacy_job_id IS NULL`) created
///   within the window that failed, or was applied or discarded with no
///   cover recommendation in its result (a no-match or unresolved answer
///   included). A request linked to an old job is judged only by its
///   history record: it has no `work_results` row, so this rule would read
///   every applied old cover job as "no candidate".
const SQL: &str = "SELECT id, dir_name, dir_artist, dir_album FROM albums
     WHERE library_path = ?1
       AND missing_since IS NULL
       AND has_embedded_cover = 0
       AND cover_file IS NULL
       AND NOT EXISTS (
         SELECT 1 FROM research_history j
         WHERE j.album_id = albums.id
           AND j.job_type IN ('cover', 'metadata+cover')
           AND julianday(j.created_at) > julianday(?2)
           AND (
             j.status = 'failed'
             OR (j.status = 'acted_on'
                 AND COALESCE(json_extract(j.result_json, '$.cover.cover_url'), '') = '')
           )
       )
       AND NOT EXISTS (
         SELECT 1 FROM delegated_work w
         LEFT JOIN work_results r ON r.id = w.result_id
         WHERE w.album_id = albums.id
           AND w.kind = 'cover.research'
           AND w.legacy_job_id IS NULL
           AND w.created_at > CAST(unixepoch(?2) AS INTEGER) * 1000
           AND (
             w.state = 'failed'
             OR (w.state IN ('applied', 'discarded')
                 AND COALESCE(json_extract(r.result_json, '$.recommendation.type'), '') <> 'cover')
           )
       )";

impl Check for NoCoverArt {
    fn meta(&self) -> &CheckMeta {
        &META
    }

    /// One query over the library: rerun library-wide.
    fn granularity(&self) -> Granularity {
        Granularity::Library
    }

    /// A finding also appears with time alone (past the recent-attempt window).
    fn refresh_after(&self) -> Option<std::time::Duration> {
        Some(std::time::Duration::from_secs(24 * 3600))
    }

    fn run(
        &self,
        ctx: &CheckCtx<'_>,
        scope: &CheckScope,
        emit: &mut Emit<'_>,
    ) -> Result<(), CheckError> {
        type Row = (String, Option<String>, Option<String>, Option<String>);
        let cutoff = musaic_db::backup::iso_seconds(
            ctx.env.clock.now()
                - std::time::Duration::from_secs(u64::from(RECENT_ATTEMPT_DAYS) * 24 * 3600),
        );
        let rows: Vec<Row> = {
            let mut stmt = ctx.conn.prepare(SQL)?;
            stmt.query_map(rusqlite::params![scope.library_path, cutoff], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?
            .collect::<rusqlite::Result<_>>()?
        };
        for (id, dir_name, dir_artist, dir_album) in rows {
            ctx.checkpoint()?;
            emit(Finding {
                album_id: Some(id),
                summary: "No cover art (embedded or sidecar)".into(),
                severity: Severity::Info,
                detail: Some(dict([
                    ("dir_name", dir_name.into()),
                    ("dir_artist", dir_artist.into()),
                    ("dir_album", dir_album.into()),
                ])),
            })?;
        }
        Ok(())
    }
}
