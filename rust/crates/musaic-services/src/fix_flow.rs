//! The check-fix flow a host runs around [`crate::check_fixes`]
//! (`musaic/api/health.py`): assembling the [`FixEnv`] from the host's
//! parts (the check fixes' sources, a host override or the production
//! [`AppFixSources`]; cover downloads; the trash directory and its
//! collision stamp). This lives here rather than in musaic-api's
//! `routes/fixes.rs` so a non-HTTP face fixes findings the same way.
//!
//! The host opens the connection, reads the library directory and the
//! resolver's parts, and takes the restore gate's work guard; where a fix
//! needs research ([`crate::check_fixes::FixOutcome::NeedsAiJob`]) the host
//! makes the research request.

use std::path::Path;

use musaic_checks::CheckEnv;
use musaic_events::Bus;
use musaic_scan::TrackOrder;
use rusqlite::Connection;

use crate::check_fixes::{FixEnv, FixSources};
use crate::production::{AppFixSources, Fingerprinting, NetCovers, Parts, SharedTransport};

/// `datetime.now().strftime("%Y-%m-%dT%H-%M-%S")` for a trash name
/// collision, from the host clock's `_now_iso()`. The Python app used local
/// time; this build has no time-zone database and stamps UTC on purpose.
pub fn trash_stamp(now_iso: &str) -> String {
    // YYYY-MM-DDTHH:MM:SSZ
    now_iso.trim_end_matches('Z').replace(':', "-")
}

/// What [`with_fix_env`] builds a [`FixEnv`] from.
pub struct FixHost<'a> {
    /// A connection the host opened for this fix.
    pub conn: Connection,
    pub bus: &'a Bus,
    pub checks: &'a CheckEnv,
    /// The host's override of the fixes' outside world (tests), else the
    /// production sources over `parts`.
    pub fix_sources: Option<&'a dyn FixSources>,
    /// `config.library_dir` as stored.
    pub library_dir: Option<String>,
    /// The resolver's parts with the AcoustID key as it is now.
    pub parts: &'a Parts,
    pub fingerprinting: &'a Fingerprinting,
    pub transport: SharedTransport,
    pub config_dir: &'a Path,
    /// `_now_iso()` from the host clock (also the trash stamp's source).
    pub now_iso: &'a dyn Fn() -> String,
    pub track_order: TrackOrder,
}

/// Run `f` with the [`FixEnv`] assembled from `host`.
pub fn with_fix_env<T>(host: FixHost<'_>, f: impl FnOnce(&mut FixEnv<'_>) -> T) -> T {
    let FixHost {
        mut conn,
        bus,
        checks,
        fix_sources,
        library_dir: lib,
        parts,
        fingerprinting,
        transport,
        config_dir,
        now_iso,
        track_order,
    } = host;
    let app_sources = AppFixSources {
        parts,
        library_dir: lib.as_deref(),
        fingerprinting,
    };
    let sources: &dyn FixSources = match fix_sources {
        Some(s) => s,
        None => &app_sources,
    };
    let covers = NetCovers { transport };
    let trash_root = musaic_fsops::trash::trash_dir(config_dir);
    let now = || now_iso();
    let stamp = || trash_stamp(&now_iso());
    let mut env = FixEnv {
        conn: &mut conn,
        bus,
        checks,
        sources,
        covers: &covers,
        library_dir: lib.as_deref(),
        trash_root: &trash_root,
        now: &now,
        trash_stamp: &stamp,
        track_order,
    };
    f(&mut env)
}
