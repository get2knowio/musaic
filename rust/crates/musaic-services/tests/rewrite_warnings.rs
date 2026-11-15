//! What a rewritten FLAC could not keep (`musaic_tags::preserve`) reaches
//! the per-file stream events as `warnings`, a key present only when there
//! is one, so every event the Python app recorded stays as it was.

// The end-to-end test needs hard links and is unix-only; its helpers and
// imports are unused elsewhere.
#![cfg_attr(not(unix), allow(unused_imports, dead_code))]

mod common;

use std::ops::ControlFlow;
use std::path::{Path, PathBuf};

use common::*;
use musaic_events::Bus;
use musaic_scan::TrackOrder;
use musaic_services::analyze::{
    AnalysisKind, AnalyzeEnv, AnalyzeOutcome, AnalyzeRequest, Analyzer, analyze_album,
};
use musaic_services::apply::{ApplyItem, tag_event_json};
use musaic_tags::{Event, FileChange};
use serde_json::{Value, json};

#[test]
fn apply_file_done_has_warnings_only_when_there_are_some() {
    let mut change = FileChange {
        path: PathBuf::from("/music/A - B [FLAC]/01 - One.flac"),
        filename: "01 - One.flac".into(),
        diffs: Vec::new(),
        art_changed: false,
        saved: true,
        error: None,
        renamed_to: None,
        warnings: Vec::new(),
    };
    let plain = tag_event_json(&Event::FileDone(change.clone())).to_value();
    assert!(plain.get("warnings").is_none(), "{plain}");
    change.warnings = vec!["Extended attribute \"user.x\" could not be kept.".into()];
    let warned = tag_event_json(&Event::FileDone(change)).to_value();
    assert_eq!(
        warned["warnings"],
        json!(["Extended attribute \"user.x\" could not be kept."])
    );
}

struct FixedBpm;

impl Analyzer for FixedBpm {
    fn bpm(&self, _: &Path) -> Result<f64, String> {
        Ok(120.0)
    }
    fn loudness(&self, _: &Path) -> Result<(f64, f64), String> {
        Ok((-14.0, 0.5))
    }
}

/// Two FLACs without padding (any tag write rewrites them), one of them
/// hard-linked elsewhere: only that one's `file_done` carries a warning.
#[cfg(unix)]
#[test]
fn analyze_file_done_reports_a_broken_hard_link() {
    let td = tempfile::tempdir().unwrap();
    let album = td.path().join("Test Artist - Test Album [FLAC]");
    std::fs::create_dir(&album).unwrap();
    for name in ["01 - One.flac", "02 - Two.flac"] {
        let tags = vec![("title".to_string(), name.to_string())];
        std::fs::write(album.join(name), flac_bytes(&tags, 1, 0)).unwrap();
    }
    std::fs::hard_link(
        album.join("01 - One.flac"),
        td.path().join("elsewhere.flac"),
    )
    .unwrap();

    let mut conn = connect(&fresh_db(td.path()));
    let bus = Bus::new();
    let now = || "2030-01-01T00:00:00Z".to_string();
    let mut env = AnalyzeEnv {
        conn: &mut conn,
        bus: &bus,
        analyzer: &FixedBpm,
        now: &now,
        track_order: TrackOrder::DiscTrack,
    };
    let req = AnalyzeRequest {
        album_id: "0000000000000000".into(),
        album_path: album.to_string_lossy().into_owned(),
        library_dir: None,
        what: AnalysisKind::Bpm,
    };
    let mut done: Vec<Value> = Vec::new();
    let outcome = analyze_album(&mut env, &req, &mut |item: ApplyItem| {
        if item.event == "file_done" {
            done.push(item.data.to_value());
        }
        ControlFlow::Continue(())
    });
    assert_eq!(outcome, AnalyzeOutcome::Finished);
    assert_eq!(done.len(), 2, "{done:?}");
    assert_eq!(done[0]["filename"], "01 - One.flac");
    assert_eq!(done[0]["bpm"], json!(120.0));
    let w = done[0]["warnings"]
        .as_array()
        .expect("warnings on the linked file");
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(w[0].as_str().unwrap().contains("hard link"), "{w:?}");
    assert!(done[1].get("warnings").is_none(), "{:?}", done[1]);
}
