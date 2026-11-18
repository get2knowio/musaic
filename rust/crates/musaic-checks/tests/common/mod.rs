//! Shared harness for the golden tests: rebuilds the setup the goldens were
//! recorded with in a temp dir, a counter clock identical to
//! the generator's, a resolver fed from recorded resolutions, and the fake
//! checks the generator registers (probe / probe-not-scan / boom / nested).
#![allow(dead_code)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use musaic_checks::base::Emit;
use musaic_checks::{
    Check, CheckCtx, CheckEnv, CheckError, CheckMeta, CheckScope, Clock, Cost, Finding, FixKind,
    Registry, Resolution, ResolutionSource, Severity, run_post_scan_checks,
};
use musaic_db::Db;
use musaic_db::findings::{FindingRow, RunRow};
use rusqlite::Connection;
use serde_json::{Value as J, json};

pub fn golden(name: &str) -> J {
    let p = musaic_testdata::public("golden/checks").join(name);
    serde_json::from_str(
        &std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display())),
    )
    .unwrap()
}

/// A temp dir whose path has no symlinks (so `Path.resolve()`-style
/// resolution in the post-scan sweep leaves it alone).
pub struct Sandbox {
    _tmp: tempfile::TempDir,
    pub root: PathBuf,
    pub lib: PathBuf,
    pub lib_s: String,
    pub db_path: PathBuf,
}

impl Sandbox {
    /// A copy of testdata/db/v16-empty.db (migrated to v17 on open).
    pub fn new() -> (Sandbox, Db) {
        let tmp = tempfile::tempdir().unwrap();
        let root = musaic_testdata::canonical(tmp.path());
        let db_path = root.join("library.db");
        std::fs::copy(musaic_testdata::public("db/v16-empty.db"), &db_path).unwrap();
        let (db, _) = Db::open(&db_path).unwrap();
        let lib = root.join("lib");
        let lib_s = lib.to_string_lossy().into_owned();
        (
            Sandbox {
                _tmp: tmp,
                root,
                lib,
                lib_s,
                db_path,
            },
            db,
        )
    }

    pub fn sub(&self, s: &str) -> String {
        s.replace("<LIB>", &self.lib_s)
    }

    pub fn unsub(&self, s: &str) -> String {
        musaic_testdata::replace_root(s, &self.lib_s, "<LIB>")
    }

    pub fn open(&self) -> Connection {
        let c = Connection::open(&self.db_path).unwrap();
        c.pragma_update(None, "foreign_keys", "ON").unwrap();
        c
    }
}

/// Mirror of the generator's `flac_bytes`: STREAMINFO + VORBIS_COMMENT, no audio.
pub fn flac_bytes(tags: &[(String, String)], seconds: u64) -> Vec<u8> {
    let total = 44100 * seconds;
    let mut si = Vec::new();
    si.extend_from_slice(&4096u16.to_be_bytes());
    si.extend_from_slice(&4096u16.to_be_bytes());
    si.extend_from_slice(&[0; 6]);
    let packed: u64 = (44100u64 << 44) | (1 << 41) | (15 << 36) | total;
    si.extend_from_slice(&packed.to_be_bytes());
    si.extend_from_slice(&[0; 16]);
    let vendor = b"fsops-golden";
    let mut vc = Vec::new();
    vc.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    vc.extend_from_slice(vendor);
    vc.extend_from_slice(&(tags.len() as u32).to_le_bytes());
    for (k, v) in tags {
        let e = format!("{k}={v}");
        vc.extend_from_slice(&(e.len() as u32).to_le_bytes());
        vc.extend_from_slice(e.as_bytes());
    }
    let mut out = b"fLaC".to_vec();
    out.push(0);
    out.extend_from_slice(&(si.len() as u32).to_be_bytes()[1..]);
    out.extend_from_slice(&si);
    out.push(0x84);
    out.extend_from_slice(&(vc.len() as u32).to_be_bytes()[1..]);
    out.extend_from_slice(&vc);
    out
}

pub fn tags_of(v: &J) -> Vec<(String, String)> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|kv| {
            (
                kv[0].as_str().unwrap().to_string(),
                kv[1].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

fn to_sql(v: &J) -> rusqlite::types::Value {
    match v {
        J::Null => rusqlite::types::Value::Null,
        J::Bool(b) => i64::from(*b).into(),
        J::Number(n) => match n.as_i64() {
            Some(i) => i.into(),
            None => n.as_f64().unwrap().into(),
        },
        J::String(s) => s.clone().into(),
        other => other.to_string().into(),
    }
}

/// The generator's `setup(case, lib)`: files, dirs, libraries, albums,
/// fingerprints, ai_jobs, settings, raw SQL. Column order is the golden's
/// (alphabetical after JSON export; order does not matter for INSERT).
pub fn setup(sb: &Sandbox, conn: &Connection, case: &J) {
    std::fs::create_dir_all(&sb.lib).unwrap();
    for f in case["files"].as_array().into_iter().flatten() {
        let p = sb
            .lib
            .join(f["dir"].as_str().unwrap())
            .join(f["name"].as_str().unwrap());
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(
            &p,
            flac_bytes(&tags_of(&f["tags"]), f["seconds"].as_u64().unwrap_or(3)),
        )
        .unwrap();
    }
    for d in case["dirs"].as_array().into_iter().flatten() {
        std::fs::create_dir_all(sb.lib.join(d.as_str().unwrap())).unwrap();
    }
    let albums: Vec<&J> = case["albums"].as_array().into_iter().flatten().collect();
    let mut libs: Vec<String> = vec!["<LIB>".into()];
    for a in &albums {
        let lp = a["library_path"].as_str().unwrap_or("<LIB>").to_string();
        if !libs.contains(&lp) {
            libs.push(lp);
        }
    }
    libs.sort();
    for lp in libs {
        conn.execute(
            "INSERT INTO libraries(path, scanned_at) VALUES (?1, '')",
            [sb.sub(&lp)],
        )
        .unwrap();
    }
    for a in albums {
        let o = a.as_object().unwrap();
        let cols: Vec<&String> = o.keys().collect();
        let vals: Vec<rusqlite::types::Value> = cols
            .iter()
            .map(|c| match &o[*c] {
                J::String(s) => sb.sub(s).into(),
                other => to_sql(other),
            })
            .collect();
        conn.execute(
            &format!(
                "INSERT INTO albums ({}) VALUES ({})",
                cols.iter()
                    .map(|c| c.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
                vec!["?"; cols.len()].join(", ")
            ),
            rusqlite::params_from_iter(vals),
        )
        .unwrap();
    }
    for f in case["fingerprints"].as_array().into_iter().flatten() {
        conn.execute(
            "INSERT INTO track_fingerprints (album_id, filename, fingerprint, duration_s, \
             acoustid_recording_mbid, acoustid_score, last_computed_at) VALUES (?1,?2,?3,?4,?5,0.9,'2026-01-01T00:00:00Z')",
            rusqlite::params![
                f["album_id"].as_str(),
                f["filename"].as_str(),
                f["fingerprint"].as_str(),
                f["duration_s"].as_f64(),
                f["acoustid_recording_mbid"].as_str()
            ],
        )
        .unwrap();
    }
    for j in case["ai_jobs"].as_array().into_iter().flatten() {
        conn.execute(
            &format!(
                "INSERT INTO ai_jobs (id, album_id, album_path, album_title, job_type, status, result_json, \
                 created_at) VALUES (?1,?2,'','',?3,?4,?5, strftime('%Y-%m-%dT%H:%M:%SZ', 'now', '{}'))",
                j["age"].as_str().unwrap()
            ),
            rusqlite::params![
                j["id"].as_str(),
                j["album_id"].as_str(),
                j["job_type"].as_str(),
                j["status"].as_str(),
                j["result_json"].as_str()
            ],
        )
        .unwrap();
    }
    for (k, v) in case["settings"].as_object().into_iter().flatten() {
        conn.execute(
            "INSERT INTO settings(key, value) VALUES (?1, ?2)",
            [k, v.as_str().unwrap()],
        )
        .unwrap();
    }
    for sql in case["sql"].as_array().into_iter().flatten() {
        conn.execute(sql.as_str().unwrap(), []).unwrap();
    }
}

/// The generator's counter clock: 2026-01-01T00:00:00Z + 1 s per call.
#[derive(Default)]
pub struct CounterClock(AtomicU64);

impl Clock for CounterClock {
    fn now_iso(&self) -> String {
        let n = self.0.fetch_add(1, Ordering::SeqCst);
        format!(
            "2026-01-01T{:02}:{:02}:{:02}Z",
            n / 3600,
            (n % 3600) / 60,
            n % 60
        )
    }
    fn now(&self) -> SystemTime {
        SystemTime::now()
    }
}

/// Resolutions recorded in the golden (`AlbumResolution.to_dict()`); an
/// album without one is an error, as the generator's stub raises.
pub struct RecordedResolver(pub HashMap<String, Resolution>);

impl RecordedResolver {
    pub fn from_case(case: &J) -> Self {
        RecordedResolver(
            case["resolutions"]
                .as_object()
                .into_iter()
                .flatten()
                .map(|(k, v)| (k.clone(), Resolution::from_python_json(v).unwrap()))
                .collect(),
        )
    }
}

impl ResolutionSource for RecordedResolver {
    fn resolve(&self, _conn: &Connection, album_id: &str) -> Result<Resolution, String> {
        self.0
            .get(album_id)
            .cloned()
            .ok_or_else(|| format!("no recorded resolution for {album_id}"))
    }
}

// ---------- fake checks (mirrors of the generator's) ----------

pub type ProbeLog = Arc<Mutex<Vec<J>>>;

macro_rules! meta {
    ($id:literal, $label:literal, $cost:expr, $rds:literal) => {
        CheckMeta {
            id: $id,
            label: $label,
            description: "",
            category: "metadata",
            cost: $cost,
            runs_during_scan: $rds,
            fix_kind: FixKind::None,
            bulk_action_label: None,
        }
    };
}

static PROBE: CheckMeta = meta!("probe", "Probe", Cost::Cheap, true);
static PROBE_NOT_SCAN: CheckMeta = meta!("probe-not-scan", "Probe (not scan)", Cost::Medium, false);
static BOOM: CheckMeta = meta!("boom", "Boom", Cost::Cheap, true);
static NESTED: CheckMeta = meta!("nested", "Nested", Cost::Cheap, true);

pub struct Probe(pub ProbeLog);
impl Check for Probe {
    fn meta(&self) -> &CheckMeta {
        &PROBE
    }
    fn run(
        &self,
        _c: &CheckCtx<'_>,
        scope: &CheckScope,
        _e: &mut Emit<'_>,
    ) -> Result<(), CheckError> {
        self.0.lock().unwrap().push(json!({
            "check": "probe",
            "in_post_scan_rerun": musaic_events::in_post_scan_rerun(),
            "kind": scope.kind.as_str(),
            "album_ids": scope.albums.iter().map(|a| a.id.clone()).collect::<Vec<_>>(),
        }));
        Ok(())
    }
}

pub struct ProbeNotScan(pub ProbeLog);
impl Check for ProbeNotScan {
    fn meta(&self) -> &CheckMeta {
        &PROBE_NOT_SCAN
    }
    fn run(&self, _c: &CheckCtx<'_>, _s: &CheckScope, _e: &mut Emit<'_>) -> Result<(), CheckError> {
        self.0
            .lock()
            .unwrap()
            .push(json!({"check": "probe-not-scan"}));
        Ok(())
    }
}

pub struct Boom(pub ProbeLog);
impl Check for Boom {
    fn meta(&self) -> &CheckMeta {
        &BOOM
    }
    fn run(
        &self,
        _c: &CheckCtx<'_>,
        _s: &CheckScope,
        emit: &mut Emit<'_>,
    ) -> Result<(), CheckError> {
        self.0.lock().unwrap().push(json!({"check": "boom"}));
        emit(Finding {
            album_id: Some("a1".into()),
            summary: "partial before boom".into(),
            severity: Severity::Warn,
            detail: Some(musaic_checks::pyjson::dict([("n", 1i64.into())])),
        })?;
        Err(CheckError::Failed("boom".into()))
    }
}

pub struct Nested(pub ProbeLog, pub Arc<AtomicU64>);
impl Check for Nested {
    fn meta(&self) -> &CheckMeta {
        &NESTED
    }
    fn run(
        &self,
        ctx: &CheckCtx<'_>,
        scope: &CheckScope,
        _e: &mut Emit<'_>,
    ) -> Result<(), CheckError> {
        self.0.lock().unwrap().push(json!({
            "check": "nested",
            "in_post_scan_rerun": musaic_events::in_post_scan_rerun(),
        }));
        if self.1.fetch_add(1, Ordering::SeqCst) == 0 {
            run_post_scan_checks(ctx.conn, ctx.env, &scope.library_path, None);
        }
        self.1.fetch_sub(1, Ordering::SeqCst);
        self.0
            .lock()
            .unwrap()
            .push(json!({"check": "nested-returned"}));
        Ok(())
    }
}

/// A registry from the golden's id list (default checks + fakes).
pub fn registry(ids: Option<&J>, log: &ProbeLog) -> Registry {
    let Some(ids) = ids.and_then(J::as_array) else {
        return Registry::default();
    };
    let base = Registry::default();
    let depth = Arc::new(AtomicU64::new(0));
    Registry::new(
        ids.iter()
            .map(|id| -> Arc<dyn Check> {
                match id.as_str().unwrap() {
                    "probe" => Arc::new(Probe(log.clone())),
                    "probe-not-scan" => Arc::new(ProbeNotScan(log.clone())),
                    "boom" => Arc::new(Boom(log.clone())),
                    "nested" => Arc::new(Nested(log.clone(), depth.clone())),
                    other => base.get(other).unwrap().clone(),
                }
            })
            .collect(),
    )
}

pub fn env(registry: Registry, resolver: Arc<dyn ResolutionSource>) -> CheckEnv {
    CheckEnv {
        registry,
        resolver,
        clock: Arc::new(CounterClock::default()),
        notify: Arc::new(|| {}),
        publish: Arc::new(|_| {}),
        conversion_logs: None,
    }
}

// ---------- row dumps in the generator's shape ----------

pub fn finding_json(sb: &Sandbox, f: &FindingRow) -> J {
    json!({
        "id": f.id,
        "check_id": f.check_id,
        "album_id": f.album_id,
        "severity": f.severity,
        "summary": f.summary,
        "detail_json": f.detail_json.as_deref().map(|d| sb.unsub(d)),
        "first_seen_at": f.first_seen_at,
        "last_seen_at": f.last_seen_at,
        "resolved_at": f.resolved_at,
        "ignored_at": f.ignored_at,
        "ignored_reason": f.ignored_reason,
    })
}

pub fn findings_json(sb: &Sandbox, conn: &Connection) -> J {
    J::Array(
        musaic_db::findings::all_findings(conn)
            .unwrap()
            .iter()
            .map(|f| finding_json(sb, f))
            .collect(),
    )
}

pub fn run_json(sb: &Sandbox, r: &RunRow) -> J {
    json!({
        "id": r.id,
        "check_id": r.check_id,
        "started_at": r.started_at,
        "completed_at": r.completed_at,
        "status": r.status,
        "library_path": sb.unsub(&r.library_path),
        "scope": r.scope,
        "issue_count": r.issue_count,
        "error": r.error,
    })
}

pub fn runs_json(sb: &Sandbox, conn: &Connection) -> J {
    J::Array(
        musaic_db::findings::all_runs(conn)
            .unwrap()
            .iter()
            .map(|r| run_json(sb, r))
            .collect(),
    )
}

pub fn str_ids(v: &J) -> Option<Vec<String>> {
    v.as_array()
        .map(|a| a.iter().map(|x| x.as_str().unwrap().to_string()).collect())
}
