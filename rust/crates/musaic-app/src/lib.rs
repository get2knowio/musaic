//! The application object: what the Python app builds in `main._lifespan` and
//! keeps in module globals, without HTTP. musaic-api wraps it (routes,
//! guards, SSE framing, Host / Origin checks); a non-HTTP face (an MCP
//! server) builds and drives the same [`App`].
//!
//! - [`app`]: [`App`], its parts ([`AppInner`]), the builder
//!   ([`AppBuilder::build`]: config dir, database open, key scrub before the
//!   startup auto-snapshot, bus subscribers before the AI worker) and the
//!   lifespan's shutdown ([`App::begin_shutdown`], [`App::shutdown`]), the
//!   key-scrub retry scheduling.
//! - [`ai`]: the AI worker host ([`ai::AiQueue`], [`ai::AppEnv`], job ids,
//!   the job transition log).
//! - [`envs`]: the services' environments over the app (apply, analyze,
//!   track tags, check fixes), as `App::with_*_env` methods.
//! - [`jobs`]: the AI job queue's host operations (enqueue with the
//!   album's fields, resume, providers, the jobs stream's subscription and
//!   payload) and the bodies the Python app answers with, as data.
//! - [`scan`]: starting a scan with the app's dependencies, and the scan
//!   stream's start-unless-running-then-subscribe step.
//! - [`logging`]: the app's log ([`logging::AppLog`], [`logging::Masks`],
//!   [`logging::MaskingStore`]) and the binary's rotating server log.
//! - [`work`]: what is running now ([`work::WorkSnapshot`]).

pub mod ai;
pub mod app;
pub mod envs;
pub mod jobs;
pub mod logging;
pub mod scan;
pub mod work;

pub use app::{
    App, AppBuilder, AppInner, Pause, RestoreHook, SHUTDOWN_CHECKPOINT_WAIT, SHUTDOWN_POOL_WAIT,
    SharedAnalyzer, SharedSecrets, ShutdownReport, StartupError, WallClock, open_conn,
};
pub use work::WorkSnapshot;
