//! The background check sweep (`musaic_services::check_sweep`) with stub
//! checks over a small database: what a pass runs, that an unchanged
//! library runs nothing, that a changed album reruns alone, that Missing
//! albums are never checked, that it waits while the app is busy, that a
//! suspension (a restore) interrupts a network wait without recording
//! anything from it, and that shutdown is prompt.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use musaic_checks::base::{CheckCtx, CheckEnv, CheckMeta, Cost, FixKind, SystemClock};
use musaic_checks::{
    Check, CheckError, CheckScope, Emit, Finding, Granularity, Registry, Resolution,
    ResolutionSource, Severity,
};
use musaic_events::Event;
use musaic_services::check_sweep::{CheckSweeper, SweepDeps, SweepStatus};
use musaic_services::host_log::HostLog;
use musaic_services::production::StoppableResolution;
use rusqlite::Connection;

const fn meta(id: &'static str) -> CheckMeta {
    CheckMeta {
        id,
        label: id,
        description: "",
        category: "metadata",
        cost: Cost::Cheap,
        runs_during_scan: false,
        fix_kind: FixKind::None,
        bulk_action_label: None,
    }
}

static PER_ALBUM: CheckMeta = meta("filename-mismatch");
static WHOLE: CheckMeta = meta("whole-library");
static NET: CheckMeta = meta("tags-out-of-sync");

type Seen = Arc<Mutex<Vec<(&'static str, Vec<String>)>>>;

/// Records the albums of each run; emits one finding per album.
struct Recording {
    meta: &'static CheckMeta,
    granularity: Granularity,
    seen: Seen,
}

impl Check for Recording {
    fn meta(&self) -> &CheckMeta {
        self.meta
    }
    fn granularity(&self) -> Granularity {
        self.granularity
    }
    fn run(
        &self,
        _ctx: &CheckCtx<'_>,
        scope: &CheckScope,
        emit: &mut Emit<'_>,
    ) -> Result<(), CheckError> {
        let ids: Vec<String> = scope.albums.iter().map(|a| a.id.clone()).collect();
        self.seen.lock().unwrap().push((self.meta.id, ids.clone()));
        for id in ids {
            emit(Finding {
                summary: format!("{} finding", self.meta.id),
                album_id: Some(id),
                severity: Severity::Info,
                detail: None,
            })?;
        }
        Ok(())
    }
}

/// A network check: asks the resolver for every album, as tags-out-of-sync
/// does, and refuses to record anything once the resolver was stopped.
struct Network {
    seen: Seen,
}

impl Check for Network {
    fn meta(&self) -> &CheckMeta {
        &NET
    }
    fn uses_network(&self) -> bool {
        true
    }
    fn run(
        &self,
        ctx: &CheckCtx<'_>,
        scope: &CheckScope,
        emit: &mut Emit<'_>,
    ) -> Result<(), CheckError> {
        let ids: Vec<String> = scope.albums.iter().map(|a| a.id.clone()).collect();
        self.seen.lock().unwrap().push((NET.id, ids.clone()));
        for id in ids {
            let r = ctx.env.resolver.resolve(ctx.conn, &id);
            if ctx.env.resolver.interrupted() {
                return Err(CheckError::Interrupted);
            }
            if r.is_err() {
                emit(Finding {
                    summary: "no resolution".into(),
                    album_id: Some(id),
                    severity: Severity::Info,
                    detail: None,
                })?;
            }
        }
        Ok(())
    }
}

/// A resolver that blocks (as a background request waiting for a quiet
/// slot) until released or stopped.
struct Gate {
    release: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    waiting: Arc<AtomicBool>,
}

impl ResolutionSource for Gate {
    fn resolve(&self, _conn: &Connection, _album_id: &str) -> Result<Resolution, String> {
        self.waiting.store(true, Ordering::SeqCst);
        while !self.release.load(Ordering::SeqCst) {
            if self.stop.load(Ordering::SeqCst) {
                self.waiting.store(false, Ordering::SeqCst);
                return Err("stopped".into());
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        self.waiting.store(false, Ordering::SeqCst);
        Ok(Resolution::default())
    }
}

#[derive(Default)]
struct Lines(Mutex<Vec<String>>);

impl HostLog for Lines {
    fn info(&self, _t: &str, m: &str) {
        self.0.lock().unwrap().push(format!("INFO {m}"));
    }
    fn warn(&self, _t: &str, m: &str) {
        self.0.lock().unwrap().push(format!("WARN {m}"));
    }
    fn error(&self, _t: &str, m: &str) {
        self.0.lock().unwrap().push(format!("ERROR {m}"));
    }
}

struct Rig {
    _tmp: tempfile::TempDir,
    db: PathBuf,
    lib: String,
    sweeper: CheckSweeper,
    seen: Seen,
    busy: Arc<AtomicBool>,
    release: Arc<AtomicBool>,
    waiting: Arc<AtomicBool>,
}

const ALBUMS: [&str; 3] = ["a1", "a2", "a3"];

fn conn(p: &Path) -> Connection {
    musaic_db::open_connection(p).unwrap()
}

/// Three Present albums and one Missing one, in a library that was scanned.
fn rig() -> Rig {
    let tmp = tempfile::tempdir().unwrap();
    let libdir = tmp.path().join("lib");
    std::fs::create_dir_all(&libdir).unwrap();
    let lib = musaic_scan::py_resolve(&libdir)
        .to_string_lossy()
        .into_owned();
    let db = tmp.path().join("library.db");
    musaic_db::Db::open_or_create(&db).unwrap();
    let c = conn(&db);
    c.execute(
        "INSERT INTO libraries(path, scanned_at) VALUES (?1, '2001-01-01T00:00:00Z')",
        [&lib],
    )
    .unwrap();
    for id in ALBUMS {
        c.execute(
            "INSERT INTO albums(id, library_path, path, dir_name, track_count) \
             VALUES (?1, ?2, ?3, ?1, 2)",
            rusqlite::params![id, lib, format!("{lib}/{id}")],
        )
        .unwrap();
    }
    c.execute(
        "INSERT INTO albums(id, library_path, path, dir_name, track_count, missing_since) \
         VALUES ('gone', ?1, ?2, 'gone', 2, '2001-01-02T00:00:00Z')",
        rusqlite::params![lib, format!("{lib}/gone")],
    )
    .unwrap();
    let seen: Seen = Arc::default();
    let sweeper = CheckSweeper::new();
    let release = Arc::new(AtomicBool::new(true));
    let waiting = Arc::new(AtomicBool::new(false));
    let registry = Registry::new(vec![
        // Registry order puts the network check first; the sweep runs it last.
        Arc::new(Network { seen: seen.clone() }),
        Arc::new(Recording {
            meta: &PER_ALBUM,
            granularity: Granularity::Album,
            seen: seen.clone(),
        }),
        Arc::new(Recording {
            meta: &WHOLE,
            granularity: Granularity::Library,
            seen: seen.clone(),
        }),
    ]);
    let env = CheckEnv {
        registry,
        resolver: Arc::new(StoppableResolution {
            inner: Arc::new(Gate {
                release: release.clone(),
                stop: sweeper.stop_flag(),
                waiting: waiting.clone(),
            }),
            stop: sweeper.stop_flag(),
        }),
        clock: Arc::new(SystemClock),
        notify: Arc::new(|| {}),
    };
    let busy = Arc::new(AtomicBool::new(false));
    let b2 = busy.clone();
    let db2 = db.clone();
    let lib2 = lib.clone();
    sweeper.start(SweepDeps {
        open_db: Arc::new(move || musaic_db::open_connection(&db2).map_err(|e| e.to_string())),
        library_dir: Arc::new(move || Some(lib2.clone())),
        env,
        busy: Arc::new(move || b2.load(Ordering::SeqCst)),
        now: Arc::new(SystemTime::now),
        log: Arc::new(Lines::default()),
        settle: Duration::from_millis(10),
    });
    Rig {
        _tmp: tmp,
        db,
        lib,
        sweeper,
        seen,
        busy,
        release,
        waiting,
    }
}

fn wait_for(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Wait for the sweep to finish what it has to do.
fn settle(r: &Rig) {
    // A wake is pending or a pass runs; then a short quiet period.
    let mut quiet_since = Instant::now();
    wait_for("the sweep to go idle", || {
        if r.sweeper.is_busy() {
            quiet_since = Instant::now();
        }
        quiet_since.elapsed() > Duration::from_millis(100)
    });
}

fn runs(r: &Rig) -> Vec<(String, String, String)> {
    let c = conn(&r.db);
    let mut s = c
        .prepare("SELECT check_id, scope, status FROM check_runs ORDER BY id")
        .unwrap();
    s.query_map([], |x| Ok((x.get(0)?, x.get(1)?, x.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn inputs(r: &Rig) -> Vec<(String, String)> {
    let c = conn(&r.db);
    musaic_db::check_inputs::rows_for_library(&c, &r.lib)
        .unwrap()
        .into_iter()
        .map(|x| (x.check_id, x.album_id))
        .collect()
}

fn ids(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

// A first pass runs every check on every Present album (local checks
// first, the network one last), records what each ran against, and a pass
// with nothing changed runs nothing. A Missing album is never checked.
#[test]
fn first_pass_runs_everything_then_nothing_is_stale() {
    let r = rig();
    settle(&r);
    let seen = r.seen.lock().unwrap().clone();
    assert_eq!(
        seen,
        vec![
            ("filename-mismatch", ids(&ALBUMS)),
            ("whole-library", ids(&ALBUMS)),
            ("tags-out-of-sync", ids(&ALBUMS)),
        ]
    );
    let mut want: Vec<(String, String)> = Vec::new();
    for a in ALBUMS {
        want.push(("filename-mismatch".into(), a.into()));
        want.push(("tags-out-of-sync".into(), a.into()));
    }
    want.push(("whole-library".into(), String::new()));
    want.sort();
    assert_eq!(inputs(&r), want);
    assert!(runs(&r).iter().all(|(_, _, s)| s == "completed"));
    assert_eq!(r.sweeper.state().status, SweepStatus::Idle);
    let st = r.sweeper.state();
    assert_eq!((st.total, st.completed), (7, 7));

    // Nothing changed: a wake plans nothing.
    let before = runs(&r).len();
    r.sweeper.wake();
    settle(&r);
    assert_eq!(runs(&r).len(), before);
    assert!(
        !r.seen
            .lock()
            .unwrap()
            .iter()
            .any(|(_, a)| a.contains(&"gone".to_string()))
    );
    assert!(r.sweeper.stop_within(Duration::from_secs(5)));
}

// An album whose row changed is rerun alone by the per-album checks;
// a whole-library check reruns library-wide.
#[test]
fn a_changed_album_reruns_alone() {
    let r = rig();
    settle(&r);
    r.seen.lock().unwrap().clear();
    conn(&r.db)
        .execute("UPDATE albums SET title = 'New' WHERE id = 'a2'", [])
        .unwrap();
    r.sweeper.on_event(&Event::AlbumTagsChanged {
        album_id: "a2".into(),
        fields: vec!["title".into()],
    });
    settle(&r);
    assert_eq!(
        r.seen.lock().unwrap().clone(),
        vec![
            ("filename-mismatch", ids(&["a2"])),
            ("whole-library", ids(&ALBUMS)),
            ("tags-out-of-sync", ids(&["a2"])),
        ]
    );
    // A setting a check depends on makes it stale everywhere.
    r.seen.lock().unwrap().clear();
    r.sweeper.on_event(&Event::SettingsChanged {
        keys: vec!["dir_template_single_artist".into()],
    });
    settle(&r);
    assert_eq!(
        r.seen.lock().unwrap().clone(),
        vec![("filename-mismatch", ids(&ALBUMS))]
    );
    // A setting no check depends on changes nothing.
    r.seen.lock().unwrap().clear();
    r.sweeper.on_event(&Event::SettingsChanged {
        keys: vec!["snapshot_retention_count".into()],
    });
    settle(&r);
    assert!(r.seen.lock().unwrap().is_empty());
    assert!(r.sweeper.stop_within(Duration::from_secs(5)));
}

// While the app is busy (a scan, other work, a restore) the sweep waits
// before it even plans, without showing progress (the work it waits for
// shows its own); it goes on once the app is free.
#[test]
fn waits_while_the_app_is_busy() {
    let r = rig();
    settle(&r);
    r.seen.lock().unwrap().clear();
    r.busy.store(true, Ordering::SeqCst);
    let n = runs(&r).len();
    r.sweeper.on_event(&Event::SettingsChanged {
        keys: vec!["filename_template_single_artist".into()],
    });
    wait_for("a pass to start", || r.sweeper.is_busy());
    std::thread::sleep(Duration::from_millis(200));
    assert!(r.seen.lock().unwrap().is_empty(), "ran while busy");
    assert_eq!(runs(&r).len(), n);
    assert_eq!(r.sweeper.state().status, SweepStatus::Idle);
    r.busy.store(false, Ordering::SeqCst);
    settle(&r);
    assert_eq!(
        r.seen.lock().unwrap().clone(),
        vec![("filename-mismatch", ids(&ALBUMS))]
    );
    assert!(r.sweeper.stop_within(Duration::from_secs(5)));
}

// A suspension (what a restore does) stops a network wait at once: the
// interrupted run is failed, records no finding and no inputs for its
// albums, no batch starts while suspended, and the sweep finishes the work
// once resumed.
#[test]
fn a_suspension_interrupts_a_network_wait_and_records_nothing_from_it() {
    let r = rig();
    settle(&r);
    r.release.store(false, Ordering::SeqCst);
    conn(&r.db)
        .execute(
            "DELETE FROM check_inputs WHERE check_id = 'tags-out-of-sync'",
            [],
        )
        .unwrap();
    let findings_before: i64 = conn(&r.db)
        .query_row(
            "SELECT COUNT(*) FROM check_findings WHERE check_id = 'tags-out-of-sync'",
            [],
            |x| x.get(0),
        )
        .unwrap();
    r.sweeper.wake();
    wait_for("the network wait", || r.waiting.load(Ordering::SeqCst));
    let t0 = Instant::now();
    let held = r.sweeper.suspend();
    assert!(t0.elapsed() < Duration::from_secs(5), "suspend was slow");
    assert!(!r.waiting.load(Ordering::SeqCst));
    let last = runs(&r).pop().unwrap();
    assert_eq!(
        (last.0.as_str(), last.2.as_str()),
        ("tags-out-of-sync", "failed")
    );
    let c = conn(&r.db);
    let err: String = c
        .query_row(
            "SELECT error FROM check_runs ORDER BY id DESC LIMIT 1",
            [],
            |x| x.get(0),
        )
        .unwrap();
    assert_eq!(err, "check run interrupted");
    assert!(
        !inputs(&r).iter().any(|(c, _)| c == "tags-out-of-sync"),
        "inputs recorded from an interrupted run"
    );
    let findings_after: i64 = c
        .query_row(
            "SELECT COUNT(*) FROM check_findings WHERE check_id = 'tags-out-of-sync' \
             AND resolved_at IS NULL",
            [],
            |x| x.get(0),
        )
        .unwrap();
    assert_eq!(findings_after, findings_before, "findings changed");
    // Suspended: nothing starts.
    let n = runs(&r).len();
    r.sweeper.wake();
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(runs(&r).len(), n, "a batch ran while suspended");
    // Resumed: the work is done.
    r.release.store(true, Ordering::SeqCst);
    drop(held);
    settle(&r);
    let mut got: Vec<String> = inputs(&r)
        .into_iter()
        .filter(|(c, _)| c == "tags-out-of-sync")
        .map(|(_, a)| a)
        .collect();
    got.sort();
    assert_eq!(got, ids(&ALBUMS));
    assert!(r.sweeper.stop_within(Duration::from_secs(5)));
}

// Shutdown stops a sweep waiting on the network promptly.
#[test]
fn shutdown_is_prompt_during_a_network_wait() {
    let r = rig();
    settle(&r);
    r.release.store(false, Ordering::SeqCst);
    conn(&r.db).execute("DELETE FROM check_inputs", []).unwrap();
    r.sweeper.wake();
    wait_for("the network wait", || r.waiting.load(Ordering::SeqCst));
    let t0 = Instant::now();
    assert!(r.sweeper.stop_within(Duration::from_secs(5)));
    assert!(t0.elapsed() < Duration::from_secs(2), "{:?}", t0.elapsed());
    // Stopped for good: a wake does nothing.
    let n = runs(&r).len();
    r.sweeper.wake();
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(runs(&r).len(), n);
}
