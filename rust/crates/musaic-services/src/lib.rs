//! Services the HTTP host drives from blocking tasks: the album apply
//! pipeline (`musaic/services/apply.py`) and the background library scan
//! (`musaic/services/scan.py`). Plain synchronous code: no HTTP, no async
//! runtime.
//!
//! - [`apply`]: [`apply::apply_album_changes`] hands a typed
//!   [`apply::ApplyEvent`] per step to a sink that can stop reading (a non-dry
//!   run still completes every step);
//!   [`apply::apply_direct`] is the check-fix consumer; the router checks
//!   ([`apply::resolve_album`], [`apply::validate_cover_upload`]).
//! - [`scan`]: [`scan::ScanService`], its replayable buffer and status.
//! - [`scan_run`]: starting a scan on its own thread under a work guard,
//!   logging its start and end, forwarding its events to a client.
//! - [`ai_jobs`]: the AI job queue's rules over the database (enqueue,
//!   acted-on, retry, delete, bulk delete, provider selection defaults).
//! - [`albums`]: album dicts as the Python app renders them (scanner dict, DB row).
//! - [`json`]: ordered JSON rendered like Python's `json.dumps`.
//! - [`recording_canonical`]: the duplicate-tracks finding's MusicBrainz
//!   recording title / artist, with the Python app's per-lifetime cache.
//! - [`gate`]: the restore gate ([`gate::WorkGate`]) and the
//!   database file lock ([`gate::DbGate`]).
//! - [`gated_reruns`]: the bus-driven check reruns under the restore gate,
//!   their log lines, and their replay after a restore.
//! - [`check_sweep`]: the background check sweep, which reruns every check
//!   whose inputs changed, yielding to scans, other work and restores.
//! - [`production`]: the production sources a host hands to the services
//!   ([`production::Network`], [`production::Fingerprinting`],
//!   [`production::ChecksResolution`], [`production::NetCovers`],
//!   [`production::AppFixSources`]).
//! - [`fix_flow`]: the check-fix flow around [`check_fixes`] (the fix
//!   environment from the host's parts, source choice, trash stamp, and
//!   the fix route's fall-through to the AI queue).
//! - [`check_ops`]: running one check with its log line, ignore /
//!   unignore of a check's finding, the filename-mismatch plan preview.
//! - [`db_admin`]: database backup, restore under the restore gate, and
//!   named snapshots.
//! - [`library_ops`]: library and album operations (snapshot drop and
//!   list, the Python app's album sort and album lookup, track listing, album
//!   removal, MusicBrainz anchor clearing).
//! - [`settings_patch`]: `PATCH /api/settings` (range checks, provider
//!   allowlist, the transaction with secret writes and undo, the pool
//!   restart on a new AcoustID key).
//! - [`startup`]: small startup helpers a host assembles the services
//!   with (database-opened log line, startup auto-snapshot, AcoustID key
//!   read, resolver parts factory, checks clock, fingerprint pool deps and
//!   log lines).
//! - [`host_log`]: the host's per-instance log ([`host_log::HostLog`]).
//! - [`covers`]: album covers (the largest embedded FLAC picture, the
//!   full-size cover choice disk → embedded → none, cover thumbnails and
//!   their ETag); [`thumbs`] makes a thumbnail as Pillow does
//!   from pixels [`jpegdec`] decodes as libjpeg-turbo does;
//!   [`thumb_pool`] is the scan's thumbnail pool.
//! - [`keyscrub`]: the secret-settings migration runs ([`keyscrub::KeyScrub`],
//!   keys moved into the OS keyring): their outcome, logging, timed retries and the
//!   gated retry / shutdown attempts.

pub mod ai_jobs;
pub mod albums;
pub mod analyze;
pub mod apply;
pub mod check_fixes;
pub mod check_ops;
pub mod check_sweep;
pub mod clock;
pub mod covers;
pub mod db_admin;
pub mod error;
pub mod fingerprint;
pub mod fingerprint_pool;
pub mod fix_flow;
pub mod gate;
pub mod gated_reruns;
pub mod host_log;
pub mod jpegdec;
pub mod json;
pub mod keyscrub;
pub mod library_ops;
pub mod multi_disc_convert;
pub mod multi_disc_gather;
pub mod production;
pub mod recording_canonical;
pub mod scan;
pub mod scan_adopt;
pub mod scan_run;
pub mod settings_patch;
pub mod sidecar_import;
pub mod sources;
pub mod startup;
pub mod thumb_pool;
pub mod thumbs;
pub mod track_tags;
pub mod vocabulary;

pub use apply::{
    ApplyEnv, ApplyEvent, ApplyOutcome, ApplyRequest, CoverSource, NoCovers, ResolvedAlbum,
    apply_album_changes, apply_direct, missing_album_detail, refuse_missing, resolve_album,
    validate_cover_upload,
};
pub use error::{OpError, Subject};
pub use json::Json;
/// What an apply may remove (the HTTP layer validates a request against it).
pub use musaic_tags::{
    Removals, TRACK_FIELDS_NOT_REMOVABLE, is_removable_album_field, is_removable_track_field,
    removable_album_fields, removable_track_fields,
};
pub use scan::{
    CancelPhase, CancelScan, Poll, ScanCancelled, ScanDeps, ScanDone, ScanEvent, ScanEventKind,
    ScanJob, ScanService, ScanState, ScanStatus, ScanSubscription, StartScan,
};
