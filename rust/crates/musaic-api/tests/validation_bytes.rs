//! FastAPI's 422 bodies byte for byte. pydantic's `ErrorDetails`
//! keys go `type, loc, msg, input[, ctx]`; serde_json's sorted `Map` wrote
//! `ctx, input, loc, msg, type`, and every test compared 422s parsed, so
//! nothing caught it.
//!
//! - every 422 of the Python app the goldens hold as bytes (golden/ai/http.json
//!   `text`, golden/checks/recording_canonical.json `raw`,
//!   golden/api/route_order.json probe `body`) is what this build's
//!   `ApiError` renders for the same detail, and what the tests' independent
//!   renderer (`common::fastapi_422_text`) writes;
//! - every 422 recorded only parsed (the other goldens, and the Python app side
//!   of the recorded verification runs) renders as that same renderer
//!   writes it, so each error type the Python app answered with is covered.
//!   The private test data holds the private halves of the goldens and the
//!   verify run's synthetic and real records; without it the public files
//!   are checked and the counts are the public ones.
//!   The end-to-end checks are in golden_cases.rs, wired_golden.rs,
//!   jobs_http.rs, recording_canonical_http.rs and starlette_routing.rs.

mod common;

use axum::response::IntoResponse;
use common::{fastapi_422_text, read};
use musaic_api::http::ApiError;
use serde_json::Value;

fn load(rel: &str) -> Value {
    musaic_testdata::golden(rel)
}

async fn rendered(body: &Value) -> (String, String) {
    let e = ApiError {
        status: 422,
        detail: body["detail"].clone(),
    };
    let got = read(e.into_response()).await;
    assert_eq!(got.status, 422);
    (got.ctype().to_string(), got.text())
}

/// (where, the Python app's bytes) for every 422 recorded as bytes.
fn python_byte_samples() -> Vec<(String, String)> {
    let mut out = Vec::new();
    let ai = load("ai/http.json");
    for c in ai["cases"].as_array().unwrap() {
        let r = &c["response"];
        if r["status"] == 422 {
            out.push((
                format!("ai/http.json {}", c["label"]),
                r["text"].as_str().unwrap().to_string(),
            ));
        }
    }
    let rc = load("checks/recording_canonical.json");
    for s in rc["steps"].as_array().unwrap() {
        let r = &s["response"];
        if r["status"] == 422 {
            out.push((
                format!("recording_canonical.json {}", s["label"]),
                r["raw"].as_str().unwrap().to_string(),
            ));
        }
    }
    let ro = load("api/route_order.json");
    for p in ro["probes"].as_array().unwrap() {
        if p["status"] == 422 {
            out.push((
                format!("route_order.json {} {}", p["method"], p["path"]),
                p["body"].as_str().unwrap().to_string(),
            ));
        }
    }
    out
}

#[tokio::test]
async fn validation_422_bytes_equal_every_python_byte_sample() {
    let samples = python_byte_samples();
    // ai/http.json 9, recording_canonical.json 1, route_order.json 1.
    assert_eq!(samples.len(), 11, "python 422 byte samples");
    let mut types = std::collections::BTreeSet::new();
    for (at, bytes) in &samples {
        let parsed: Value = serde_json::from_str(bytes).unwrap();
        for item in parsed["detail"].as_array().unwrap() {
            types.insert(item["type"].as_str().unwrap().to_string());
        }
        assert_eq!(&fastapi_422_text(&parsed), bytes, "{at}: test renderer");
        let (ct, text) = rendered(&parsed).await;
        assert_eq!(ct, "application/json", "{at}");
        assert_eq!(&text, bytes, "{at}: ApiError rendering");
    }
    // ctx (json_invalid) and the commonest types are among the byte samples.
    for t in ["missing", "json_invalid", "int_parsing"] {
        assert!(types.contains(t), "{t} sampled as bytes");
    }
}

fn collect_422s(v: &Value, out: &mut Vec<Value>) {
    match v {
        Value::Object(m) => {
            if let Some(Value::Array(d)) = m.get("detail")
                && d.first()
                    .and_then(Value::as_object)
                    .is_some_and(|i| i.contains_key("loc"))
            {
                out.push(v.clone());
                return;
            }
            m.values().for_each(|x| collect_422s(x, out));
        }
        Value::Array(xs) => xs.iter().for_each(|x| collect_422s(x, out)),
        _ => {}
    }
}

fn json_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            json_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "json") {
            out.push(p);
        }
    }
}

/// Of the 62 recorded 422 bodies, those in the public test data.
const PUBLIC_422_BODIES: usize = 44;

#[tokio::test]
async fn every_recorded_422_renders_in_fastapi_key_order() {
    // Each 422 body sits in exactly one half of a split golden, so walking
    // both trees sees every recorded body once.
    let mut files = Vec::new();
    json_files(&musaic_testdata::public("golden"), &mut files);
    files.push(musaic_testdata::public(
        "captures/verify-records/many/records.json",
    ));
    let private = musaic_testdata::private_available(&[
        "golden",
        "captures/verify-records/synthetic/records.json",
        "captures/verify-records/real/records.json",
    ]);
    if private {
        json_files(&musaic_testdata::private_required("golden"), &mut files);
        for s in ["synthetic", "real"] {
            files.push(musaic_testdata::private_required(format!(
                "captures/verify-records/{s}/records.json"
            )));
        }
    }
    files.sort();
    let mut bodies = Vec::new();
    for f in &files {
        let Ok(v) = serde_json::from_str::<Value>(&std::fs::read_to_string(f).unwrap()) else {
            continue;
        };
        let mut found = Vec::new();
        collect_422s(&v, &mut found);
        bodies.extend(found.into_iter().map(|b| (f.clone(), b)));
    }
    let mut types = std::collections::BTreeSet::new();
    for (f, b) in &bodies {
        for item in b["detail"].as_array().unwrap() {
            types.insert(item["type"].as_str().unwrap().to_string());
        }
        let (_, text) = rendered(b).await;
        assert_eq!(text, fastapi_422_text(b), "{}", f.display());
    }
    // Every validation error type the Python app answered with in the goldens
    // and the verify run's captures.
    let want: std::collections::BTreeSet<String> = [
        "bool_parsing",
        "dict_type",
        "int_from_float",
        "int_parsing",
        "int_type",
        "json_invalid",
        "list_type",
        "literal_error",
        "missing",
        "model_attributes_type",
        "string_type",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    if private {
        assert_eq!(types, want);
        assert_eq!(bodies.len(), 62, "recorded 422 bodies");
    } else {
        assert!(types.is_subset(&want), "{types:?}");
        assert_eq!(
            bodies.len(),
            PUBLIC_422_BODIES,
            "recorded 422 bodies (public)"
        );
        musaic_testdata::skip_private_cases();
    }
}
