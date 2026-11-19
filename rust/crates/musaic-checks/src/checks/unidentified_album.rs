//! `unidentified-album` (port of `checks/unidentified_album.py`): albums with
//! no fingerprints and/or no MusicBrainz anchor (past the staleness window).

use std::time::Duration;

use crate::base::{
    Check, CheckCtx, CheckError, CheckMeta, CheckScope, Cost, Emit, Finding, FixKind, Granularity,
    Severity,
};
use crate::pyjson::{PyJson, dict};

/// `mb_lookup.CACHE_REFRESH_DAYS`.
pub const CACHE_REFRESH_DAYS: u64 = 90;

pub struct UnidentifiedAlbum;

pub static META: CheckMeta = CheckMeta {
    id: "unidentified-album",
    label: "Album not yet identified",
    description: "Albums not yet matched to a release on MusicBrainz, or whose tracks have no audio fingerprints yet. \
Most other fixes need both: fingerprints find duplicate and misnumbered tracks, and the MusicBrainz \
match is where covers, tags and track lists are looked up. \"Identify all\" fingerprints and matches \
the whole library in one go.",
    category: "external",
    cost: Cost::Cheap,
    runs_during_scan: true,
    fix_kind: FixKind::AiJob,
    bulk_action_label: Some("Identify all"),
};

/// `_summarize`: which prerequisites are missing.
pub fn summarize(
    needs_fp: bool,
    needs_anchor: bool,
    previously_attempted: bool,
    last_checked_at: Option<&str>,
) -> String {
    let at = last_checked_at.unwrap_or("None");
    if needs_fp && needs_anchor {
        if previously_attempted {
            return format!(
                "Not fingerprinted; MusicBrainz match was attempted ({at}) but staleness window has elapsed"
            );
        }
        return "Not fingerprinted and not yet identified on MusicBrainz".into();
    }
    if needs_fp {
        return "Not yet fingerprinted (AcoustID coverage missing)".into();
    }
    if previously_attempted {
        return format!("Re-checking after staleness window — last MB lookup was {at}");
    }
    "Not yet identified on MusicBrainz".into()
}

const SQL: &str = "
    SELECT
        a.id, a.dir_name, a.dir_artist, a.dir_album, a.title, a.album_artist, a.track_count,
        a.mb_release_group_mbid, a.mb_last_checked_at,
        (NOT EXISTS (SELECT 1 FROM track_fingerprints tf WHERE tf.album_id = a.id)) AS needs_fingerprints,
        (a.mb_release_group_mbid IS NULL
         AND (a.mb_last_checked_at IS NULL OR a.mb_last_checked_at < ?1)) AS needs_anchor
    FROM albums a
    WHERE a.library_path = ?2
      AND a.missing_since IS NULL
      AND a.track_count > 0
      AND (
        NOT EXISTS (SELECT 1 FROM track_fingerprints tf WHERE tf.album_id = a.id)
        OR (a.mb_release_group_mbid IS NULL
            AND (a.mb_last_checked_at IS NULL OR a.mb_last_checked_at < ?1))
      )";

struct Row {
    id: String,
    dir_name: Option<String>,
    dir_artist: Option<String>,
    dir_album: Option<String>,
    track_count: i64,
    mbid: Option<String>,
    last_checked: Option<String>,
    needs_fp: bool,
    needs_anchor: bool,
}

impl Check for UnidentifiedAlbum {
    fn meta(&self) -> &CheckMeta {
        &META
    }

    /// One query over the library: rerun library-wide.
    fn granularity(&self) -> Granularity {
        Granularity::Library
    }

    /// A finding also appears with time alone (past the MusicBrainz staleness window).
    fn refresh_after(&self) -> Option<std::time::Duration> {
        Some(std::time::Duration::from_secs(24 * 3600))
    }

    fn run(
        &self,
        ctx: &CheckCtx<'_>,
        scope: &CheckScope,
        emit: &mut Emit<'_>,
    ) -> Result<(), CheckError> {
        let now = ctx.env.clock.now();
        let cutoff = musaic_db::backup::iso_seconds(
            now - Duration::from_secs(CACHE_REFRESH_DAYS * 24 * 3600),
        );
        let rows: Vec<Row> = {
            let mut stmt = ctx.conn.prepare(SQL)?;
            stmt.query_map(rusqlite::params![cutoff, scope.library_path], |r| {
                Ok(Row {
                    id: r.get(0)?,
                    dir_name: r.get(1)?,
                    dir_artist: r.get(2)?,
                    dir_album: r.get(3)?,
                    track_count: r.get(6)?,
                    mbid: r.get(7)?,
                    last_checked: r.get(8)?,
                    needs_fp: r.get::<_, i64>(9)? != 0,
                    needs_anchor: r.get::<_, Option<i64>>(10)?.unwrap_or(0) != 0,
                })
            })?
            .collect::<rusqlite::Result<_>>()?
        };
        for row in rows {
            let previously_attempted =
                row.last_checked.is_some() && row.mbid.as_deref().is_none_or(str::is_empty);
            let summary = summarize(
                row.needs_fp,
                row.needs_anchor,
                previously_attempted,
                row.last_checked.as_deref(),
            );
            emit(Finding {
                album_id: Some(row.id),
                summary,
                severity: Severity::Info,
                detail: Some(dict([
                    ("dir_name", row.dir_name.into()),
                    ("dir_artist", row.dir_artist.into()),
                    ("dir_album", row.dir_album.into()),
                    ("track_count", PyJson::Int(row.track_count)),
                    ("needs_fingerprints", row.needs_fp.into()),
                    ("needs_anchor", row.needs_anchor.into()),
                    ("previously_attempted", previously_attempted.into()),
                    ("mb_last_checked_at", row.last_checked.into()),
                ])),
            })?;
        }
        Ok(())
    }
}
