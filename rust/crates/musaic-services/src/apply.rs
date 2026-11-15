//! The single album mutation pipeline: port of
//! `musaic/services/apply.py::apply_album_changes_iter`, the request checks of
//! `musaic/api/apply.py`, and the draining consumer `checks/_apply.py::apply_direct`.
//!
//! # Shape
//!
//! [`apply_album_changes`] calls a sink once per item the Python app's generator
//! yields, in the same order and with the same `(event, payload)`. The sink
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
use musaic_tags::{Event as TagEvent, Removals, Suggestions, TrackSuggestions};
use rusqlite::Connection;

use crate::albums::{self, album_to_json};
use crate::json::Json;

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

/// One item of the stream: the SSE event name and its JSON payload.
#[derive(Debug, Clone, PartialEq)]
pub struct ApplyItem {
    pub event: &'static str,
    pub data: Json,
}

impl ApplyItem {
    /// `services.sse.sse_event(event, data)`.
    pub fn sse_frame(&self) -> String {
        format!("event: {}\ndata: {}\n\n", self.event, self.data.dumps())
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

/// Deliberate change from the Python app: `Err(409)`
/// when the album's row is Missing (`missing_since` set and not empty, the
/// runner's test). An album without a row is not refused here.
pub fn refuse_missing(conn: &Connection, album_id: &str) -> Result<(), HttpError> {
    match musaic_db::albums::missing_since(conn, album_id) {
        Ok(Some(since)) if !since.is_empty() => {
            Err(HttpError::new(409, missing_album_detail(&since)))
        }
        Ok(_) => Ok(()),
        Err(e) => Err(HttpError::new(500, e.to_string())),
    }
}

/// The text of the failure for an upload of zero bytes (the upload
/// route itself already answers 400 with this text).
pub const EMPTY_UPLOAD_DETAIL: &str = "Empty file";

/// Hands items to the consumer until it stops reading.
struct Forward<'s> {
    sink: &'s mut dyn FnMut(ApplyItem) -> ControlFlow<()>,
    gone: bool,
    dry_run: bool,
}

impl Forward<'_> {
    /// Forward one item unless the consumer is gone. A panic in the sink
    /// counts as `Break`. `Err(Stopped)` only for a dry run whose consumer is
    /// gone (nothing is mutated, so it may end early).
    fn emit(&mut self, event: &'static str, data: Json) -> Result<(), Halt> {
        if !self.gone {
            let sink = &mut *self.sink;
            let flow = catch_unwind(AssertUnwindSafe(|| sink(ApplyItem { event, data })))
                .unwrap_or(ControlFlow::Break(()));
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

/// `cleared` is not in the Python app's payloads: added only when not empty,
/// so an apply without removals sends exactly what it always did.
fn with_cleared(mut j: Json, cleared: &[String]) -> Json {
    if !cleared.is_empty() {
        j.set(
            "cleared",
            Json::Arr(cleared.iter().map(|f| Json::from(f.as_str())).collect()),
        );
    }
    j
}

fn str_map(pairs: &[(String, String)]) -> Json {
    Json::obj(
        pairs
            .iter()
            .map(|(k, v)| (k.clone(), Json::from(v.as_str()))),
    )
}

/// The payload `tags.apply_to_album_iter` yields for each event.
pub fn tag_event_json(ev: &TagEvent) -> Json {
    match ev {
        TagEvent::CoverDownloaded {
            source_bytes,
            source_mime,
            bytes,
            mime,
        } => Json::obj([
            ("source_bytes", Json::from(*source_bytes)),
            ("source_mime", Json::from(source_mime.as_str())),
            ("bytes", Json::from(*bytes)),
            ("mime", Json::from(mime.as_str())),
        ]),
        TagEvent::Start {
            album_path,
            dry_run,
            file_count,
            suggestions_applied,
            cleared,
            has_cover_change,
            track_change_count,
        } => with_cleared(
            Json::obj([
                ("album_path", Json::from(path_str(album_path))),
                ("dry_run", Json::from(*dry_run)),
                ("file_count", Json::from(*file_count)),
                ("suggestions_applied", str_map(suggestions_applied)),
                ("has_cover_change", Json::from(*has_cover_change)),
                ("track_change_count", Json::from(*track_change_count)),
            ]),
            cleared,
        ),
        TagEvent::FileStarted {
            index,
            filename,
            path,
        } => Json::obj([
            ("index", Json::from(*index)),
            ("filename", Json::from(filename.as_str())),
            ("path", Json::from(path_str(path))),
        ]),
        TagEvent::FileDone(c) => {
            let mut j = Json::obj([
                ("path", Json::from(path_str(&c.path))),
                ("filename", Json::from(c.filename.as_str())),
                (
                    "diffs",
                    Json::obj(c.diffs.iter().map(|(k, d)| {
                        (
                            k.clone(),
                            Json::obj([
                                ("old", Json::from(d.old.as_str())),
                                // null: the tag is removed.
                                ("new", Json::from(d.new.clone())),
                            ]),
                        )
                    })),
                ),
                ("art_changed", Json::from(c.art_changed)),
                ("saved", Json::from(c.saved)),
                ("error", Json::from(c.error.clone())),
            ]);
            if let Some(r) = &c.renamed_to {
                j.set("renamed_to", Json::from(r.as_str()));
            }
            // Only when there is something to say, so the event stays the
            // Python app's otherwise.
            if !c.warnings.is_empty() {
                j.set(
                    "warnings",
                    Json::Arr(c.warnings.iter().map(|w| Json::from(w.as_str())).collect()),
                );
            }
            j
        }
        TagEvent::FileRenamed {
            old_path,
            new_path,
            old_filename,
            new_filename,
        } => Json::obj([
            ("old_path", Json::from(path_str(old_path))),
            ("new_path", Json::from(path_str(new_path))),
            ("old_filename", Json::from(old_filename.as_str())),
            ("new_filename", Json::from(new_filename.as_str())),
        ]),
        TagEvent::Done {
            album_path,
            dry_run,
            saved_count,
            art_changed_count,
            renamed_count,
            errors,
            suggestions_applied,
            cleared,
        } => with_cleared(
            Json::obj([
                ("album_path", Json::from(path_str(album_path))),
                ("dry_run", Json::from(*dry_run)),
                ("saved_count", Json::from(*saved_count)),
                ("art_changed_count", Json::from(*art_changed_count)),
                ("renamed_count", Json::from(*renamed_count)),
                ("errors", Json::from(*errors)),
                ("suggestions_applied", str_map(suggestions_applied)),
            ]),
            cleared,
        ),
        TagEvent::CoverSaved { path, filename } => Json::obj([
            ("path", Json::from(path_str(path))),
            ("filename", Json::from(filename.as_str())),
        ]),
    }
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
    sink: &mut dyn FnMut(ApplyItem) -> ControlFlow<()>,
) -> ApplyOutcome {
    let mut fwd = Forward {
        sink,
        gone: false,
        dry_run: req.dry_run,
    };
    if let Err(refusal) = refuse_missing(env.conn, &req.album_id) {
        let _ = fwd.emit("error", Json::obj([("detail", Json::from(refusal.detail))]));
        return fwd.outcome();
    }
    let cover_url = req.cover_url.as_deref().filter(|u| !u.is_empty());
    if cover_url.is_some() && req.cover_bytes.is_some() {
        let detail = "Pass cover_url OR cover_bytes — not both. (Caller bug.)";
        let _ = fwd.emit("error", Json::obj([("detail", Json::from(detail))]));
        return fwd.outcome();
    }
    let mut emit = |event: &'static str, data: Json| fwd.emit(event, data);
    let result = run(env, req, cover_url, &mut emit);
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
            let _ = fwd.emit("error", Json::obj([("detail", Json::from(detail))]));
        }
        Err(Halt::Refused(detail)) => {
            let _ = fwd.emit("error", Json::obj([("detail", Json::from(detail))]));
        }
        Ok(()) | Err(Halt::Stopped) => {}
    }
    fwd.outcome()
}

fn run(
    env: &mut ApplyEnv<'_>,
    req: &ApplyRequest,
    cover_url: Option<&str>,
    emit: &mut dyn FnMut(&'static str, Json) -> Result<(), Halt>,
) -> Result<(), Halt> {
    let album_dir = Path::new(&req.album_path);

    // 1. Cover bytes, normalised to JPEG q95.
    let mut final_cover: Option<Vec<u8>> = None;
    if let Some(url) = cover_url {
        emit("cover_downloading", Json::obj([("url", Json::from(url))]))?;
        // Zero bytes are a failed download.
        let Some((raw, raw_mime)) = env.covers.download(url).filter(|(b, _)| !b.is_empty()) else {
            return Err(Halt::Refused(format!(
                "Failed to download cover from {url}"
            )));
        };
        let out = musaic_tags::normalize_to_jpeg(&raw, 95);
        let data = tag_event_json(&TagEvent::CoverDownloaded {
            source_bytes: raw.len(),
            source_mime: raw_mime,
            bytes: out.len(),
            mime: "image/jpeg".into(),
        });
        final_cover = Some(out);
        emit("cover_downloaded", data)?;
    } else if let Some(raw) = &req.cover_bytes {
        // An empty upload is a failed download too.
        if raw.is_empty() {
            return Err(Halt::Refused(EMPTY_UPLOAD_DETAIL.into()));
        }
        let out = musaic_tags::normalize_to_jpeg(raw, 95);
        let data = tag_event_json(&TagEvent::CoverDownloaded {
            source_bytes: raw.len(),
            source_mime: req
                .cover_mime
                .clone()
                .filter(|m| !m.is_empty())
                .unwrap_or_else(|| "image/jpeg".into()),
            bytes: out.len(),
            mime: "image/jpeg".into(),
        });
        final_cover = Some(out);
        emit("cover_downloaded", data)?;
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
        let _ = emit(ev.name(), tag_event_json(&ev));
    };
    musaic_tags::apply_to_album_iter(
        album_dir,
        &req.suggestions,
        &req.track_suggestions,
        &req.removals,
        picture,
        dry_run,
        &mut callback,
    )
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
        emit(
            "cover_saved",
            Json::obj([
                ("path", Json::from(path_str(&saved))),
                ("filename", Json::from(filename)),
            ]),
        )?;
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
        return emit(
            "removed_album",
            Json::obj([
                ("id", Json::from(req.album_id.as_str())),
                ("path", Json::from(req.album_path.as_str())),
            ]),
        );
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
        fields: changed_fields(&req.suggestions, &req.removals),
    });
    if had_art_change {
        env.bus.publish(&BusEvent::AlbumCoverChanged {
            album_id: req.album_id.clone(),
        });
    }
    emit("updated_album", fresh_json)
}

/// An HTTP refusal the router would raise before streaming anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpError {
    pub status: u16,
    pub detail: String,
}

impl HttpError {
    fn new(status: u16, detail: impl Into<String>) -> Self {
        HttpError {
            status,
            detail: detail.into(),
        }
    }
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

/// `services.albums.resolve_album_dict`: 409 without a configured library;
/// the cached row; else a live walk of the library; else 404.
///
/// Deliberate change: a cached row that is Missing is refused with
/// 409 ([`missing_album_detail`], which says to rescan) before anything is
/// read or written, dry runs and cover uploads included. The Python app returned
/// the row and let the apply run.
pub fn resolve_album(
    conn: &Connection,
    library_dir: Option<&str>,
    album_id: &str,
    order: TrackOrder,
) -> Result<ResolvedAlbum, HttpError> {
    let Some(lib) = library_dir.filter(|l| !l.is_empty()) else {
        return Err(HttpError::new(409, "No library directory configured"));
    };
    let resolved = path_str(&py_resolve(Path::new(lib)));
    let cached =
        albums::load_snapshot(conn, &resolved).map_err(|e| HttpError::new(500, e.to_string()))?;
    if let Some(row) = cached.into_iter().find(|r| r.id() == album_id) {
        // A Missing album is refused before anything else happens.
        if let Some(since) = row.missing_since.as_deref().filter(|s| !s.is_empty()) {
            return Err(HttpError::new(409, missing_album_detail(since)));
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
        None => Err(HttpError::new(
            404,
            "Album not found (try rescanning the library)",
        )),
    }
}

/// `_MAX_COVER_UPLOAD_BYTES`.
pub const MAX_COVER_UPLOAD_BYTES: usize = 25 * 1024 * 1024;

/// The checks `POST /api/albums/{id}/cover/upload` makes BEFORE resolving
/// the album, in its order: MIME prefix `image/` (400), empty body (400),
/// size cap (413). Returns the lower-cased MIME to pass as `cover_mime`.
pub fn validate_cover_upload(content_type: Option<&str>, body: &[u8]) -> Result<String, HttpError> {
    let mime = content_type.unwrap_or("").to_lowercase();
    if !mime.starts_with("image/") {
        return Err(HttpError::new(
            400,
            format!(
                "Unsupported file type {} — please upload an image",
                musaic_fsops::py_repr(&mime)
            ),
        ));
    }
    if body.is_empty() {
        return Err(HttpError::new(400, "Empty file"));
    }
    if body.len() > MAX_COVER_UPLOAD_BYTES {
        return Err(HttpError::new(
            413,
            format!(
                "Cover too large ({} bytes). Max accepted is {} MB.",
                body.len(),
                MAX_COVER_UPLOAD_BYTES / (1024 * 1024)
            ),
        ));
    }
    Ok(mime)
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
    apply_album_changes(env, &req, &mut |item| {
        if item.event == "file_done" && item.data.get("saved") == Some(&Json::Bool(true)) {
            saved += 1;
            if let Some(Json::Arr(w)) = item.data.get("warnings") {
                warnings += w.len();
            }
        } else if item.event == "error" {
            let detail = item
                .data
                .get("detail")
                .and_then(Json::as_str)
                .filter(|d| !d.is_empty())
                .unwrap_or("apply failed");
            error = Some(detail.to_string());
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
