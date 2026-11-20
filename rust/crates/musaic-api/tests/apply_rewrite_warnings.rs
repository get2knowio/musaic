//! (Moved here from `musaic-services/tests` when the apply events began to be
//! rendered by `musaic_api::render::apply`; the assertions are the old ones.)
//!
//! What a rewritten FLAC could not keep (`musaic_tags::preserve`) reaches
//! the per-file stream events as `warnings`, a key present only when there
//! is one, so every event the Python app recorded stays as it was. (The
//! analysis's half is `musaic-api/tests/analyze_rewrite_warnings.rs`.)

use std::path::PathBuf;

use musaic_api::render::apply::tag_event_json;
use musaic_tags::{Event, FileChange};
use serde_json::json;

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
