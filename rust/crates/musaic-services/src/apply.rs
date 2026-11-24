//! The single album mutation pipeline: port of
//! `musaic/services/apply.py::apply_album_changes_iter`, the request checks of
//! `musaic/api/apply.py`, and the draining consumer `checks/_apply.py::apply_direct`.
//!
//! # Shape
//!
//! [`apply_album_changes`] calls a sink once per [`ApplyEvent`], in the order and
//! with the content of the items the Python app's generator yields (the HTTP face
//! renders each as that item: `musaic-api/src/render/apply.rs`). The sink
//! returns [`ControlFlow::Break`] to stop reading. The sequence is
//!
//! ```text
//! cover_downloading {url}                         (cover_url only)
//! cover_downloaded  {source_bytes, source_mime, bytes, mime}
//! start / file_started / file_done / file_renamed* / done   (musaic-tags)
//! cover_saved {path, filename}                    (cover given, not dry run)
//! updated_album <merged row> | removed_album {id, path}
//! error {detail}                                  (instead of the rest)
//! ```
//!
//! Bus events (`AlbumTagsChanged`, then `AlbumCoverChanged` when art changed)
//! are published BEFORE `updated_album` is handed to the sink.
//!
//! # A consumer that stops
//!
//! A non-dry-run apply runs to completion whatever the consumer does (a
//! deliberate change from the Python app). Once the
//! sink returns `Break`, or panics, it is never called again, but every step
//! still runs: the remaining file writes, the second rename pass, the
//! track_tags / fingerprint migration, cover.jpg, the sidecar resync, the
//! single-album upsert and the bus events. Only the forwarding stops. In the
//! Python app the generator stopped at the suspended yield (`gen.close()`),
//! which could leave files staged under hidden `.NAME.renaming.tmp` names,
//! or written files with no database update and no event.
//!
//! A dry run mutates nothing, so it may stop early: after a `Break` it ends
//! at the next step boundary (the per-file loop, which only reads, finishes).
//!
//! No unwinding is used to stop: the crate does not rely on
//! `panic = "unwind"` for any of this. A panic inside the sink is caught
//! (`catch_unwind` around the sink call only) and treated as `Break`; under
//! `panic = "abort"` such a panic aborts the process, as any panic would.
//!
//! # Refusals before anything is read
//!
//! - An album whose row is Missing (`missing_since` set) is refused
//!   before anything is read or written, dry runs and cover uploads
//!   included: [`resolve_album`] answers 409 ([`refuse_missing`]), and
//!   [`apply_album_changes`] itself yields only `error {detail}` with the
//!   same text, so `apply_direct` refuses too.
//! - Cover bytes that are empty (from the [`CoverSource`] or an
//!   upload) are a failed download, with the Python app's failed-download
//!   items: `cover_downloading {url}` (url path only), then `error {detail}`
//!   and nothing else, so no FLAC, cover.jpg or row is touched and nothing
//!   is published, exactly as when the downloader returns nothing. The
//!   Python app embedded nothing, wrote an empty cover.jpg over the existing one
//!   and published AlbumCoverChanged.
//!
//! # Other deliberate changes
//!
//! - A rename moves the file's `track_fingerprints` row too
//!   (`musaic_db::renames::rename_track_files`); the Python app moved only
//!   `track_tags`.
//! - A save whose new metadata does not fit rewrites the file
//!   (`musaic_tags::preserve`); what it could not carry over (an extended
//!   attribute, the owner, a hard link) is listed in `file_done.warnings`,
//!   a key present only then. `apply_direct` adds a count of them to its
//!   message.
//! - The post-apply rescan derives album fields in
//!   (DISCNUMBER, TRACKNUMBER) order (`TrackOrder::DiscTrack`).
//! - Cover bytes come from a [`CoverSource`]; the real one
//!   (musaic-net) enforces the public-HTTPS, image type and 8 MiB rules.
//! - A tag can be removed, by an explicit [`ApplyRequest::removals`] only
//!   (musaic-tags [`Removals`]); a suggestion never removes one. A removal
//!   shows in `file_done.diffs` with `new: null`, `start` / `done` carry
//!   `cleared` (album-level fields) when there are any, and the cleared
//!   album-level fields are listed in `AlbumTagsChanged.fields`. The
//!   rescan + upsert that follows writes the now-absent fields to the row as
//!   NULL. [`apply_direct`] (the check-fix path) never removes anything.

use std::ops::ControlFlow;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;

use musaic_events::{Bus, Event as BusEvent};
use musaic_scan::{TrackOrder, py_resolve};
/// The tag writer's per-file events, as `ApplyEvent::Tags` carries them.
pub use musaic_tags::Event as TagEvent;
use musaic_tags::{Removals, Suggestions, TrackSuggestions};
use rusqlite::Connection;

use crate::albums::{self, album_to_json};
use crate::error::{OpError, Subject};
use crate::json::Json;
use crate::library_ops::no_library;
use crate::track_tags::conflict;

/// Fetches cover bytes for `cover_url` (`tags.download_cover`): `(bytes, mime)`
/// or `None` on any failure. musaic-net's CoverArtClient implements the real
/// one; this crate never touches the network.
pub trait CoverSource {
    fn download(&self, url: &str) -> Option<(Vec<u8>, String)>;
}

/// A [`CoverSource`] that always fails (no network configured).
pub struct NoCovers;

impl CoverSource for NoCovers {
    fn download(&self, _url: &str) -> Option<(Vec<u8>, String)> {
        None
    }
}

/// One step of an apply, in the order the pipeline reaches it.
///
/// A run is: `CoverDownloading`, `CoverDownloaded` (a cover only), then the
/// per-file `Tags` events (`Start`, `FileStarted`, `FileDone`, `FileRenamed`,
/// `Done`), `CoverSaved` (a cover, not a dry run), and last `UpdatedAlbum` or
/// `RemovedAlbum`; an `Error` replaces whatever would have followed. A dry run
/// ends with the `Done` of its `Tags`.
///
/// `A` is the album's form in `UpdatedAlbum`: the document here (the
/// default), a typed record on the operations surface ([`ApplyEvent::map_album`]
/// converts), so the steps are listed once.
#[derive(Debug, Clone, PartialEq)]
pub enum ApplyEvent<A = Json> {
    /// The cover at `url` is being fetched.
    CoverDownloading { url: String },
    /// The cover was fetched and normalised to JPEG.
    CoverDownloaded {
        source_bytes: usize,
        source_mime: String,
        bytes: usize,
        mime: String,
    },
    /// A per-file step from the tag writer: never its `CoverDownloaded` or
    /// `CoverSaved`, which are the variants of this enum.
    Tags(TagEvent),
    /// `cover.jpg` was written to the album folder.
    CoverSaved { path: String, filename: String },
    /// The album as the database holds it after the run (the merged row when a
    /// library folder is set, else what a scan reads).
    UpdatedAlbum(A),
    /// The album's folder is gone after the run: its row is now Missing.
    RemovedAlbum { id: String, path: String },
    /// The run failed or was refused; nothing more follows.
    Error { detail: String },
}

impl<A> ApplyEvent<A> {
    /// The event ends the run: nothing follows it (a dry run ends with the
    /// `Done` of its `Tags`).
    pub fn is_terminal(&self) -> bool {
        match self {
            ApplyEvent::UpdatedAlbum(_)
            | ApplyEvent::RemovedAlbum { .. }
            | ApplyEvent::Error { .. } => true,
            ApplyEvent::Tags(TagEvent::Done { dry_run, .. }) => *dry_run,
            _ => false,
        }
    }

    /// The same event with the album of `UpdatedAlbum` converted by `f`.
    pub fn map_album<B>(self, f: impl FnOnce(A) -> B) -> ApplyEvent<B> {
        match self {
            ApplyEvent::CoverDownloading { url } => ApplyEvent::CoverDownloading { url },
            ApplyEvent::CoverDownloaded {
                source_bytes,
                source_mime,
                bytes,
                mime,
            } => ApplyEvent::CoverDownloaded {
                source_bytes,
                source_mime,
                bytes,
                mime,
            },
            ApplyEvent::Tags(t) => ApplyEvent::Tags(t),
            ApplyEvent::CoverSaved { path, filename } => ApplyEvent::CoverSaved { path, filename },
            ApplyEvent::UpdatedAlbum(a) => ApplyEvent::UpdatedAlbum(f(a)),
            ApplyEvent::RemovedAlbum { id, path } => ApplyEvent::RemovedAlbum { id, path },
            ApplyEvent::Error { detail } => ApplyEvent::Error { detail },
        }
    }
}

impl ApplyEvent {
    fn from_tag(ev: TagEvent) -> ApplyEvent {
        match ev {
            TagEvent::CoverDownloaded {
                source_bytes,
                source_mime,
                bytes,
                mime,
            } => ApplyEvent::CoverDownloaded {
                source_bytes,
                source_mime,
                bytes,
                mime,
            },
            TagEvent::CoverSaved { path, filename } => ApplyEvent::CoverSaved {
                path: path_str(&path),
                filename,
            },
            other => ApplyEvent::Tags(other),
        }
    }
}

/// How a renumbered file's prefix is written: by the number part of the
/// filename pattern the album's row is named by, so a renumber re-applies
/// the pattern the user has defined.
enum Numbering {
    /// The settings could not be read: the track alone, two digits.
    Default,
    /// The pattern's leading number part; `None` when the pattern does not
    /// start with the track number, and then no file is renamed.
    Pattern {
        number: Option<musaic_fsops::template::NumberTemplate>,
        /// The set has more than one disc, so `{disc}` is rendered.
        multi_disc: bool,
        /// The folder's disc number, for a file that names none itself.
        folder_disc: Option<i64>,
    },
}

impl Numbering {
    fn for_album(conn: &Connection, album_id: &str, album_dir: &Path) -> Numbering {
        let Ok(templates) = musaic_fsops::plan::Templates::from_settings(conn) else {
            return Numbering::Default;
        };
        let Ok(row) = musaic_db::renames::rename_plan_row(conn, album_id) else {
            return Numbering::Default;
        };
        let (template, disc_number, disc_total) = match &row {
            Some(r) if r.is_compilation => {
                (&templates.filename_compilation, r.disc_number, r.disc_total)
            }
            Some(r) => (&templates.filename_single, r.disc_number, r.disc_total),
            None => (&templates.filename_single, None, None),
        };
        let number = musaic_fsops::template::NumberTemplate::parse(template);
        let multi_disc = number.as_ref().is_some_and(|n| n.has_disc())
            && musaic_scan::flacmeta::set_spans_discs(disc_number, disc_total, || {
                musaic_scan::flacmeta::DiscEvidence::of_folder(album_dir)
            });
        Numbering::Pattern {
            number,
            multi_disc,
            folder_disc: disc_number,
        }
    }

    fn number(&self, track: i64, file_disc: Option<i64>) -> Option<String> {
        match self {
            Numbering::Default => musaic_tags::default_numbering(track, file_disc),
            Numbering::Pattern {
                number,
                multi_disc,
                folder_disc,
            } => {
                let disc = if *multi_disc {
                    file_disc.or(*folder_disc)
                } else {
                    None
                };
                number.as_ref()?.render(track, disc)
            }
        }
    }
}

/// What to apply (`apply_album_changes_iter`'s keyword arguments).
#[derive(Debug, Clone, Default)]
pub struct ApplyRequest {
    pub album_id: String,
    pub album_path: String,
    /// The configured library root; `None` or `""` skips the upsert and
    /// `updated_album` then carries the scanner dict (the Python app's `""`).
    pub library_dir: Option<String>,
    pub suggestions: Suggestions,
    pub track_suggestions: TrackSuggestions,
    /// Tags to remove: only an explicit user request fills this.
    pub removals: Removals,
    pub cover_url: Option<String>,
    /// The upload path: bytes already in hand.
    pub cover_bytes: Option<Vec<u8>>,
    pub cover_mime: Option<String>,
    pub dry_run: bool,
}

/// Everything the pipeline touches besides the album directory.
pub struct ApplyEnv<'a> {
    pub conn: &'a mut Connection,
    pub bus: &'a Bus,
    pub covers: &'a dyn CoverSource,
    /// `_now_iso()` for missing_since / the libraries row.
    pub now: &'a dyn Fn() -> String,
    pub track_order: TrackOrder,
}

/// How a run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyOutcome {
    /// The sink was handed every item, the last one included (which may be
    /// `error`).
    Finished,
    /// The sink returned `Break` (or panicked) and was not called again.
    /// A non-dry run still ran every step to completion; a dry run
    /// may have ended early.
    Stopped,
}

/// The text of the refusal (409) for a Missing album.
pub fn missing_album_detail(missing_since: &str) -> String {
    format!(
        "Album is missing from disk (missing since {missing_since}). Rescan the library once its \
         folder is back, then try again."
    )
}

/// Deliberate change from the Python app: `Conflict`
/// when the album's row is Missing (`missing_since` set and not empty, the
/// runner's test). An album without a row is not refused here.
pub fn refuse_missing(conn: &Connection, album_id: &str) -> Result<(), OpError> {
    match musaic_db::albums::missing_since(conn, album_id) {
        Ok(Some(since)) if !since.is_empty() => Err(conflict(missing_album_detail(&since))),
        Ok(_) => Ok(()),
        Err(e) => Err(OpError::failed(e)),
    }
}

/// The text of the failure for an upload of zero bytes (the upload
/// route itself already answers 400 with this text).
pub const EMPTY_UPLOAD_DETAIL: &str = "Empty file";

/// Hands items to the consumer until it stops reading.
struct Forward<'s> {
    sink: &'s mut dyn FnMut(ApplyEvent) -> ControlFlow<()>,
    gone: bool,
    dry_run: bool,
}

impl Forward<'_> {
    /// Forward one item unless the consumer is gone. A panic in the sink
    /// counts as `Break`. `Err(Stopped)` only for a dry run whose consumer is
    /// gone (nothing is mutated, so it may end early).
    fn emit(&mut self, event: ApplyEvent) -> Result<(), Halt> {
        if !self.gone {
            let sink = &mut *self.sink;
            let flow =
                catch_unwind(AssertUnwindSafe(|| sink(event))).unwrap_or(ControlFlow::Break(()));
            self.gone = flow.is_break();
        }
        if self.gone && self.dry_run {
            Err(Halt::Stopped)
        } else {
            Ok(())
        }
    }

    fn outcome(&self) -> ApplyOutcome {
        if self.gone {
            ApplyOutcome::Stopped
        } else {
            ApplyOutcome::Finished
        }
    }
}

enum Halt {
    Stopped,
    /// An `error {detail}` the Python app yields itself (a failed cover
    /// download, an empty upload): no log line.
    Refused(String),
    /// What the Python app's outer `except Exception` catches: logged
    /// ([`APPLY_LOG`]) and yielded as `error {str(e)}`.
    Failed(String),
}

/// The Python app's logger for `services/apply.py` (`musaic.services.apply`).
pub const APPLY_LOG: &str = "musaic.services.apply";

impl From<rusqlite::Error> for Halt {
    fn from(e: rusqlite::Error) -> Halt {
        Halt::Failed(e.to_string())
    }
}

impl From<musaic_db::Error> for Halt {
    fn from(e: musaic_db::Error) -> Halt {
        Halt::Failed(e.to_string())
    }
}

fn path_str(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// `tuple(suggestions.keys())`: the caller's dict keys, first position kept,
/// then the album-level fields removed that are not among them.
fn changed_fields(s: &Suggestions, removals: &Removals) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    let removed = removals
        .album
        .iter()
        .filter(|f| musaic_tags::is_removable_album_field(f));
    for k in s.iter().map(|(k, _)| k).chain(removed) {
        if !keys.contains(k) {
            keys.push(k.clone());
        }
    }
    keys
}

/// Run one apply, handing every item to `sink` as it happens.
///
/// A non-dry run always runs to completion: when `sink` returns
/// `Break` (or panics) it is not called again, but the remaining file
/// writes, renames, database updates, cover.jpg and bus events still happen,
/// and the return value is [`ApplyOutcome::Stopped`]. A dry run may end
/// early instead.
///
/// For an HTTP host this means the stream needs no rendezvous with the
/// client: run this on a blocking thread and send each item down any
/// channel (unbounded is fine); when the client has gone the send fails and
/// the sink returns `Break`. There is no need to pace the pipeline to the
/// client (a zero-capacity / rendezvous channel) or to keep the client
/// connected for the apply to finish.
///
/// Refused before anything is read: a Missing album and the
/// caller bug of passing both `cover_url` and `cover_bytes`; each yields only
/// `error {detail}`.
pub fn apply_album_changes(
    env: &mut ApplyEnv<'_>,
    req: &ApplyRequest,
    sink: &mut dyn FnMut(ApplyEvent) -> ControlFlow<()>,
) -> ApplyOutcome {
    apply_with(env, req, Special::None, sink)
}

/// The fields a multi-disc conversion's `AlbumTagsChanged` names.
pub const DISC_CONVERSION_FIELDS: [&str; 4] =
    ["tracknumber", "tracktotal", "discnumber", "disctotal"];

/// The tag writes of a multi-disc conversion
/// (`musaic_services::multi_disc_convert`), through this pipeline: the
/// same items, the same Missing refusal, the same rescan, upsert and
/// `AlbumTagsChanged` (naming [`DISC_CONVERSION_FIELDS`]). The writes are
/// exactly `conversion`'s ([`musaic_tags::apply_disc_conversion`]); no file
/// is renamed and nothing else is written. Not reachable from
/// [`ApplyRequest`]: the general apply can neither set nor remove these
/// tags.
pub fn apply_disc_conversion(
    env: &mut ApplyEnv<'_>,
    album_id: &str,
    album_path: &str,
    library_dir: Option<&str>,
    conversion: &musaic_tags::DiscConversion,
    sink: &mut dyn FnMut(ApplyEvent) -> ControlFlow<()>,
) -> ApplyOutcome {
    let req = ApplyRequest {
        album_id: album_id.to_string(),
        album_path: album_path.to_string(),
        library_dir: library_dir.map(str::to_string),
        ..ApplyRequest::default()
    };
    apply_with(env, &req, Special::Conversion(conversion), sink)
}

/// What an undo puts back through this pipeline
/// ([`musaic_tags::Restore`], plus the cover sidecars).
#[derive(Debug, Clone, Default)]
pub struct RestoreRequest {
    pub tags: musaic_tags::Restore,
    /// The cover sidecars, by bare name and bytes; `None` leaves them alone,
    /// `Some(empty)` leaves the folder without one.
    pub sidecars: Option<Vec<(String, Vec<u8>)>>,
    /// The fields `AlbumTagsChanged` names (the album and track fields the
    /// undone change touched).
    pub fields: Vec<String>,
}

/// The writes of an undo (`musaic_app`'s agent undo), through this
/// pipeline: the same items, the same Missing refusal, the same rescan,
/// upsert, `AlbumTagsChanged` (naming [`RestoreRequest::fields`]) and
/// `AlbumCoverChanged` when a picture or a sidecar changed. The writes are
/// exactly `restore`'s ([`musaic_tags::apply_restore`], then
/// [`musaic_tags::restore_cover_sidecars`]); no file is renamed and nothing
/// else is written. Not reachable from [`ApplyRequest`].
pub fn apply_restore(
    env: &mut ApplyEnv<'_>,
    album_id: &str,
    album_path: &str,
    library_dir: Option<&str>,
    restore: &RestoreRequest,
    sink: &mut dyn FnMut(ApplyEvent) -> ControlFlow<()>,
) -> ApplyOutcome {
    let req = ApplyRequest {
        album_id: album_id.to_string(),
        album_path: album_path.to_string(),
        library_dir: library_dir.map(str::to_string),
        ..ApplyRequest::default()
    };
    apply_with(env, &req, Special::Restore(restore), sink)
}

/// What a run writes besides the request's suggestions, removals and cover.
#[derive(Clone, Copy)]
enum Special<'a> {
    None,
    Conversion(&'a musaic_tags::DiscConversion),
    Restore(&'a RestoreRequest),
}

fn apply_with(
    env: &mut ApplyEnv<'_>,
    req: &ApplyRequest,
    special: Special<'_>,
    sink: &mut dyn FnMut(ApplyEvent) -> ControlFlow<()>,
) -> ApplyOutcome {
    let mut fwd = Forward {
        sink,
        gone: false,
        dry_run: req.dry_run,
    };
    if let Err(refusal) = refuse_missing(env.conn, &req.album_id) {
        let _ = fwd.emit(ApplyEvent::Error {
            detail: refusal.message().to_string(),
        });
        return fwd.outcome();
    }
    let cover_url = req.cover_url.as_deref().filter(|u| !u.is_empty());
    if cover_url.is_some() && req.cover_bytes.is_some() {
        let detail = "Pass cover_url OR cover_bytes — not both. (Caller bug.)";
        let _ = fwd.emit(ApplyEvent::Error {
            detail: detail.to_string(),
        });
        return fwd.outcome();
    }
    let mut emit = |event: ApplyEvent| fwd.emit(event);
    let result = run(env, req, cover_url, special, &mut emit);
    match result {
        Err(Halt::Failed(detail)) => {
            // apply.py:257 `_log.exception("apply_album_changes_iter failed
            // for album %s", album_id)`: ERROR; the error text stands in for
            // the traceback.
            musaic_log::error(
                APPLY_LOG,
                &format!(
                    "apply_album_changes_iter failed for album {}: {detail}",
                    req.album_id
                ),
            );
            let _ = fwd.emit(ApplyEvent::Error { detail });
        }
        Err(Halt::Refused(detail)) => {
            let _ = fwd.emit(ApplyEvent::Error { detail });
        }
        Ok(()) | Err(Halt::Stopped) => {}
    }
    fwd.outcome()
}

fn run(
    env: &mut ApplyEnv<'_>,
    req: &ApplyRequest,
    cover_url: Option<&str>,
    special: Special<'_>,
    emit: &mut dyn FnMut(ApplyEvent) -> Result<(), Halt>,
) -> Result<(), Halt> {
    let album_dir = Path::new(&req.album_path);

    // 1. Cover bytes, normalised to JPEG q95.
    let mut final_cover: Option<Vec<u8>> = None;
    if let Some(url) = cover_url {
        emit(ApplyEvent::CoverDownloading {
            url: url.to_string(),
        })?;
        // Zero bytes are a failed download.
        let Some((raw, raw_mime)) = env.covers.download(url).filter(|(b, _)| !b.is_empty()) else {
            return Err(Halt::Refused(format!(
                "Failed to download cover from {url}"
            )));
        };
        let out = musaic_tags::normalize_to_jpeg(&raw, 95);
        let event = ApplyEvent::CoverDownloaded {
            source_bytes: raw.len(),
            source_mime: raw_mime,
            bytes: out.len(),
            mime: "image/jpeg".into(),
        };
        final_cover = Some(out);
        emit(event)?;
    } else if let Some(raw) = &req.cover_bytes {
        // An empty upload is a failed download too.
        if raw.is_empty() {
            return Err(Halt::Refused(EMPTY_UPLOAD_DETAIL.into()));
        }
        let out = musaic_tags::normalize_to_jpeg(raw, 95);
        let event = ApplyEvent::CoverDownloaded {
            source_bytes: raw.len(),
            source_mime: req
                .cover_mime
                .clone()
                .filter(|m| !m.is_empty())
                .unwrap_or_else(|| "image/jpeg".into()),
            bytes: out.len(),
            mime: "image/jpeg".into(),
        };
        final_cover = Some(out);
        emit(event)?;
    }

    // 2. Per-file writes, streamed; renames followed in the database.
    let mut had_art_change = final_cover.is_some();
    let mut had_renames = false;
    let dry_run = req.dry_run;
    // `if cover_bytes:` in tags.py: empty bytes embed nothing (unreachable
    // now that empty input fails above, unless normalising yields nothing).
    let picture = final_cover
        .as_deref()
        .filter(|b| !b.is_empty())
        .map(|b| (b, "image/jpeg"));
    let numbering = Numbering::for_album(env.conn, &req.album_id, album_dir);
    let mut callback = |ev: TagEvent| {
        match &ev {
            TagEvent::FileDone(c) if c.art_changed => had_art_change = true,
            TagEvent::FileRenamed {
                old_filename,
                new_filename,
                ..
            } if !dry_run
                && !old_filename.is_empty()
                && !new_filename.is_empty()
                && old_filename != new_filename =>
            {
                // Tag rows follow the rename (and fingerprints too). A failure is
                // logged and the apply carries on (apply.py:141
                // `_log.exception(...)`: ERROR; the error text stands in for
                // the traceback).
                match musaic_db::renames::rename_track_files(
                    env.conn,
                    &req.album_id,
                    old_filename,
                    new_filename,
                ) {
                    Ok(_) => had_renames = true,
                    Err(e) => musaic_log::error(
                        APPLY_LOG,
                        &format!(
                            "Failed to migrate track tags {old_filename} \u{2192} {new_filename} on album {}: {e}",
                            req.album_id
                        ),
                    ),
                }
            }
            _ => {}
        }
        // A consumer that is gone only stops the forwarding; the
        // loop and the rename passes always finish. (A dry run's `Stopped`
        // takes effect at the next step boundary.)
        let _ = emit(ApplyEvent::from_tag(ev));
    };
    match special {
        Special::Conversion(c) => musaic_tags::apply_disc_conversion(album_dir, c, &mut callback),
        Special::Restore(r) => musaic_tags::apply_restore(album_dir, &r.tags, &mut callback),
        Special::None => musaic_tags::apply_to_album_iter_numbered(
            album_dir,
            &req.suggestions,
            &req.track_suggestions,
            &req.removals,
            picture,
            dry_run,
            &mut callback,
            &|track, disc| numbering.number(track, disc),
        ),
    }
    .map_err(|e| Halt::Failed(e.to_string()))?;
    if dry_run {
        return Ok(());
    }

    // 3. cover.jpg sidecar.
    if let Some(bytes) = &final_cover
        && let Some(saved) = musaic_tags::save_cover_to_album_dir(album_dir, bytes)
    {
        let filename = saved
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        emit(ApplyEvent::CoverSaved {
            path: path_str(&saved),
            filename,
        })?;
    }
    // 3b. An undo's cover sidecars. A failure is reported after the
    // album is rescanned and the events are published: the files may have
    // changed already.
    let mut sidecar_error = None;
    if let Special::Restore(RestoreRequest {
        sidecars: Some(saved),
        ..
    }) = special
    {
        match musaic_tags::restore_cover_sidecars(album_dir, saved) {
            Ok(changed) => had_art_change |= changed,
            Err(e) => sidecar_error = Some(format!("the cover files could not be put back: {e}")),
        }
    }

    // 5. Track-tag sidecar after renames (only when the album has tags).
    if had_renames {
        // A failed write is logged by write_sidecar; a failed read of the
        // album's tags is apply.py:186's `_log.exception(...)` (ERROR; the
        // error text stands in for the traceback).
        if let Err(e) =
            musaic_fsops::sidecar::resync_after_renames(env.conn, &req.album_id, album_dir)
        {
            musaic_log::error(
                APPLY_LOG,
                &format!(
                    "Failed to resync track-tag sidecar after rename for album {}: {e}",
                    req.album_id
                ),
            );
        }
    }

    // 6. Rescan this album only, upsert, publish, then the final item.
    let Some(fresh) = musaic_scan::scan_album(album_dir, env.track_order) else {
        let since = (env.now)();
        if musaic_db::albums::mark_album_missing(env.conn, &req.album_id, &since)? {
            env.bus.publish(&BusEvent::AlbumMarkedMissing {
                album_id: req.album_id.clone(),
                missing_since: since,
            });
        }
        return emit(ApplyEvent::RemovedAlbum {
            id: req.album_id.clone(),
            path: req.album_path.clone(),
        });
    };
    let mut fresh_json = album_to_json(&fresh);
    if let Some(lib) = req.library_dir.as_deref().filter(|l| !l.is_empty()) {
        let lib = path_str(&py_resolve(Path::new(lib)));
        musaic_db::albums::upsert_album(env.conn, &lib, &(env.now)(), &fresh)?;
        if let Some(merged) = albums::get_album_by_id(env.conn, &req.album_id)? {
            fresh_json = merged.json;
        }
    }
    // Publish before the final yield.
    env.bus.publish(&BusEvent::AlbumTagsChanged {
        album_id: req.album_id.clone(),
        fields: match special {
            Special::Conversion(_) => DISC_CONVERSION_FIELDS
                .iter()
                .map(|f| f.to_string())
                .collect(),
            Special::Restore(r) => r.fields.clone(),
            Special::None => changed_fields(&req.suggestions, &req.removals),
        },
    });
    if had_art_change {
        env.bus.publish(&BusEvent::AlbumCoverChanged {
            album_id: req.album_id.clone(),
        });
    }
    if let Some(e) = sidecar_error {
        return Err(Halt::Failed(e));
    }
    emit(ApplyEvent::UpdatedAlbum(fresh_json))
}

/// The album an apply targets.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedAlbum {
    pub id: String,
    pub path: String,
    /// The dict the Python app passes as `album` (a DB row, or the scanner dict
    /// when it came from the live fallback).
    pub json: Json,
}

/// `services.albums.resolve_album_dict`: `NoLibrary` without a configured
/// library; the cached row; else a live walk of the library; else `NotFound`.
/// A database failure is `Failed` (the route showed its cause in the body).
///
/// Deliberate change: a cached row that is Missing is refused with
/// `Conflict` ([`missing_album_detail`], which says to rescan) before anything is
/// read or written, dry runs and cover uploads included. The Python app returned
/// the row and let the apply run.
pub fn resolve_album(
    conn: &Connection,
    library_dir: Option<&str>,
    album_id: &str,
    order: TrackOrder,
) -> Result<ResolvedAlbum, OpError> {
    let Some(lib) = library_dir.filter(|l| !l.is_empty()) else {
        return Err(no_library());
    };
    let resolved = path_str(&py_resolve(Path::new(lib)));
    let cached = albums::load_snapshot(conn, &resolved).map_err(OpError::failed)?;
    if let Some(row) = cached.into_iter().find(|r| r.id() == album_id) {
        // A Missing album is refused before anything else happens.
        if let Some(since) = row.missing_since.as_deref().filter(|s| !s.is_empty()) {
            return Err(conflict(missing_album_detail(since)));
        }
        return Ok(ResolvedAlbum {
            id: row.album.id.clone(),
            path: row.album.path.clone(),
            json: row.json,
        });
    }
    match albums::find_album_by_id(Path::new(lib), album_id, order) {
        Some(a) => Ok(ResolvedAlbum {
            id: a.id.clone(),
            path: a.path.clone(),
            json: album_to_json(&a),
        }),
        None => Err(OpError::NotFound {
            details: None,
            what: Subject::Album,
            id: album_id.to_string(),
            message: "Album not found (try rescanning the library)".to_string(),
        }),
    }
}

/// `_MAX_COVER_UPLOAD_BYTES`.
pub const MAX_COVER_UPLOAD_BYTES: usize = 25 * 1024 * 1024;

/// The upload's content type alone: it must start with `image/` (`Rejected`).
/// Returns the lower-cased type to pass as `cover_mime`.
pub fn check_cover_type(content_type: Option<&str>) -> Result<String, OpError> {
    let mime = content_type.unwrap_or("").to_lowercase();
    if !mime.starts_with("image/") {
        return Err(OpError::Rejected {
            message: format!(
                "Unsupported file type {} \u{2014} please upload an image",
                musaic_fsops::py_repr(&mime)
            ),
            details: None,
        });
    }
    Ok(mime)
}

/// The checks `POST /api/albums/{id}/cover/upload` makes BEFORE resolving
/// the album, in its order: MIME prefix `image/` (`Rejected`), empty body
/// (`Rejected`), size cap (`TooLarge`). Returns the lower-cased MIME to pass as
/// `cover_mime`.
pub fn validate_cover_upload(content_type: Option<&str>, body: &[u8]) -> Result<String, OpError> {
    let mime = check_cover_type(content_type)?;
    if body.is_empty() {
        return Err(OpError::Rejected {
            message: "Empty file".into(),
            details: None,
        });
    }
    if body.len() > MAX_COVER_UPLOAD_BYTES {
        return Err(OpError::TooLarge {
            limit: MAX_COVER_UPLOAD_BYTES as u64,
            message: format!(
                "Cover too large ({} bytes). Max accepted is {} MB.",
                body.len(),
                MAX_COVER_UPLOAD_BYTES / (1024 * 1024)
            ),
        });
    }
    Ok(mime)
}

/// `result.cover.cover_url` of a stored job result, when it is a non-empty
/// string (written by an assistant's result, an old job, or the model-free
/// MusicBrainz synthesizer).
pub fn stored_cover_url(result_json: Option<&str>) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(result_json?).ok()?;
    v.get("cover")?
        .get("cover_url")?
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// `checks._apply.apply_direct`: find the album in the cache, drain the
/// pipeline, report `(ok, message)`.
pub fn apply_direct(
    env: &mut ApplyEnv<'_>,
    library_dir: Option<&str>,
    album_id: &str,
    suggestions: &Suggestions,
    track_suggestions: &TrackSuggestions,
    cover_url: Option<&str>,
) -> (bool, String) {
    let Some(lib) = library_dir.filter(|l| !l.is_empty()) else {
        return (false, "No library configured".into());
    };
    let lib = path_str(&py_resolve(Path::new(lib)));
    let row = match albums::load_snapshot(env.conn, &lib) {
        Ok(rows) => rows.into_iter().find(|r| r.id() == album_id),
        Err(e) => return (false, format!("Apply failed: {e}")),
    };
    let Some(row) = row else {
        return (false, "Album not found in cache".into());
    };
    let req = ApplyRequest {
        album_id: row.album.id.clone(),
        album_path: row.album.path.clone(),
        library_dir: Some(lib),
        suggestions: suggestions.clone(),
        track_suggestions: track_suggestions.clone(),
        // A check fix applies what a source found; it never removes a tag.
        removals: Removals::default(),
        cover_url: cover_url.map(str::to_string),
        cover_bytes: None,
        cover_mime: None,
        dry_run: false,
    };
    let mut saved = 0usize;
    let mut warnings = 0usize;
    let mut error: Option<String> = None;
    apply_album_changes(env, &req, &mut |event| {
        match event {
            ApplyEvent::Tags(TagEvent::FileDone(c)) if c.saved => {
                saved += 1;
                warnings += c.warnings.len();
            }
            ApplyEvent::Error { detail } => {
                let detail = if detail.is_empty() {
                    "apply failed".to_string()
                } else {
                    detail
                };
                error = Some(detail);
            }
            _ => {}
        }
        ControlFlow::Continue(())
    });
    match error {
        Some(e) => (false, e),
        None => {
            let mut msg = format!("Wrote {saved} file{}", if saved != 1 { "s" } else { "" });
            // Each warning is in the log (musaic.tags); the fix outcome
            // only says there were some.
            if warnings > 0 {
                let s = if warnings != 1 { "s" } else { "" };
                msg.push_str(&format!(
                    " ({warnings} warning{s} about what rewritten files could not keep; see the log)"
                ));
            }
            (true, msg)
        }
    }
}
