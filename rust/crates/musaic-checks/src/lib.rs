//! Health-check framework: port of the Python app's `musaic/checks/` (base,
//! runner, registry, the seven checks) and `musaic/services/post_scan.py`,
//! plus one check the Python app did not have (multi-disc-layout).
//!
//! * [`base`] — `Check`, `CheckMeta`, `CheckScope`, `Finding`, the clock and
//!   the environment a run needs.
//! * [`registry`] — the eight checks: the Python app's seven in its order,
//!   with multi-disc-layout before filename-mismatch.
//! * [`runner`] — `run_check`: findings upsert keyed
//!   `(check_id, album_id, summary)`, scope-aware auto-resolve, Missing albums
//!   out of scope, `check_runs` bookkeeping, change notification. The SQL for
//!   findings and runs lives in `musaic_db::findings`; each check's detection
//!   query lives in its module here, over a `&rusqlite::Connection`.
//! * [`inputs`] — the digest of what a check last ran against
//!   (`check_inputs`, schema v19), recorded by every completed run, and
//!   the staleness rule the background check sweep plans from.
//! * [`post_scan`] — the guarded post-scan sweep and the check-runner side of
//!   musaic-events' `SideEffects` (bus-driven reruns really run).
//! * [`checks`] — detection for all eight checks, plus the pure parts of the
//!   fixes that need nothing unported (duplicate cluster preview + trash,
//!   artist-sort bulk plan, track-gaps renumber plans).
//! * [`resolution`] — the narrow view of a resolver result the checks read,
//!   behind a trait until `musaic-resolve` lands.
//! * [`wording`] — check descriptions and finding summaries as
//!   shown, without AI wording when no AI provider is ready.
//!
//! Differences from the Python app, all deliberate:
//! duplicate-tracks compares only fingerprints of one implementation;
//! filename-mismatch ignores normalisation-only differences
//! (in musaic-fsops); bus reruns run and the
//! inconsistent-artist-sort bulk apply no longer runs its own sweep.
//! One more follows from the recursion guard rather than a separate
//! decision: checks run on the caller's thread, so a check body sees
//! `in_post_scan_rerun`; in the Python app `run_in_executor` drops the context,
//! so a nested `run_post_scan_checks` from inside a check ran a second sweep.

pub mod base;
pub mod checks;
pub mod inputs;
pub mod post_scan;
pub mod pyjson;
pub mod registry;
pub mod rerun;
pub mod resolution;
pub mod runner;
pub mod wording;

pub use base::{
    Check, CheckCtx, CheckEnv, CheckError, CheckMeta, CheckScope, Clock, Cost, Emit, Finding,
    FixKind, FixResult, Granularity, ScopeAlbum, ScopeKind, Severity, SystemClock,
};
pub use post_scan::{ChecksSideEffects, SweepEntry, post_scan_sweep, run_post_scan_checks};
pub use registry::Registry;
pub use rerun::{RERUN_ALBUM_LIST_MAX, RerunReport};
pub use resolution::{Fact, NoResolver, Resolution, ResolutionSource, TrackMatch};
pub use runner::{RunError, run_check};
