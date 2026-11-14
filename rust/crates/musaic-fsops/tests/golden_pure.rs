//! Pure functions against the table the Python app evaluated
//! (testdata/golden/fsops/pure.json).

use musaic_core::album_fields::{display_artist, sort_key};
use musaic_fsops::plan::summarize;
use musaic_fsops::template::{Value, Values, render, sanitize_for_filename};
use serde_json::Value as J;

fn golden() -> J {
    musaic_testdata::golden("fsops/pure.json")
}

fn value(j: &J) -> Value {
    match j {
        J::Null => Value::None,
        J::Bool(b) => Value::Bool(*b),
        J::Number(n) if n.is_i64() => Value::Int(n.as_i64().unwrap()),
        J::Number(n) => Value::Float(n.as_f64().unwrap()),
        J::String(s) => Value::Str(s.clone()),
        other => panic!("unexpected value {other}"),
    }
}

/// Template rendering (format specs, int coercion, sanitising)
/// exactly as `filename_template.render` does it.
#[test]
fn render_matches_python() {
    let g = golden();
    let mut failures = Vec::new();
    for case in g["render"].as_array().unwrap() {
        let template = case["template"].as_str().unwrap();
        let values: Values = case["values"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.as_str(), value(v)))
            .collect();
        let got = render(template, &values).unwrap();
        if got != case["out"].as_str().unwrap() {
            failures.push(format!(
                "{template:?} {}: got {got:?}, want {}",
                case["values"], case["out"]
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// `sanitize_for_filename`.
#[test]
fn sanitize_matches_python() {
    for case in golden()["sanitize"].as_array().unwrap() {
        assert_eq!(
            sanitize_for_filename(case["in"].as_str().unwrap()),
            case["out"].as_str().unwrap(),
            "{}",
            case["in"]
        );
    }
}

/// The artist cascade the dir template uses: `album_fields`.
#[test]
fn album_fields_match_python() {
    for case in golden()["album_fields"].as_array().unwrap() {
        let a = &case["album"];
        let f = |k: &str| a[k].as_str();
        assert_eq!(
            display_artist(f("album_artist"), f("artist"), f("dir_artist"), false),
            case["display_artist"].as_str().unwrap(),
            "{a}"
        );
        assert_eq!(
            display_artist(f("album_artist"), f("artist"), f("dir_artist"), true),
            case["display_artist_dir"].as_str().unwrap(),
            "{a}"
        );
        assert_eq!(
            sort_key(
                f("album_artist_sort"),
                f("artist_sort"),
                f("album_artist"),
                f("artist")
            ),
            case["sort_key"].as_str().unwrap(),
            "{a}"
        );
    }
}

/// Finding summaries, including Python's `repr()` quoting.
#[test]
fn summaries_match_python() {
    for case in golden()["summarize"].as_array().unwrap() {
        assert_eq!(
            summarize(
                case["dir_name"].as_str(),
                case["renames"].as_u64().unwrap() as usize,
                case["dir_rename"].as_bool().unwrap()
            ),
            case["out"].as_str().unwrap()
        );
    }
}
