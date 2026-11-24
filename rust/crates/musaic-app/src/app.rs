//! The application object and its builder: the config dir, the database, the
//! secrets store, the event bus with its subscribers and SSE relay, the scan
//! service, the checks environment, the resolver and the fingerprint pool.
//! The Python app keeps all of this in module globals and builds it in
//! `main._lifespan`. No HTTP here: musaic-api wraps an [`App`] (with its
//! own Host / Origin, body-limit, SPA and SSE settings) and a non-HTTP face
//! can build and drive one without it.
//!
//! The fingerprint pool and the restore gate: pool work never
//! takes a [`musaic_services::gate::WorkGuard`]. A pass can run for hours; holding the gate for it
//! would refuse every restore until it ends. Instead a restore, once it
//! holds the [`musaic_services::gate::RestoreGuard`], stops the pool
//! ([`FingerprintPool::stop`] returns after the pool thread has exited,
//! between two files) before it touches the database, and the pool is not
//! restarted afterwards (the Python app restarts it only after a scan). A new
//! pass can only start from a scan, which needs a
//! [`musaic_services::gate::WorkGuard`] and is
//! therefore refused while the restore runs.
//!
//! The check sweep (`musaic_services::check_sweep`) is the same: it takes
//! no work guard (a quit does not ask about it, a restore is not refused
//! because of it); a restore suspends it before touching the database, and
//! shutdown stops it first. It is off unless the host asks for it
//! ([`AppBuilder::check_sweep`]; both binaries do), so a test app's check
//! runs are only the ones the test makes.
//!
//! The pool stream: one listener on the pool's change
//! event bumps a [`watch`] counter; every stream connection holds its own
//! receiver, so each connection is woken once per change it has not sent
//! yet, no connection can consume another's wake-up, and a new connection
//! starts with every earlier change already seen (no second first frame).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use musaic_checks::{CheckEnv, ChecksSideEffects, Registry, ResolutionSource};
use musaic_core::config::ConfigDir;
use musaic_db::Db;
use musaic_events::{Bus, Relay};
use musaic_resolve::Resolver;
use musaic_scan::TrackOrder;
use musaic_services::analyze::{Analyzer, DspAnalyzer};
use musaic_services::check_fixes::FixSources;
use musaic_services::check_sweep::{CheckSweeper, SweepDeps};
use musaic_services::fingerprint_pool::{FingerprintPool, PoolDeps, PoolSleeper, RealSleeper};
use musaic_services::startup;
use musaic_services::{ScanDeps, ScanService};
use rusqlite::Connection;
use tokio::sync::watch;

use musaic_services::gate::{DbGate, WorkGate};
use musaic_services::keyscrub::{KeyScrub, Logger, Retry, ScrubOutcome};
use musaic_services::production::{
    ChecksResolution, Fingerprinting, Network, Parts, PartsFactory, SharedTransport,
    StoppableResolution,
};
use musaic_services::thumb_pool::ThumbPool;

use crate::logging::{AppLog, LogSink, MaskingStore, Masks};

/// The log target of the shutdown lines.
const SHUTDOWN_TARGET: &str = "musaic.main";

/// Test hook run by a restore while it holds the restore gate.
pub type RestoreHook = Arc<dyn Fn() + Send + Sync>;

/// A secrets store shared across threads.
pub type SharedSecrets = musaic_services::startup::SharedSecrets;
/// Wall clock (tests script it).
pub type WallClock = musaic_services::startup::WallClock;

/// Called between two quick-fix-all findings. The Python
/// app's fixed one-second pause (`await asyncio.sleep(1.0)`,
/// musaic/api/health.py:650-653) is dropped and the MusicBrainz limiter
/// paces the requests, so the default does nothing. Kept only as a test
/// seam (the check-fix tests hold a run between two findings with it).
pub type Pause = Arc<dyn Fn() + Send + Sync>;

/// The analyzer `POST /api/albums/{id}/analyze/stream` runs.
pub type SharedAnalyzer = Arc<dyn Analyzer + Send + Sync>;

#[derive(Debug, thiserror::Error)]
pub enum StartupError {
    #[error("config dir {0}: {1}")]
    ConfigDir(PathBuf, std::io::Error),
    #[error("database: {0}")]
    Db(#[from] musaic_db::Error),
    #[error(transparent)]
    PreUpgradeCopy(#[from] musaic_services::startup::PreUpgradeCopyFailed),
}

/// Builds an [`App`].
pub struct AppBuilder {
    config_dir: PathBuf,
    secrets: Option<SharedSecrets>,
    agent_secrets: Option<SharedSecrets>,
    agent_dev_credentials: bool,
    network: Network,
    clock: WallClock,
    subscribers: bool,
    auto_snapshot: bool,
    track_order: TrackOrder,
    home: Option<PathBuf>,
    on_scan_album: Option<musaic_services::scan::AlbumHook>,
    on_restore: Option<RestoreHook>,
    fingerprinting: Fingerprinting,
    pool_sleeper: Arc<dyn PoolSleeper>,
    quick_fix_pause: Pause,
    fix_sources: Option<Arc<dyn FixSources + Send + Sync>>,
    checks_resolution: Option<Arc<dyn ResolutionSource>>,
    analyzer: SharedAnalyzer,
    log_sink: LogSink,
    log_masks: Masks,
    key_scrub_delays: Vec<Duration>,
    // Research request ids.
    ai_job_ids: Option<crate::ai::JobIds>,
    shutdown_pool_wait: Duration,
    check_sweep: bool,
    sweep_settle: Duration,
    sweep_network: Option<bool>,
    sweep_refresh_tick: Duration,
}

/// How long [`App::shutdown`] waits for the fingerprint pool's thread to
/// exit after cancelling it (it stops between two files; one file rarely
/// takes more than a few seconds).
pub const SHUTDOWN_POOL_WAIT: Duration = Duration::from_secs(15);
/// How long [`App::shutdown`] waits for a running scan to end after
/// cancelling it (it stops after the album it is reading, then waits for
/// the thumbnails already queued).
pub const SHUTDOWN_SCAN_WAIT: Duration = Duration::from_secs(15);
/// How long [`App::shutdown`] waits for the check sweep's thread (it stops
/// its network request at once and ends after the batch of local checks in
/// progress).
pub const SHUTDOWN_SWEEP_WAIT: Duration = Duration::from_secs(5);
/// How long [`App::shutdown`] waits for the threads of agent operations.
pub const SHUTDOWN_AGENT_WAIT: Duration = Duration::from_secs(5);
/// How long the final WAL checkpoint on shutdown waits for any connection
/// still open (a scan or check rerun ending at the same moment).
pub const SHUTDOWN_CHECKPOINT_WAIT: Duration = Duration::from_secs(5);

/// What [`App::shutdown`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShutdownReport {
    /// No scan was running at the end of the wait (a running one was
    /// cancelled).
    pub scan_stopped: bool,
    /// The fingerprint pool's thread exited within the wait (or none ran).
    pub pool_stopped: bool,
    /// The check sweep's thread exited within the wait (or none ran).
    pub sweep_stopped: bool,
    /// The final WAL checkpoint, or why it could not run.
    pub checkpoint: Result<musaic_db::CloseCheckpoint, String>,
}

impl AppBuilder {
    /// Defaults: in-memory secrets, network offline, system clock, bus
    /// subscribers registered, startup auto-snapshot on,
    /// (DISCNUMBER, TRACKNUMBER) track order, in-process
    /// Chromaprint, the real pool pause (0.5 s), no pause between
    /// quick-fix-all findings, the real analyzers, stderr log.
    pub fn new(config_dir: impl Into<PathBuf>) -> Self {
        AppBuilder {
            config_dir: config_dir.into(),
            secrets: None,
            agent_secrets: None,
            agent_dev_credentials: false,
            network: Network::Offline,
            clock: Arc::new(SystemTime::now),
            subscribers: true,
            auto_snapshot: true,
            track_order: TrackOrder::DiscTrack,
            home: musaic_core::config::home_dir(),
            on_scan_album: None,
            on_restore: None,
            fingerprinting: Fingerprinting::chromaprint(),
            pool_sleeper: Arc::new(RealSleeper),
            quick_fix_pause: Arc::new(|| {}),
            fix_sources: None,
            checks_resolution: None,
            analyzer: Arc::new(DspAnalyzer),
            log_sink: AppLog::stderr_sink(),
            log_masks: Masks::new(),
            key_scrub_delays: musaic_services::keyscrub::RETRY_DELAYS.to_vec(),
            ai_job_ids: None,
            shutdown_pool_wait: SHUTDOWN_POOL_WAIT,
            check_sweep: false,
            sweep_settle: musaic_services::check_sweep::SETTLE,
            sweep_network: None,
            sweep_refresh_tick: musaic_services::check_sweep::REFRESH_TICK,
        }
    }

    /// Whether the sweep may run checks that use the network. Default:
    /// unless the network is [`Network::Offline`]; a host whose offline
    /// mode is another transport (a logged one) says so here.
    pub fn sweep_network(mut self, on: bool) -> Self {
        self.sweep_network = Some(on);
        self
    }

    /// How often the sweep looks again without an event (default
    /// [`musaic_services::check_sweep::REFRESH_TICK`]; tests shorten it).
    pub fn sweep_refresh_tick(mut self, d: Duration) -> Self {
        self.sweep_refresh_tick = d;
        self
    }

    /// Run the background check sweep (`musaic_services::check_sweep`):
    /// every check reruns on its own where its inputs changed. Off by
    /// default, so a test's check runs are its own; the binaries turn it on.
    /// Needs the bus subscribers ([`Self::subscribers`]) to hear of changes.
    pub fn check_sweep(mut self, on: bool) -> Self {
        self.check_sweep = on;
        self
    }

    /// How long the sweep waits after the last event before a pass
    /// (default [`musaic_services::check_sweep::SETTLE`]; tests shorten it).
    pub fn sweep_settle(mut self, d: Duration) -> Self {
        self.sweep_settle = d;
        self
    }

    /// How long shutdown waits for the fingerprint pool's thread (default
    /// [`SHUTDOWN_POOL_WAIT`]; tests shorten it).
    pub fn shutdown_pool_wait(mut self, d: Duration) -> Self {
        self.shutdown_pool_wait = d;
        self
    }

    /// How new job card ids are made (tests script them). Default: 16 random
    /// hex characters.
    pub fn ai_job_ids(mut self, ids: crate::ai::JobIds) -> Self {
        self.ai_job_ids = Some(ids);
        self
    }

    /// The BPM / loudness measurements of the analyze route (tests stub
    /// them). Default: musaic-bpm and musaic-dsp.
    pub fn analyzer(mut self, a: SharedAnalyzer) -> Self {
        self.analyzer = a;
        self
    }

    /// Where the app's own log lines go, as text lines
    /// `[LEVEL] logger: message` (default: stderr). Secret values are
    /// replaced before a line reaches the sink (`crate::logging`).
    pub fn log(mut self, l: Logger) -> Self {
        self.log_sink = AppLog::line_sink(l);
        self
    }

    /// Where the app's log records go, structured (the binary passes its
    /// `ServerLog`). Secret values are replaced first.
    pub fn log_sink(mut self, s: LogSink) -> Self {
        self.log_sink = s;
        self
    }

    /// The secret values to keep out of the log, shared with the host's
    /// sink. Every value the app's secret store reads or writes is added.
    pub fn log_masks(mut self, m: Masks) -> Self {
        self.log_masks = m;
        self
    }

    /// Pauses before the timed retries of an unfinished secret scrub (the
    /// last repeats; default [`musaic_services::keyscrub::RETRY_DELAYS`]).
    pub fn key_scrub_retry_delays(mut self, d: Vec<Duration>) -> Self {
        self.key_scrub_delays = d;
        self
    }

    /// How tracks are fingerprinted (pool, Tier 0c, fix paths).
    pub fn fingerprinting(mut self, f: Fingerprinting) -> Self {
        self.fingerprinting = f;
        self
    }

    /// The fingerprint pool's pause between albums (tests: no wait).
    pub fn pool_sleeper(mut self, s: Arc<dyn PoolSleeper>) -> Self {
        self.pool_sleeper = s;
        self
    }

    /// What quick-fix-all does between two findings (a test seam;
    /// the default does nothing).
    pub fn quick_fix_pause(mut self, p: Pause) -> Self {
        self.quick_fix_pause = p;
        self
    }

    /// Replace the check fixes' outside world (tests replay the Python app's
    /// stubs). Default: [`musaic_services::production::AppFixSources`].
    pub fn fix_sources(mut self, s: Arc<dyn FixSources + Send + Sync>) -> Self {
        self.fix_sources = Some(s);
        self
    }

    /// Replace the checks' resolution source (tests replay the Python app's
    /// stubs). Default: the resolver over this app's sources.
    pub fn checks_resolution(mut self, r: Arc<dyn ResolutionSource>) -> Self {
        self.checks_resolution = Some(r);
        self
    }

    /// Called by both restore routes while they hold the restore
    /// gate, before the database is touched. Tests use it to hold a restore.
    pub fn on_restore(mut self, hook: RestoreHook) -> Self {
        self.on_restore = Some(hook);
        self
    }

    /// Called after each album of a scan (the Python app submits the thumbnail
    /// there). Tests use it to hold a scan mid-way.
    pub fn on_scan_album(mut self, hook: musaic_services::scan::AlbumHook) -> Self {
        self.on_scan_album = Some(hook);
        self
    }

    pub fn secrets(mut self, s: SharedSecrets) -> Self {
        self.secrets = Some(s);
        self
    }
    /// Where agent pairing secrets are kept: the OS keyring under service
    /// `musaic-agent` in a normal run, memory in a test or a development run.
    pub fn agent_secrets(mut self, s: SharedSecrets) -> Self {
        self.agent_secrets = Some(s);
        self
    }
    /// Also write each new pairing's secret to
    /// `<config dir>/agent-dev-credentials/<pairing id>` (mode 0600), for a
    /// separate CLI process to read. Only for runs whose secrets are in memory
    /// (`--dev-memory-secrets`, `--verification-memory-secrets`), which are
    /// already limited to scratch directories.
    pub fn agent_dev_credentials(mut self, on: bool) -> Self {
        self.agent_dev_credentials = on;
        self
    }
    pub fn network(mut self, n: Network) -> Self {
        self.network = n;
        self
    }
    pub fn clock(mut self, c: WallClock) -> Self {
        self.clock = c;
        self
    }
    /// Register the `library_events` subscribers (the Python app's lifespan).
    pub fn subscribers(mut self, on: bool) -> Self {
        self.subscribers = on;
        self
    }
    /// Take the lifespan's automatic snapshot.
    pub fn auto_snapshot(mut self, on: bool) -> Self {
        self.auto_snapshot = on;
        self
    }
    pub fn track_order(mut self, o: TrackOrder) -> Self {
        self.track_order = o;
        self
    }
    /// Home directory for `~` expansion in `POST /api/config`.
    pub fn home(mut self, h: Option<PathBuf>) -> Self {
        self.home = h;
        self
    }

    pub fn build(self) -> Result<App, StartupError> {
        let config = ConfigDir::new(&self.config_dir);
        std::fs::create_dir_all(config.dir())
            .map_err(|e| StartupError::ConfigDir(config.dir().to_path_buf(), e))?;
        let db_path = config.db_path();
        let log = AppLog::new(self.log_sink.clone(), self.log_masks.clone());
        // Every secret value read or written is kept out of the log.
        let secrets: SharedSecrets = Arc::new(MaskingStore::new(
            self.secrets
                .unwrap_or_else(|| Arc::new(musaic_secrets::MemoryStore::new())),
            self.log_masks.clone(),
        ));
        // The pairing index (ids only, no secret) is written to the same store
        // but not through the masks: a masked index would redact every log
        // line naming the one active pairing, and add a mask per index state.
        let agent_index_store: SharedSecrets = self
            .agent_secrets
            .unwrap_or_else(|| Arc::new(musaic_secrets::MemoryStore::new()));
        let agent_secrets: SharedSecrets = Arc::new(MaskingStore::new(
            agent_index_store.clone(),
            self.log_masks.clone(),
        ));
        let clock = self.clock.clone();
        let key_scrub = Arc::new(KeyScrub::new(log.clone(), self.key_scrub_delays));
        let startup_scrub;
        {
            // A database this build wrote is copied (as a snapshot) before
            // a schema step; no copy, no upgrade. No database yet: created
            // at the current schema (no backup: nothing to go back to).
            match startup::snapshot_before_schema_step(&db_path, &config.snapshot_dir(), clock())? {
                startup::PreUpgradeCopy::Taken(name) => log.info(
                    "musaic.db",
                    &format!("database copied to snapshot {name} before the schema upgrade"),
                ),
                startup::PreUpgradeCopy::Existing(name) => log.info(
                    "musaic.db",
                    &format!(
                        "snapshot {name} already holds the database before the schema upgrade; no second copy"
                    ),
                ),
                startup::PreUpgradeCopy::NotNeeded => {}
            }
            let (db, opened) = Db::open_or_create(&db_path)?;
            startup::log_opened(&log, &db_path, &opened);
            musaic_db::delegated_work::recover_reserved(
                db.connection(),
                crate::desktop_research::millis(clock()),
            )?;
            musaic_db::delegated_work::sync_root(
                db.connection(),
                config.load().library_dir.as_deref(),
                crate::desktop_research::millis(clock()),
            )?;
            // Secret rows leave SQLite on every open. This
            // is the process's only connection at this point, so nothing of
            // ours can block the scrub's checkpoint.
            startup_scrub = key_scrub.run(&db, secrets.as_ref(), "startup");
            // The old provider's keyring entries go once (a marker row
            // records it; a failure is retried at the next start).
            musaic_services::keyscrub::remove_retired_keys(&db, secrets.as_ref(), &log);
            if self.auto_snapshot {
                startup::auto_snapshot_on_startup(&db, &config.snapshot_dir(), &*clock, &log);
            }
        }

        let bus = Bus::new();
        let relay = Relay::new();
        let (checks_tick, _) = watch::channel(0u64);
        let tick = checks_tick.clone();
        let notify: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            tick.send_modify(|n| *n += 1);
        });
        let resolver = Arc::new(Resolver::new());
        let net_cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let transport = self.network.shared_cancellable(net_cancel.clone());
        let parts = startup::parts_factory(
            resolver.clone(),
            transport.clone(),
            secrets.clone(),
            self.fingerprinting.fingerprinter(),
            bus.clone(),
            clock.clone(),
        );
        let cfg2 = config.clone();
        let library_dir: Arc<dyn Fn() -> Option<String> + Send + Sync> =
            Arc::new(move || cfg2.load().library_dir);
        let checks_resolution = self.checks_resolution.clone().unwrap_or_else(|| {
            Arc::new(ChecksResolution {
                parts: parts.clone(),
                library_dir: library_dir.clone(),
            })
        });
        let checks_env = CheckEnv {
            registry: Registry::default(),
            resolver: checks_resolution,
            clock: Arc::new(startup::ChecksClock(clock.clone())),
            notify,
            publish: {
                let bus = bus.clone();
                Arc::new(move |ev| bus.publish(&ev))
            },
            conversion_logs: Some(musaic_services::multi_disc_convert::log_dir(config.dir())),
        };
        let thumbs_dir = config.dir().join("thumbs");
        let gate = WorkGate::new();
        let sweeper = CheckSweeper::new();
        if self.subscribers {
            let db_for_fx = db_path.clone();
            let r2 = resolver.clone();
            let effects = ChecksSideEffects {
                env: checks_env.clone(),
                open: Arc::new(move || open_conn(&db_for_fx).map_err(|e| e.to_string().into())),
                library_dir: library_dir.clone(),
                resolver_invalidate: Arc::new(move |id: &str| {
                    r2.invalidate(Some(id));
                    Ok(())
                }),
                delete_thumb: Arc::new(move |id: &str| {
                    musaic_services::covers::delete_thumb(&thumbs_dir, id);
                    Ok(())
                }),
            };
            // Reruns take the gate; those refused during
            // a restore run once it ends.
            let gated = musaic_services::gated_reruns::install(
                effects,
                gate.clone(),
                Arc::new(log.clone()),
            );
            if self.check_sweep {
                gated.set_sweeper(sweeper.clone());
            }
            musaic_events::register_all(&bus, gated, Some(&relay));
        } else {
            // The relay alone: the SSE stream still sees every event.
            relay.register_on(&bus);
        }

        let pool = FingerprintPool::new();
        let (pool_changes, _) = watch::channel(0u64);
        let tx = pool_changes.clone();
        // The pool's `_notify_changed()` bumps the counter; each
        // stream connection has its own receiver.
        pool.changed().add_listener(Arc::new(move || {
            tx.send_modify(|n| *n = n.wrapping_add(1));
        }));

        let (sweep_changes, _) = watch::channel(0u64);
        let tx = sweep_changes.clone();
        sweeper.changed().add_listener(Arc::new(move || {
            tx.send_modify(|n| *n = n.wrapping_add(1));
        }));
        let sweep_resolution = self.checks_resolution.clone();
        let network = self.network.clone();

        let clock_for_agent = clock.clone();
        let state = App(Arc::new(AppInner {
            config,
            db_path,
            secrets,
            transport,
            parts,
            resolver,
            fingerprinting: self.fingerprinting,
            pool,
            pool_changes,
            pool_sleeper: self.pool_sleeper,
            quick_fix_pause: self.quick_fix_pause,
            fix_sources: self.fix_sources,
            clock,
            bus,
            relay,
            scan: ScanService::new(),
            checks_env,
            checks_tick,
            track_order: self.track_order,
            home: self.home,
            on_scan_album: self.on_scan_album,
            db_gate: DbGate::new(()),
            gate,
            on_restore: self.on_restore,
            analyzer: self.analyzer,
            log,
            pool_logged: std::sync::Mutex::new(None),
            key_scrub,
            recording_canonical: musaic_services::recording_canonical::RecordingCanonicalCache::new(
            ),
            net_cancel,
            shutting_down: watch::channel(false).0,
            shutdown_pool_wait: self.shutdown_pool_wait,
            sweeper,
            sweep_changes,
            agent: crate::agent::AgentHub::new(clock_for_agent),
            agent_secrets,
            agent_index_store,
            agent_dev_credentials: self.agent_dev_credentials,
            running_batches: std::sync::Mutex::default(),
            ai: crate::ai::AiQueue::new(
                self.ai_job_ids
                    .unwrap_or_else(|| Arc::new(crate::ai::random_job_id)),
            ),
        }));
        // Scans leave the albums of an unfinished multi-disc conversion as
        // they are (its resume, from the record, puts them together).
        let logs = musaic_services::multi_disc_convert::log_dir(state.config_dir());
        state.0.scan.set_held(Arc::new(move || {
            musaic_services::multi_disc_convert::held_albums(&logs)
        }));
        // Pool passes in the log (fingerprint_pool.py's INFO lines).
        let weak = Arc::downgrade(&state.0);
        state.0.pool.changed().add_listener(Arc::new(move || {
            if let Some(inner) = weak.upgrade() {
                startup::log_pool(&inner.pool, &inner.pool_logged, &inner.log);
            }
        }));
        if startup_scrub == (ScrubOutcome::Pending { retry: true }) {
            state.schedule_key_scrub_retry();
        }
        crate::desktop_research::hydrate(&state, &musaic_db::open_connection(state.db_path())?)?;
        crate::desktop_research::subscribe_legacy_hydration(&state);
        crate::desktop_research::subscribe_rename_rederive(&state);
        crate::agent::startup_housekeeping(&state);
        crate::agent::start_expiry_ticker(&state);
        if self.check_sweep {
            let online = self
                .sweep_network
                .unwrap_or(!matches!(network, Network::Offline));
            state.start_check_sweep(
                &network,
                sweep_resolution,
                self.sweep_settle,
                online,
                self.sweep_refresh_tick,
            );
        }
        Ok(state)
    }
}

/// A further connection to the library database, configured as the
/// Python app's `db.connect()` (30 s busy timeout, foreign keys on,
/// `synchronous=FULL`), through [`musaic_db::open_connection`].
pub fn open_conn(path: &Path) -> musaic_db::Result<Connection> {
    musaic_db::open_connection(path)
}

fn retry_once(inner: &AppInner) -> Option<ScrubOutcome> {
    musaic_services::keyscrub::retry_once(
        &inner.key_scrub,
        &inner.gate,
        &inner.db_gate,
        &inner.db_path,
        inner.secrets.as_ref(),
    )
}

/// The application's parts. Fields are public so a host (musaic-api's
/// handlers) reads them directly.
pub struct AppInner {
    /// Set when shutdown begins: a request waiting on the network's pacing
    /// or a retry delay ends at once instead of holding shutdown.
    pub net_cancel: Arc<std::sync::atomic::AtomicBool>,
    pub config: ConfigDir,
    pub db_path: PathBuf,
    pub secrets: SharedSecrets,
    /// Every outbound request (resolver, fixes, covers, pool).
    pub transport: SharedTransport,
    /// The resolver's parts with the current AcoustID key.
    pub parts: PartsFactory,
    pub resolver: Arc<Resolver>,
    pub fingerprinting: Fingerprinting,
    pub pool: FingerprintPool,
    /// Bumped on every pool state change; one receiver per stream
    /// connection.
    pub pool_changes: watch::Sender<u64>,
    pub pool_sleeper: Arc<dyn PoolSleeper>,
    pub quick_fix_pause: Pause,
    pub fix_sources: Option<Arc<dyn FixSources + Send + Sync>>,
    pub clock: WallClock,
    pub bus: Bus,
    pub relay: Relay,
    pub scan: ScanService,
    pub checks_env: CheckEnv,
    pub checks_tick: watch::Sender<u64>,
    pub track_order: TrackOrder,
    pub home: Option<PathBuf>,
    pub on_scan_album: Option<musaic_services::scan::AlbumHook>,
    /// Restores replace the database file: they take this for writing,
    /// every other database user for reading
    /// ([`musaic_services::gate::DbGate`]).
    pub db_gate: DbGate,
    /// Restore vs scans / check runs / applies.
    pub gate: Arc<WorkGate>,
    pub on_restore: Option<RestoreHook>,
    pub analyzer: SharedAnalyzer,
    pub log: AppLog,
    /// The fingerprint pool state last logged (status, started_at, total).
    pub pool_logged: startup::PoolLogged,
    pub key_scrub: Arc<KeyScrub>,
    /// `mb_lookup.get_recording_canonical`'s lru_cache: one per server
    /// lifetime (the duplicate-tracks canonical route).
    pub recording_canonical: musaic_services::recording_canonical::RecordingCanonicalCache,
    /// The research requests' change signal, stream changes and job ids.
    pub ai: crate::ai::AiQueue,
    /// Flips to `true` when the server begins shutting down; every
    /// SSE body watches it and ends (a clean end of stream, not a cut
    /// connection).
    pub shutting_down: watch::Sender<bool>,
    /// How long [`App::shutdown`] waits for the fingerprint pool.
    pub shutdown_pool_wait: Duration,
    /// The background check sweep (idle unless started:
    /// [`AppBuilder::check_sweep`]).
    pub sweeper: CheckSweeper,
    /// Bumped on every sweep state change; one receiver per checks stream.
    pub sweep_changes: watch::Sender<u64>,
    /// The agent bridge's per-run state (tokens, lockout, operations).
    pub agent: crate::agent::AgentHub,
    /// Agent pairing secrets (service `musaic-agent`), never the settings keys.
    pub agent_secrets: SharedSecrets,
    /// The same store without the log masks, for the pairing index only (it
    /// holds ids, never a secret).
    pub agent_index_store: SharedSecrets,
    /// Write the dev credential file for each new pairing (scratch runs only).
    pub agent_dev_credentials: bool,
    /// The research batches of the quick-fix-all runs in progress: a batch
    /// with no `finished_at` that no run holds is "from an unfinished run".
    pub running_batches: std::sync::Mutex<std::collections::BTreeSet<String>>,
}

/// The application (cheap to clone).
#[derive(Clone)]
pub struct App(pub Arc<AppInner>);

impl App {
    pub fn builder(config_dir: impl Into<PathBuf>) -> AppBuilder {
        AppBuilder::new(config_dir)
    }

    pub fn config_dir(&self) -> &Path {
        self.0.config.dir()
    }

    pub fn bus(&self) -> &Bus {
        &self.0.bus
    }

    pub fn relay(&self) -> &Relay {
        &self.0.relay
    }

    pub fn scan(&self) -> &ScanService {
        &self.0.scan
    }

    pub fn db_path(&self) -> &Path {
        &self.0.db_path
    }

    /// The app's log.
    pub fn log(&self) -> &AppLog {
        &self.0.log
    }

    /// The restore gate.
    pub fn gate(&self) -> &Arc<WorkGate> {
        &self.0.gate
    }

    pub fn library_dir(&self) -> Option<String> {
        self.0.config.load().library_dir
    }

    pub fn now(&self) -> SystemTime {
        (self.0.clock)()
    }

    pub fn now_iso(&self) -> String {
        musaic_db::backup::iso_seconds(self.now())
    }

    /// The resolver's parts with the AcoustID key as it is now.
    pub fn parts(&self) -> Parts {
        (self.0.parts)()
    }

    /// The fingerprint pool (`services.fingerprint_pool`).
    pub fn pool(&self) -> &FingerprintPool {
        &self.0.pool
    }

    /// The background check sweep.
    pub fn sweeper(&self) -> &CheckSweeper {
        &self.0.sweeper
    }

    /// True while the sweep should not start a batch: a restore, a scan,
    /// or any work holding the work gate (applies, bulk runs, user-started
    /// check runs, short writes).
    pub fn sweep_should_wait(&self) -> bool {
        let (active, restoring) = self.0.gate.snapshot();
        restoring || active > 0 || self.0.scan.is_running()
    }

    /// Start the check sweep: its checks resolve through the background
    /// network path (`network`'s, over this app's transport) and stop with
    /// the sweep; `resolution` replaces the resolver (tests).
    fn start_check_sweep(
        &self,
        network: &Network,
        resolution: Option<Arc<dyn ResolutionSource>>,
        settle: Duration,
        online: bool,
        refresh_tick: Duration,
    ) {
        let inner = &self.0;
        let stop = inner.sweeper.stop_flag();
        let weak = Arc::downgrade(&self.0);
        let busy: Arc<dyn Fn() -> bool + Send + Sync> =
            Arc::new(move || weak.upgrade().is_some_and(|i| App(i).sweep_should_wait()));
        let background = network.background(&inner.transport, busy.clone(), stop.clone());
        let resolution = resolution.unwrap_or_else(|| {
            let cfg = inner.config.clone();
            Arc::new(ChecksResolution {
                parts: startup::parts_factory(
                    inner.resolver.clone(),
                    background,
                    inner.secrets.clone(),
                    inner.fingerprinting.fingerprinter(),
                    inner.bus.clone(),
                    inner.clock.clone(),
                ),
                library_dir: Arc::new(move || cfg.load().library_dir),
            })
        });
        let env = CheckEnv {
            resolver: Arc::new(StoppableResolution {
                inner: resolution,
                stop,
            }),
            ..inner.checks_env.clone()
        };
        let db_path = inner.db_path.clone();
        let cfg = inner.config.clone();
        let clock = inner.clock.clone();
        inner.sweeper.start(SweepDeps {
            open_db: Arc::new(move || open_conn(&db_path).map_err(|e| e.to_string())),
            library_dir: Arc::new(move || cfg.load().library_dir),
            env,
            busy,
            now: Arc::new(move || clock()),
            log: Arc::new(inner.log.clone()),
            settle,
            network: online,
            refresh_tick,
        });
    }

    /// Start shutting down: every open SSE stream ends cleanly
    /// (its body finishes, so the client sees the end of the response and
    /// the server's graceful drain is not held up by `EventSource`s), and
    /// streams opened from now on end at once. Idempotent; does not stop
    /// the work itself ([`Self::shutdown`] does what the lifespan does).
    pub fn begin_shutdown(&self) {
        self.0.shutting_down.send_replace(true);
        // A parked event poll holds its HTTP request open for up to 25 s;
        // woken now it sees the flag and answers, so the server's graceful
        // drain is not held up by it.
        self.0.agent.notify_events();
    }

    /// Whether [`Self::begin_shutdown`] was called.
    pub fn is_shutting_down(&self) -> bool {
        *self.0.shutting_down.borrow()
    }

    /// A receiver for the shutdown flag (SSE bodies watch it).
    pub fn shutdown_signal(&self) -> watch::Receiver<bool> {
        self.0.shutting_down.subscribe()
    }

    /// What is running now: the work gate's labelled guards, a
    /// restore, the scan service and the fingerprint pool.
    pub fn work_snapshot(&self) -> crate::work::WorkSnapshot {
        let (kinds, restoring) = self.0.gate.running();
        let _db = self.0.db_gate.read().unwrap_or_else(|e| e.into_inner());
        crate::work::WorkSnapshot {
            kinds,
            restoring,
            scan_running: self.0.scan.is_running(),
            pool_running: self.0.pool.is_running(),
            external_claims: open_conn(self.db_path())
                .ok()
                .and_then(|conn| {
                    musaic_db::delegated_work::active_external_count(
                        &conn,
                        crate::desktop_research::millis(self.now()),
                    )
                    .ok()
                })
                .unwrap_or_default() as usize,
        }
    }

    /// The lifespan's shutdown, blocking: stop the check sweep (waiting at
    /// most [`SHUTDOWN_SWEEP_WAIT`]), cancel a running scan and wait (at
    /// most [`SHUTDOWN_SCAN_WAIT`]) for it to end, cancel the fingerprint
    /// pool and wait (at most [`AppBuilder::shutdown_pool_wait`]) for its
    /// thread to exit, give an unfinished secret scrub
    /// one more try, then fold the WAL into the main database file and
    /// truncate it ([`musaic_db::checkpoint_on_close`]) so a stop in the
    /// middle of a pass leaves no un-checkpointed WAL behind.
    ///
    /// The scan goes first, through the same cancel as `POST
    /// /api/scan/cancel`: a cancelled scan writes nothing and does not
    /// start the pool, and its connection is closed before the checkpoint.
    /// (Before, shutdown left the scan running: the process could exit in
    /// the middle of the walk, the checkpoint could find the scan's
    /// connection open, and a scan finishing during shutdown restarted the
    /// pool that had just been stopped.)
    pub fn shutdown(&self) -> ShutdownReport {
        self.0
            .net_cancel
            .store(true, std::sync::atomic::Ordering::SeqCst);
        // Parked event polls end now instead of at their wait.
        self.0.agent.notify_events();
        // The sweep first: it stops its request in flight at once, and it
        // must not start a batch after the scan below ends.
        let sweep_stopped = self.0.sweeper.stop_within(SHUTDOWN_SWEEP_WAIT);
        if !sweep_stopped {
            self.0.log.warn(
                SHUTDOWN_TARGET,
                &format!(
                    "check sweep still busy after {} s; closing the database without waiting for it",
                    SHUTDOWN_SWEEP_WAIT.as_secs_f64()
                ),
            );
        }
        // Agent operations stop at their next item boundary; the scan one is
        // cancelled through the scan's own cancel just below.
        if !self.0.agent.stop_workers(SHUTDOWN_AGENT_WAIT) {
            self.0.log.warn(
                SHUTDOWN_TARGET,
                "an agent operation is still finishing; closing without waiting for it",
            );
        }
        let scan_stopped = self.cancel_scan_and_wait(SHUTDOWN_SCAN_WAIT);
        if !scan_stopped {
            self.0.log.warn(
                SHUTDOWN_TARGET,
                &format!(
                    "library scan still stopping after {} s; closing the database without waiting for it",
                    SHUTDOWN_SCAN_WAIT.as_secs_f64()
                ),
            );
        }
        let pool_stopped = self.0.pool.stop_within(self.0.shutdown_pool_wait);
        if !pool_stopped {
            self.0.log.warn(
                SHUTDOWN_TARGET,
                &format!(
                    "fingerprint pool still busy after {} s; closing the database without waiting for it",
                    self.0.shutdown_pool_wait.as_secs_f64()
                ),
            );
        }
        // An unfinished secret scrub gets one more try now that the server
        // has stopped and the pool has exited.
        musaic_services::keyscrub::shutdown_retry(
            &self.0.key_scrub,
            &self.0.db_gate,
            &self.0.db_path,
            self.0.secrets.as_ref(),
        );
        let checkpoint = {
            // A restore swaps the file under the write side of this lock.
            let _db = self
                .0
                .db_gate
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            musaic_db::checkpoint_on_close(&self.0.db_path, SHUTDOWN_CHECKPOINT_WAIT)
                .map_err(|e| e.to_string())
        };
        match &checkpoint {
            Ok(musaic_db::CloseCheckpoint::Done) => self.0.log.log(
                crate::logging::Level::Debug,
                SHUTDOWN_TARGET,
                "database checkpointed and closed",
            ),
            Ok(musaic_db::CloseCheckpoint::Blocked) => self.0.log.warn(
                SHUTDOWN_TARGET,
                "database still in use at shutdown: the write-ahead log was not fully folded \
                 into the database file (it is applied on the next start)",
            ),
            Ok(_) => {}
            Err(e) => self.0.log.warn(
                SHUTDOWN_TARGET,
                &format!("final database checkpoint failed: {e}"),
            ),
        }
        ShutdownReport {
            scan_stopped,
            pool_stopped,
            sweep_stopped,
            checkpoint,
        }
    }

    /// True while keys (or old copies of them) are left in the database file
    /// or its pre-migration backup (the settings body's
    /// `keys_pending_removal`).
    pub fn key_scrub_pending(&self) -> bool {
        self.0.key_scrub.pending()
    }

    /// Run the secret migration on `db` (after a restore) and schedule the
    /// timed retry when it could not finish.
    pub fn scrub_keys(&self, db: &Db, when: &str) {
        if self.0.key_scrub.run(db, self.0.secrets.as_ref(), when)
            == (ScrubOutcome::Pending { retry: true })
        {
            self.schedule_key_scrub_retry();
        }
    }

    /// One retry of an unfinished secret scrub, now: `None` (nothing done)
    /// while writing work or a restore holds the gate. The database file
    /// lock is held for reading so a restore cannot swap the file under it.
    pub fn retry_key_scrub(&self) -> Option<ScrubOutcome> {
        retry_once(&self.0)
    }

    fn schedule_key_scrub_retry(&self) {
        let weak = Arc::downgrade(&self.0);
        self.0.key_scrub.spawn_retry(move || match weak.upgrade() {
            None => Retry::Gone,
            Some(inner) => match retry_once(&inner) {
                None => Retry::Busy,
                Some(o) => Retry::Ran(o),
            },
        });
    }

    /// What a pool pass needs (`fingerprint_pool._run_pool`,
    /// [`musaic_services::startup::pool_deps`]).
    pub fn pool_deps(&self) -> PoolDeps {
        startup::pool_deps(
            &self.0.db_path,
            &self.0.clock,
            &self.0.secrets,
            &self.0.fingerprinting,
            &self.0.transport,
            &self.0.bus,
            &self.0.pool_sleeper,
        )
    }

    /// The scan's dependencies and its thumbnail pool
    /// ([`musaic_services::scan_run::scan_deps_with_thumbs`]): every album
    /// the scan reads is submitted (`force` for a fresh one) and
    /// `on_worker_done` waits for the pool once the scan worker returns,
    /// before `LibraryScanCompleted` and the terminal status, on every path
    /// (services/scan.py:402-404).
    pub fn scan_deps_with_thumbs(&self) -> (ScanDeps, ThumbPool) {
        musaic_services::scan_run::scan_deps_with_thumbs(musaic_services::scan_run::ScanHost {
            db_path: self.0.db_path.clone(),
            clock: self.0.clock.clone(),
            bus: self.0.bus.clone(),
            track_order: self.0.track_order,
            pool: self.0.pool.clone(),
            pool_deps: self.pool_deps(),
            thumbs_dir: self.thumbs_dir(),
            on_scan_album: self.0.on_scan_album.clone(),
            log: Arc::new(self.0.log.clone()),
        })
    }

    pub fn thumbs_dir(&self) -> PathBuf {
        self.0.config.dir().join("thumbs")
    }
}
