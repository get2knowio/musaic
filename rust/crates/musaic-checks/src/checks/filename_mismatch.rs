//! `filename-mismatch` (port of `checks/filename_mismatch.py::run`): FLACs
//! whose names, or album directories whose names, differ from the templates.
//! Planning is musaic-fsops' (`check_album` / `check_dir` / `derive_plan`,
//! normalisation-only differences yield nothing.

use std::path::Path;

use musaic_db::renames::RenamePlanRow;
use musaic_fsops::plan::{Templates, check_album, check_dir, summarize};

use crate::base::{
    Check, CheckCtx, CheckError, CheckMeta, CheckScope, Cost, Emit, Finding, FixKind, ScopeKind,
    Severity,
};
use crate::pyjson::{PyJson, dict};

pub struct FilenameMismatch;

pub static META: CheckMeta = CheckMeta {
    id: "filename-mismatch",
    label: "Filename doesn't match template",
    description: "Albums with files not named the way your file name pattern says (Settings, Filename templates). \
Names drift: rippers use different conventions, tags get edited after the rip, or you change the \
pattern. Resolve shows the current and the new name of every track, then renames them together.",
    category: "files",
    cost: Cost::Medium,
    runs_during_scan: false,
    fix_kind: FixKind::Deterministic,
    bulk_action_label: None,
};

const COLS: &str = "id, path, dir_name, album_artist, artist, dir_artist, title, year, \
                    is_compilation, disc_number, disc_total";

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<RenamePlanRow> {
    Ok(RenamePlanRow {
        id: r.get(0)?,
        path: r.get(1)?,
        dir_name: r.get(2)?,
        album_artist: r.get(3)?,
        artist: r.get(4)?,
        dir_artist: r.get(5)?,
        title: r.get(6)?,
        year: r.get(7)?,
        is_compilation: r.get::<_, Option<i64>>(8)?.unwrap_or(0) != 0,
        disc_number: r.get(9)?,
        disc_total: r.get(10)?,
    })
}

impl Check for FilenameMismatch {
    fn meta(&self) -> &CheckMeta {
        &META
    }

    /// The four templates the expected names are rendered from.
    fn input_extras(&self, ctx: &CheckCtx<'_>) -> Result<String, CheckError> {
        let t = Templates::from_settings(ctx.conn)?;
        Ok(serde_json::json!([
            t.filename_single,
            t.filename_compilation,
            t.dir_single,
            t.dir_compilation
        ])
        .to_string())
    }

    fn run(
        &self,
        ctx: &CheckCtx<'_>,
        scope: &CheckScope,
        emit: &mut Emit<'_>,
    ) -> Result<(), CheckError> {
        let templates = Templates::from_settings(ctx.conn)?;
        let rows: Vec<RenamePlanRow> = if scope.kind == ScopeKind::Albums {
            let ids: Vec<&str> = scope.albums.iter().map(|a| a.id.as_str()).collect();
            if ids.is_empty() {
                return Ok(());
            }
            let sql = format!(
                "SELECT {COLS} FROM albums WHERE library_path = ? AND id IN ({}) AND missing_since IS NULL",
                vec!["?"; ids.len()].join(",")
            );
            let mut params: Vec<&str> = vec![&scope.library_path];
            params.extend(ids);
            let mut stmt = ctx.conn.prepare(&sql)?;
            stmt.query_map(rusqlite::params_from_iter(params), row)?
                .collect::<rusqlite::Result<_>>()?
        } else {
            let mut stmt = ctx.conn.prepare(&format!(
                "SELECT {COLS} FROM albums WHERE library_path = ?1 AND missing_since IS NULL"
            ))?;
            stmt.query_map([&scope.library_path], row)?
                .collect::<rusqlite::Result<_>>()?
        };
        for r in rows {
            let Some(path) = r.path.as_deref().filter(|p| !p.is_empty()) else {
                continue;
            };
            // A folder that cannot be read (an unmounted drive, a folder
            // gone since the last scan) says nothing about its names.
            if !Path::new(path).is_dir() {
                ctx.could_not_evaluate(&r.id);
                continue;
            }
            let (template, dir_template) = if r.is_compilation {
                (&templates.filename_compilation, &templates.dir_compilation)
            } else {
                (&templates.filename_single, &templates.dir_single)
            };
            let album_path = Path::new(path);
            // A render error skips the album, logged as filename_mismatch.py:139
            // `log.exception("filename-mismatch: error scanning album %s",
            // album_path)` does (ERROR; the error text stands in for the
            // traceback).
            let planned = check_album(album_path, template, &r)
                .and_then(|renames| Ok((renames, check_dir(album_path, dir_template, &r)?)));
            let (renames, dir_rename) = match planned {
                Ok(p) => p,
                Err(e) => {
                    musaic_log::error(
                        "musaic.checks.filename_mismatch",
                        &format!("filename-mismatch: error scanning album {path}: {e}"),
                    );
                    ctx.could_not_evaluate(&r.id);
                    continue;
                }
            };
            if renames.is_empty() && dir_rename.is_none() {
                continue;
            }
            let headline = r
                .dir_name
                .clone()
                .filter(|d| !d.is_empty())
                .unwrap_or_else(|| r.id.clone());
            emit(Finding {
                album_id: Some(r.id.clone()),
                summary: summarize(r.dir_name.as_deref(), renames.len(), dir_rename.is_some()),
                severity: Severity::Info,
                detail: Some(dict([
                    ("dir_name", r.dir_name.clone().into()),
                    ("is_compilation", r.is_compilation.into()),
                    ("template", template.as_str().into()),
                    ("dir_template", dir_template.as_str().into()),
                    ("headline", headline.into()),
                    (
                        "renames",
                        PyJson::List(
                            renames
                                .iter()
                                .map(|e| {
                                    dict([
                                        ("filename", e.filename.clone().into()),
                                        ("expected", e.expected.clone().into()),
                                        ("tracknumber", e.tracknumber.clone().into()),
                                        ("title", e.title.clone().into()),
                                        ("artist", e.artist.clone().into()),
                                        ("collides_with", e.collides_with.clone().into()),
                                    ])
                                })
                                .collect(),
                        ),
                    ),
                    (
                        "dir_rename",
                        dir_rename.map_or(PyJson::Null, |d| {
                            dict([("old", d.old.into()), ("new", d.new.into())])
                        }),
                    ),
                ])),
            })?;
        }
        Ok(())
    }
}
