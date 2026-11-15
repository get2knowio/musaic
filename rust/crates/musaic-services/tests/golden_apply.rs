//! The apply pipeline against testdata/golden/services/apply.json, recorded
//! by a generator that drove the Python app's own
//! `services.apply.apply_album_changes_iter`.
//!
//! Per scenario: the same album bytes, the same starting database (built by
//! this crate's dependencies the way the generator built it with the
//! Python app's scanner + save_snapshot, and compared with the Python app's), the
//! same payload, the same point at which the consumer stops reading. Then
//! compared item by item: every streamed item, the number of bus events
//! published before each item, the bus events, every file of the library
//! (bytes, or parsed layout where a re-encoded JPEG is involved), and the
//! database rows.
//!
//! Differences are allowed only where listed in [`deltas`]
//! (fingerprint moves), in [`record_deltas`] (whole
//! parts of a scenario's record replaced, each by a value recorded from the
//! Python app wherever one exists) or in the two field-level rules below:
//! - `file_done.error`: presence only; the text is the OS / mutagen message
//!   (it differs between platforms and libraries).
//! - cover scenarios: the JPEG encoder is not Pillow: `cover_downloaded.bytes`, and the size / sha / block sizes of
//!   files carrying the re-encoded JPEG, are replaced by checks: PSNR of our
//!   JPEG against the Python app's own normalised JPEG >= 45 dB, the embedded
//!   picture is byte-identical to our cover.jpg, and `bytes` equals its size.

mod common;

use std::collections::HashMap;
use std::ops::ControlFlow;
use std::path::Path;
use std::sync::OnceLock;

use common::*;
use musaic_scan::TrackOrder;
use musaic_services::apply::{
    ApplyEnv, ApplyItem, ApplyOutcome, ApplyRequest, CoverSource, apply_album_changes,
    apply_direct, missing_album_detail, resolve_album, validate_cover_upload,
};
use serde_json::{Value, json};

const SEED_TS: &str = "2001-01-01T00:00:00Z";
const PSNR_MIN_DB: f64 = 45.0;

fn golden() -> &'static Value {
    static G: OnceLock<Value> = OnceLock::new();
    G.get_or_init(|| load_golden("apply.json"))
}

fn album_columns() -> Vec<String> {
    golden()["album_columns"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c.as_str().unwrap().to_string())
        .collect()
}

/// The generator's local cover table (`https://covers.invalid/*`), plus the
/// url its downloader answered with zero bytes (`empty_cover_url` of the
/// empty-cover scenarios, MIME image/jpeg as the generator returned it).
struct LocalCovers;

impl CoverSource for LocalCovers {
    fn download(&self, url: &str) -> Option<(Vec<u8>, String)> {
        let empty = golden()["scenarios"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["empty_cover_url"] == url);
        if empty {
            return Some((Vec::new(), "image/jpeg".into()));
        }
        let name = golden()["cover_urls"][url].as_str()?;
        let mime = golden()["cover_images"][name]["mime"].as_str()?.to_string();
        Some((image_bytes(name), mime))
    }
}

/// An explicitly enumerated deliberate change: in `scenario`, the Python app's
/// `track_fingerprints` row for `old` is expected under `new`
/// (fingerprints follow a rename; the Python app moved only track_tags).
struct FingerprintMove {
    scenario: &'static str,
    old: &'static str,
    new: &'static str,
}

fn deltas() -> &'static [FingerprintMove] {
    &[
        FingerprintMove {
            scenario: "track-tags-rename",
            old: "01 - Dawn.flac",
            new: "04 - Dawn.flac",
        },
        FingerprintMove {
            scenario: "track-tags-swap",
            old: "01 - Dawn.flac",
            new: "03 - Dawn.flac",
        },
        FingerprintMove {
            scenario: "track-tags-swap",
            old: "03 - Dusk.flac",
            new: "01 - Dusk.flac",
        },
        // stop-after-first-rename runs to completion, so its
        // database is track-tags-swap's (see record_deltas): both moves.
        FingerprintMove {
            scenario: "stop-after-first-rename",
            old: "01 - Dawn.flac",
            new: "03 - Dawn.flac",
        },
        FingerprintMove {
            scenario: "stop-after-first-rename",
            old: "03 - Dusk.flac",
            new: "01 - Dusk.flac",
        },
    ]
}

/// Where the new value of a [`RecordDelta`] comes from.
enum NewValue {
    /// The same place in another scenario of the Python app's record: the
    /// Python app's run of the same payload, on the same album, to completion.
    Scenario(&'static str),
    /// Another place in the same scenario's record (e.g. `/before/files`:
    /// nothing was changed).
    Path(&'static str),
    /// A value built in the test (from the Python app's record where possible).
    Value(fn() -> Value),
}

/// One deliberate change, applied to a scenario's record before the
/// comparison: in `scenario`, the value at JSON pointer `path` is replaced.
/// The Python app's value must differ from the new one, so a delta that no
/// longer changes anything fails.
struct RecordDelta {
    scenario: &'static str,
    path: &'static str,
    why: &'static str,
    new: NewValue,
}

const STOP_PARTS: [&str; 3] = ["/after/files", "/after/db", "/bus_events"];

fn record_deltas() -> Vec<RecordDelta> {
    let mut d = Vec::new();
    // An apply whose consumer stops reading
    // (or raises / panics) still runs every step. Old: what the Python app's
    // closed generator left behind. New: the Python app's own full run of the
    // same payload on the same album (identical starting point, checked in
    // `stop_scenarios_share_the_full_runs_input`). Items, `stopped_early`
    // and bus_events_so_far are unchanged: the consumer saw the same items.
    for (scenario, full) in [
        ("stop-after-first", "album-fields"),
        ("stop-after-file-done", "album-fields"),
        ("stop-after-file-done-raise", "album-fields"),
        ("stop-after-done", "stop-after-updated-album"),
        ("stop-after-first-rename", "track-tags-swap"),
    ] {
        for path in STOP_PARTS {
            d.push(RecordDelta {
                scenario,
                path,
                why: "apply-completes",
                new: NewValue::Scenario(full),
            });
        }
    }
    // Empty cover bytes are a failed download.
    // New items: the Python app's failed-download shape (scenario
    // cover-download-fails: cover_downloading {url} then error {detail});
    // for an upload, error {detail} with the upload route's own text for an
    // empty body (http case upload-empty). Nothing else happens: files and
    // rows as before, no bus event.
    d.push(RecordDelta {
        scenario: "cover-empty-url",
        path: "/items",
        why: "empty-cover",
        new: NewValue::Value(|| {
            let url = scenario("cover-empty-url")["empty_cover_url"]
                .as_str()
                .unwrap();
            let fails = scenario("cover-download-fails");
            let failed_url = fails["payload"]["cover_url"].as_str().unwrap();
            let mut items = fails["items"].clone();
            for item in items.as_array_mut().unwrap() {
                for v in item["data"].as_object_mut().unwrap().values_mut() {
                    *v = json!(v.as_str().unwrap().replace(failed_url, url));
                }
            }
            items
        }),
    });
    d.push(RecordDelta {
        scenario: "cover-empty-bytes",
        path: "/items",
        why: "empty-cover",
        new: NewValue::Value(|| {
            json!([{"event": "error", "bus_events_so_far": 0,
                    "data": {"detail": http_case("upload-empty")["json"]["detail"]}}])
        }),
    });
    for scenario in ["cover-empty-url", "cover-empty-bytes"] {
        d.push(RecordDelta {
            scenario,
            path: "/after/files",
            why: "empty-cover",
            new: NewValue::Path("/before/files"),
        });
        d.push(RecordDelta {
            scenario,
            path: "/after/db",
            why: "empty-cover",
            new: NewValue::Path("/before/db"),
        });
        d.push(RecordDelta {
            scenario,
            path: "/bus_events",
            why: "empty-cover",
            new: NewValue::Value(|| json!([])),
        });
    }
    // A Missing album is refused before
    // anything is read or written. New items: one error with the refusal
    // text (missing_since as the generator seeded it); nothing else.
    for scenario in ["missing-dir-gone", "missing-dir-present"] {
        d.push(RecordDelta {
            scenario,
            path: "/items",
            why: "apply-refuses-missing",
            new: NewValue::Value(|| {
                json!([{"event": "error", "bus_events_so_far": 0,
                        "data": {"detail": missing_album_detail(MISSING_SINCE_SEED)}}])
            }),
        });
    }
    // missing-dir-gone: the Python app changed nothing either (its error came
    // from the missing directory), so only the items differ.
    for path in STOP_PARTS {
        d.push(RecordDelta {
            scenario: "missing-dir-present",
            path,
            why: "apply-refuses-missing",
            new: if path == "/bus_events" {
                NewValue::Value(|| json!([]))
            } else {
                NewValue::Path(if path == "/after/files" {
                    "/before/files"
                } else {
                    "/before/db"
                })
            },
        });
    }
    d
}

/// The generator's `mark_missing` timestamp.
const MISSING_SINCE_SEED: &str = "2001-02-01T00:00:00Z";

/// The Python app's record of `sc` with its [`record_deltas`] applied.
fn expected_record(sc: &Value) -> Value {
    let id = sc["id"].as_str().unwrap();
    let mut out = sc.clone();
    for d in record_deltas().iter().filter(|d| d.scenario == id) {
        let new = match &d.new {
            NewValue::Scenario(other) => scenario(other)
                .pointer(d.path)
                .unwrap_or_else(|| panic!("{other}: no {}", d.path))
                .clone(),
            NewValue::Path(p) => sc.pointer(p).unwrap().clone(),
            NewValue::Value(f) => f(),
        };
        let slot = out
            .pointer_mut(d.path)
            .unwrap_or_else(|| panic!("{id}: no {} in the Python app's record", d.path));
        assert_ne!(
            *slot, new,
            "{id} {} ({}): delta changes nothing",
            d.path, d.why
        );
        *slot = new;
    }
    out
}

fn pairs(v: &Value) -> Vec<(String, Option<String>)> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|kv| {
            (
                kv[0].as_str().unwrap().to_string(),
                kv[1].as_str().map(str::to_string),
            )
        })
        .collect()
}

fn track_pairs(v: &Value) -> musaic_tags::TrackSuggestions {
    v.as_array()
        .unwrap()
        .iter()
        .map(|e| (e[0].as_str().unwrap().to_string(), pairs(&e[1])))
        .collect()
}

/// A library + database prepared like the generator's `run_apply_scenario`.
struct Setup {
    _td: tempfile::TempDir,
    lib: std::path::PathBuf,
    db: std::path::PathBuf,
    norm: Norm,
    album_id: String,
}

fn seed_scan(conn: &mut rusqlite::Connection, lib: &Path) {
    let albums = musaic_scan::scan_library(lib, TrackOrder::DiscTrack);
    musaic_db::albums::save_snapshot(conn, &lib.to_string_lossy(), SEED_TS, SEED_TS, &albums)
        .unwrap();
}

fn setup(sc: &Value) -> Setup {
    let (td, lib) = temp_lib(golden());
    for a in sc["albums"].as_array().unwrap() {
        build_album(&lib, a);
    }
    let mut norm = Norm::new(&lib);
    norm.learn_all();
    let db = fresh_db(td.path());
    let mut conn = connect(&db);
    seed_scan(&mut conn, &lib);
    let album_dir = lib.join(sc["albums"][0]["dir"].as_str().unwrap());
    let album_id = musaic_core::album_id(&musaic_scan::py_resolve(&album_dir).to_string_lossy());
    let seed = &sc["seed"];
    for r in seed["track_tags"].as_array().into_iter().flatten() {
        conn.execute(
            "INSERT INTO track_tags(album_id, filename, tag, created_at) VALUES (?1,?2,?3,?4)",
            [
                &album_id,
                r[0].as_str().unwrap(),
                r[1].as_str().unwrap(),
                SEED_TS,
            ],
        )
        .unwrap();
    }
    for r in seed["track_fingerprints"].as_array().into_iter().flatten() {
        conn.execute(
            "INSERT INTO track_fingerprints(album_id, filename, fingerprint, duration_s, last_computed_at) \
             VALUES (?1,?2,?3,3.0,?4)",
            [&album_id, r[0].as_str().unwrap(), r[1].as_str().unwrap(), SEED_TS],
        )
        .unwrap();
    }
    if sc["mark_missing"].as_bool() == Some(true) {
        musaic_db::albums::mark_album_missing(&conn, &album_id, "2001-02-01T00:00:00Z").unwrap();
    }
    if sc["remove_dir"].as_bool() == Some(true) {
        std::fs::remove_dir_all(&album_dir).unwrap();
    }
    if sc["delete_flacs"].as_bool() == Some(true) {
        for e in std::fs::read_dir(&album_dir).unwrap().flatten() {
            if e.path().extension().is_some_and(|x| x == "flac") {
                std::fs::remove_file(e.path()).unwrap();
            }
        }
    }
    for rel in sc["readonly"].as_array().into_iter().flatten() {
        let p = lib.join(rel.as_str().unwrap());
        let mut perm = std::fs::metadata(&p).unwrap().permissions();
        perm.set_readonly(true);
        std::fs::set_permissions(&p, perm).unwrap();
    }
    Setup {
        _td: td,
        lib,
        db,
        norm,
        album_id,
    }
}

fn make_writable(dir: &Path) {
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            make_writable(&p);
        } else {
            let mut perm = std::fs::metadata(&p).unwrap().permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            perm.set_readonly(false);
            let _ = std::fs::set_permissions(&p, perm);
        }
    }
}

struct Run {
    items: Vec<Value>,
    outcome: ApplyOutcome,
    bus: Vec<Value>,
    files: Vec<Value>,
    db: Value,
}

/// Drive the pipeline the way the generator drove the Python app's generator.
fn run(sc: &Value, s: &Setup) -> Run {
    let (bus, log) = recording_bus();
    let mut conn = connect(&s.db);
    let row = musaic_services::albums::get_album_by_id(&conn, &s.album_id)
        .unwrap()
        .expect("album row");
    let payload = &sc["payload"];
    let pp = &sc["payload_pairs"];
    let lib_str = s.lib.to_string_lossy().into_owned();
    let req = ApplyRequest {
        album_id: row.album.id.clone(),
        album_path: row.album.path.clone(),
        library_dir: Some(
            sc.get("library_dir")
                .and_then(Value::as_str)
                .map_or(lib_str, str::to_string),
        ),
        suggestions: pairs(&pp["suggestions"]),
        track_suggestions: track_pairs(&pp["track_suggestions"]),
        cover_url: payload["cover_url"].as_str().map(str::to_string),
        cover_bytes: payload["cover_bytes"].as_str().map(payload_image),
        cover_mime: payload["cover_mime"].as_str().map(str::to_string),
        dry_run: payload["dry_run"].as_bool().unwrap_or(false),
    };
    let stop_after = sc["stop_after"].as_u64().map(|n| n as usize);
    let stop_event = sc["stop_after_event"].as_str();
    let now = || "2030-01-01T00:00:00Z".to_string();
    let mut env = ApplyEnv {
        conn: &mut conn,
        bus: &bus,
        covers: &LocalCovers,
        now: &now,
        track_order: TrackOrder::DiscTrack,
    };
    // The Python app's consumer that raised (`stop_by: raise`) is a sink that
    // panics here (treated as Break).
    let panics = sc["stop_by"] == "raise";
    let mut items = Vec::new();
    let outcome = apply_album_changes(&mut env, &req, &mut |item: ApplyItem| {
        items.push(json!({"event": item.event, "data": item.data.to_value(),
            "bus_events_so_far": log.lock().unwrap().len()}));
        if stop_after.is_some_and(|n| items.len() >= n) || stop_event == Some(item.event) {
            if panics {
                panic!("consumer raised (deliberate, stop test)");
            }
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    });
    let bus_events: Vec<Value> = log.lock().unwrap().iter().map(event_json).collect();
    let mut norm = Norm {
        lib: s.norm.lib.clone(),
        ids: s.norm.ids.clone(),
    };
    norm.learn_all();
    let files = describe_lib(&s.lib);
    let db = dump_db(&conn, &norm, &album_columns());
    make_writable(&s.lib);
    Run {
        items: items.iter().map(|i| norm.value(i)).collect(),
        outcome,
        bus: bus_events.iter().map(|e: &Value| norm.value(e)).collect(),
        files,
        db,
    }
}

fn has_cover(sc: &Value) -> bool {
    let p = &sc["payload"];
    (p["cover_url"].is_string()
        && golden()["cover_urls"][p["cover_url"].as_str().unwrap()].is_string())
        || p["cover_bytes"].as_str().is_some_and(|n| n != EMPTY_IMAGE)
}

/// The generator's payload name for `cover_bytes=b""` (empty-cover scenarios).
const EMPTY_IMAGE: &str = "<empty>";

fn payload_image(name: &str) -> Vec<u8> {
    if name == EMPTY_IMAGE {
        Vec::new()
    } else {
        image_bytes(name)
    }
}

/// Item-level rules (per-file error text; encoder size).
fn normalise_item(mut item: Value, cover: bool) -> Value {
    if item["event"] == "file_done" && !item["data"]["error"].is_null() {
        item["data"]["error"] = json!("<ERROR>");
    }
    if cover && item["event"] == "cover_downloaded" {
        item["data"]["bytes"] = json!("<JPEG-SIZE>");
    }
    item
}

/// Files with a re-encoded JPEG lose their byte-level fields.
fn normalise_files(files: &[Value], cover: bool) -> Vec<Value> {
    files
        .iter()
        .map(|f| {
            let mut f = f.clone();
            let carries_jpeg = f["flac"]["pictures"]
                .as_array()
                .is_some_and(|p| !p.is_empty())
                || f["path"]
                    .as_str()
                    .is_some_and(|p| p.ends_with("/cover.jpg"));
            if cover && carries_jpeg {
                for k in ["size", "sha256", "audio_offset"] {
                    if let Some(o) = f.as_object_mut() {
                        o.remove(k);
                    }
                }
                if let Some(fl) = f.get_mut("flac").and_then(Value::as_object_mut) {
                    fl.remove("audio_offset");
                    let codes: Vec<Value> = fl["blocks"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|b| b[0].clone())
                        .collect();
                    fl.insert("blocks".into(), Value::Array(codes));
                    for p in fl["pictures"].as_array_mut().unwrap() {
                        let o = p.as_object_mut().unwrap();
                        o.remove("data_len");
                        o.remove("sha256");
                    }
                }
            }
            f
        })
        .collect()
}

/// The JPEG checks that replace byte equality in cover scenarios.
fn check_jpeg(sc: &Value, s: &Setup, r: &Run) {
    let id = sc["id"].as_str().unwrap();
    let p = &sc["payload"];
    let name = p["cover_bytes"]
        .as_str()
        .or_else(|| golden()["cover_urls"][p["cover_url"].as_str().unwrap()].as_str())
        .unwrap();
    let python_sha = golden()["cover_images"][name]["python_normalized_sha256"]
        .as_str()
        .unwrap();
    let python_jpeg = python_normalized(python_sha);
    let ours = musaic_tags::normalize_to_jpeg(&image_bytes(name), 95);
    let db = psnr(&python_jpeg, &ours);
    assert!(db >= PSNR_MIN_DB, "{id}: PSNR {db:.2} dB < {PSNR_MIN_DB}");
    let downloaded = r
        .items
        .iter()
        .find(|i| i["event"] == "cover_downloaded")
        .unwrap();
    assert_eq!(downloaded["data"]["bytes"], json!(ours.len()), "{id}");
    let cover_jpg = s
        .lib
        .join(sc["albums"][0]["dir"].as_str().unwrap())
        .join("cover.jpg");
    let dry = p["dry_run"].as_bool() == Some(true);
    // A consumer that stopped before cover_saved still gets cover.jpg.
    if !dry {
        assert_eq!(std::fs::read(&cover_jpg).unwrap(), ours, "{id}: cover.jpg");
    }
    for f in &r.files {
        for pic in f["flac"]["pictures"].as_array().into_iter().flatten() {
            assert_eq!(
                pic["sha256"],
                json!(sha256(&ours)),
                "{id}: embedded picture"
            );
        }
    }
}

fn expected_db(sc: &Value) -> Value {
    let id = sc["id"].as_str().unwrap();
    let mut db = sc["after"]["db"].clone();
    let mut fps: Vec<Value> = db["track_fingerprints"].as_array().unwrap().clone();
    for d in deltas().iter().filter(|d| d.scenario == id) {
        let row = fps
            .iter_mut()
            .find(|r| r["filename"] == d.old)
            .unwrap_or_else(|| panic!("{id}: delta row {} not in the Python app's record", d.old));
        row["filename"] = json!(d.new);
    }
    fps.sort_by_key(|r| {
        (
            r["album_id"].as_str().unwrap().to_string(),
            r["filename"].as_str().unwrap().to_string(),
        )
    });
    db["track_fingerprints"] = Value::Array(fps);
    db
}

fn check_scenario(sc: &Value) {
    let id = sc["id"].as_str().unwrap();
    let recorded = sc;
    let expected = expected_record(sc);
    let sc = &expected;
    let s = setup(recorded);
    // The starting point is the Python app's.
    assert_eq!(
        describe_lib(&s.lib),
        sc["before"]["files"].as_array().unwrap().clone(),
        "{id}: files before"
    );
    let conn = connect(&s.db);
    assert_eq!(
        dump_db(&conn, &s.norm, &album_columns()),
        sc["before"]["db"],
        "{id}: db before"
    );
    drop(conn);

    let r = run(recorded, &s);
    let cover = has_cover(sc);
    let ours: Vec<Value> = r
        .items
        .iter()
        .map(|i| normalise_item(i.clone(), cover))
        .collect();
    let theirs: Vec<Value> = sc["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| normalise_item(i.clone(), cover))
        .collect();
    assert_eq!(ours.len(), theirs.len(), "{id}: item count\n{ours:#?}");
    for (n, (a, b)) in ours.iter().zip(&theirs).enumerate() {
        assert_eq!(a, b, "{id}: item {n}");
    }
    assert_eq!(
        r.outcome == ApplyOutcome::Stopped,
        sc["stopped_early"].as_bool().unwrap(),
        "{id}: stopped"
    );
    assert_eq!(
        r.bus,
        sc["bus_events"].as_array().unwrap().clone(),
        "{id}: bus events"
    );
    let after: Vec<Value> = sc["after"]["files"].as_array().unwrap().clone();
    assert_eq!(
        normalise_files(&r.files, cover),
        normalise_files(&after, cover),
        "{id}: files after"
    );
    if cover && r.items.iter().any(|i| i["event"] == "cover_downloaded") {
        check_jpeg(sc, &s, &r);
    }
    assert_eq!(r.db, expected_db(sc), "{id}: db after");
}

fn scenario(id: &str) -> &'static Value {
    golden()["scenarios"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == id)
        .unwrap_or_else(|| panic!("no scenario {id}"))
}

macro_rules! scenario_tests {
    ($($name:ident => $id:literal, $why:literal;)*) => {
        $(
            #[test]
            #[doc = $why]
            fn $name() {
                check_scenario(scenario($id));
            }
        )*

        /// Every scenario in the golden has a test here (or is listed as
        /// handled elsewhere).
        #[test]
        fn every_golden_scenario_is_covered() {
            let ids: Vec<&str> = vec![$($id),*];
            for s in golden()["scenarios"].as_array().unwrap() {
                assert!(ids.contains(&s["id"].as_str().unwrap()), "untested scenario {}", s["id"]);
            }
        }
    };
}

scenario_tests! {
    album_fields => "album-fields", "Album-level fields.";
    track_fields_renames => "track-fields-renames", "Per-track fields + file_renamed.";
    cover_png_url => "cover-png-url", "Cover from PNG via the downloader.";
    cover_jpeg_url => "cover-jpeg-url", "Cover from JPEG via the downloader.";
    cover_jpeg_bytes => "cover-jpeg-bytes", "Upload path (bytes in hand).";
    cover_and_tags => "cover-and-tags", "Cover + tags + rename together.";
    album_fields_dry => "album-fields-dry", "Dry run changes nothing, publishes nothing.";
    track_fields_renames_dry => "track-fields-renames-dry", "Dry run, renames planned only.";
    cover_png_url_dry => "cover-png-url-dry", "Dry run with a downloaded cover.";
    cover_jpeg_url_dry => "cover-jpeg-url-dry", "Dry run with a downloaded JPEG.";
    cover_jpeg_bytes_dry => "cover-jpeg-bytes-dry", "Dry run, upload path.";
    cover_and_tags_dry => "cover-and-tags-dry", "Dry run of everything at once.";
    noop_same_values => "noop-same-values", "No diffs still rescans, upserts, publishes.";
    noop_empty => "noop-empty", "Empty payload, AlbumTagsChanged(fields=[]).";
    one_file_fails => "one-file-fails", "Negative: one read-only file fails, the rest apply.";
    stop_after_first => "stop-after-first", "Cancellation (deliberate change): stop after 'start'; the apply still completes.";
    stop_after_file_done => "stop-after-file-done", "Cancellation (deliberate change): stop mid-loop; the apply still completes.";
    stop_after_done => "stop-after-done", "Cancellation (deliberate change): stop before the publish point; cover.jpg, upsert and events still happen.";
    stop_after_updated_album => "stop-after-updated-album", "Stop right after updated_album; events already published.";
    stop_after_first_rename => "stop-after-first-rename", "Cancellation (deliberate change): the second rename still lands; nothing left hidden.";
    track_tags_rename => "track-tags-rename", "Tag rows follow the rename (fingerprints too: deliberate change), sidecar rewritten.";
    track_tags_rename_dry => "track-tags-rename-dry", "Dry run leaves track_tags, fingerprints and the sidecar alone.";
    track_tags_swap => "track-tags-swap", "Swap renames (fingerprints follow: deliberate change).";
    missing_dir_gone => "missing-dir-gone", "Negative (deliberate change): Missing album refused before anything is read.";
    missing_dir_present => "missing-dir-present", "Deliberate change: Missing album refused even though its directory is back.";
    no_flacs_left => "no-flacs-left", "removed_album, AlbumMarkedMissing.";
    cover_download_fails => "cover-download-fails", "Negative: downloader returns nothing.";
    cover_url_and_bytes => "cover-url-and-bytes", "Negative: caller bug.";
    no_library_dir => "no-library-dir", "No library dir skips the upsert; scanner dict.";
    one_file_fails_grows => "one-file-fails-grows", "Negative: a read-only FLAC whose new metadata does not fit is refused as in the Python app (no temp+rename).";
    stop_after_file_done_raise => "stop-after-file-done-raise", "Deliberate change: the Python app's consumer raised; here the sink panics: treated as Break, the apply completes.";
    cover_empty_url => "cover-empty-url", "Deliberate change: zero bytes from the downloader are a failed download.";
    cover_empty_bytes => "cover-empty-bytes", "Deliberate change: an empty upload is a failed download.";
}

/// apply_direct (checks/_apply.py): drains the same pipeline.
#[test]
fn apply_direct_cases() {
    for case in golden()["apply_direct"].as_array().unwrap() {
        let id = case["id"].as_str().unwrap();
        let (td, lib) = temp_lib(golden());
        build_album(&lib, &scenario("album-fields")["albums"][0]);
        let mut norm = Norm::new(&lib);
        norm.learn_all();
        let db = fresh_db(td.path());
        let mut conn = connect(&db);
        seed_scan(&mut conn, &lib);
        let (bus, log) = recording_bus();
        let album_id = if case["album"] == "known" {
            musaic_core::album_id(
                &musaic_scan::py_resolve(&lib.join("Aurora Fields - First Light [FLAC]"))
                    .to_string_lossy(),
            )
        } else {
            "0123456789abcdef".into()
        };
        let now = || "2030-01-01T00:00:00Z".to_string();
        let mut env = ApplyEnv {
            conn: &mut conn,
            bus: &bus,
            covers: &LocalCovers,
            now: &now,
            track_order: TrackOrder::DiscTrack,
        };
        let (ok, msg) = apply_direct(
            &mut env,
            Some(&lib.to_string_lossy()),
            &album_id,
            &pairs(&case["payload_pairs"]["suggestions"]),
            &track_pairs(&case["payload_pairs"]["track_suggestions"]),
            case["cover_url"].as_str(),
        );
        assert_eq!(json!([ok, msg]), case["result"], "{id}");
        let events: Vec<Value> = log
            .lock()
            .unwrap()
            .iter()
            .map(|e| norm.value(&event_json(e)))
            .collect();
        assert_eq!(
            events,
            case["bus_events"].as_array().unwrap().clone(),
            "{id}"
        );
        assert_eq!(
            describe_lib(&lib),
            case["after"]["files"].as_array().unwrap().clone(),
            "{id}"
        );
        assert_eq!(
            dump_db(&conn, &norm, &album_columns()),
            case["after"]["db"],
            "{id}"
        );
    }
}

fn http_case(id: &str) -> &'static Value {
    golden()["http"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == id)
        .unwrap()
}

/// The router checks (api/apply.py): status and detail as the Python app answers.
#[test]
fn router_checks() {
    // Negative: 400 / 413 before the album lookup, 404, 409.
    for (id, mime, body) in [
        ("upload-bad-mime", "text/plain", b"hello".to_vec()),
        ("upload-empty", "image/png", Vec::new()),
        (
            "upload-too-large",
            "image/png",
            vec![0u8; 25 * 1024 * 1024 + 1],
        ),
    ] {
        let err = validate_cover_upload(Some(mime), &body).unwrap_err();
        let c = http_case(id);
        assert_eq!(json!(err.status), c["status"], "{id}");
        assert_eq!(json!(err.detail), c["json"]["detail"], "{id}");
    }
    assert_eq!(
        validate_cover_upload(Some("IMAGE/PNG"), b"x").unwrap(),
        "image/png"
    );

    let (td, lib) = temp_lib(golden());
    build_album(&lib, &scenario("album-fields")["albums"][0]);
    let db = fresh_db(td.path());
    let mut conn = connect(&db);
    seed_scan(&mut conn, &lib);
    let lib_s = lib.to_string_lossy().into_owned();
    for id in ["apply-stream-unknown-album", "upload-unknown-album"] {
        let err = resolve_album(
            &conn,
            Some(&lib_s),
            "0123456789abcdef",
            TrackOrder::DiscTrack,
        )
        .unwrap_err();
        let c = http_case(id);
        assert_eq!(json!(err.status), c["status"], "{id}");
        assert_eq!(json!(err.detail), c["json"]["detail"], "{id}");
    }
    let err = resolve_album(&conn, None, "x", TrackOrder::DiscTrack).unwrap_err();
    let c = http_case("apply-stream-no-library");
    assert_eq!(json!(err.status), c["status"]);
    assert_eq!(json!(err.detail), c["json"]["detail"]);
}

/// At the byte level: the SSE body the Python app's router streamed for
/// `{"suggestions": {"year": "2020"}}` equals ours frame for frame
/// (dir_mtime and ids masked the way the generator masked them). The dry-run
/// body is compared too.
#[test]
fn sse_body_is_byte_identical() {
    for id in ["apply-stream-dry-run", "apply-stream-real"] {
        let c = http_case(id);
        let (td, lib) = temp_lib(golden());
        build_album(&lib, &scenario("album-fields")["albums"][0]);
        let mut norm = Norm::new(&lib);
        norm.learn_all();
        let db = fresh_db(td.path());
        let mut conn = connect(&db);
        seed_scan(&mut conn, &lib);
        let resolved = resolve_album(
            &conn,
            Some(&lib.to_string_lossy()),
            norm.ids
                .iter()
                .find(|(_, v)| v.as_str() == "Aurora Fields - First Light [FLAC]")
                .unwrap()
                .0,
            TrackOrder::DiscTrack,
        )
        .unwrap();
        let (bus, log) = recording_bus();
        let now = || "2030-01-01T00:00:00Z".to_string();
        let mut env = ApplyEnv {
            conn: &mut conn,
            bus: &bus,
            covers: &LocalCovers,
            now: &now,
            track_order: TrackOrder::DiscTrack,
        };
        let req = ApplyRequest {
            album_id: resolved.id.clone(),
            album_path: resolved.path.clone(),
            library_dir: Some(lib.to_string_lossy().into_owned()),
            suggestions: vec![("year".into(), Some("2020".into()))],
            dry_run: id.ends_with("dry-run"),
            ..Default::default()
        };
        let mut body = String::new();
        apply_album_changes(&mut env, &req, &mut |item| {
            body.push_str(&item.sse_frame());
            ControlFlow::Continue(())
        });
        let body = mask_sse(&body, &norm);
        assert_eq!(body, c["sse_body"].as_str().unwrap(), "{id}");
        let events: Vec<Value> = log
            .lock()
            .unwrap()
            .iter()
            .map(|e| norm.value(&event_json(e)))
            .collect();
        assert_eq!(
            Value::Array(events),
            c.get("bus_events").cloned().unwrap_or(json!([])),
            "{id}"
        );
    }
}

/// The generator's masking of a raw SSE body.
fn mask_sse(body: &str, norm: &Norm) -> String {
    let mut out = String::new();
    let mut rest = body;
    let key = "\"dir_mtime\": ";
    while let Some(i) = rest.find(key) {
        out.push_str(&rest[..i + key.len()]);
        rest = &rest[i + key.len()..];
        let end = rest
            .find(|c: char| !(c.is_ascii_digit() || matches!(c, '.' | 'e' | '+' | '-')))
            .unwrap_or(rest.len());
        out.push_str("\"<MTIME>\"");
        rest = &rest[end..];
    }
    out.push_str(rest);
    let mut out = musaic_testdata::replace_root(&out, &norm.lib, "<LIB>");
    let ids: HashMap<&String, &String> = norm.ids.iter().collect();
    for (real, label) in ids {
        out = out.replace(real.as_str(), &format!("<ID:{label}>"));
    }
    out
}

/// Each stop scenario's replacement comes from a Python app run of the
/// same payload on the same album, from the same starting point.
#[test]
fn stop_scenarios_share_the_full_runs_input() {
    for d in record_deltas() {
        if let NewValue::Scenario(full) = d.new {
            let (a, b) = (scenario(d.scenario), scenario(full));
            for k in ["payload", "albums", "seed", "before", "album_row_in"] {
                assert_eq!(a.get(k), b.get(k), "{} vs {full}: {k}", d.scenario);
            }
            assert_eq!(
                b["stopped_early"],
                json!(full == "stop-after-updated-album")
            );
            // The full run's consumer saw everything up to its last item.
            let last = b["items"].as_array().unwrap().last().unwrap();
            assert_eq!(last["event"], "updated_album", "{full}");
        }
    }
}

/// Every record delta names a scenario of the golden (and so is applied by
/// that scenario's test, which fails if the delta changes nothing).
#[test]
fn every_record_delta_names_a_scenario() {
    let ids: Vec<&str> = golden()["scenarios"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap())
        .collect();
    for d in record_deltas() {
        assert!(ids.contains(&d.scenario), "{} ({})", d.scenario, d.why);
        assert!(!d.why.is_empty());
    }
}

/// A seeded library whose album row is Missing (the generator's
/// `mark_missing` seed); the directory is still there.
fn missing_album_setup() -> (
    tempfile::TempDir,
    std::path::PathBuf,
    std::path::PathBuf,
    String,
) {
    let (td, lib) = temp_lib(golden());
    build_album(&lib, &scenario("album-fields")["albums"][0]);
    let db = fresh_db(td.path());
    let mut conn = connect(&db);
    seed_scan(&mut conn, &lib);
    let id = musaic_core::album_id(
        &musaic_scan::py_resolve(&lib.join("Aurora Fields - First Light [FLAC]")).to_string_lossy(),
    );
    musaic_db::albums::mark_album_missing(&conn, &id, MISSING_SINCE_SEED).unwrap();
    (td, lib, db, id)
}

/// At the router level: resolve_album answers 409 for a Missing
/// album (apply/stream, dry run or not, and cover upload all resolve first),
/// with text that says to rescan; nothing is read or written.
#[test]
fn missing_album_is_refused_with_409() {
    let (_td, lib, db, id) = missing_album_setup();
    let conn = connect(&db);
    let before = (
        describe_lib(&lib),
        dump_db(&conn, &Norm::new(&lib), &album_columns()),
    );
    let err = resolve_album(
        &conn,
        Some(&lib.to_string_lossy()),
        &id,
        TrackOrder::DiscTrack,
    )
    .unwrap_err();
    assert_eq!(err.status, 409);
    assert_eq!(err.detail, missing_album_detail(MISSING_SINCE_SEED));
    assert!(err.detail.contains("Rescan"), "{}", err.detail);
    // The pipeline refuses on its own too, for a dry run and for an upload.
    let (bus, log) = recording_bus();
    let mut conn = connect(&db);
    let now = || "2030-01-01T00:00:00Z".to_string();
    for (dry_run, cover_bytes) in [(true, None), (false, Some(image_bytes("jpeg_rgb_160x120")))] {
        let mut env = ApplyEnv {
            conn: &mut conn,
            bus: &bus,
            covers: &LocalCovers,
            now: &now,
            track_order: TrackOrder::DiscTrack,
        };
        let req = ApplyRequest {
            album_id: id.clone(),
            album_path: lib
                .join("Aurora Fields - First Light [FLAC]")
                .to_string_lossy()
                .into_owned(),
            library_dir: Some(lib.to_string_lossy().into_owned()),
            suggestions: vec![("year".into(), Some("2020".into()))],
            cover_bytes,
            cover_mime: Some("image/jpeg".into()),
            dry_run,
            ..Default::default()
        };
        let mut items = Vec::new();
        let outcome = apply_album_changes(&mut env, &req, &mut |i| {
            items.push(i);
            ControlFlow::Continue(())
        });
        assert_eq!(outcome, ApplyOutcome::Finished);
        assert_eq!(items.len(), 1, "dry_run={dry_run}");
        assert_eq!(items[0].event, "error");
        assert_eq!(
            items[0].data.get("detail").and_then(|d| d.as_str()),
            Some(missing_album_detail(MISSING_SINCE_SEED).as_str())
        );
    }
    // apply_direct (the check-fix path) is refused the same way.
    let mut env = ApplyEnv {
        conn: &mut conn,
        bus: &bus,
        covers: &LocalCovers,
        now: &now,
        track_order: TrackOrder::DiscTrack,
    };
    let (ok, msg) = apply_direct(
        &mut env,
        Some(&lib.to_string_lossy()),
        &id,
        &vec![("title".into(), Some("X".into()))],
        &Vec::new(),
        None,
    );
    assert!(!ok);
    assert_eq!(msg, missing_album_detail(MISSING_SINCE_SEED));
    assert!(log.lock().unwrap().is_empty(), "nothing published");
    let after = (
        describe_lib(&lib),
        dump_db(&conn, &Norm::new(&lib), &album_columns()),
    );
    assert_eq!(before, after, "nothing read-and-written");
}

/// A dry run may stop early. After a Break on `cover_downloading`
/// the download never happens and nothing else is handed over.
#[test]
fn dry_run_stops_early() {
    struct Counting(std::cell::Cell<usize>);
    impl CoverSource for Counting {
        fn download(&self, url: &str) -> Option<(Vec<u8>, String)> {
            self.0.set(self.0.get() + 1);
            LocalCovers.download(url)
        }
    }
    let (td, lib) = temp_lib(golden());
    build_album(&lib, &scenario("album-fields")["albums"][0]);
    let db = fresh_db(td.path());
    let mut conn = connect(&db);
    seed_scan(&mut conn, &lib);
    let (bus, _log) = recording_bus();
    let covers = Counting(std::cell::Cell::new(0));
    let now = || "2030-01-01T00:00:00Z".to_string();
    let mut env = ApplyEnv {
        conn: &mut conn,
        bus: &bus,
        covers: &covers,
        now: &now,
        track_order: TrackOrder::DiscTrack,
    };
    let album_dir = lib.join("Aurora Fields - First Light [FLAC]");
    let req = ApplyRequest {
        album_id: musaic_core::album_id(&musaic_scan::py_resolve(&album_dir).to_string_lossy()),
        album_path: album_dir.to_string_lossy().into_owned(),
        library_dir: Some(lib.to_string_lossy().into_owned()),
        cover_url: Some("https://covers.invalid/png".into()),
        dry_run: true,
        ..Default::default()
    };
    let mut n = 0;
    let outcome = apply_album_changes(&mut env, &req, &mut |_| {
        n += 1;
        ControlFlow::Break(())
    });
    assert_eq!((outcome, n, covers.0.get()), (ApplyOutcome::Stopped, 1, 0));
}
