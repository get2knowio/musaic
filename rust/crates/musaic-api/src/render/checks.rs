//! The `checks` group as bodies and frames: the check list and detail, the
//! findings, a run, the three previews, the canonical recording and the frames
//! of the check-runs stream, in the keys and order the Python app answered.

use musaic_app::ops::checks::{
    CanonicalRecording, CheckList, CheckRun, CheckRunsState, ConversionPreview, PlanPreview,
    SweepState,
};
use musaic_app::ops::record::{
    CheckDetailRecord, CheckSummaryRecord, ClusterPreview, FindingRecord,
};
use musaic_checks::runner;
use musaic_fsops::multi_disc::PreviewState;
use musaic_services::Json;
use serde_json::{Value, json};

use crate::sse;

/// Key order of the Python app's dicts, for bodies the checks crate builds as
/// (unordered) `serde_json::Value`s; SSE frames are compared byte for byte.
const KEY_ORDER: &[&str] = &[
    // summary
    "meta",
    "open_count",
    "last_run",
    // meta
    "id",
    "label",
    "description",
    "category",
    "cost",
    "runs_during_scan",
    "fix_kind",
    "bulk_action_label",
    // run
    "check_id",
    "started_at",
    "completed_at",
    "status",
    "library_path",
    "scope",
    "issue_count",
    "error",
    "duration_ms",
];

/// A `serde_json::Value` as an ordered Python dict.
pub fn ordered(v: &Value) -> Json {
    match v {
        Value::Null => Json::Null,
        Value::Bool(b) => Json::Bool(*b),
        Value::Number(n) => n
            .as_i64()
            .map(Json::Int)
            .unwrap_or_else(|| Json::Float(n.as_f64().unwrap_or(0.0))),
        Value::String(s) => Json::Str(s.clone()),
        Value::Array(a) => Json::Arr(a.iter().map(ordered).collect()),
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort_by_key(|k| {
                (
                    KEY_ORDER
                        .iter()
                        .position(|o| o == k)
                        .unwrap_or(KEY_ORDER.len()),
                    (*k).clone(),
                )
            });
            Json::Obj(
                keys.into_iter()
                    .map(|k| (k.clone(), ordered(&m[k])))
                    .collect(),
            )
        }
    }
}

/// `GET /api/checks`.
pub fn check_list(checks: &[CheckSummaryRecord]) -> Value {
    json!({"checks": checks.iter().map(|c| c.document().clone()).collect::<Vec<_>>()})
}

/// `GET /api/checks/{check_id}`.
pub fn check_detail(c: &CheckDetailRecord) -> Value {
    c.document().clone()
}

/// `GET /api/checks/findings`.
pub fn finding_list(findings: &[FindingRecord]) -> Value {
    json!({"findings": findings.iter().map(|f| f.document().clone()).collect::<Vec<_>>()})
}

/// The answer of ignore / unignore: the finding, or null when nothing changed.
pub fn finding_or_null(f: Option<&FindingRecord>) -> Value {
    f.map_or(Value::Null, |f| f.document().clone())
}

/// `POST /api/checks/{check_id}/run`.
pub fn run(r: &CheckRun) -> Value {
    runner::run_to_json(r)
}

/// `GET /api/checks/duplicate-tracks/findings/{id}/cluster-preview`.
pub fn cluster_preview(c: &ClusterPreview) -> Value {
    c.document().clone()
}

/// `GET /api/checks/duplicate-tracks/findings/{id}/canonical`: `{title, artist,
/// mbid, reason, reason_code}` in that key order.
pub fn canonical(c: &CanonicalRecording) -> Json {
    let s = |v: Option<&str>| v.map_or(Json::Null, |v| Json::Str(v.to_owned()));
    Json::obj([
        ("title", s(c.title.as_deref())),
        ("artist", s(c.artist.as_deref())),
        ("mbid", s(c.mbid.as_deref())),
        ("reason", s(c.reason.as_ref().map(|r| r.text.as_str()))),
        ("reason_code", s(c.reason.as_ref().map(|r| r.code))),
    ])
}

/// `GET /api/checks/sweep`.
pub fn sweep(s: &SweepState) -> Json {
    s.to_json()
}

/// The stream's `state` frame: the check list, in the Python app's key order.
pub fn state_frame(checks: &[CheckSummaryRecord]) -> String {
    let list = Value::Array(checks.iter().map(|c| c.document().clone()).collect());
    let data = Json::obj([("checks", ordered(&list))]).dumps();
    sse::frame("state", &data)
}

/// The stream's `sweep` frame.
pub fn sweep_frame(s: &SweepState) -> String {
    sse::frame("sweep", &sweep(s).dumps())
}

/// The frames one value of the check-runs feed makes, in the order they are
/// sent: `state` when the checks changed, `sweep` when the sweep did. `None`
/// when the checks could not be read: the stream ends.
pub fn frames(state: &CheckRunsState) -> Option<Vec<String>> {
    let mut out = Vec::new();
    match &state.checks {
        CheckList::Unchanged => {}
        CheckList::Listed(l) => out.push(state_frame(l)),
        CheckList::Unreadable => return None,
    }
    if let Some(s) = &state.sweep {
        out.push(sweep_frame(s));
    }
    Some(out)
}

/// `GET /api/checks/filename-mismatch/findings/{id}/plan-preview`.
pub fn plan_preview(p: &PlanPreview) -> Value {
    let (album_id, plan) = match p {
        PlanPreview::AlbumGone {
            album_id,
            dir_name,
            headline,
        } => {
            return json!({
                "album_id": album_id,
                "is_empty": true,
                "album_missing": true,
                "dir_name": dir_name,
                "is_compilation": false,
                "template": "",
                "dir_template": "",
                "headline": headline,
                "renames": [],
                "dir_rename": null,
            });
        }
        PlanPreview::Derived { album_id, plan } => (album_id, plan),
    };
    let renames: Vec<Value> = plan
        .renames
        .iter()
        .map(|r| {
            json!({
                "filename": r.filename,
                "expected": r.expected,
                "tracknumber": r.tracknumber,
                "title": r.title,
                "artist": r.artist,
                "collides_with": r.collides_with,
            })
        })
        .collect();
    let mut body = json!({
        "album_id": album_id,
        "is_empty": plan.is_empty(),
        "dir_name": plan.dir_name,
        "is_compilation": plan.is_compilation,
        "template": plan.template,
        "dir_template": plan.dir_template,
        "headline": plan.headline,
        "renames": renames,
        "dir_rename": plan.dir_rename.as_ref().map(|d| json!({"old": d.old, "new": d.new})),
    });
    if plan.album_missing {
        body["album_missing"] = json!(true);
    }
    if let Some(e) = &plan.error {
        body["error"] = json!(e);
    }
    body
}

/// `GET /api/checks/multi-disc-layout/findings/{id}/conversion-preview`.
pub fn conversion_preview(c: &ConversionPreview) -> Value {
    let (id, headline, records_dir, p) = match c {
        ConversionPreview::NotInLibrary {
            finding_id,
            album_id,
            headline,
        } => {
            return json!({
                "finding_id": finding_id,
                "album_id": album_id,
                "state": PreviewState::NotASet.as_str(),
                "convertible": false,
                "conversion_available": false,
                "headline": headline,
                "reason": "This album is no longer in the library.",
                "members": [],
                "tracks": [],
                "folders": [],
                "blockers": [],
            });
        }
        ConversionPreview::Derived {
            finding_id,
            headline,
            records_dir,
            preview,
        } => (*finding_id, headline, records_dir, preview.as_ref()),
    };
    let plan = &p.plan;
    let tracks: Vec<Value> = plan
        .tracks
        .iter()
        .map(|t| {
            json!({
                "album_id": t.album_id,
                "disc": t.disc,
                "filename": t.filename,
                "title": t.title,
                "tracknumber": t.tracknumber,
                "track": t.track,
                "new_track": t.new_track,
                "new_filename": t.new_filename,
                "has_disc_tags": t.has_disc_tags,
                "tags_done": t.tags_done,
                "state": t.state.as_str(),
                "folder": t.folder,
            })
        })
        .collect();
    let folders: Vec<Value> = plan
        .folders
        .iter()
        .map(|f| {
            json!({
                "album_id": f.album_id,
                "dir_name": f.dir_name,
                "disc": f.disc,
                "destination": f.destination,
                "covers": f.covers.iter().map(|c| json!({"name": c.name, "action": c.action.as_str(), "done": c.done})).collect::<Vec<_>>(),
                "sidecar": f.sidecar,
                "sidecar_done": f.sidecar_done,
                "other_files": f.other_files,
                "strays": f.strays.iter().map(|s| json!({
                    "name": s.name,
                    "action": s.action.as_str(),
                    "new_name": s.new_name,
                    "done": s.done,
                })).collect::<Vec<_>>(),
                "junk": f.junk.iter().map(|(n, d)| json!({"name": n, "done": d})).collect::<Vec<_>>(),
                "exists": f.exists,
            })
        })
        .collect();
    let converts = matches!(p.state, PreviewState::OneFolder | PreviewState::Folders);
    let resume = p.resume.as_ref().map(|r| {
        json!({
            "record": r.path.to_string_lossy(),
            "started_at": r.started_at,
            "attempts": r.attempts.len(),
        })
    });
    let gather = p.gather.as_ref().map(|g| {
        json!({
            "folded": g.folded,
            "folder_renamed": g.folder_renamed,
            "merges": g.members.iter().skip(1).filter(|m| m.has_row).count(),
            "orphan_rows": g.orphan_rows.len(),
        })
    });
    json!({
        "finding_id": id,
        "album_id": p.album_id,
        "state": p.state.as_str(),
        "convertible": p.convertible(),
        "conversion_available": p.conversion_available(),
        "gather": gather,
        // Where the conversion keeps its record, and where what it no
        // longer needs goes: the confirmation names both.
        "records_dir": records_dir,
        "digest": p.digest(),
        "resume": resume,
        "headline": headline,
        "reason": p.reason,
        "numbering": p.numbering.map(|n| n.as_str()),
        "disc_total": p.disc_total,
        "present_discs": p.present_discs,
        "absent_discs": p.absent_discs,
        "members": p.members.iter().map(|m| json!({
            "album_id": m.album_id,
            "dir_name": m.dir_name,
            "disc": m.disc,
            "missing": m.missing,
        })).collect::<Vec<_>>(),
        "is_compilation": p.is_compilation,
        "template": p.template,
        "dir_template": p.dir_template,
        "tracks": tracks,
        "track_total": converts.then_some(plan.track_total),
        "target_folder": converts.then(|| json!({
            "current": p.target_current,
            "new": plan.target_name,
        })),
        "removes_disc_tags": converts,
        "folders": folders,
        "blockers": plan.blockers.iter().map(|b| json!({"code": b.code, "message": b.message})).collect::<Vec<_>>(),
    })
}
