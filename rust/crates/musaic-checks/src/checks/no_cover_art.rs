//! `no-cover-art` (port of `checks/no_cover_art.py`): no embedded cover and
//! no sidecar, unless a cover job failed (or ended without a cover) in the
//! last 30 days. Detection only; the fix needs MusicBrainz/CAA, the apply
//! pipeline and the AI queue.

use crate::base::{
    Check, CheckCtx, CheckError, CheckMeta, CheckScope, Cost, Emit, Finding, FixKind, Severity,
};
use crate::pyjson::dict;

pub struct NoCoverArt;

/// `_RECENT_ATTEMPT_DAYS`.
pub const RECENT_ATTEMPT_DAYS: u32 = 30;

pub static META: CheckMeta = CheckMeta {
    id: "no-cover-art",
    label: "No cover art",
    description: "Albums missing both embedded cover art and a sidecar cover.jpg / folder.jpg / front.jpg. \
Fix tries MusicBrainz / Cover Art Archive first (free) and falls back to AI only when MB doesn't have a \
cover. Albums with a recent failed lookup are skipped so bulk passes don't keep retrying dead-ends.",
    category: "cover",
    cost: Cost::Cheap,
    runs_during_scan: true,
    fix_kind: FixKind::AiJob,
    bulk_action_label: Some("Try MusicBrainz on all"),
};

/// The Python app's query, verbatim in substance. Note (reproduced): `created_at`
/// is ISO-8601 with a `T` while SQLite's `datetime()` uses a space, so on
/// the cut-off day itself the comparison is decided by `'T' > ' '`.
fn sql() -> String {
    format!(
        "SELECT id, dir_name, dir_artist, dir_album FROM albums
         WHERE library_path = ?1
           AND missing_since IS NULL
           AND has_embedded_cover = 0
           AND cover_file IS NULL
           AND NOT EXISTS (
             SELECT 1 FROM ai_jobs j
             WHERE j.album_id = albums.id
               AND j.job_type IN ('cover', 'metadata+cover')
               AND j.created_at > datetime('now', '-{RECENT_ATTEMPT_DAYS} days')
               AND (
                 j.status = 'failed'
                 OR (j.status = 'acted_on'
                     AND COALESCE(json_extract(j.result_json, '$.cover.cover_url'), '') = '')
               )
           )"
    )
}

impl Check for NoCoverArt {
    fn meta(&self) -> &CheckMeta {
        &META
    }

    fn run(
        &self,
        ctx: &CheckCtx<'_>,
        scope: &CheckScope,
        emit: &mut Emit<'_>,
    ) -> Result<(), CheckError> {
        type Row = (String, Option<String>, Option<String>, Option<String>);
        let rows: Vec<Row> = {
            let mut stmt = ctx.conn.prepare(&sql())?;
            stmt.query_map([&scope.library_path], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?
            .collect::<rusqlite::Result<_>>()?
        };
        for (id, dir_name, dir_artist, dir_album) in rows {
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
