//! `multi-disc-layout` (not a check of the Python app): multi-disc sets that
//! are not in the one-folder layout. Detection is musaic-fsops'
//! ([`musaic_fsops::multi_disc::detect`]), over album rows only, so a run
//! reads no file; the Resolve dialog's preview re-derives the plan from disk
//! (`musaic_fsops::multi_disc::derive_conversion`).
//!
//! One finding per set, stored on its anchor album (the one folder, disc
//! 1's folder, or the lowest disc present of an incomplete set). The summary
//! names the set's shape and size only, so it stays the same while nothing
//! changes, and a set that changes shape is a new finding (an ignore follows
//! it, see `findings::inherit_ignore`). Member ids in the detail are labels
//! (rule 2).

use musaic_fsops::multi_disc::{DiscSet, Numbering, SetKind, detect, load_rows};
use musaic_fsops::plan::Templates;
use musaic_fsops::py_repr;

use crate::base::{
    Check, CheckCtx, CheckError, CheckMeta, CheckScope, Cost, Emit, Finding, FixKind, Severity,
};
use crate::pyjson::{PyJson, dict};

pub struct MultiDiscLayout;

pub static META: CheckMeta = CheckMeta {
    id: "multi-disc-layout",
    label: "Multi-disc set not in one folder",
    description: "Multi-disc sets kept as one folder per disc, or one folder whose tracks are numbered \
per disc (or still carry disc tags). The layout Musaic aims for is one folder per set, tracks numbered \
straight through all the discs, no disc number in the file names and no disc tags. Sets are found only \
from disc numbers in the files' tags or in folder names such as \"(disc 2 of 3)\", never guessed. \
Preview shows what a conversion would do; converting is not available yet.",
    category: "files",
    cost: Cost::Cheap,
    runs_during_scan: false,
    fix_kind: FixKind::None,
    bulk_action_label: None,
};

fn ints(v: &[i64]) -> PyJson {
    PyJson::List(v.iter().map(|&i| PyJson::Int(i)).collect())
}

fn join(v: &[i64]) -> String {
    v.iter().map(i64::to_string).collect::<Vec<_>>().join(", ")
}

fn s(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// The finding of one set.
pub fn finding_for(set: &DiscSet) -> Finding {
    let anchor = set
        .members
        .iter()
        .find(|m| m.album_id == set.anchor)
        .or(set.members.first());
    let label = py_repr(
        anchor
            .and_then(|m| m.dir_name.as_deref())
            .filter(|d| !d.is_empty())
            .unwrap_or(&set.anchor),
    );
    let mut detail: Vec<(&str, PyJson)> = Vec::new();
    let summary = match &set.kind {
        SetKind::OneFolder { discs, numbering } => {
            detail.push(("kind", "one_folder".into()));
            detail.push(("numbering", numbering.as_str().into()));
            detail.push(("discs", ints(discs)));
            if *numbering == Numbering::Continuous {
                format!(
                    "{label} is numbered straight through its {} discs but still has disc tags",
                    discs.len()
                )
            } else {
                format!("{label} holds {} discs in one folder", discs.len())
            }
        }
        SetKind::Folders { total } => {
            detail.push(("kind", "folders".into()));
            detail.push(("disc_total", PyJson::Int(*total)));
            format!("{label} is disc 1 of a set kept in {total} folders")
        }
        SetKind::Incomplete {
            total,
            present,
            absent,
        } => {
            detail.push(("kind", "incomplete".into()));
            detail.push(("disc_total", total.map_or(PyJson::Null, PyJson::Int)));
            detail.push(("present", ints(present)));
            detail.push(("absent", ints(absent)));
            let of = total.map(|t| format!(" of {t}")).unwrap_or_default();
            format!(
                "{label}: only disc{} {}{of} found, disc{} {} not",
                s(present.len()),
                join(present),
                s(absent.len()),
                join(absent)
            )
        }
    };
    detail.push(("headline", label.into()));
    detail.push((
        "members",
        PyJson::List(
            set.members
                .iter()
                .map(|m| {
                    dict([
                        ("album_id", m.album_id.clone().into()),
                        ("dir_name", m.dir_name.clone().into()),
                        ("disc", PyJson::Int(m.disc)),
                    ])
                })
                .collect(),
        ),
    ));
    Finding {
        album_id: Some(set.anchor.clone()),
        summary,
        // A layout to tidy, not something wrong with the music.
        severity: Severity::Info,
        detail: Some(PyJson::Dict(
            detail
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
        )),
    }
}

impl Check for MultiDiscLayout {
    fn meta(&self) -> &CheckMeta {
        &META
    }

    /// A set spans albums and its finding is stored on one of them, so
    /// every run reads the whole library and resolves across it.
    fn sees_whole_library(&self) -> bool {
        true
    }

    /// The four templates the conversion renders names from. Detection
    /// does not read them yet; they are part of the inputs so that a rule
    /// which does (a set already named as the templates want) is rerun
    /// when they change, without anyone having to remember this list.
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
        let rows = load_rows(ctx.conn, &scope.library_path)?;
        let detection = detect(&rows);
        // Not read yet (a row from before the per-file disc numbers were
        // stored): keep whatever it had until a scan reads it.
        for id in &detection.unknown {
            ctx.could_not_evaluate(id);
        }
        for set in &detection.sets {
            // A set with a Missing disc cannot be judged now: its findings
            // stay as they are until the folder returns or is removed.
            if set.has_missing() {
                for id in set.album_ids() {
                    ctx.could_not_evaluate(id);
                }
                continue;
            }
            emit(finding_for(set))?;
        }
        Ok(())
    }
}
