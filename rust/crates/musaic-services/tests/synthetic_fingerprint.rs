//! Fingerprinting and the eager fingerprint pool against synthetic AcoustID
//! answers (public test data only).
//!
//! The audio is the generated synthetic library, fingerprinted by the real
//! in-process Chromaprint (wrapped to count its calls). AcoustID is an
//! in-memory transport behind the real `AcoustIdClient`: each file's answer
//! is chosen by the fingerprint the request carries, so the requests are
//! built, sent and parsed exactly as in the app. Every recording id below
//! is invented (freshly generated UUIDs).

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use musaic_db::fingerprints_ext::{FingerprintRecord, get_album_fingerprints};
use musaic_db::{Db, FINGERPRINT_IMPL_RUST, LOOKUP_DONE, LOOKUP_FAILED};
use musaic_events::{Bus, Event, Subscriber, Topic};
use musaic_net::{AcoustIdClient, Request, Response, Transport, TransportError};
use musaic_services::clock::{Clock, format_utc};
use musaic_services::fingerprint::{
    AcoustIdLookup, CancelToken, Chromaprint, FingerprintEnv, Fingerprinter, LOOKUP_RETRY_MAX_S,
    fingerprint_album,
};
use musaic_services::fingerprint_pool::{
    FingerprintPool, PoolDeps, PoolSleeper, PoolState, PoolStatus, fingerprint_job,
};
use rusqlite::{Connection, params};
use serde_json::{Value, json};

// Invented ids (fresh random UUIDs; they name nothing real).
const REC_A: &str = "a3b67bae-16d8-4248-9420-fad23e87c8f0";
const REC_B: &str = "e694bfc0-7e60-41e1-bb09-66f2043b432f";
const REC_C: &str = "849b6799-b1b4-40c4-9756-5cb6fc2884a3";
const REC_D: &str = "b8eba0cc-7b17-47a6-9d73-2ee5d5472a5b";
const REC_E: &str = "c6b78f1c-155c-41a9-ba7e-15723281a03b";
const REC_F: &str = "f1f86997-5cde-4485-86c8-566b40e2b8bb";
const REC_G: &str = "0160fc81-b26a-4a28-81c4-887656b07991";
const RESULT_ID: &str = "2382fca2-a9c8-4d9b-8f27-a4dd97d56f3d";

const SECOND_LIGHT: &str = "Aurora Fields - Second Light [FLAC]";
const FOLDER_ART: &str = "Aurora Fields - Folder Art [FLAC]";
const PORCHLIGHT: &str = "The Lanterns - Porchlight [FLAC]";
const SORTED: &str = "Mono Harbor - Sorted [FLAC]";
const LEGACY: &str = "Legacy Folder Name";

/// 2030-01-01T00:00:00Z.
const T0: i64 = 1_893_456_000;
const HOUR: i64 = 3600;

fn at(secs: i64) -> String {
    format_utc(secs)
}

// ---------------------------------------------------------------------------
// A library copied from the synthetic one, with its album rows.

struct World {
    _td: tempfile::TempDir,
    root: PathBuf,
    db: PathBuf,
}

impl World {
    /// `present`: album directories copied from the synthetic library and
    /// inserted as Present rows. `rows_only`: rows inserted, directory not
    /// copied (an album whose directory is gone but is not marked Missing).
    /// `missing`: rows marked Missing (directory not copied).
    fn new(present: &[&str], rows_only: &[&str], missing: &[&str]) -> Self {
        let td = tempfile::tempdir().unwrap();
        let base = musaic_scan::py_resolve(td.path());
        let root = base.join("library");
        fs::create_dir(&root).unwrap();
        let db = base.join("library.db");
        fs::copy(musaic_testdata::public("db/v16-empty.db"), &db).unwrap();
        drop(Db::open(&db).unwrap());
        let w = World { _td: td, root, db };
        let src = musaic_testdata::synthetic_library();
        for name in present {
            let to = w.root.join(name);
            fs::create_dir(&to).unwrap();
            for e in fs::read_dir(src.join(name)).unwrap() {
                let e = e.unwrap();
                fs::copy(e.path(), to.join(e.file_name())).unwrap();
            }
        }
        let conn = w.conn();
        conn.execute(
            "INSERT INTO libraries (path, scanned_at) VALUES (?1, '2001-01-01T00:00:00Z')",
            [w.lib_str()],
        )
        .unwrap();
        for (names, missing_since) in [
            (present, None),
            (rows_only, None),
            (missing, Some("2001-01-01T00:00:00Z")),
        ] {
            for name in names {
                let tracks = flac_names(&src.join(name)).len() as i64;
                conn.execute(
                    "INSERT INTO albums (id, library_path, path, dir_name, track_count, missing_since) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![w.id(name), w.lib_str(), w.path(name), name, tracks, missing_since],
                )
                .unwrap();
            }
        }
        w
    }

    fn conn(&self) -> Connection {
        let c = Connection::open(&self.db).unwrap();
        c.pragma_update(None, "foreign_keys", "ON").unwrap();
        c
    }

    fn lib_str(&self) -> String {
        self.root.to_string_lossy().into_owned()
    }

    fn path(&self, album: &str) -> String {
        self.root.join(album).to_string_lossy().into_owned()
    }

    fn id(&self, album: &str) -> String {
        musaic_core::album_id(&self.path(album))
    }

    fn rows(&self, album: &str) -> Vec<FingerprintRecord> {
        get_album_fingerprints(&self.conn(), &self.id(album)).unwrap()
    }

    fn row(&self, album: &str, file: &str) -> FingerprintRecord {
        self.rows(album)
            .into_iter()
            .find(|r| r.filename == file)
            .unwrap_or_else(|| panic!("no row for {file}"))
    }
}

/// The FLAC file names of a directory, sorted (what the album listing yields
/// for the synthetic albums used here: no hidden or duplicate files).
fn flac_names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".flac") && !n.starts_with("._"))
        .collect();
    v.sort();
    v
}

// ---------------------------------------------------------------------------
// The real fingerprinter, counted.

#[derive(Default)]
struct CountingChromaprint {
    calls: Mutex<Vec<String>>,
}

impl CountingChromaprint {
    fn calls(&self) -> usize {
        self.calls.lock().unwrap().len()
    }
}

impl Fingerprinter for CountingChromaprint {
    fn fingerprint(&self, path: &Path) -> Result<(String, u64), String> {
        self.calls
            .lock()
            .unwrap()
            .push(path.file_name().unwrap().to_string_lossy().into_owned());
        Chromaprint.fingerprint(path)
    }
    fn implementation(&self) -> &str {
        Chromaprint.implementation()
    }
    fn rejection_is_about_the_file(&self) -> bool {
        Chromaprint.rejection_is_about_the_file()
    }
}

/// `filename -> (compressed fingerprint, whole seconds)` as Chromaprint
/// computes it, so the stub can answer per file. Checks the files of the
/// album fingerprint differently (otherwise per-file answers are ambiguous).
fn prints(w: &World, album: &str) -> HashMap<String, (String, u64)> {
    let dir = w.root.join(album);
    let m: HashMap<String, (String, u64)> = flac_names(&dir)
        .into_iter()
        .map(|n| {
            let p = Chromaprint.fingerprint(&dir.join(&n)).unwrap();
            (n, p)
        })
        .collect();
    let mut distinct: Vec<&String> = m.values().map(|(f, _)| f).collect();
    distinct.sort();
    distinct.dedup();
    assert_eq!(distinct.len(), m.len(), "{album}: fingerprints must differ");
    m
}

// ---------------------------------------------------------------------------
// AcoustID, in memory.

#[derive(Clone, Debug)]
enum Answer {
    /// 200 with this JSON body.
    Body(Value),
    /// Any other status.
    Status(u16),
    /// No answer at all.
    Down,
    /// The request was cancelled while it waited (the app is closing).
    Cancelled,
}

/// `{"status": "ok", "results": [...]}`: one result per `(score, recording ids)`.
fn ok(results: &[(f64, &[&str])]) -> Answer {
    Answer::Body(json!({
        "status": "ok",
        "results": results.iter().map(|(score, recs)| json!({
            "id": RESULT_ID,
            "score": score,
            "recordings": recs.iter().map(|r| json!({"id": r})).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
    }))
}

fn no_match() -> Answer {
    ok(&[])
}

/// AcoustID's own error answer (HTTP 200, `status: error`).
fn service_error() -> Answer {
    Answer::Body(json!({"status": "error", "error": {"code": 4, "message": "invalid API key"}}))
}

#[derive(Default)]
struct AcoustIdStub {
    /// By compressed fingerprint.
    answers: Mutex<HashMap<String, Answer>>,
    /// Fingerprints asked, in order.
    asked: Mutex<Vec<String>>,
}

impl AcoustIdStub {
    fn set(&self, fingerprint: &str, a: Answer) {
        self.answers
            .lock()
            .unwrap()
            .insert(fingerprint.to_owned(), a);
    }
    fn asked(&self) -> Vec<String> {
        self.asked.lock().unwrap().clone()
    }
    fn asked_count(&self) -> usize {
        self.asked.lock().unwrap().len()
    }
}

impl Transport for AcoustIdStub {
    fn send(&self, request: &Request) -> Result<Response, TransportError> {
        let form = request.form.clone().unwrap_or_default();
        let fp = form
            .iter()
            .find(|(k, _)| k == "fingerprint")
            .map(|(_, v)| v.clone())
            .expect("an AcoustID lookup carries a fingerprint");
        self.asked.lock().unwrap().push(fp.clone());
        let answer = self
            .answers
            .lock()
            .unwrap()
            .get(&fp)
            .cloned()
            .unwrap_or_else(no_match);
        match answer {
            Answer::Body(v) => Ok(Response {
                status: 200,
                headers: vec![("Content-Type".into(), "application/json".into())],
                body: serde_json::to_vec(&v).unwrap(),
            }),
            Answer::Status(s) => Ok(Response {
                status: s,
                headers: vec![("Content-Type".into(), "text/html".into())],
                body: b"<html>busy</html>".to_vec(),
            }),
            Answer::Cancelled => Err(TransportError::Stopped),
            Answer::Down => Err(TransportError::Connect {
                url: request.redacted_url(),
                detail: "connection refused (test)".into(),
            }),
        }
    }
}

fn recording_bus() -> (Bus, Arc<Mutex<Vec<Event>>>) {
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

/// The `TrackFingerprinted` events published, as (album id, filenames).
fn announced(log: &Mutex<Vec<Event>>) -> Vec<(String, Vec<String>)> {
    std::mem::take(&mut *log.lock().unwrap())
        .into_iter()
        .filter_map(|e| match e {
            Event::TrackFingerprinted {
                album_id,
                filenames,
            } => Some((album_id, filenames)),
            _ => None,
        })
        .collect()
}

/// One `fingerprint_album` call at `now`, with the key set when `key`.
#[allow(clippy::too_many_arguments)]
fn run(
    w: &World,
    album: &str,
    fpr: &CountingChromaprint,
    stub: &AcoustIdStub,
    key: bool,
    bus: &Bus,
    now: &str,
    force: bool,
) -> musaic_services::fingerprint::AlbumFingerprints {
    let conn = w.conn();
    let client = AcoustIdClient::new(stub, "synthetic-test-key");
    let env = FingerprintEnv {
        conn: &conn,
        fingerprinter: fpr,
        acoustid: key.then_some(&client as &dyn AcoustIdLookup),
        bus,
        now_iso: now,
        cancel: None,
    };
    fingerprint_album(&env, &w.id(album), &w.path(album), force).unwrap()
}

fn state(r: &FingerprintRecord) -> (Option<&str>, Option<f64>, Option<&str>, i64, Option<&str>) {
    (
        r.acoustid_recording_mbid.as_deref(),
        r.acoustid_score,
        r.acoustid_lookup_state.as_deref(),
        r.acoustid_lookup_failures,
        r.acoustid_retry_after.as_deref(),
    )
}

// ---------------------------------------------------------------------------
// fingerprint_album

/// A completed lookup stores the best recording: the highest-scoring result
/// at or above 0.85, its first recording with an id. Below the floor, or no
/// result at all, is a completed lookup with no recording ("done", never
/// asked again). Every file is fingerprinted and asked once; one
/// `TrackFingerprinted` names every stored file, in file order. The next
/// call finds rows for exactly the files on disk, all done: a cache hit
/// that fingerprints nothing, asks nothing and announces nothing.
#[test]
fn completed_lookups_store_the_best_recording_or_no_match() {
    let w = World::new(&[SECOND_LIGHT], &[], &[]);
    let p = prints(&w, SECOND_LIGHT);
    let files = flac_names(&w.root.join(SECOND_LIGHT));
    assert_eq!(files.len(), 4);
    let stub = AcoustIdStub::default();
    // File 1: two results above the floor; the later, higher one wins, and
    // its first recording.
    stub.set(
        &p[&files[0]].0,
        ok(&[(0.92, &[REC_A]), (0.97, &[REC_B, REC_C])]),
    );
    // File 2: the only result is below the floor.
    stub.set(&p[&files[1]].0, ok(&[(0.80, &[REC_D])]));
    // File 3: no result.
    stub.set(&p[&files[2]].0, no_match());
    // File 4: exactly at the floor counts.
    stub.set(&p[&files[3]].0, ok(&[(0.85, &[REC_E])]));
    let fpr = CountingChromaprint::default();
    let (bus, log) = recording_bus();
    let now = at(T0);

    let out = run(&w, SECOND_LIGHT, &fpr, &stub, true, &bus, &now, false);
    assert!(!out.from_cache);
    assert_eq!(fpr.calls(), 4);
    assert_eq!(stub.asked_count(), 4);
    assert_eq!(out.lookups.len(), 4, "every answer is handed on to Tier 0c");

    let rows = w.rows(SECOND_LIGHT);
    let names: Vec<&str> = rows.iter().map(|r| r.filename.as_str()).collect();
    assert_eq!(names, files);
    let done = Some(LOOKUP_DONE);
    assert_eq!(state(&rows[0]), (Some(REC_B), Some(0.97), done, 0, None));
    assert_eq!(state(&rows[1]), (None, None, done, 0, None));
    assert_eq!(state(&rows[2]), (None, None, done, 0, None));
    assert_eq!(state(&rows[3]), (Some(REC_E), Some(0.85), done, 0, None));
    for r in &rows {
        // The stored print and duration are the fingerprinter's; stamped now.
        assert_eq!((r.fingerprint.clone(), r.duration_s as u64), p[&r.filename]);
        assert_eq!(r.fingerprint_impl, FINGERPRINT_IMPL_RUST);
        assert_eq!(r.last_computed_at, now);
    }
    assert_eq!(
        announced(&log),
        [(w.id(SECOND_LIGHT), files.clone())],
        "announced once, every stored file"
    );

    let again = run(
        &w,
        SECOND_LIGHT,
        &fpr,
        &stub,
        true,
        &bus,
        &at(T0 + HOUR),
        false,
    );
    assert!(again.from_cache);
    assert!(again.lookups.is_empty());
    assert_eq!(again.tracks.len(), 4);
    assert_eq!(fpr.calls(), 4, "a cache hit fingerprints nothing");
    assert_eq!(stub.asked_count(), 4, "a done row is never asked again");
    assert!(announced(&log).is_empty());
}

/// A lookup that does not complete (an HTTP error status, no connection,
/// AcoustID's own error answer) leaves the row "failed" and counts
/// consecutive failures. Retry rule: after the first failure the row is due
/// again at once; after the n-th (n >= 2) it waits 1 h * 4^(n-2), capped at
/// 7 days. A due row is looked up from its stored fingerprint (nothing is
/// fingerprinted again) and only the files asked are announced; a row not
/// yet due makes the call a plain cache hit. A completed lookup resets the
/// count and clears the retry time.
#[test]
fn failed_lookups_are_counted_and_retried_on_the_backoff_schedule() {
    let w = World::new(&[FOLDER_ART], &[], &[]);
    let p = prints(&w, FOLDER_ART);
    let files = flac_names(&w.root.join(FOLDER_ART));
    assert_eq!(files.len(), 2);
    let (good, bad) = (&files[0], &files[1]);
    let stub = AcoustIdStub::default();
    stub.set(&p[good].0, ok(&[(0.95, &[REC_F])]));
    stub.set(&p[bad].0, Answer::Status(503));
    let fpr = CountingChromaprint::default();
    let (bus, log) = recording_bus();
    let id = w.id(FOLDER_ART);
    let failed = Some(LOOKUP_FAILED);

    // First failure: due again at once (retry time = now).
    run(&w, FOLDER_ART, &fpr, &stub, true, &bus, &at(T0), false);
    assert_eq!(
        state(&w.row(FOLDER_ART, good)),
        (Some(REC_F), Some(0.95), Some(LOOKUP_DONE), 0, None)
    );
    let t0 = at(T0);
    assert_eq!(
        state(&w.row(FOLDER_ART, bad)),
        (None, None, failed, 1, Some(t0.as_str()))
    );
    assert_eq!(announced(&log), [(id.clone(), files.clone())]);
    assert_eq!(fpr.calls(), 2);

    // Due at once: only the failed file is asked, from its stored print.
    stub.set(&p[bad].0, Answer::Down);
    let out = run(&w, FOLDER_ART, &fpr, &stub, true, &bus, &at(T0), false);
    assert!(!out.from_cache);
    assert_eq!(out.lookups.keys().collect::<Vec<_>>(), [bad]);
    assert_eq!(fpr.calls(), 2, "a due lookup does not fingerprint again");
    assert_eq!(
        stub.asked(),
        [p[good].0.clone(), p[bad].0.clone(), p[bad].0.clone()]
    );
    assert_eq!(announced(&log), [(id.clone(), vec![bad.clone()])]);
    let retry = at(T0 + HOUR);
    assert_eq!(
        state(&w.row(FOLDER_ART, bad)),
        (None, None, failed, 2, Some(retry.as_str()))
    );

    // Not yet due: a cache hit, nothing asked.
    let out = run(
        &w,
        FOLDER_ART,
        &fpr,
        &stub,
        true,
        &bus,
        &at(T0 + HOUR - 1),
        false,
    );
    assert!(out.from_cache);
    assert_eq!(stub.asked_count(), 3);
    assert!(announced(&log).is_empty());

    // Each further failure at its retry time waits 1 h * 4^(n-2), up to 7 d.
    stub.set(&p[bad].0, service_error());
    let mut now = T0 + HOUR;
    for n in 3..=8_i64 {
        run(&w, FOLDER_ART, &fpr, &stub, true, &bus, &at(now), false);
        let wait = (HOUR * 4_i64.pow((n - 2) as u32)).min(7 * 86_400);
        let expect = at(now + wait);
        assert_eq!(
            state(&w.row(FOLDER_ART, bad)),
            (None, None, failed, n, Some(expect.as_str())),
            "failure {n}"
        );
        now += wait;
    }
    // 1 h, 4 h, 16 h, 64 h, 256 h (> 168 h: capped), capped.
    assert_eq!(now - T0, HOUR * (1 + 4 + 16 + 64) + 3 * LOOKUP_RETRY_MAX_S);
    assert_eq!(fpr.calls(), 2);

    // A completed lookup resets the row.
    stub.set(&p[bad].0, ok(&[(0.9, &[REC_G])]));
    run(&w, FOLDER_ART, &fpr, &stub, true, &bus, &at(now), false);
    assert_eq!(
        state(&w.row(FOLDER_ART, bad)),
        (Some(REC_G), Some(0.9), Some(LOOKUP_DONE), 0, None)
    );
    // The good file was asked exactly once in all of this.
    assert_eq!(stub.asked().iter().filter(|f| **f == p[good].0).count(), 1);
}

/// A forced call fingerprints and asks every file again. A lookup that does
/// not complete keeps the recording already stored for the file (and marks
/// the row failed); a completed lookup replaces it, even with "no match".
#[test]
fn a_failed_lookup_keeps_the_recording_already_stored() {
    let w = World::new(&[FOLDER_ART], &[], &[]);
    let p = prints(&w, FOLDER_ART);
    let files = flac_names(&w.root.join(FOLDER_ART));
    let stub = AcoustIdStub::default();
    stub.set(&p[&files[0]].0, ok(&[(0.95, &[REC_A])]));
    stub.set(&p[&files[1]].0, ok(&[(0.88, &[REC_B])]));
    let fpr = CountingChromaprint::default();
    let (bus, _log) = recording_bus();
    run(&w, FOLDER_ART, &fpr, &stub, true, &bus, &at(T0), false);

    for f in &files {
        stub.set(&p[f].0, Answer::Down);
    }
    run(
        &w,
        FOLDER_ART,
        &fpr,
        &stub,
        true,
        &bus,
        &at(T0 + HOUR),
        true,
    );
    assert_eq!(fpr.calls(), 4, "forced: every file fingerprinted again");
    let t1 = at(T0 + HOUR);
    assert_eq!(
        state(&w.row(FOLDER_ART, &files[0])),
        (
            Some(REC_A),
            Some(0.95),
            Some(LOOKUP_FAILED),
            1,
            Some(t1.as_str())
        )
    );
    assert_eq!(
        state(&w.row(FOLDER_ART, &files[1])),
        (
            Some(REC_B),
            Some(0.88),
            Some(LOOKUP_FAILED),
            1,
            Some(t1.as_str())
        )
    );

    stub.set(&p[&files[0]].0, no_match());
    stub.set(&p[&files[1]].0, ok(&[(0.5, &[REC_C])]));
    run(
        &w,
        FOLDER_ART,
        &fpr,
        &stub,
        true,
        &bus,
        &at(T0 + 2 * HOUR),
        true,
    );
    for f in &files {
        assert_eq!(
            state(&w.row(FOLDER_ART, f)),
            (None, None, Some(LOOKUP_DONE), 0, None),
            "{f}: a completed no-match replaces the stored recording"
        );
    }
}

/// Without an AcoustID key the files are still fingerprinted and stored,
/// with no recording and no lookup outcome, and announced; nothing is
/// asked. Once a key is set, those rows (no outcome yet) are due: they are
/// looked up from the stored fingerprints, without fingerprinting again.
#[test]
fn without_a_key_prints_are_stored_and_looked_up_once_a_key_is_set() {
    let w = World::new(&[FOLDER_ART], &[], &[]);
    let p = prints(&w, FOLDER_ART);
    let files = flac_names(&w.root.join(FOLDER_ART));
    let stub = AcoustIdStub::default();
    stub.set(&p[&files[0]].0, ok(&[(0.99, &[REC_D])]));
    let fpr = CountingChromaprint::default();
    let (bus, log) = recording_bus();
    let id = w.id(FOLDER_ART);

    let out = run(&w, FOLDER_ART, &fpr, &stub, false, &bus, &at(T0), false);
    assert!(out.lookups.is_empty());
    assert_eq!(stub.asked_count(), 0);
    for r in w.rows(FOLDER_ART) {
        assert_eq!(state(&r), (None, None, None, 0, None), "{}", r.filename);
    }
    assert_eq!(announced(&log), [(id.clone(), files.clone())]);

    // Still no key: a cache hit (nothing is due without a key).
    assert!(run(&w, FOLDER_ART, &fpr, &stub, false, &bus, &at(T0), false).from_cache);
    assert!(announced(&log).is_empty());

    run(
        &w,
        FOLDER_ART,
        &fpr,
        &stub,
        true,
        &bus,
        &at(T0 + HOUR),
        false,
    );
    assert_eq!(
        fpr.calls(),
        2,
        "the stored prints are looked up as they are"
    );
    assert_eq!(stub.asked_count(), 2);
    assert_eq!(announced(&log), [(id, files.clone())]);
    assert_eq!(
        state(&w.row(FOLDER_ART, &files[0])),
        (Some(REC_D), Some(0.99), Some(LOOKUP_DONE), 0, None)
    );
    assert_eq!(
        state(&w.row(FOLDER_ART, &files[1])),
        (None, None, Some(LOOKUP_DONE), 0, None)
    );
}

// ---------------------------------------------------------------------------
// The pool

struct NoSleep(Mutex<Vec<Duration>>);

impl PoolSleeper for NoSleep {
    fn sleep(&self, d: Duration, cancel: &CancelToken) -> bool {
        self.0.lock().unwrap().push(d);
        !cancel.is_cancelled()
    }
}

/// A clock the test sets.
fn settable_clock(start: i64) -> (Clock, Arc<Mutex<i64>>) {
    let now = Arc::new(Mutex::new(start));
    let n = Arc::clone(&now);
    (Arc::new(move || format_utc(*n.lock().unwrap())), now)
}

struct PoolRig {
    pool: FingerprintPool,
    stub: Arc<AcoustIdStub>,
    fpr: Arc<CountingChromaprint>,
    sleeps: Arc<NoSleep>,
    clock: Clock,
    now: Arc<Mutex<i64>>,
    key: Arc<Mutex<Option<String>>>,
    bus: Bus,
}

impl PoolRig {
    fn new() -> Self {
        let (clock, now) = settable_clock(T0);
        let (bus, _log) = recording_bus();
        Self {
            pool: FingerprintPool::new(),
            stub: Arc::default(),
            fpr: Arc::default(),
            sleeps: Arc::new(NoSleep(Mutex::default())),
            clock,
            now,
            key: Arc::new(Mutex::new(Some("synthetic-test-key".into()))),
            bus,
        }
    }

    /// One whole pass of the app's job (`fingerprint_job`) over `w`.
    fn pass(&self, w: &World) -> PoolState {
        let db = w.db.clone();
        let key = Arc::clone(&self.key);
        let deps = PoolDeps {
            open_db: Arc::new(move || {
                let c = Connection::open(&db).map_err(|e| e.to_string())?;
                c.pragma_update(None, "foreign_keys", "ON")
                    .map_err(|e| e.to_string())?;
                Ok(c)
            }),
            availability: Arc::new(|| Ok(())),
            job: fingerprint_job(
                Arc::clone(&self.stub),
                Arc::new(move || key.lock().unwrap().clone()),
                Arc::clone(&self.fpr) as Arc<dyn Fingerprinter>,
                self.bus.clone(),
                Arc::clone(&self.clock),
            ),
            clock: Arc::clone(&self.clock),
            sleeper: Arc::clone(&self.sleeps) as Arc<dyn PoolSleeper>,
            fingerprint_impl: FINGERPRINT_IMPL_RUST.into(),
        };
        let first = self.pool.warm_library(&w.lib_str(), deps);
        assert_eq!(first.status, PoolStatus::Running);
        self.pool.join();
        self.pool.state()
    }

    fn set_now(&self, t: i64) {
        *self.now.lock().unwrap() = t;
    }
}

fn counts(s: &PoolState) -> (PoolStatus, usize, usize, usize) {
    (s.status, s.total, s.completed, s.errors)
}

/// A full pass: the candidates are the library's Present albums with fewer
/// fingerprint rows than tracks. An album already fully fingerprinted (all
/// lookups done) and a Missing album are left alone; an album whose
/// directory is gone fails and counts in `errors`, and the walk goes on.
/// Every candidate is visited once with a 0.5 s pause after it; the pass
/// ends "done" with no current album. A second pass has nothing to do.
#[test]
fn a_full_pool_pass_fingerprints_every_candidate_once() {
    let w = World::new(
        &[SECOND_LIGHT, FOLDER_ART, PORCHLIGHT],
        &[SORTED],
        &[LEGACY],
    );
    let rig = PoolRig::new();
    // Folder Art is fingerprinted (and looked up) beforehand.
    let (bus, _l) = recording_bus();
    run(
        &w,
        FOLDER_ART,
        &CountingChromaprint::default(),
        &rig.stub,
        true,
        &bus,
        &at(T0),
        false,
    );
    let before = rig.stub.asked_count();

    let s = rig.pass(&w);
    // Second Light and Porchlight complete; Sorted (no directory) fails.
    assert_eq!(counts(&s), (PoolStatus::Done, 3, 2, 1));
    assert_eq!(s.current_album_id, None);
    assert_eq!(s.library_dir.as_deref(), Some(w.lib_str().as_str()));
    assert!(s.completed_at.is_some());
    assert_eq!(
        *rig.sleeps.0.lock().unwrap(),
        vec![Duration::from_millis(500); 3]
    );
    // Rows: one per track of each fingerprinted album; none for the others.
    for (album, n) in [(SECOND_LIGHT, 4), (FOLDER_ART, 2), (PORCHLIGHT, 3)] {
        let rows = w.rows(album);
        assert_eq!(rows.len(), n, "{album}");
        assert!(
            rows.iter()
                .all(|r| r.acoustid_lookup_state.as_deref() == Some(LOOKUP_DONE))
        );
    }
    assert!(w.rows(SORTED).is_empty());
    assert!(w.rows(LEGACY).is_empty());
    assert_eq!(rig.fpr.calls(), 7, "only the two candidates' files");
    assert_eq!(rig.stub.asked_count() - before, 7);

    // Nothing left but the album that cannot be read.
    let s = rig.pass(&w);
    assert_eq!(counts(&s), (PoolStatus::Done, 1, 0, 1));
    assert_eq!(rig.fpr.calls(), 7);
}

/// Across passes a failed lookup makes its album a candidate again only
/// once the row's retry time has come (the pass's start time is the "now"
/// it is compared with), and each pass asks it at most once.
#[test]
fn the_pool_revisits_failed_lookups_only_when_their_retry_is_due() {
    let w = World::new(&[FOLDER_ART], &[], &[]);
    let p = prints(&w, FOLDER_ART);
    let files = flac_names(&w.root.join(FOLDER_ART));
    let rig = PoolRig::new();
    rig.stub.set(&p[&files[0]].0, ok(&[(0.95, &[REC_A])]));
    rig.stub.set(&p[&files[1]].0, Answer::Status(503));
    let bad = &p[&files[1]].0;
    let asked_bad = || rig.stub.asked().iter().filter(|f| *f == bad).count();

    // Pass 1 at T0: first failure, due again at once.
    assert_eq!(counts(&rig.pass(&w)), (PoolStatus::Done, 1, 1, 0));
    assert_eq!(asked_bad(), 1);
    // Pass 2, same instant: due, asked once more, now waits 1 h.
    assert_eq!(counts(&rig.pass(&w)), (PoolStatus::Done, 1, 1, 0));
    assert_eq!(asked_bad(), 2);
    assert_eq!(w.row(FOLDER_ART, &files[1]).acoustid_lookup_failures, 2);
    // Pass 3, one second before the retry time: not a candidate.
    rig.set_now(T0 + HOUR - 1);
    assert_eq!(counts(&rig.pass(&w)), (PoolStatus::Done, 0, 0, 0));
    assert_eq!(asked_bad(), 2);
    // Pass 4 at the retry time: asked, and this time it completes.
    rig.stub.set(bad, ok(&[(0.9, &[REC_B])]));
    rig.set_now(T0 + HOUR);
    assert_eq!(counts(&rig.pass(&w)), (PoolStatus::Done, 1, 1, 0));
    assert_eq!(asked_bad(), 3);
    assert_eq!(
        state(&w.row(FOLDER_ART, &files[1])),
        (Some(REC_B), Some(0.9), Some(LOOKUP_DONE), 0, None)
    );
    // Pass 5: nothing due.
    assert_eq!(counts(&rig.pass(&w)), (PoolStatus::Done, 0, 0, 0));
    // The good file was asked once; the files were fingerprinted once.
    assert_eq!(
        rig.stub
            .asked()
            .iter()
            .filter(|f| **f == p[&files[0]].0)
            .count(),
        1
    );
    assert_eq!(rig.fpr.calls(), 2);
}

/// Without a key, an album fingerprinted without lookups is not walked
/// again (its rows have no outcome, but nothing could be asked). Once a key
/// is set, those rows are due and the album is a candidate: the pass looks
/// them up without fingerprinting again.
#[test]
fn rows_without_a_lookup_become_candidates_once_a_key_is_set() {
    let w = World::new(&[PORCHLIGHT], &[], &[]);
    let rig = PoolRig::new();
    *rig.key.lock().unwrap() = None;
    assert_eq!(counts(&rig.pass(&w)), (PoolStatus::Done, 1, 1, 0));
    assert_eq!(rig.fpr.calls(), 3);
    assert_eq!(rig.stub.asked_count(), 0);
    assert!(
        w.rows(PORCHLIGHT)
            .iter()
            .all(|r| r.acoustid_lookup_state.is_none())
    );

    assert_eq!(counts(&rig.pass(&w)), (PoolStatus::Done, 0, 0, 0));

    *rig.key.lock().unwrap() = Some("synthetic-test-key".into());
    assert_eq!(counts(&rig.pass(&w)), (PoolStatus::Done, 1, 1, 0));
    assert_eq!(rig.fpr.calls(), 3, "looked up from the stored prints");
    assert_eq!(rig.stub.asked_count(), 3);
    assert!(
        w.rows(PORCHLIGHT)
            .iter()
            .all(|r| r.acoustid_lookup_state.as_deref() == Some(LOOKUP_DONE))
    );
}

// ---------------------------------------------------------------------------
// Rejected files, unreadable directories, a fingerprinter that cannot run

const BAD: &str = "03 - Bad.flac";

/// Remembered rejects of an album, by filename: (filename, implementation).
fn rejects(w: &World, album: &str) -> Vec<(String, String)> {
    musaic_db::fingerprints_ext::get_rejects(&w.conn(), &w.id(album))
        .unwrap()
        .into_iter()
        .map(|r| (r.filename, r.fingerprint_impl))
        .collect()
}

fn calls_for(fpr: &CountingChromaprint, name: &str) -> usize {
    fpr.calls
        .lock()
        .unwrap()
        .iter()
        .filter(|c| *c == name)
        .count()
}

fn row_names(w: &World, album: &str) -> Vec<String> {
    w.rows(album).into_iter().map(|r| r.filename).collect()
}

/// A file the fingerprinter rejects (here a stand-in that is not FLAC at
/// all, which the real Chromaprint refuses) is skipped and remembered with
/// its signature and the implementation. While it is unchanged, the cache
/// comparison leaves it out and a non-forced call does not try it again.
/// Rewritten (new signature), it is tried again; once it fingerprints it
/// loses its reject row and gains a row. The cache rule compares file
/// names only, so breaking it again goes unseen until a forced call, which
/// tries every file: rejected again, its old row is dropped and it is
/// remembered again. A reject remembered by another implementation does
/// not count: that file is tried again.
#[test]
fn a_rejected_file_is_remembered_until_it_changes() {
    let w = World::new(&[FOLDER_ART], &[], &[]);
    let dir = w.root.join(FOLDER_ART);
    let bad = dir.join(BAD);
    fs::write(&bad, b"not audio at all").unwrap();
    let good = flac_names(&musaic_testdata::synthetic_library().join(FOLDER_ART));
    let stub = AcoustIdStub::default();
    let fpr = CountingChromaprint::default();
    let (bus, log) = recording_bus();
    let id = w.id(FOLDER_ART);
    let rust = FINGERPRINT_IMPL_RUST.to_owned();

    run(&w, FOLDER_ART, &fpr, &stub, true, &bus, &at(T0), false);
    assert_eq!(calls_for(&fpr, BAD), 1);
    assert_eq!(row_names(&w, FOLDER_ART), good);
    assert_eq!(rejects(&w, FOLDER_ART), [(BAD.to_owned(), rust.clone())]);
    assert_eq!(announced(&log), [(id.clone(), good.clone())]);

    // Unchanged: left out of the comparison, so the rows are a cache hit.
    let out = run(&w, FOLDER_ART, &fpr, &stub, true, &bus, &at(T0), false);
    assert!(out.from_cache);
    assert_eq!(
        calls_for(&fpr, BAD),
        1,
        "an unchanged reject is not tried again"
    );

    // Rewritten as a real FLAC (from another synthetic album): tried again,
    // stored, and no longer remembered.
    let donor = musaic_testdata::synthetic_library()
        .join(PORCHLIGHT)
        .join("01 - Screen Door.flac");
    fs::copy(&donor, &bad).unwrap();
    let out = run(&w, FOLDER_ART, &fpr, &stub, true, &bus, &at(T0), false);
    assert!(!out.from_cache);
    assert_eq!(calls_for(&fpr, BAD), 2);
    let mut all = good.clone();
    all.push(BAD.to_owned());
    assert_eq!(row_names(&w, FOLDER_ART), all);
    assert!(rejects(&w, FOLDER_ART).is_empty());

    // Broken again: the rows still match the file names, so a plain call
    // is a cache hit and does not notice.
    fs::write(&bad, b"broken again, and longer than before").unwrap();
    assert!(run(&w, FOLDER_ART, &fpr, &stub, true, &bus, &at(T0), false).from_cache);
    assert_eq!(calls_for(&fpr, BAD), 2);

    // Forced: tried, rejected, its old row dropped, remembered again.
    run(&w, FOLDER_ART, &fpr, &stub, true, &bus, &at(T0), true);
    assert_eq!(calls_for(&fpr, BAD), 3);
    assert_eq!(row_names(&w, FOLDER_ART), good);
    assert_eq!(rejects(&w, FOLDER_ART), [(BAD.to_owned(), rust.clone())]);
    // A forced call tries a remembered, unchanged reject too.
    run(&w, FOLDER_ART, &fpr, &stub, true, &bus, &at(T0), true);
    assert_eq!(calls_for(&fpr, BAD), 4);
    // Unforced again: a cache hit.
    assert!(run(&w, FOLDER_ART, &fpr, &stub, true, &bus, &at(T0), false).from_cache);
    assert_eq!(calls_for(&fpr, BAD), 4);

    // Remembered by another implementation: does not count.
    w.conn()
        .execute(
            "UPDATE fingerprint_rejects SET fingerprint_impl = 'fpcalc'",
            [],
        )
        .unwrap();
    run(&w, FOLDER_ART, &fpr, &stub, true, &bus, &at(T0), false);
    assert_eq!(
        calls_for(&fpr, BAD),
        5,
        "another fingerprinter's reject is tried again"
    );
    assert_eq!(rejects(&w, FOLDER_ART), [(BAD.to_owned(), rust)]);
    assert_eq!(row_names(&w, FOLDER_ART), good);
}

/// Restores a directory's permissions when dropped (even on a failed assert).
#[cfg(unix)]
struct Unlock(PathBuf);

#[cfg(unix)]
impl Drop for Unlock {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o755));
    }
}

/// Rows are deleted only for files a working listing no longer shows: a
/// directory that cannot be listed deletes nothing, forced or not.
#[cfg(unix)]
#[test]
fn an_unlistable_directory_deletes_no_rows() {
    use std::os::unix::fs::PermissionsExt;
    let w = World::new(&[FOLDER_ART], &[], &[]);
    let dir = w.root.join(FOLDER_ART);
    let stub = AcoustIdStub::default();
    let fpr = CountingChromaprint::default();
    let (bus, _log) = recording_bus();
    run(&w, FOLDER_ART, &fpr, &stub, true, &bus, &at(T0), false);
    let before = w.rows(FOLDER_ART);
    assert_eq!(before.len(), 2);

    fs::set_permissions(&dir, fs::Permissions::from_mode(0o000)).unwrap();
    let _unlock = Unlock(dir.clone());
    if fs::read_dir(&dir).is_ok() {
        // Running with privileges that list it anyway (root): nothing to test.
        println!("unlistable directory not reproducible here (running as root?)");
        return;
    }
    for force in [false, true] {
        run(
            &w,
            FOLDER_ART,
            &fpr,
            &stub,
            true,
            &bus,
            &at(T0 + HOUR),
            force,
        );
        assert_eq!(w.rows(FOLDER_ART), before, "force={force}");
    }
    assert!(rejects(&w, FOLDER_ART).is_empty());
}

/// A fingerprinter whose refusals are not about the file (one that cannot
/// run at all) remembers no reject and keeps the rows the files already
/// have; with nothing fingerprinted the call is "no-fingerprints".
#[test]
fn a_refusing_fingerprinter_keeps_rows_and_remembers_nothing() {
    struct Refuse;
    impl Fingerprinter for Refuse {
        fn fingerprint(&self, _p: &Path) -> Result<(String, u64), String> {
            Err("cannot run".into())
        }
        fn implementation(&self) -> &str {
            FINGERPRINT_IMPL_RUST
        }
    }
    let w = World::new(&[FOLDER_ART], &[], &[]);
    let stub = AcoustIdStub::default();
    let (bus, log) = recording_bus();
    run(
        &w,
        FOLDER_ART,
        &CountingChromaprint::default(),
        &stub,
        true,
        &bus,
        &at(T0),
        false,
    );
    let before = w.rows(FOLDER_ART);
    assert_eq!(before.len(), 2);
    announced(&log);

    let conn = w.conn();
    let env = FingerprintEnv {
        conn: &conn,
        fingerprinter: &Refuse,
        acoustid: None,
        bus: &bus,
        now_iso: &at(T0 + HOUR),
        cancel: None,
    };
    let err = fingerprint_album(&env, &w.id(FOLDER_ART), &w.path(FOLDER_ART), true).unwrap_err();
    assert_eq!(err.code(), "no-fingerprints");
    assert_eq!(w.rows(FOLDER_ART), before);
    assert!(rejects(&w, FOLDER_ART).is_empty());
    assert!(announced(&log).is_empty());
}

// ---------------------------------------------------------------------------
// Cancelled requests

/// [`run`] that hands back the result.
fn try_run(
    w: &World,
    album: &str,
    fpr: &CountingChromaprint,
    stub: &AcoustIdStub,
    bus: &Bus,
    now: &str,
) -> Result<
    musaic_services::fingerprint::AlbumFingerprints,
    musaic_services::fingerprint::FingerprintError,
> {
    let conn = w.conn();
    let client = AcoustIdClient::new(stub, "synthetic-test-key");
    let env = FingerprintEnv {
        conn: &conn,
        fingerprinter: fpr,
        acoustid: Some(&client as &dyn AcoustIdLookup),
        bus,
        now_iso: now,
        cancel: None,
    };
    fingerprint_album(&env, &w.id(album), &w.path(album), false)
}

/// A lookup cancelled at shutdown is not a failed lookup: the album's run
/// ends as cancelled, the file keeps its fingerprint but records no outcome
/// (no failure counted, no retry delay), nothing further is asked, and
/// nothing is logged above DEBUG. The next run asks again.
#[test]
fn a_cancelled_lookup_is_not_recorded_as_a_failed_one() {
    use musaic_services::fingerprint::FingerprintError;
    let w = World::new(&[FOLDER_ART], &[], &[]);
    let p = prints(&w, FOLDER_ART);
    let files = flac_names(&w.root.join(FOLDER_ART));
    let (first, second) = (&files[0], &files[1]);
    let stub = AcoustIdStub::default();
    stub.set(&p[first].0, Answer::Cancelled);
    let fpr = CountingChromaprint::default();
    let (bus, _log) = recording_bus();

    // First computation: the fingerprint is kept, no lookup outcome.
    let (got, lines) = musaic_log::capture(|| try_run(&w, FOLDER_ART, &fpr, &stub, &bus, &at(T0)));
    assert!(
        matches!(got, Err(FingerprintError::Cancelled { stored: 1 })),
        "{got:?}"
    );
    assert_eq!(stub.asked_count(), 1, "nothing is asked after a cancel");
    assert_eq!(
        state(&w.row(FOLDER_ART, first)),
        (None, None, None, 0, None)
    );
    let loud: Vec<_> = lines
        .iter()
        .filter(|l| l.level != musaic_log::Level::Debug)
        .collect();
    assert!(loud.is_empty(), "{loud:?}");

    // Asked again, it completes.
    stub.set(&p[first].0, ok(&[(0.95, &[REC_A])]));
    stub.set(&p[second].0, ok(&[(0.95, &[REC_B])]));
    try_run(&w, FOLDER_ART, &fpr, &stub, &bus, &at(T0)).unwrap();
    assert_eq!(
        state(&w.row(FOLDER_ART, first)),
        (Some(REC_A), Some(0.95), Some(LOOKUP_DONE), 0, None)
    );

    // A stored print whose earlier lookup failed: a cancelled retry leaves
    // the row (failure count and retry time) exactly as it was.
    stub.set(&p[second].0, Answer::Down);
    run(&w, FOLDER_ART, &fpr, &stub, true, &bus, &at(T0), true);
    let before = w.row(FOLDER_ART, second);
    assert_eq!(before.acoustid_lookup_failures, 1);
    stub.set(&p[second].0, Answer::Cancelled);
    let (got, lines) = musaic_log::capture(|| try_run(&w, FOLDER_ART, &fpr, &stub, &bus, &at(T0)));
    assert!(
        matches!(got, Err(FingerprintError::Cancelled { stored: 0 })),
        "{got:?}"
    );
    assert_eq!(state(&w.row(FOLDER_ART, second)), state(&before));
    let loud: Vec<_> = lines
        .iter()
        .filter(|l| l.level != musaic_log::Level::Debug)
        .collect();
    assert!(loud.is_empty(), "{loud:?}");
}
