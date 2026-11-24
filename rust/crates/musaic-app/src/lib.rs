//! The application object: what the Python app builds in `main._lifespan` and
//! keeps in module globals, without HTTP. musaic-api wraps it (routes,
//! guards, SSE framing, Host / Origin checks); a non-HTTP face (an MCP
//! server) builds and drives the same [`App`].
//!
//! - [`app`]: [`App`], its parts ([`AppInner`]), the builder
//!   ([`AppBuilder::build`]: config dir, database open, key scrub before the
//!   startup auto-snapshot, then the bus subscribers) and the
//!   lifespan's shutdown ([`App::begin_shutdown`], [`App::shutdown`]), the
//!   key-scrub retry scheduling.
//! - [`ai`]: the research requests' host side ([`ai::AiQueue`]: the jobs
//!   stream's change signal and job card ids; [`ai::AppEnv`], the resolver
//!   environment of the synthesis).
//! - [`envs`]: the services' environments over the app (apply, analyze,
//!   track tags, check fixes), as `App::with_*_env` methods.
//! - [`jobs`]: the jobs stream's subscription and the library-change notice
//!   for research requests (the operations are `ops::jobs`).
//! - [`scan`]: starting a scan with the app's dependencies, and the scan
//!   stream's start-unless-running-then-subscribe step.
//! - [`logging`]: the app's log ([`logging::AppLog`], [`logging::Masks`],
//!   [`logging::MaskingStore`]) and the binary's rotating server log.
//! - [`ops`]: the operations surface (`App::ops()`): typed, blocking operations
//!   and the error type every face handles.
//! - [`work`]: what is running now ([`work::WorkSnapshot`]).

pub mod agent;
pub mod ai;
pub mod app;
mod desktop_research;
pub mod envs;
pub mod jobs;
pub mod logging;
pub mod ops;
mod research;
pub mod scan;
pub mod work;

pub use app::{
    App, AppBuilder, AppInner, Pause, RestoreHook, SHUTDOWN_CHECKPOINT_WAIT, SHUTDOWN_POOL_WAIT,
    SHUTDOWN_SCAN_WAIT, SharedAnalyzer, SharedSecrets, ShutdownReport, StartupError, WallClock,
    open_conn,
};
pub use work::WorkSnapshot;
