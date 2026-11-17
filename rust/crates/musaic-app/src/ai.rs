//! The AI job queue's host side: the queue state
//! [`AiQueue`] kept in the [`crate::App`] (the change signal, one watch
//! channel the jobs stream connections subscribe to, the provider, the
//! dispatcher), and [`AppEnv`], the [`WorkerEnv`] that gives
//! `musaic_ai`'s worker this app's database, album data, resolver, network
//! and bus.
//!
//! Choices, each reported as a candidate unknown:
//! * The provider is [`musaic_ai::Unconfigured`] unless the builder is
//!   given another (tests: the replay provider). No network provider exists
//!   in this build.
//! * The jobs stream: one watch receiver per connection (as for the pool
//!   stream), so no connection and not the worker can consume
//!   another's wake-up; the Python app shares one `asyncio.Event` that each of
//!   them `clear()`s.
//! * The worker starts with the lifespan: by default whenever the bus
//!   subscribers are registered (`AppBuilder::subscribers`), so a
//!   state built "without lifespan" (the Python app's TestClient runs) has no
//!   worker, as in the Python app. `AppBuilder::ai_worker` overrides it.
//! * A job runs holding the restore gate's work guard.
//! * The cover for the prompt comes from the thumbnail cache, generating
//!   (and keeping) the thumbnail when missing; the Python app builds it in
//!   memory without writing it.
//! * The MusicBrainz cover fallback stores the anchor its name search finds
//!   only after the track-count / duration check (a deliberate change);
//!   the Python app's `find_cover` stored
//!   every confident hit.
//! * Job state changes are logged: one thread follows the change
//!   signal and logs each job's transitions (queued, running, completed,
//!   failed with its message, deleted), the Python app's worker INFO /
//!   WARNING lines.

use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use musaic_ai::provider::ImagePart;
use musaic_ai::{Provider, Py, RunOptions, Signal, Worker, WorkerEnv};
use musaic_events::Event;
use musaic_resolve::AlbumResolution;
use musaic_resolve::find_release::{self, FindOptions};
use musaic_resolve::source::AlbumSource;
use musaic_services::sources::{DbAlbums, NetMusicBrainz};
use rusqlite::Connection;
use tokio::sync::watch;

use crate::app::{App, AppInner, open_conn};
use crate::logging::Level;

/// Makes new job ids (`secrets.token_hex(8)`: 16 hex characters).
pub type JobIds = Arc<dyn Fn() -> String + Send + Sync>;

/// 16 random hex characters (std's randomly seeded hasher over a counter,
/// the clock and the thread: unique, not secret).
pub fn random_job_id() -> String {
    use std::hash::{BuildHasher, Hasher};
    static N: AtomicU64 = AtomicU64::new(0);
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u64(N.fetch_add(1, Ordering::Relaxed));
    h.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    );
    h.write(format!("{:?}", std::thread::current().id()).as_bytes());
    format!("{:016x}", h.finish())
}

/// The queue's shared state.
pub struct AiQueue {
    pub signal: Arc<Signal>,
    /// Bumped on every [`Signal::notify`]; one receiver per stream
    /// connection.
    pub changes: watch::Sender<u64>,
    pub provider: Arc<dyn Provider>,
    pub opts: RunOptions,
    pub ids: JobIds,
    worker: Mutex<Option<Worker>>,
}

impl AiQueue {
    pub fn new(provider: Arc<dyn Provider>, ids: JobIds, opts: RunOptions) -> AiQueue {
        let signal = Signal::new();
        let (changes, _) = watch::channel(0u64);
        let tx = changes.clone();
        signal.add_listener(Arc::new(move || {
            tx.send_modify(|n| *n = n.wrapping_add(1));
        }));
        AiQueue {
            signal,
            changes,
            provider,
            opts,
            ids,
            worker: Mutex::new(None),
        }
    }

    /// `notify_jobs_changed()`.
    pub fn notify(&self) {
        self.signal.notify();
    }

    /// Start the dispatcher (the lifespan's `worker_loop()`).
    pub(crate) fn start(&self, inner: Weak<AppInner>) {
        let env: Arc<dyn WorkerEnv> = Arc::new(AppEnv { inner });
        let w = Worker::start(env, self.provider.clone(), self.signal.clone(), self.opts);
        *self.worker.lock().unwrap_or_else(|e| e.into_inner()) = Some(w);
    }

    /// Stop the dispatcher, if running.
    pub fn stop(&self) {
        let w = self.worker.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(w) = w {
            w.stop();
        }
    }

    pub fn worker_running(&self) -> bool {
        self.worker
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
    }
}

/// The logger name of the worker's lines (`musaic.services.worker`).
pub const WORKER_TARGET: &str = "musaic.services.worker";

/// The level the Python app's worker / ai.py logs each of musaic-ai's lines at
/// (services/worker.py, ai.py:1457).
pub fn worker_level(line: &str) -> Level {
    let warn = [
        "AI queue paused",
        "AI MBID verify failed",
        "MB fallback also failed",
    ];
    if warn.iter().any(|p| line.starts_with(p)) {
        Level::Warning
    } else if line.contains("failed") || line.starts_with("Couldn't") || line.contains("cannot ") {
        Level::Error
    } else {
        Level::Info
    }
}

/// Follow the change signal and log every job's transitions.
/// The thread ends once the app is gone.
pub(crate) fn start_job_log(state: &App) {
    let weak = Arc::downgrade(&state.0);
    let signal = state.0.ai.signal.clone();
    let _ = std::thread::Builder::new()
        .name("job-log".into())
        .spawn(move || {
            // id -> (status, job_type, album_id) of the jobs not yet terminal.
            let mut active: HashMap<String, (String, String, String)> = HashMap::new();
            let mut first = true;
            let mut seen = signal.value();
            loop {
                {
                    let Some(inner) = weak.upgrade() else {
                        return;
                    };
                    if let Some(next) = job_transitions(&inner, &active, first) {
                        active = next;
                        first = false;
                    }
                }
                signal.wait_since(seen, Duration::from_secs(1));
                seen = signal.value();
            }
        });
}

/// One pass: the active jobs now, logging what changed since `before`
/// (nothing on the first pass). `None` when the database cannot be read.
fn job_transitions(
    inner: &AppInner,
    before: &HashMap<String, (String, String, String)>,
    first: bool,
) -> Option<HashMap<String, (String, String, String)>> {
    let _g = inner.db_gate.read().unwrap_or_else(|e| e.into_inner());
    let conn = open_conn(&inner.db_path).ok()?;
    let mut now: HashMap<String, (String, String, String)> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    {
        let mut q = conn
            .prepare(
                "SELECT id, status, job_type, album_id FROM ai_jobs \
                 WHERE status IN ('queued', 'running') ORDER BY created_at, id",
            )
            .ok()?;
        let rows = q
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    (
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                    ),
                ))
            })
            .ok()?;
        for (id, v) in rows.flatten() {
            order.push(id.clone());
            now.insert(id, v);
        }
    }
    if first {
        return Some(now);
    }
    let log = &inner.log;
    let name = |id: &str, t: &str, a: &str| format!("job {id} ({t}, album={a})");
    for id in &order {
        let (status, t, a) = &now[id];
        match before.get(id) {
            Some((s, ..)) if s == status => {}
            _ => log.info(WORKER_TARGET, &format!("{} {status}", name(id, t, a))),
        }
    }
    let mut gone: Vec<&String> = before.keys().filter(|id| !now.contains_key(*id)).collect();
    gone.sort();
    for id in gone {
        let (_, t, a) = &before[id];
        let row: Option<(String, Option<String>)> = conn
            .query_row(
                "SELECT status, error FROM ai_jobs WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok();
        match row {
            None => log.info(WORKER_TARGET, &format!("{} deleted", name(id, t, a))),
            Some((s, Some(err))) if s == "failed" => {
                log.warn(WORKER_TARGET, &format!("{} failed: {err}", name(id, t, a)))
            }
            Some((s, _)) => log.info(WORKER_TARGET, &format!("{} {s}", name(id, t, a))),
        }
    }
    Some(now)
}

/// The worker's view of this app.
pub struct AppEnv {
    pub(crate) inner: Weak<AppInner>,
}

impl AppEnv {
    pub fn new(state: &App) -> AppEnv {
        AppEnv {
            inner: Arc::downgrade(&state.0),
        }
    }

    fn st(&self) -> Option<App> {
        self.inner.upgrade().map(App)
    }
}

impl WorkerEnv for AppEnv {
    fn open_db(&self) -> Result<Connection, String> {
        let st = self.st().ok_or("the app has stopped")?;
        open_conn(st.db_path()).map_err(|e| e.to_string())
    }

    fn now_iso(&self) -> String {
        self.st().map(|s| s.now_iso()).unwrap_or_default()
    }

    fn notify(&self) {
        if let Some(st) = self.st() {
            st.0.ai.notify();
        }
    }

    fn log(&self, line: &str) {
        if let Some(st) = self.st() {
            st.0.log.log(worker_level(line), WORKER_TARGET, line);
        }
    }

    fn album(&self, conn: &Connection, album_id: &str) -> Option<Py> {
        let st = self.st()?;
        let lib = st.library_dir();
        musaic_services::sources::cached_album_logged(conn, lib.as_deref(), album_id)
            .map(|row| Py::from(&row.json))
    }

    fn read_tracks(&self, album_path: &str) -> Py {
        Py::from(&musaic_services::library_ops::read_tracks(album_path))
    }

    fn resolve(
        &self,
        conn: &Connection,
        album_id: &str,
        hint: &str,
    ) -> Result<Arc<AlbumResolution>, String> {
        let st = self.st().ok_or("the app has stopped")?;
        let lib = st.library_dir();
        st.parts()
            .resolve(conn, lib.as_deref(), album_id, hint, false)
    }

    fn cover_for_prompt(&self, album: &Py) -> Option<ImagePart> {
        let st = self.st()?;
        let id = album.get("id").and_then(Py::as_str)?.to_string();
        let path = album
            .get("path")
            .and_then(Py::as_str)
            .unwrap_or("")
            .to_string();
        let thumbs = st.thumbs_dir();
        let thumb = thumbs.join(format!("{id}.jpg"));
        let file = if thumb.is_file() {
            thumb
        } else {
            musaic_services::covers::ensure_thumb(&thumbs, &id, &path, &album.to_json(), false)?
        };
        let bytes = match std::fs::read(&file) {
            Ok(b) => b,
            Err(e) => {
                // ai.py:700; `e` is io::Error's text. The Python app went on to
                // build the cover from the source image in memory; here the
                // prompt goes without one.
                musaic_log::warning(
                    "musaic.ai",
                    &format!("ai cover-attach: thumb read failed for {id}: {e}"),
                );
                return None;
            }
        };
        Some(ImagePart {
            bytes,
            media_type: "image/jpeg".into(),
        })
    }

    fn firecrawl_key(&self) -> Option<String> {
        let st = self.st()?;
        musaic_secrets::secret(st.0.secrets.as_ref(), "firecrawl_api_key")
            .ok()
            .filter(|k| !k.is_empty())
    }

    fn verify_cover_url(&self, url: &str) -> (bool, String) {
        let Some(st) = self.st() else {
            return (false, "the app has stopped".into());
        };
        match musaic_net::CoverArtClient::new(st.0.transport.clone()).fetch(url) {
            Ok(_) => (true, "ok".into()),
            Err(e) => (false, e.to_string()),
        }
    }

    fn find_cover(&self, conn: &Connection, album: &Py, hint: &str) -> Result<Py, String> {
        let st = self.st().ok_or("the app has stopped")?;
        let lib = st.library_dir();
        let id = album.get("id").and_then(Py::as_str).unwrap_or("");
        let rec = DbAlbums {
            conn,
            library_dir: lib.as_deref(),
        }
        .album(id);
        let m = match rec {
            // The same name search as the check fixes,
            // saving a confident hit as the anchor only after the track-count /
            // duration check (the Python app's `find_release` saved every one).
            Some(rec) => musaic_services::sources::find_release_for_fix(
                conn,
                &st.0.bus,
                &rec,
                FindOptions {
                    hint,
                    blocked: &BTreeSet::new(),
                    fetch_cover: true,
                    fetch_tracklist: false,
                },
                &NetMusicBrainz::new(st.0.transport.clone()),
                &st.now_iso(),
            ),
            None => find_release::MbMatch::default(),
        };
        Ok(Py::dict([
            ("cover_url", Py::opt_str(m.cover_url)),
            ("source", Py::str(m.source)),
            ("notes", Py::str(m.notes)),
            ("confidence", Py::str(m.confidence)),
        ]))
    }

    fn verify_release_group(&self, mbid: &str) -> Result<u16, String> {
        let st = self.st().ok_or("the app has stopped")?;
        let url = format!("https://musicbrainz.org/ws/2/release-group/{mbid}");
        let req = musaic_net::Request::from_url(musaic_net::Method::Get, &url)
            .ok_or_else(|| format!("bad url {url}"))?
            .query("fmt", "json")
            .header("User-Agent", musaic_net::USER_AGENT)
            .timeout(Duration::from_secs(10));
        st.0.transport
            .send(&req)
            .map(|r| r.status)
            .map_err(|e| e.to_string())
    }

    fn anchor_set(&self, album_id: &str) {
        if let Some(st) = self.st() {
            st.0.bus.publish(&Event::AlbumMbAnchorSet {
                album_id: album_id.to_string(),
                source: "ai".into(),
            });
        }
    }

    fn begin_work(&self) -> Option<Box<dyn Send>> {
        let st = self.st()?;
        st.0.gate
            .begin(musaic_services::gate::WorkKind::AiJob)
            .ok()
            .map(|g| Box::new(g) as Box<dyn Send>)
    }

    fn alive(&self) -> bool {
        self.inner.strong_count() > 0
    }
}

impl App {
    /// Run the oldest queued AI job now, on this thread (blocking; holding
    /// the restore gate's work guard), as the dispatcher would. `None` when
    /// nothing is queued or a restore runs. Tests drive the queue with it
    /// where the Python app's recordings ran `run_one_job` by hand.
    pub fn ai_run_next(&self) -> Option<String> {
        let env = AppEnv::new(self);
        musaic_ai::run_next(&env, &self.0.ai.provider, &self.0.ai.opts)
    }

    /// `notify_jobs_changed()`: wakes the dispatcher and every jobs stream.
    pub fn ai_jobs_changed(&self) {
        self.0.ai.notify();
    }

    /// Whether the AI dispatcher runs (see `AppBuilder::ai_worker`).
    pub fn ai_worker_running(&self) -> bool {
        self.0.ai.worker_running()
    }
}
