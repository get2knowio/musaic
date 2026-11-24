//! The resolver synthesizers against testdata/golden/ai/units.json
//! (recorded by running the Python app's own functions; two runs
//! byte-identical). Order-sensitive outputs are compared as the Python app's
//! `json.dumps` text.

mod common;

use common::golden;
use musaic_ai::Py;
use musaic_ai::synth::{self, synthesize_cover, synthesize_metadata};
use serde_json::Value;

/// The Python app's `notes` of a synthesized suggestion, as the record holds
/// them.
const PYTHON_DERIVED_NOTES: &str = "Derived from MusicBrainz; no AI call needed.";

fn s(v: &Value) -> &str {
    v.as_str().unwrap()
}

// The synthesizers' output is the Python app's record, byte for byte, with
// one enumerated delta: every `notes` equal to the Python app's text reads
// `synth::DERIVED_NOTES`, which names no AI. Nothing else changes, and the
// delta occurs in the record (metadata and cover).
#[test]
fn synthesizers_match() {
    let (mut meta_changed, mut cover_changed) = (0, 0);
    for c in golden("units.json")["synthesize"].as_array().unwrap() {
        let label = s(&c["label"]);
        let r = common::resolution(&c["resolution"], "x");
        let m = synthesize_metadata(
            &Py::from_value(&c["album"]),
            &Py::from_value(&c["tracks"]),
            &r,
        );
        let recorded = s(&c["metadata_text"]);
        let quoted = format!("\"notes\": \"{PYTHON_DERIVED_NOTES}\"");
        let want_text = if recorded.contains(&quoted) {
            meta_changed += 1;
            recorded.replace(&quoted, &format!("\"notes\": \"{}\"", synth::DERIVED_NOTES))
        } else {
            recorded.to_owned()
        };
        assert_eq!(m.to_py().dumps(), want_text, "{label}");
        let mut want_cover = c["cover"].clone();
        if want_cover["notes"] == PYTHON_DERIVED_NOTES {
            want_cover["notes"] = synth::DERIVED_NOTES.into();
            cover_changed += 1;
        }
        assert_eq!(
            synthesize_cover(&r).to_py().to_value(),
            want_cover,
            "{label}"
        );
        for text in [m.notes.as_str(), want_cover["notes"].as_str().unwrap()] {
            assert!(
                !text.contains("AI") && !text.contains("Claude"),
                "{label}: {text}"
            );
        }
    }
    assert!(
        meta_changed > 0 && cover_changed > 0,
        "{meta_changed} {cover_changed}"
    );
}
