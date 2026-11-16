//! Shared harness for the musaic-ai golden tests: the goldens recorded from
//! the Python app, resolutions built from their specs (as the
//! generator's `build_resolution` does), the generator's normalisation, and
//! a stub [`WorkerEnv`] that feeds a worker scenario's scripted outside
//! world (album dict and tracks as the Python app read them, the stubbed
//! resolution, cover verification, MusicBrainz fallback and release-group
//! check) over a fresh library database.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use musaic_ai::provider::replay::{Ending, ReplayProvider, Script};
use musaic_ai::provider::{ImagePart, ProviderError, ProviderEvent};
use musaic_ai::{Py, WorkerEnv};
use musaic_resolve::{AlbumResolution, FieldFact, TrackResolution};
use regex::Regex;
use rusqlite::Connection;
use serde_json::Value;

/// A golden under golden/ai/, with its private half merged in when available.
pub fn golden(name: &str) -> Value {
    musaic_testdata::golden(&format!("ai/{name}"))
}

/// `<NOW>` for ISO timestamps, `<JOB:n>` for the 16-hex job ids the
/// generator hands out (`0000000000000001`, ...).
pub fn norm(s: &str) -> String {
    let iso = Regex::new(r"\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(?:\.\d+)?(?:Z|\+00:00)").unwrap();
    let job = Regex::new(r"\b0{8}[0-9a-f]{8}\b").unwrap();
    let s = job.replace_all(s, |c: &regex::Captures| {
        format!("<JOB:{}>", u64::from_str_radix(&c[0], 16).unwrap())
    });
    iso.replace_all(&s, "<NOW>").into_owned()
}

pub fn norm_value(v: &Value) -> Value {
    match v {
        Value::String(s) => Value::String(norm(s)),
        Value::Array(a) => Value::Array(a.iter().map(norm_value).collect()),
        Value::Object(m) => {
            Value::Object(m.iter().map(|(k, x)| (norm(k), norm_value(x))).collect())
        }
        other => other.clone(),
    }
}

/// The job id the generator gave `<JOB:n>`.
pub fn job_id(normalised: &str) -> String {
    let n: u64 = normalised
        .trim_start_matches("<JOB:")
        .trim_end_matches('>')
        .parse()
        .unwrap();
    format!("{n:016x}")
}

fn fact(v: &Value) -> Option<FieldFact> {
    let a = v.as_array()?;
    Some(FieldFact::new(
        a[0].as_str().unwrap(),
        a[1].as_str().unwrap(),
        a[2].as_f64().unwrap(),
    ))
}

/// The generator's `build_resolution(spec)`.
pub fn resolution(spec: &Value, album_id: &str) -> AlbumResolution {
    let mut r = AlbumResolution::new(album_id);
    let f = |k: &str| spec.get(k).and_then(fact);
    r.anchor = f("anchor");
    r.anchor_strategy = spec
        .get("anchor_strategy")
        .and_then(Value::as_str)
        .unwrap_or("none")
        .to_string();
    r.canonical_title = f("canonical_title");
    r.canonical_album_artist = f("canonical_album_artist");
    r.canonical_year = f("canonical_year");
    r.canonical_album_artist_sort = f("canonical_album_artist_sort");
    r.canonical_genre = f("canonical_genre");
    r.cover_url = f("cover_url");
    for t in spec
        .get("tracks")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        let g = |k: &str| t.get(k).and_then(fact);
        r.tracks.push(TrackResolution {
            filename: t["filename"].as_str().unwrap().into(),
            recording_mbid: t
                .get("recording_mbid")
                .and_then(Value::as_str)
                .map(str::to_string),
            fingerprint_score: t.get("fingerprint_score").and_then(Value::as_f64),
            fingerprinted: t
                .get("fingerprinted")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            canonical_title: g("canonical_title"),
            canonical_artist: g("canonical_artist"),
            canonical_tracknumber: g("canonical_tracknumber"),
            canonical_duration_s: t.get("canonical_duration_s").and_then(Value::as_f64),
        });
    }
    r.unresolved_track_count = spec
        .get("unresolved_track_count")
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    r.notes = spec
        .get("notes")
        .and_then(Value::as_array)
        .map(|a| a.iter().map(|n| n.as_str().unwrap().to_string()).collect())
        .unwrap_or_default();
    r
}

/// A recorded event script as a replay [`Script`]. `timeout` becomes a hang
/// (the worker's wall-clock cap fires); the others become the typed errors
/// the generator raised.
pub fn script(s: &Value) -> Script {
    let events = s["events"].as_array().unwrap().iter().map(event).collect();
    let ending = match s.get("raise").filter(|r| !r.is_null()) {
        None => Ending::Done,
        Some(r) => match r["kind"].as_str().unwrap() {
            "auth" => Ending::Fail(ProviderError::Auth(r["message"].as_str().unwrap().into())),
            "transient" => Ending::Fail(ProviderError::Transient(
                r["message"].as_str().unwrap().into(),
            )),
            "timeout" => Ending::Hang,
            "other" => Ending::Fail(ProviderError::Other {
                message: r["message"].as_str().unwrap().into(),
                cause: r.get("cause").and_then(Value::as_array).map(|c| {
                    let (ty, msg) = (c[0].as_str().unwrap(), c[1].as_str().unwrap());
                    // str(KeyError("k")) is "'k'"; str(ValueError("x")) is "x".
                    let text = if ty == "KeyError" {
                        musaic_fsops::py_repr(msg)
                    } else {
                        msg.to_string()
                    };
                    (ty.to_string(), text)
                }),
            }),
            other => panic!("unknown raise kind {other}"),
        },
    };
    Script { events, ending }
}

pub fn event(e: &Value) -> ProviderEvent {
    match e["type"].as_str().unwrap() {
        "text" => ProviderEvent::Text(e["text"].as_str().unwrap().into()),
        "reasoning" => ProviderEvent::Reasoning(e["text"].as_str().unwrap().into()),
        "tool_start" => ProviderEvent::ToolCallStart {
            tool_name: e["name"].as_str().unwrap().into(),
            arguments_preview: e["args"].as_str().unwrap().into(),
        },
        "tool_result" => ProviderEvent::ToolCallResult {
            output: Py::from_value(&e["output"]),
            is_error: e["is_error"].as_bool().unwrap(),
        },
        "complete" => ProviderEvent::TurnComplete {
            text: e["text"].as_str().unwrap().into(),
        },
        other => panic!("unknown event {other}"),
    }
}

/// The Python app's thumbnail bytes for the album that has one (`_meta.thumb_b64`).
pub fn thumb_bytes(g: &Value) -> Vec<u8> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(g["_meta"]["thumb_b64"].as_str().unwrap())
        .unwrap()
}

/// A fresh library database with one row per album of the golden (ids as
/// the golden spells them: `<ID:dir>`).
pub fn fresh_db(dir: &Path, album_ids: &[String]) -> PathBuf {
    let path = dir.join("library.db");
    let (db, _) = musaic_db::Db::open_or_create(&path).unwrap();
    let c = db.connection();
    c.execute("INSERT INTO libraries (path) VALUES ('<LIB>')", [])
        .unwrap();
    for id in album_ids {
        c.execute(
            "INSERT INTO albums (id, library_path, path) VALUES (?1, '<LIB>', ?2)",
            [id, &format!("<LIB>/{id}")],
        )
        .unwrap();
    }
    path
}

/// A clock that moves one second per call.
#[derive(Default)]
pub struct Clock(AtomicU64);

impl Clock {
    pub fn now(&self) -> String {
        let t = self.0.fetch_add(1, Ordering::SeqCst) + 1;
        format!(
            "2026-01-01T{:02}:{:02}:{:02}Z",
            t / 3600,
            t / 60 % 60,
            t % 60
        )
    }
}

/// One worker scenario's outside world.
pub struct StubEnv {
    pub db: PathBuf,
    pub clock: Clock,
    pub albums: BTreeMap<String, Py>,
    pub tracks_by_path: BTreeMap<String, Py>,
    pub resolution: Mutex<Option<Value>>,
    pub thumb: Option<(String, Vec<u8>)>,
    pub firecrawl: Mutex<String>,
    pub verify: Mutex<BTreeMap<String, (bool, String)>>,
    pub find_cover: Mutex<Value>,
    pub mb_status: Mutex<BTreeMap<String, Value>>,
    pub anchor_events: Mutex<Vec<String>>,
    pub notifies: AtomicU64,
    pub log: Mutex<Vec<String>>,
    pub gate_closed: std::sync::atomic::AtomicBool,
    pub guards: Arc<AtomicU64>,
    /// Runs while a release group is being verified, i.e. while the user
    /// could be changing the album.
    pub on_verify: Mutex<Option<Box<dyn Fn() + Send>>>,
}

impl StubEnv {
    pub fn new(
        db: PathBuf,
        albums: BTreeMap<String, Py>,
        tracks_by_path: BTreeMap<String, Py>,
    ) -> StubEnv {
        StubEnv {
            db,
            clock: Clock::default(),
            albums,
            tracks_by_path,
            resolution: Mutex::new(None),
            thumb: None,
            firecrawl: Mutex::new(String::new()),
            verify: Mutex::default(),
            find_cover: Mutex::new(Value::Null),
            mb_status: Mutex::default(),
            anchor_events: Mutex::default(),
            notifies: AtomicU64::new(0),
            log: Mutex::default(),
            gate_closed: std::sync::atomic::AtomicBool::new(false),
            guards: Arc::new(AtomicU64::new(0)),
            on_verify: Mutex::new(None),
        }
    }
}

/// Counts live guards (the restore gate's "work in progress").
pub struct Guard(Arc<AtomicU64>);

impl Drop for Guard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl WorkerEnv for StubEnv {
    fn open_db(&self) -> Result<Connection, String> {
        musaic_db::open_connection(&self.db).map_err(|e| e.to_string())
    }
    fn now_iso(&self) -> String {
        self.clock.now()
    }
    fn notify(&self) {
        self.notifies.fetch_add(1, Ordering::SeqCst);
    }
    fn log(&self, line: &str) {
        self.log.lock().unwrap().push(line.to_string());
    }
    fn album(&self, conn: &Connection, album_id: &str) -> Option<Py> {
        let exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM albums WHERE id = ?1)",
                [album_id],
                |r| r.get(0),
            )
            .unwrap();
        if !exists {
            return None;
        }
        self.albums.get(album_id).cloned()
    }
    fn read_tracks(&self, album_path: &str) -> Py {
        self.tracks_by_path
            .get(album_path)
            .cloned()
            .unwrap_or(Py::List(Vec::new()))
    }
    fn resolve(
        &self,
        _conn: &Connection,
        album_id: &str,
        _hint: &str,
    ) -> Result<Arc<AlbumResolution>, String> {
        let spec = self.resolution.lock().unwrap().clone();
        let spec = spec
            .filter(|s| !s.is_null())
            .unwrap_or_else(|| serde_json::json!({"tracks": [], "unresolved_track_count": 0}));
        Ok(Arc::new(resolution(&spec, album_id)))
    }
    fn cover_for_prompt(&self, album: &Py) -> Option<ImagePart> {
        let (id, bytes) = self.thumb.as_ref()?;
        (album.get("id").and_then(Py::as_str) == Some(id.as_str())).then(|| ImagePart {
            bytes: bytes.clone(),
            media_type: "image/jpeg".into(),
        })
    }
    fn firecrawl_key(&self) -> Option<String> {
        Some(self.firecrawl.lock().unwrap().clone()).filter(|k| !k.is_empty())
    }
    fn verify_cover_url(&self, url: &str) -> (bool, String) {
        self.verify
            .lock()
            .unwrap()
            .get(url)
            .cloned()
            .unwrap_or((false, "unscripted".into()))
    }
    fn find_cover(&self, _conn: &Connection, _album: &Py, _hint: &str) -> Result<Py, String> {
        let f = self.find_cover.lock().unwrap().clone();
        match f {
            Value::String(s) if s == "raise" => Err("MusicBrainz down".into()),
            Value::Null => Ok(Py::from_value(&serde_json::json!({
                "cover_url": null, "source": "", "notes": "", "confidence": "low"
            }))),
            other => Ok(Py::from_value(&other)),
        }
    }
    fn verify_release_group(&self, mbid: &str) -> Result<u16, String> {
        if let Some(f) = self.on_verify.lock().unwrap().as_ref() {
            f();
        }
        match self.mb_status.lock().unwrap().get(mbid) {
            Some(Value::Number(n)) => Ok(n.as_u64().unwrap() as u16),
            Some(_) => Err("network disabled".into()),
            None => Err(format!("unscripted {mbid}")),
        }
    }
    fn anchor_set(&self, album_id: &str) {
        self.anchor_events
            .lock()
            .unwrap()
            .push(album_id.to_string());
    }
    fn begin_work(&self) -> Option<Box<dyn Send>> {
        if self.gate_closed.load(Ordering::SeqCst) {
            return None;
        }
        self.guards.fetch_add(1, Ordering::SeqCst);
        Some(Box::new(Guard(self.guards.clone())))
    }
}

/// A replay provider with the Python app's provider id (the default `claude`).
pub fn replay() -> ReplayProvider {
    ReplayProvider::new("claude")
}

/// Rows of a query as JSON objects (column name -> value).
pub fn rows(conn: &Connection, sql: &str) -> Vec<Value> {
    let mut st = conn.prepare(sql).unwrap();
    let names: Vec<String> = st.column_names().map(|s| s.to_string()).collect();
    st.query_map([], |r| {
        let mut m = serde_json::Map::new();
        for (i, n) in names.iter().enumerate() {
            let v = match r.get_ref(i).unwrap() {
                rusqlite::types::ValueRef::Null => Value::Null,
                rusqlite::types::ValueRef::Integer(x) => Value::from(x),
                rusqlite::types::ValueRef::Real(x) => Value::from(x),
                rusqlite::types::ValueRef::Text(t) => {
                    Value::String(norm(&String::from_utf8_lossy(t)))
                }
                rusqlite::types::ValueRef::Blob(_) => unreachable!(),
            };
            m.insert(n.clone(), v);
        }
        Ok(Value::Object(m))
    })
    .unwrap()
    .map(Result::unwrap)
    .collect()
}
