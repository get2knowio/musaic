//! The HTTP face's application state: the [`musaic_app::App`] (config dir,
//! database, secrets store, event bus with its subscribers and SSE relay,
//! scan service, checks environment, resolver, fingerprint pool, AI queue;
//! its builder, startup and shutdown live in musaic-app) wrapped with the
//! settings only HTTP has: the SPA directory, the SSE ping interval, the
//! Host / Origin allowlist and the body caps.
//!
//! [`AppState`] dereferences to the [`App`], so handlers call the app's
//! methods and read its parts (`st.0.bus`, `st.now_iso()`) as before. The
//! fingerprint pool and restore gate notes are in
//! [`musaic_app::app`].

use std::ops::Deref;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use musaic_app::{App, AppBuilder, AppInner};
use musaic_checks::ResolutionSource;
use musaic_db::Db;
use musaic_scan::TrackOrder;
use musaic_services::check_fixes::FixSources;
use musaic_services::fingerprint_pool::PoolSleeper;
use rusqlite::Connection;

use crate::gate::WorkGuard;
use crate::host_check::AllowedHosts;
use crate::http::ApiError;
use crate::keyscrub::Logger;
use crate::logging::{LogSink, Masks};
use crate::sources::{Fingerprinting, Network};

pub use musaic_app::app::{
    Pause, RestoreHook, SharedAnalyzer, SharedSecrets, ShutdownReport, StartupError, WallClock,
    open_conn,
};

/// Cap on a cover upload's request body. The Python app
/// refuses an image over 25 MiB (`_MAX_COVER_UPLOAD_BYTES`, after reading
/// it whole); the body may carry that image plus multipart overhead, so the
/// cap is 25 MiB + 1 MiB. An image up to the Python app's limit + a little gets
/// the Python app's own 413; a body over this cap gets a 413 of the same shape.
pub const COVER_BODY_LIMIT: usize = musaic_services::apply::MAX_COVER_UPLOAD_BYTES + 1024 * 1024;

/// Cap on the bytes of a database restore upload (the
/// file part, streamed to disk). Over it: 413 and the partial file removed.
pub const RESTORE_UPLOAD_LIMIT: u64 = 2 * 1024 * 1024 * 1024;

/// Builds an [`AppState`]: the app's builder ([`AppBuilder`]) plus the
/// HTTP settings.
pub struct AppStateBuilder {
    app: AppBuilder,
    frontend_dir: Option<PathBuf>,
    ping_interval: Duration,
    allowed_hosts: AllowedHosts,
    cover_body_limit: usize,
    restore_upload_limit: u64,
}

impl AppStateBuilder {
    /// Defaults: the app's ([`AppBuilder::new`]), no SPA, 30 s pings,
    /// loopback hosts only, the default upload caps.
    pub fn new(config_dir: impl Into<PathBuf>) -> Self {
        AppStateBuilder {
            app: AppBuilder::new(config_dir),
            frontend_dir: None,
            ping_interval: Duration::from_secs(30),
            allowed_hosts: AllowedHosts::none(),
            cover_body_limit: COVER_BODY_LIMIT,
            restore_upload_limit: RESTORE_UPLOAD_LIMIT,
        }
    }

    fn with(mut self, f: impl FnOnce(AppBuilder) -> AppBuilder) -> Self {
        self.app = f(self.app);
        self
    }

    /// [`AppBuilder::ai_provider`].
    pub fn ai_provider(self, p: Arc<dyn musaic_ai::Provider>) -> Self {
        self.with(|b| b.ai_provider(p))
    }

    /// [`AppBuilder::ai_worker`].
    pub fn ai_worker(self, on: bool) -> Self {
        self.with(|b| b.ai_worker(on))
    }

    /// [`AppBuilder::ai_job_ids`].
    pub fn ai_job_ids(self, ids: crate::ai::JobIds) -> Self {
        self.with(|b| b.ai_job_ids(ids))
    }

    /// [`AppBuilder::ai_call_timeout`].
    pub fn ai_call_timeout(self, d: Duration) -> Self {
        self.with(|b| b.ai_call_timeout(d))
    }

    /// [`AppBuilder::analyzer`].
    pub fn analyzer(self, a: SharedAnalyzer) -> Self {
        self.with(|b| b.analyzer(a))
    }

    /// [`AppBuilder::log`].
    pub fn log(self, l: Logger) -> Self {
        self.with(|b| b.log(l))
    }

    /// [`AppBuilder::log_sink`].
    pub fn log_sink(self, s: LogSink) -> Self {
        self.with(|b| b.log_sink(s))
    }

    /// [`AppBuilder::log_masks`].
    pub fn log_masks(self, m: Masks) -> Self {
        self.with(|b| b.log_masks(m))
    }

    /// [`AppBuilder::key_scrub_retry_delays`].
    pub fn key_scrub_retry_delays(self, d: Vec<Duration>) -> Self {
        self.with(|b| b.key_scrub_retry_delays(d))
    }

    /// [`AppBuilder::fingerprinting`].
    pub fn fingerprinting(self, f: Fingerprinting) -> Self {
        self.with(|b| b.fingerprinting(f))
    }

    /// [`AppBuilder::pool_sleeper`].
    pub fn pool_sleeper(self, s: Arc<dyn PoolSleeper>) -> Self {
        self.with(|b| b.pool_sleeper(s))
    }

    /// [`AppBuilder::shutdown_pool_wait`].
    pub fn shutdown_pool_wait(self, d: Duration) -> Self {
        self.with(|b| b.shutdown_pool_wait(d))
    }

    /// [`AppBuilder::quick_fix_pause`].
    pub fn quick_fix_pause(self, p: Pause) -> Self {
        self.with(|b| b.quick_fix_pause(p))
    }

    /// [`AppBuilder::fix_sources`].
    pub fn fix_sources(self, s: Arc<dyn FixSources + Send + Sync>) -> Self {
        self.with(|b| b.fix_sources(s))
    }

    /// [`AppBuilder::checks_resolution`].
    pub fn checks_resolution(self, r: Arc<dyn ResolutionSource>) -> Self {
        self.with(|b| b.checks_resolution(r))
    }

    /// Host names accepted besides the loopback ones (a deliberate
    /// non-loopback bind; see `main.rs`).
    pub fn allowed_hosts(mut self, h: AllowedHosts) -> Self {
        self.allowed_hosts = h;
        self
    }

    /// Cover upload body cap (default [`COVER_BODY_LIMIT`]).
    pub fn cover_body_limit(mut self, n: usize) -> Self {
        self.cover_body_limit = n;
        self
    }

    /// Restore upload cap (default [`RESTORE_UPLOAD_LIMIT`]).
    pub fn restore_upload_limit(mut self, n: u64) -> Self {
        self.restore_upload_limit = n;
        self
    }

    /// [`AppBuilder::on_restore`].
    pub fn on_restore(self, hook: RestoreHook) -> Self {
        self.with(|b| b.on_restore(hook))
    }

    /// [`AppBuilder::on_scan_album`].
    pub fn on_scan_album(self, hook: musaic_services::scan::AlbumHook) -> Self {
        self.with(|b| b.on_scan_album(hook))
    }

    pub fn secrets(self, s: SharedSecrets) -> Self {
        self.with(|b| b.secrets(s))
    }
    pub fn network(self, n: Network) -> Self {
        self.with(|b| b.network(n))
    }
    pub fn clock(self, c: WallClock) -> Self {
        self.with(|b| b.clock(c))
    }
    pub fn frontend_dir(mut self, d: Option<PathBuf>) -> Self {
        self.frontend_dir = d;
        self
    }
    pub fn ping_interval(mut self, d: Duration) -> Self {
        self.ping_interval = d;
        self
    }
    /// [`AppBuilder::subscribers`].
    pub fn subscribers(self, on: bool) -> Self {
        self.with(|b| b.subscribers(on))
    }
    /// [`AppBuilder::auto_snapshot`].
    pub fn auto_snapshot(self, on: bool) -> Self {
        self.with(|b| b.auto_snapshot(on))
    }
    pub fn track_order(self, o: TrackOrder) -> Self {
        self.with(|b| b.track_order(o))
    }
    /// [`AppBuilder::home`].
    pub fn home(self, h: Option<PathBuf>) -> Self {
        self.with(|b| b.home(h))
    }

    /// Build the app ([`AppBuilder::build`]: every startup step) and wrap
    /// it with the HTTP settings.
    pub fn build(self) -> Result<AppState, StartupError> {
        let app = self.app.build()?;
        Ok(AppState(Arc::new(Inner {
            app,
            frontend_dir: self.frontend_dir,
            ping_interval: self.ping_interval,
            allowed_hosts: self.allowed_hosts,
            cover_body_limit: self.cover_body_limit,
            restore_upload_limit: self.restore_upload_limit,
        })))
    }
}

/// The app and the HTTP settings. Dereferences to the app's parts, so
/// `st.0.bus` reads the app's bus.
pub(crate) struct Inner {
    pub app: App,
    pub frontend_dir: Option<PathBuf>,
    pub ping_interval: Duration,
    /// Host names accepted besides loopback.
    pub allowed_hosts: AllowedHosts,
    /// Upload caps.
    pub cover_body_limit: usize,
    pub restore_upload_limit: u64,
}

impl Deref for Inner {
    type Target = AppInner;
    fn deref(&self) -> &AppInner {
        &self.app.0
    }
}

/// Shared application state (cheap to clone). Dereferences to the
/// [`App`].
#[derive(Clone)]
pub struct AppState(pub(crate) Arc<Inner>);

impl Deref for AppState {
    type Target = App;
    fn deref(&self) -> &App {
        &self.0.app
    }
}

impl AppState {
    pub fn builder(config_dir: impl Into<PathBuf>) -> AppStateBuilder {
        AppStateBuilder::new(config_dir)
    }

    /// The application this state wraps.
    pub fn app(&self) -> &App {
        &self.0.app
    }

    /// A [`WorkGuard`] for writing work, or the 409.
    pub(crate) fn begin_work(&self) -> Result<WorkGuard, ApiError> {
        Ok(self.0.gate.begin_work()?)
    }

    /// [`Self::begin_work`] labelled with the kind of work.
    pub(crate) fn begin_work_as(&self, kind: crate::gate::WorkKind) -> Result<WorkGuard, ApiError> {
        Ok(self.0.gate.begin(kind)?)
    }

    pub(crate) fn conn(&self) -> Result<Connection, ApiError> {
        open_conn(self.db_path()).map_err(ApiError::internal)
    }

    pub(crate) fn db(&self) -> Result<Db, ApiError> {
        let (db, _) = Db::open(self.db_path())?;
        Ok(db)
    }
}
