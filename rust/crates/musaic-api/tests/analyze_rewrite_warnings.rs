//! What a rewritten FLAC could not keep (`musaic_tags::preserve`) reaches the
//! analysis's per-file events as `warnings`, a key present only when there is
//! one. (Moved here from `musaic-services/tests/rewrite_warnings.rs` with the
//! rendering of the events; the apply half stays there. Assertions unchanged.)

// The end-to-end test needs hard links and is unix-only; its helpers and
// imports are unused elsewhere.
#![cfg_attr(not(unix), allow(unused_imports, dead_code))]

#[path = "../../musaic-services/tests/common/mod.rs"]
mod common;

use std::ops::ControlFlow;
use std::path::Path;

use common::*;
use musaic_api::render::analyze as render;
use musaic_events::Bus;
use musaic_scan::TrackOrder;
use musaic_services::analyze::{
    AnalysisKind, AnalyzeEnv, AnalyzeEvent, AnalyzeOutcome, AnalyzeRequest, Analyzer, analyze_album,
};
use serde_json::{Value, json};

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
    let outcome = analyze_album(&mut env, &req, &mut |item: AnalyzeEvent| {
        if render::event_name(&item) == "file_done" {
            done.push(render::payload(&item).to_value());
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
