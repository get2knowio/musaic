//! The pure functions of the AI layer against testdata/golden/ai/units.json
//! (recorded by running the Python app's own functions; two runs
//! byte-identical). Order-sensitive outputs are compared as the Python app's
//! `json.dumps` text.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::golden;
use musaic_ai::parse::{extract_json, parse_metadata, validate_mbid};
use musaic_ai::payload::{album_payload, format_user_message};
use musaic_ai::provider::{ProviderError, ProviderEvent};
use musaic_ai::synth::{self, synthesize_cover, synthesize_metadata};
use musaic_ai::transcript::{
    Buffer, Recorder, format_transcript, summarize_tool_args, summarize_tool_input,
    summarize_tool_result, truncate, truncate_to,
};
use musaic_ai::worker::is_no_op_confirmation;
use musaic_ai::{Provider, Py, RunOptions};
use musaic_core::AiWording;
use serde_json::Value;

fn s(v: &Value) -> &str {
    v.as_str().unwrap()
}

// The system prompts and constants, byte for byte.
#[test]
fn prompts_are_the_python_apps() {
    let p = &golden("units.json")["prompts"];
    use musaic_ai::prompts::*;
    assert_eq!(SYSTEM_PROMPT_METADATA, s(&p["metadata"]));
    assert_eq!(SYSTEM_PROMPT_COVER, s(&p["cover"]));
    assert_eq!(FIRECRAWL_PROMPT_HINT, s(&p["firecrawl_hint"]));
    assert_eq!(FIRECRAWL_MCP_URL_TEMPLATE, s(&p["firecrawl_url_template"]));
    assert_eq!(i64::from(AGENT_MAX_TURNS), p["max_turns"].as_i64().unwrap());
    assert_eq!(
        serde_json::json!(SUGGESTABLE_FIELDS),
        p["suggestable_fields"]
    );
    assert_eq!(
        serde_json::json!(TRACK_SUGGESTABLE_FIELDS),
        p["track_suggestable_fields"]
    );
    assert_eq!(
        musaic_ai::worker::WALL_CLOCK_TIMEOUT.as_secs_f64(),
        p["wall_clock_timeout_s"].as_f64().unwrap()
    );
    assert_eq!(
        musaic_ai::worker::AI_MBID_SCORE,
        p["ai_mbid_confidence"].as_i64().unwrap()
    );
}

// `_extract_json` over fenced, prose-wrapped, broken
// and odd inputs.
#[test]
fn extract_json_matches() {
    let cases = golden("units.json")["extract_json"].clone();
    for c in cases.as_array().unwrap() {
        let got = extract_json(s(&c["raw"])).map_or("null".to_string(), |v| v.dumps());
        assert_eq!(got, s(&c["text"]), "raw {:?}", c["raw"]);
    }
}

// The metadata response handling, including
// the Python app's own exceptions for wrongly-shaped fields.
#[test]
fn parse_metadata_matches() {
    let cases = golden("units.json")["parse_metadata"].clone();
    let mut errors = 0;
    for c in cases.as_array().unwrap() {
        let got = parse_metadata(s(&c["raw"]));
        match c.get("error") {
            Some(e) => {
                errors += 1;
                let want = s(e).split_once(": ").unwrap().1;
                assert_eq!(got, Err(want.to_string()), "raw {:?}", c["raw"]);
            }
            None => assert_eq!(
                got.unwrap().to_py().dumps(),
                s(&c["text"]),
                "raw {:?}",
                c["raw"]
            ),
        }
    }
    assert_eq!(errors, 5);
}

#[test]
fn validate_mbid_matches() {
    for c in golden("units.json")["validate_mbid"].as_array().unwrap() {
        let got = validate_mbid(&Py::from_value(&c["in"]));
        assert_eq!(serde_json::json!(got), c["out"], "{:?}", c["in"]);
    }
}

// The per-block digests.
#[test]
fn transcript_digests_match() {
    let u = golden("units.json");
    for c in u["truncate"].as_array().unwrap() {
        assert_eq!(truncate(s(&c["in"])), s(&c["out"]), "{:?}", c["in"]);
    }
    for c in u["truncate_limit"].as_array().unwrap() {
        let limit = c["limit"].as_u64().unwrap() as usize;
        assert_eq!(truncate_to(s(&c["in"]), limit), s(&c["out"]));
    }
    for c in u["summarize_tool_input"].as_array().unwrap() {
        assert_eq!(
            summarize_tool_input(&Py::from_value(&c["in"])),
            s(&c["out"]),
            "{:?}",
            c["in"]
        );
    }
    for c in u["summarize_tool_result"].as_array().unwrap() {
        assert_eq!(
            summarize_tool_result(&Py::from_value(&c["in"])),
            s(&c["out"]),
            "{:?}",
            c["in"]
        );
    }
    for c in u["summarize_tool_args"].as_array().unwrap() {
        assert_eq!(
            summarize_tool_args(s(&c["in"])),
            s(&c["out"]),
            "{:?}",
            c["in"]
        );
    }
}

// `_TranscriptRecorder` over event scripts, before and after the
// final flush.
#[test]
fn recorder_matches() {
    for c in golden("units.json")["recorder"].as_array().unwrap() {
        let buf = Buffer::new();
        let mut r = Recorder::new(buf.clone());
        for e in c["events"].as_array().unwrap() {
            let ev: ProviderEvent = common::event(e);
            r.feed(&ev);
        }
        assert_eq!(serde_json::json!(buf.lines()), c["before_finish"]);
        r.finish();
        assert_eq!(serde_json::json!(buf.lines()), c["buffer"]);
    }
}

// `_format_transcript`, including (private tier) a recorded timeout
// transcript of a real run cut at several byte budgets.
#[test]
fn format_transcript_matches() {
    let cases = golden("units.json")["format_transcript"].clone();
    let mut from_capture = 0;
    for c in cases.as_array().unwrap() {
        let buf: Vec<String> = c["buffer"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| s(x).to_string())
            .collect();
        let got = format_transcript(&buf, c["max_bytes"].as_u64().unwrap() as usize);
        assert_eq!(serde_json::json!(got), c["out"], "max {}", c["max_bytes"]);
        from_capture += usize::from(c.get("buffer_from").is_some());
    }
    // The four transcript cases are cut from a real model transcript: private.
    if musaic_testdata::json("golden/ai/units.json").private {
        assert_eq!(from_capture, 4);
    } else {
        assert_eq!(from_capture, 0);
        musaic_testdata::skip_private_cases();
    }
}

// The album payload with the resolution embedded, and the user
// message (hint stripped, cut at 2000 characters).
#[test]
fn payload_and_user_message_match() {
    for c in golden("units.json")["payload"].as_array().unwrap() {
        let label = s(&c["label"]);
        let res = c
            .get("resolution")
            .filter(|r| !r.is_null())
            .map(|r| common::resolution(r, "x"));
        // The generator's album dicts are Python dicts; only the keys the
        // payload reads matter, and it reads them by name.
        let p = album_payload(
            &Py::from_value(&c["album"]),
            &Py::from_value(&c["tracks"]),
            res.as_ref(),
        );
        assert_eq!(p.dumps_indent2(), s(&c["text"]), "{label}");
        let hint = c.get("hint").and_then(Value::as_str).unwrap_or("");
        assert_eq!(
            format_user_message(&p, hint),
            s(&c["user_message"]),
            "{label}"
        );
    }
}

// The synthesizers (with a ready provider: the Python app's wording,
// byte for byte).
#[test]
fn synthesizers_match() {
    for c in golden("units.json")["synthesize"].as_array().unwrap() {
        let label = s(&c["label"]);
        let r = common::resolution(&c["resolution"], "x");
        let m = synthesize_metadata(
            &Py::from_value(&c["album"]),
            &Py::from_value(&c["tracks"]),
            &r,
            AiWording::NamesAi,
        );
        assert_eq!(m.to_py().dumps(), s(&c["metadata_text"]), "{label}");
        assert_eq!(
            synthesize_cover(&r, AiWording::NamesAi).to_py().to_value(),
            c["cover"],
            "{label}"
        );
    }
}

// Backend text names AI only when a provider is ready: with no ready provider the synthesizers' output is
// the Python app's record with one enumerated delta: every `notes` equal to
// DERIVED_NOTES reads DERIVED_NOTES_NEUTRAL. Nothing else changes, and the
// delta occurs in the record (metadata and cover).
#[test]
fn synthesizers_without_a_provider() {
    let (mut meta_changed, mut cover_changed) = (0, 0);
    for c in golden("units.json")["synthesize"].as_array().unwrap() {
        let label = s(&c["label"]);
        let r = common::resolution(&c["resolution"], "x");
        let m = synthesize_metadata(
            &Py::from_value(&c["album"]),
            &Py::from_value(&c["tracks"]),
            &r,
            AiWording::Neutral,
        );
        let mut want_meta: serde_json::Value =
            serde_json::from_str(s(&c["metadata_text"])).unwrap();
        if want_meta["notes"] == synth::DERIVED_NOTES {
            want_meta["notes"] = synth::DERIVED_NOTES_NEUTRAL.into();
            meta_changed += 1;
        }
        assert_eq!(m.to_py().to_value(), want_meta, "{label}");
        let mut want_cover = c["cover"].clone();
        if want_cover["notes"] == synth::DERIVED_NOTES {
            want_cover["notes"] = synth::DERIVED_NOTES_NEUTRAL.into();
            cover_changed += 1;
        }
        assert_eq!(
            synthesize_cover(&r, AiWording::Neutral).to_py().to_value(),
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

// The no-op confirmation rule (positive, negative, malformed).
#[test]
fn no_op_confirmation_matches() {
    for c in golden("units.json")["no_op_confirmation"]
        .as_array()
        .unwrap()
    {
        assert_eq!(
            is_no_op_confirmation(&Py::from_value(&c["result"])),
            c["out"].as_bool().unwrap(),
            "{}",
            c["result"]
        );
    }
}

// `_friendly_agent_error` for each error class.
#[test]
fn friendly_errors_match() {
    let u = golden("units.json");
    let errs = [
        ProviderError::Auth("bad key".into()),
        ProviderError::Transient("429".into()),
        ProviderError::Other {
            message: "plain".into(),
            cause: None,
        },
        ProviderError::Other {
            message: "boom".into(),
            cause: Some(("KeyError".into(), "'k'".into())),
        },
        ProviderError::Other {
            message: "boom2".into(),
            cause: Some(("ValueError".into(), "bad value".into())),
        },
    ];
    let cases = u["friendly_error"].as_array().unwrap();
    assert_eq!(cases.len(), errs.len());
    for (e, c) in errs.iter().zip(cases) {
        assert_eq!(e.message("claude"), s(&c["message"]), "{}", c["exception"]);
    }
    let st = &u["provider_runtime_status"]["claude_missing"];
    let not_ready = ProviderError::NotReady {
        requirement: s(&st["requirement"]).into(),
        remedy: s(&st["remedy"]).into(),
    };
    // The preflight's text, as the worker golden's "provider-not-ready"
    // job records it.
    let w = golden("worker.json");
    let job = w["scenarios"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"] == "provider-not-ready")
        .unwrap();
    assert_eq!(not_ready.message("claude"), s(&job["job"]["error"]));
}

// The cover response handling (`suggest_cover`: parse, URL check,
// MusicBrainz fallback, note merging) through a cover job.
#[test]
fn parse_cover_matches() {
    let w = golden("worker.json");
    let tmp = tempfile::tempdir().unwrap();
    let ids = vec!["c1".to_string()];
    let db = common::fresh_db(tmp.path(), &ids);
    let album = serde_json::json!({"id": "c1", "path": "<LIB>/c1", "dir_name": "C"});
    let env = common::StubEnv::new(
        db,
        [("c1".to_string(), Py::from_value(&album))].into(),
        Default::default(),
    );
    let _ = &w;
    for (i, c) in golden("units.json")["parse_cover"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
    {
        *env.find_cover.lock().unwrap() = c["find_cover"].clone();
        // The generator's verifier: ok for https://good.*, else this reason.
        let raw = s(&c["raw"]);
        let parsed = extract_json(raw);
        let url = parsed
            .as_ref()
            .and_then(|p| p.get("cover_url"))
            .filter(|v| !matches!(v, Py::Null))
            .map(Py::py_str)
            .unwrap_or_default();
        *env.verify.lock().unwrap() = [(
            url.clone(),
            (
                url.starts_with("https://good."),
                "content-type text/html".to_string(),
            ),
        )]
        .into();
        let replay = common::replay();
        replay.push(musaic_ai::provider::replay::Script {
            events: vec![ProviderEvent::TurnComplete { text: raw.into() }],
            ending: musaic_ai::provider::replay::Ending::Done,
        });
        let provider: Arc<dyn Provider> = Arc::new(replay);
        let mut conn = musaic_db::open_connection(&env.db).unwrap();
        conn.execute("DELETE FROM ai_jobs", []).unwrap();
        let job = musaic_db::ai_jobs::enqueue(
            &mut conn,
            &musaic_db::ai_jobs::NewJob {
                album_id: "c1",
                album_path: "<LIB>/c1",
                album_title: "C",
                job_type: "cover",
                hint: "",
                attach_cover: true,
            },
            || format!("{i:016x}"),
            "2026-01-01T00:00:00Z",
        )
        .unwrap()
        .unwrap();
        musaic_ai::run_one_job(
            &env,
            &provider,
            &job,
            &RunOptions {
                call_timeout: Duration::from_secs(30),
                ..RunOptions::default()
            },
        );
        let row = musaic_db::ai_jobs::get_job(&conn, &job.id)
            .unwrap()
            .unwrap();
        let result = musaic_ai::pyjson::loads(row.result_json.as_deref().unwrap()).unwrap();
        assert_eq!(
            result.get("cover").unwrap().to_value(),
            c["result"],
            "raw {raw:?}"
        );
    }
}
