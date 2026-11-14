//! Helpers for the tests against testdata/golden/fingerprint/*.json
//! (recorded from the Python app's own
//! `fingerprint_album`, `_best_recording` and fingerprint pool in replay mode).
//!
//! The library is rebuilt as stand-in files (the golden's file names, no
//! audio): the fingerprinter is [`GoldenFingerprints`], which answers each
//! file with the fingerprint and duration the Python app's fpcalc produced for it,
//! so the AcoustID lookups are the recorded ones. The real Chromaprint port is
//! checked against fpcalc by musaic-dsp.
#![allow(dead_code)]

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use musaic_db::Db;
use musaic_events::{Bus, Event, FieldValue, Subscriber, Topic};
use musaic_net::{ReplayTransport, Request, Response, Transport, TransportError};
use musaic_scan::py_resolve;
use musaic_services::fingerprint::Fingerprinter;
use rusqlite::{Connection, params};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

/// A golden under golden/fingerprint/, merged with its private half when available.
pub fn golden(name: &str) -> Value {
    musaic_testdata::golden(&format!("fingerprint/{name}"))
}

pub fn album_golden() -> &'static Value {
    static G: OnceLock<Value> = OnceLock::new();
    G.get_or_init(|| golden("album.json"))
}

pub fn sha256(s: &str) -> String {
    Sha256::digest(s.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A temp library of stand-in files.
pub struct Lib {
    _td: tempfile::TempDir,
    pub root: PathBuf,
    pub db: PathBuf,
    ids: HashMap<String, String>,
}

impl Lib {
    /// Every album directory of `albums` (the golden's table) and every
    /// file of `files` (the golden's `fpcalc_files`), as empty files.
    pub fn build(albums: &Value, files: &Value) -> Self {
        let td = tempfile::tempdir().unwrap();
        let root = py_resolve(td.path()).join("library");
        fs::create_dir(&root).unwrap();
        let mut lib = Lib {
            db: py_resolve(td.path()).join("library.db"),
            _td: td,
            root,
            ids: HashMap::new(),
        };
        for a in albums.as_array().unwrap() {
            let rel = a["path"].as_str().unwrap().strip_prefix("<LIB>/").unwrap();
            fs::create_dir_all(lib.root.join(rel)).unwrap();
            lib.learn(rel);
        }
        for rel in files.as_object().unwrap().keys() {
            let p = lib.root.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(&p, b"").unwrap();
        }
        fs::copy(musaic_testdata::public("db/v16-empty.db"), &lib.db).unwrap();
        drop(Db::open(&lib.db).unwrap());
        lib
    }

    pub fn learn(&mut self, rel: &str) {
        let p = py_resolve(&self.root.join(rel));
        self.ids
            .insert(musaic_core::album_id(&p.to_string_lossy()), rel.to_owned());
    }

    pub fn conn(&self) -> Connection {
        let c = Connection::open(&self.db).unwrap();
        c.pragma_update(None, "foreign_keys", "ON").unwrap();
        c
    }

    pub fn lib_str(&self) -> String {
        self.root.to_string_lossy().into_owned()
    }

    /// `<LIB>/x` -> real path; `<ID:rel>` -> real id; anything else as is.
    pub fn real(&self, s: &str) -> String {
        if let Some(rel) = s.strip_prefix("<ID:").and_then(|r| r.strip_suffix('>')) {
            let p = py_resolve(&self.root.join(rel));
            return musaic_core::album_id(&p.to_string_lossy());
        }
        s.replace("<LIB>", &self.lib_str())
    }

    pub fn norm(&self, s: &str) -> String {
        if let Some(rel) = self.ids.get(s) {
            return format!("<ID:{rel}>");
        }
        s.replace(&self.lib_str(), "<LIB>")
    }

    pub fn norm_value(&self, v: &Value) -> Value {
        match v {
            Value::String(s) => Value::String(self.norm(s)),
            Value::Array(a) => Value::Array(a.iter().map(|x| self.norm_value(x)).collect()),
            Value::Object(o) => Value::Object(
                o.iter()
                    .map(|(k, x)| (self.norm(k), self.norm_value(x)))
                    .collect(),
            ),
            other => other.clone(),
        }
    }

    /// The golden's album table, inserted in its (rowid) order, plus the
    /// library row.
    pub fn insert_albums(&self, conn: &Connection, albums: &Value) {
        conn.execute(
            "INSERT OR IGNORE INTO libraries (path, scanned_at) VALUES (?1, '2001-01-01T00:00:00Z')",
            [self.lib_str()],
        )
        .unwrap();
        for a in albums.as_array().unwrap() {
            let s = |k: &str| a[k].as_str().map(|v| self.real(v));
            conn.execute(
                "INSERT INTO albums (id, library_path, path, dir_name, dir_artist, dir_album, \
                 is_compilation, track_count, total_seconds, title, album_artist, artist, \
                 disc_number, disc_total, mb_release_group_mbid, missing_since) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
                params![
                    s("id"),
                    s("library_path"),
                    s("path"),
                    s("dir_name"),
                    s("dir_artist"),
                    s("dir_album"),
                    a["is_compilation"].as_i64(),
                    a["track_count"].as_i64(),
                    a["total_seconds"].as_f64(),
                    s("title"),
                    s("album_artist"),
                    s("artist"),
                    a["disc_number"].as_i64(),
                    a["disc_total"].as_i64(),
                    s("mb_release_group_mbid"),
                    s("missing_since"),
                ],
            )
            .unwrap();
        }
    }
}

/// The Python app's fpcalc output per file, served as this build's fingerprint
/// (labelled with this build's implementation, as rows it computes are).
pub struct GoldenFingerprints {
    pub root: PathBuf,
    /// `album/file` -> (fingerprint, duration) or None for a rejected file.
    pub files: Mutex<HashMap<String, Option<(String, u64)>>>,
    pub calls: Mutex<Vec<String>>,
}

impl GoldenFingerprints {
    pub fn new(root: &Path, files: &Value) -> Self {
        let prints = &album_golden()["fingerprints"];
        let map = files
            .as_object()
            .unwrap()
            .iter()
            .map(|(rel, f)| {
                let v = f["fingerprint_sha256"].as_str().map(|sha| {
                    (
                        prints[sha].as_str().unwrap().to_owned(),
                        f["duration_s"].as_f64().unwrap() as u64,
                    )
                });
                (rel.clone(), v)
            })
            .collect();
        Self {
            root: root.to_path_buf(),
            files: Mutex::new(map),
            calls: Mutex::new(Vec::new()),
        }
    }

    /// A new name answering like an existing file (a copied FLAC).
    pub fn alias(&self, new_rel: &str, existing_rel: &str) {
        let mut f = self.files.lock().unwrap();
        let v = f[existing_rel].clone();
        f.insert(new_rel.to_owned(), v);
    }
}

impl Fingerprinter for GoldenFingerprints {
    fn fingerprint(&self, path: &Path) -> Result<(String, u64), String> {
        let rel = path
            .strip_prefix(&self.root)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        self.calls.lock().unwrap().push(rel.clone());
        match self.files.lock().unwrap().get(&rel) {
            Some(Some(v)) => Ok(v.clone()),
            Some(None) => Err("fingerprinter rejected the file".into()),
            None => panic!("no golden fingerprint for {rel}"),
        }
    }

    fn implementation(&self) -> &str {
        musaic_db::FINGERPRINT_IMPL_RUST
    }

    fn rejection_is_about_the_file(&self) -> bool {
        true
    }
}

/// the recorded replay set (private), plus the generator's injected failures by fingerprint,
/// with every AcoustID request logged in the golden's shape.
pub struct TestNet {
    pub replay: ReplayTransport,
    pub inject: Mutex<HashMap<String, String>>,
    pub log: Mutex<Vec<Value>>,
}

impl Default for TestNet {
    fn default() -> Self {
        Self {
            replay: ReplayTransport::new(musaic_testdata::private_required("replay")),
            inject: Mutex::new(HashMap::new()),
            log: Mutex::new(Vec::new()),
        }
    }
}

impl TestNet {
    pub fn take_log(&self) -> Vec<Value> {
        std::mem::take(&mut *self.log.lock().unwrap())
    }
}

fn constructed(kind: &str) -> Result<Response, TransportError> {
    let (status, ctype, body) = match kind {
        "connection-error" => {
            return Err(TransportError::Connect {
                url: "https://api.acoustid.org/v2/lookup".into(),
                detail: "injected: connection refused".into(),
            });
        }
        "http-503-html" => (
            503,
            "text/html",
            "<html><body>503 Service Unavailable</body></html>".to_owned(),
        ),
        "http-400-json-error" => (
            400,
            "application/json",
            r#"{"error": {"code": 4, "message": "invalid API key"}, "status": "error"}"#.to_owned(),
        ),
        "ok-below-floor" => (
            200,
            "application/json",
            r#"{"results": [{"id": "00000000-0000-0000-0000-00000000aaaa", "score": 0.5, "recordings": [{"id": "00000000-0000-0000-0000-00000000bbbb"}]}], "status": "ok"}"#.to_owned(),
        ),
        other => panic!("unknown injection {other}"),
    };
    Ok(Response {
        status,
        headers: vec![("Content-Type".into(), ctype.into())],
        body: body.into_bytes(),
    })
}

impl Transport for TestNet {
    fn send(&self, request: &Request) -> Result<Response, TransportError> {
        let form: Vec<(String, String)> = request.form.clone().unwrap_or_default();
        let fp = form
            .iter()
            .find(|(k, _)| k == "fingerprint")
            .map(|(_, v)| sha256(v));
        let mut f = Map::new();
        for (k, v) in &form {
            if k == "fingerprint" {
                continue;
            }
            let v = if k == "client" { "<REDACTED>" } else { v };
            f.insert(k.clone(), json!(v));
        }
        let mut entry = json!({
            "method": request.method.as_str(),
            "url": request.url(),
            "form": f,
            "fingerprint_sha256": fp,
        });
        let kind = fp
            .as_ref()
            .and_then(|s| self.inject.lock().unwrap().get(s).cloned());
        let out = if let Some(kind) = kind {
            entry["served"] = json!(format!("injected:{kind}"));
            constructed(&kind)
        } else {
            let r = self.replay.send(request);
            match &r {
                Ok(resp) => {
                    entry["served"] = json!("recorded");
                    entry["status"] = json!(resp.status);
                }
                Err(_) => entry["served"] = json!("unobserved"),
            }
            r
        };
        self.log.lock().unwrap().push(entry);
        out
    }
}

pub fn event_json(ev: &Event) -> Value {
    let mut o = Map::new();
    o.insert("type".into(), json!(ev.name()));
    for (k, v) in ev.fields() {
        o.insert(
            k.into(),
            match v {
                FieldValue::Str(s) => json!(s),
                FieldValue::Int(n) => json!(n),
                FieldValue::StrList(l) => json!(l),
            },
        );
    }
    Value::Object(o)
}

/// A bus with one recorder (the generator replaces `publish` by one).
pub fn recording_bus() -> (Bus, Arc<Mutex<Vec<Event>>>) {
    let bus = Bus::new();
    let log: Arc<Mutex<Vec<Event>>> = Arc::default();
    let l = Arc::clone(&log);
    bus.register(
        Topic::Event,
        Subscriber::from_fn(move |ev| {
            l.lock().unwrap().push(ev.clone());
            Ok(())
        }),
    );
    (bus, log)
}

/// The golden's `rows_after` shape for one album, plus the implementation
/// column (checked separately).
pub fn album_rows(conn: &Connection, album_id: &str) -> (Vec<Value>, Vec<String>) {
    let rows = musaic_db::fingerprints_ext::get_album_fingerprints(conn, album_id).unwrap();
    (
        rows.iter()
            .map(|r| {
                json!({
                    "filename": r.filename,
                    "fingerprint_sha256": sha256(&r.fingerprint),
                    "duration_s": r.duration_s,
                    "acoustid_recording_mbid": r.acoustid_recording_mbid,
                    "acoustid_score": r.acoustid_score,
                    "last_computed_at": r.last_computed_at,
                })
            })
            .collect(),
        rows.iter().map(|r| r.fingerprint_impl.clone()).collect(),
    )
}

/// `(filename, acoustid_lookup_state, acoustid_lookup_failures,
/// acoustid_retry_after)` per row of the album, by filename.
pub fn lookup_states(
    conn: &Connection,
    album_id: &str,
) -> Vec<(String, Option<String>, i64, Option<String>)> {
    musaic_db::fingerprints_ext::get_album_fingerprints(conn, album_id)
        .unwrap()
        .into_iter()
        .map(|r| {
            (
                r.filename,
                r.acoustid_lookup_state,
                r.acoustid_lookup_failures,
                r.acoustid_retry_after,
            )
        })
        .collect()
}

/// The album's remembered rejected files.
pub fn reject_names(conn: &Connection, album_id: &str) -> Vec<String> {
    musaic_db::fingerprints_ext::get_rejects(conn, album_id)
        .unwrap()
        .into_iter()
        .map(|r| r.filename)
        .collect()
}

/// One enumerated deliberate difference from a Python app record: in
/// `scenario`, the value at JSON pointer `path` was `old` (None: absent)
/// and is `new` (None: absent) in this build, for the reason `why`.
#[derive(Debug, Clone)]
pub struct Delta {
    pub scenario: String,
    pub path: String,
    pub old: Option<Value>,
    pub new: Option<Value>,
    pub why: &'static str,
}

impl Delta {
    pub fn new(
        scenario: impl Into<String>,
        path: &str,
        old: Option<Value>,
        new: Option<Value>,
        why: &'static str,
    ) -> Self {
        Self {
            scenario: scenario.into(),
            path: path.to_owned(),
            old,
            new,
            why,
        }
    }
}

/// A list of [`Delta`]s applied to Python app records. Applying checks the old
/// value first; [`Deltas::assert_all_used`] fails on a delta that never
/// applied. Whatever differs and is not listed fails the caller's
/// comparison.
pub struct Deltas {
    list: Vec<Delta>,
    used: Vec<bool>,
}

impl Deltas {
    pub fn new(list: Vec<Delta>) -> Self {
        for d in &list {
            assert_ne!(d.old, d.new, "{d:?}: a delta must change something");
        }
        let used = vec![false; list.len()];
        Self { list, used }
    }

    /// Turn the Python app's record of `scenario` into this build's expected one.
    pub fn apply(&mut self, scenario: &str, want: &mut Value) {
        for (d, used) in self.list.iter().zip(self.used.iter_mut()) {
            if d.scenario != scenario {
                continue;
            }
            let (parent, key) = d.path.rsplit_once('/').expect("JSON pointer");
            let obj = if parent.is_empty() {
                &mut *want
            } else {
                want.pointer_mut(parent)
                    .unwrap_or_else(|| panic!("{scenario}: no {parent} ({})", d.why))
            };
            let obj = obj.as_object_mut().expect("delta parent is an object");
            assert_eq!(
                obj.get(key),
                d.old.as_ref(),
                "{scenario}: {} old value ({})",
                d.path,
                d.why
            );
            match &d.new {
                Some(v) => {
                    obj.insert(key.to_owned(), v.clone());
                }
                None => {
                    obj.remove(key);
                }
            }
            *used = true;
        }
    }

    pub fn assert_all_used(&self) {
        let unused: Vec<&Delta> = self
            .list
            .iter()
            .zip(&self.used)
            .filter(|(_, u)| !**u)
            .map(|(d, _)| d)
            .collect();
        assert!(unused.is_empty(), "unused deltas: {unused:?}");
    }
}
