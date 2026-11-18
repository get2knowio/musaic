//! Base types of the check framework (port of `musaic/checks/base.py`).

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use rusqlite::Connection;

use crate::pyjson::PyJson;
use crate::registry::Registry;
use crate::resolution::ResolutionSource;

/// How loudly the UI shows a finding. Presentational only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Info,
    Warn,
    Error,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Info => "info",
            Severity::Warn => "warn",
            Severity::Error => "error",
        }
    }
}

/// Scheduling tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cost {
    Cheap,
    Medium,
    Heavy,
}

impl Cost {
    pub fn as_str(self) -> &'static str {
        match self {
            Cost::Cheap => "cheap",
            Cost::Medium => "medium",
            Cost::Heavy => "heavy",
        }
    }
}

/// How a finding can be acted on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixKind {
    None,
    AiJob,
    Deterministic,
}

impl FixKind {
    pub fn as_str(self) -> &'static str {
        match self {
            FixKind::None => "none",
            FixKind::AiJob => "ai-job",
            FixKind::Deterministic => "deterministic",
        }
    }
}

/// Static metadata about a check (`CheckMeta`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckMeta {
    pub id: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    /// `metadata` | `cover` | `audio` | `files` | `duplicates` | `external`.
    pub category: &'static str,
    pub cost: Cost,
    /// Runs as part of the post-scan sweep.
    pub runs_during_scan: bool,
    pub fix_kind: FixKind,
    pub bulk_action_label: Option<&'static str>,
}

/// A single detected issue. Identity is `(check_id, album_id, summary)`.
#[derive(Debug, Clone, PartialEq)]
pub struct Finding {
    pub summary: String,
    pub album_id: Option<String>,
    pub severity: Severity,
    pub detail: Option<PyJson>,
}

/// `CheckScope.kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeKind {
    /// The whole library; unseen findings are auto-resolved library-wide.
    All,
    /// A runner-chosen subset; nothing is auto-resolved.
    Incremental,
    /// An explicit album list; auto-resolve is limited to those albums.
    Albums,
}

impl ScopeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ScopeKind::All => "all",
            ScopeKind::Incremental => "incremental",
            ScopeKind::Albums => "albums",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "all" => Some(ScopeKind::All),
            "incremental" => Some(ScopeKind::Incremental),
            "albums" => Some(ScopeKind::Albums),
            _ => None,
        }
    }
}

/// The album fields a check may read from `scope.albums` (the runner loads
/// them from the library's Present album rows).
#[derive(Debug, Clone, PartialEq)]
pub struct ScopeAlbum {
    pub id: String,
    pub path: String,
    pub dir_name: Option<String>,
    pub track_count: i64,
    /// `track_numbers_json` decoded like `cache._parse_track_numbers_json`.
    pub track_numbers: Vec<Option<i64>>,
}

/// What a check inspects on one run.
#[derive(Debug, Clone, PartialEq)]
pub struct CheckScope {
    pub library_path: String,
    pub kind: ScopeKind,
    pub albums: Vec<ScopeAlbum>,
}

impl CheckScope {
    /// Findings missing from this run should be auto-resolved.
    pub fn can_resolve(&self) -> bool {
        matches!(self.kind, ScopeKind::All | ScopeKind::Albums)
    }

    /// For a check that reads album rows itself: `Some(ids)` (a JSON list,
    /// for `json_each`) when only these albums are to be read (an
    /// album-scoped run), `None` for the whole library.
    pub fn album_ids_json(&self) -> Option<String> {
        (self.kind == ScopeKind::Albums).then(|| {
            serde_json::Value::from(self.albums.iter().map(|a| a.id.clone()).collect::<Vec<_>>())
                .to_string()
        })
    }

    /// `None` = resolve across the library; `Some(ids)` = only these albums.
    pub fn resolve_scope_album_ids(&self) -> Option<Vec<String>> {
        match self.kind {
            ScopeKind::Albums => Some(self.albums.iter().map(|a| a.id.clone()).collect()),
            _ => None,
        }
    }
}

/// `FixResult`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixResult {
    pub ok: bool,
    pub message: String,
    pub job_id: Option<String>,
}

/// Why a check run failed (Python: any exception out of `check.run`).
#[derive(Debug, thiserror::Error)]
pub enum CheckError {
    #[error(transparent)]
    Db(#[from] musaic_db::Error),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    /// Message stored in `check_runs.error`, like Python's `str(e)`.
    #[error("{0}")]
    Failed(String),
    /// The run was told to stop before it finished (the background sweep
    /// shutting down or yielding to a restore). Ends the run as failed:
    /// nothing is auto-resolved and its inputs are not recorded.
    #[error("check run interrupted")]
    Interrupted,
}

/// What a check's result for an album depends on, for the background
/// sweep's staleness rule ([`crate::inputs`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Granularity {
    /// The finding of an album depends only on that album's inputs: the
    /// check is rerun for the albums whose inputs changed.
    Album,
    /// The check reads the whole library (or is one cheap query over it):
    /// it is rerun library-wide when any album's inputs changed.
    Library,
}

/// The runner's clock. `now_iso` is the Python app's `runner._now_iso()`
/// (`YYYY-MM-DDTHH:MM:SSZ`, UTC); `now` is wall time for other cut-offs
/// (the Python app's `datetime.now()` in unidentified-album).
pub trait Clock: Send + Sync {
    fn now_iso(&self) -> String;
    fn now(&self) -> SystemTime;
}

/// The real clock.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_iso(&self) -> String {
        musaic_db::backup::iso_seconds(SystemTime::now())
    }
    fn now(&self) -> SystemTime {
        SystemTime::now()
    }
}

/// Everything a check run needs besides the connection.
#[derive(Clone)]
pub struct CheckEnv {
    pub registry: Registry,
    pub resolver: Arc<dyn ResolutionSource>,
    pub clock: Arc<dyn Clock>,
    /// `runner.notify_changed()`: wakes `/api/checks/stream`.
    pub notify: Arc<dyn Fn() + Send + Sync>,
    /// Publishes an event on the app's bus: the callers that run a batch of
    /// checks publish `ChecksRerunFinished` through it ([`crate::rerun`]).
    pub publish: Arc<dyn Fn(musaic_events::Event) + Send + Sync>,
    /// Where multi-disc conversions keep their records
    /// (`musaic_fsops::conversion_log`): the multi-disc-layout check reports
    /// a conversion that did not finish. `None`: no records are looked for.
    pub conversion_logs: Option<std::path::PathBuf>,
}

impl CheckEnv {
    /// The Python app's registry, the system clock, no notifier.
    pub fn new(resolver: Arc<dyn ResolutionSource>) -> Self {
        CheckEnv {
            registry: Registry::default(),
            resolver,
            clock: Arc::new(SystemClock),
            notify: Arc::new(|| {}),
            publish: Arc::new(|_| {}),
            conversion_logs: None,
        }
    }
}

/// What a check's `run` sees.
pub struct CheckCtx<'a> {
    pub conn: &'a Connection,
    pub env: &'a CheckEnv,
    /// Albums the run could not evaluate ([`CheckCtx::could_not_evaluate`]).
    pub(crate) unevaluated: RefCell<BTreeSet<String>>,
}

impl<'a> CheckCtx<'a> {
    pub fn new(conn: &'a Connection, env: &'a CheckEnv) -> Self {
        CheckCtx {
            conn,
            env,
            unevaluated: RefCell::default(),
        }
    }

    /// The check could not look at this album (its folder cannot be read,
    /// the network failed): it emits nothing for it, and the runner leaves
    /// the album's open findings of this check as they are and does not
    /// record its inputs, so it is evaluated again later. Without this an
    /// unmounted drive or an offline run would read as "nothing wrong".
    pub fn could_not_evaluate(&self, album_id: &str) {
        self.unevaluated.borrow_mut().insert(album_id.to_string());
    }

    /// The albums passed to [`Self::could_not_evaluate`] so far.
    pub fn unevaluated(&self) -> BTreeSet<String> {
        self.unevaluated.borrow().clone()
    }
}

/// Sink a check yields findings into. An error ends the run as failed.
pub type Emit<'e> = dyn FnMut(Finding) -> Result<(), CheckError> + 'e;

/// A health check (`Check`). `run` yields findings through `emit` as it finds
/// them; the runner upserts each one immediately, so findings emitted before
/// an error stay recorded, as in the Python app.
pub trait Check: Send + Sync {
    fn meta(&self) -> &CheckMeta;
    /// `true` when every run, whatever its scope, looks at the whole
    /// library and emits every finding the check has there. An album-scoped
    /// run of such a check then auto-resolves library-wide: a finding that
    /// spans several albums but is stored on one of them (an artist-sort
    /// cluster) is resolved when a fix to another member clears it.
    fn sees_whole_library(&self) -> bool {
        false
    }
    /// How the background sweep tracks this check's inputs: per album
    /// unless the check sees the whole library.
    fn granularity(&self) -> Granularity {
        if self.sees_whole_library() {
            Granularity::Library
        } else {
            Granularity::Album
        }
    }
    /// A result that also depends on the passing of time (a staleness
    /// window) or on data outside the library (MusicBrainz) is rerun once
    /// it is this old, even when its inputs have not changed. `None`: only
    /// when its inputs change.
    fn refresh_after(&self) -> Option<Duration> {
        None
    }
    /// The check can make network requests (through the resolver). The
    /// background sweep runs it in small batches, through the background
    /// network path, after the local checks.
    fn uses_network(&self) -> bool {
        false
    }
    /// Inputs outside the album rows that change this check's results
    /// (settings, the resolver's configuration), as text folded into its
    /// inputs digest. Never a secret value.
    fn input_extras(&self, _ctx: &CheckCtx<'_>) -> Result<String, CheckError> {
        Ok(String::new())
    }
    fn run(
        &self,
        ctx: &CheckCtx<'_>,
        scope: &CheckScope,
        emit: &mut Emit<'_>,
    ) -> Result<(), CheckError>;
}
