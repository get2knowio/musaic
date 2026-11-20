//! The `apply` group: write corrections (tags, a cover, renames the numbering
//! implies) into an album's files, and the cover previews that precede one.
//!
//! [`Apply::apply_changes`] and [`Apply::upload_cover`] are runs with a sink, the
//! shape of [`analyze`](super::analyze): they refuse before they say anything
//! (`InvalidInput`, `Busy`, `NoLibrary`, `NotFound`, `Conflict`, and for an
//! upload `Rejected` / `TooLarge`: nothing was read or written and the sink was
//! never called), then hand the sink one [`ApplyEvent`] per step on the calling
//! thread and return how the run ended. The work guard is taken first and held
//! until the last event has been handed out. A sink that answers
//! [`Listening::Gone`] is not called again; a run that writes still writes every
//! file, saves the cover, updates the database and publishes `AlbumTagsChanged`
//! (and `AlbumCoverChanged` when art changed). A dry run writes nothing and may
//! end early.
//!
//! The previews fetch a proposed remote cover through `musaic-net`'s preview
//! rule (`https`, public hosts, every redirect re-checked, size cap, image
//! types only). Nothing is cached.

use std::ops::ControlFlow;
use std::panic::{AssertUnwindSafe, catch_unwind};

use musaic_net::preview::{PreviewError, fetch_preview};
use musaic_resolve::db_store::DbStore;
use musaic_resolve::resolver::anchored_cover_url;
use musaic_services::apply::{
    ApplyEvent as Step, ApplyRequest, apply_album_changes, missing_album_detail, resolve_album,
    stored_cover_url, validate_cover_upload,
};
pub use musaic_services::apply::{
    ApplyOutcome, MAX_COVER_UPLOAD_BYTES, TagEvent, check_cover_type,
};
use musaic_services::gate::WorkKind;
use musaic_services::{Removals, Subject};

use super::error::{OpError, Service};
pub use super::events::Listening;
use super::record::AlbumRecord;
use crate::App;

/// 404 message when the job stores no cover URL.
pub const NO_COVER_URL: &str = "This job has no suggested cover";
/// 404 message when the album has no MusicBrainz cover to propose.
pub const NO_ALBUM_COVER: &str = "This album has no MusicBrainz cover to preview";
/// 404 message for an album id with no row.
pub const ALBUM_NOT_FOUND: &str = "Album not found";

/// The log target the HTTP route used for a failed preview, kept so the server
/// log reads as before.
const PREVIEW_LOG_TARGET: &str = "musaic.api.preview";

/// One step of an apply run, in the order the pipeline reaches it: the cover
/// being fetched and normalised (`CoverDownloading`, `CoverDownloaded`, a cover
/// only), the per-file steps (`Tags`), the saved `cover.jpg` (`CoverSaved`), and
/// last `UpdatedAlbum` or `RemovedAlbum`. `Error` replaces whatever would have
/// followed. A dry run ends with the `Done` of its `Tags`.
pub type ApplyEvent = musaic_services::apply::ApplyEvent<AlbumRecord>;

/// A services apply event, with its album as a typed record.
pub fn typed(e: Step) -> ApplyEvent {
    e.map_album(AlbumRecord::new)
}

/// What to write. An empty or absent suggestion means "no change": only the
/// explicit `clear` / `track_clear` lists remove a tag.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ApplyChanges {
    /// Album-level fields to set, in the caller's order (that order is the
    /// order the tags are appended and `AlbumTagsChanged.fields` lists them).
    pub suggestions: Vec<(String, String)>,
    /// Per-track fields to set, by file name.
    pub track_suggestions: Vec<(String, Vec<(String, String)>)>,
    /// Album-level fields to remove from every file.
    pub clear: Vec<String>,
    /// Per-track fields to remove, by file name.
    pub track_clear: Vec<(String, Vec<String>)>,
    /// A cover to fetch and embed (and save as `cover.jpg`).
    pub cover_url: Option<String>,
    /// Report what would change, write nothing.
    pub dry_run: bool,
}

/// Where a [`RemovalProblem`] was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemovalTarget {
    /// `clear[index]`.
    Album { index: usize },
    /// `track_clear[filename][index]`.
    Track { filename: String, index: usize },
}

/// Why a field cannot be removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemovalFault {
    /// Not a field that can be removed; `allowed` are the ones that can.
    Unknown { allowed: Vec<String> },
    /// A field that can only be set (the track number: the scanner orders
    /// tracks by it).
    SetOnly,
    /// The same field is suggested and cleared for the same target.
    AlsoSuggested,
}

/// One entry of `clear` / `track_clear` that cannot be honoured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemovalProblem {
    pub target: RemovalTarget,
    pub field: String,
    pub fault: RemovalFault,
}

impl RemovalProblem {
    /// A sentence saying what is wrong.
    pub fn message(&self) -> String {
        let field = &self.field;
        match (&self.fault, &self.target) {
            (RemovalFault::Unknown { allowed }, _) => format!(
                "{field} is not a field that can be removed (one of {})",
                allowed.join(", ")
            ),
            (RemovalFault::SetOnly, _) => {
                format!("{field} cannot be removed; set a value instead")
            }
            (RemovalFault::AlsoSuggested, RemovalTarget::Album { .. }) => {
                format!("{field} is both in suggestions and in clear")
            }
            (RemovalFault::AlsoSuggested, RemovalTarget::Track { .. }) => {
                format!("{field} is both in track_suggestions and in track_clear")
            }
        }
    }
}

impl ApplyChanges {
    /// Every `clear` / `track_clear` entry that cannot be honoured, in request
    /// order (`clear` first): an unknown field, the track number, a field also
    /// suggested for the same target. Empty when the request is acceptable.
    pub fn removal_problems(&self) -> Vec<RemovalProblem> {
        let suggested =
            |pairs: &[(String, String)], field: &str| pairs.iter().any(|(k, _)| k == field);
        let mut out = Vec::new();
        for (index, field) in self.clear.iter().enumerate() {
            let fault = if !musaic_services::is_removable_album_field(field) {
                Some(RemovalFault::Unknown {
                    allowed: musaic_services::removable_album_fields()
                        .into_iter()
                        .map(str::to_string)
                        .collect(),
                })
            } else if suggested(&self.suggestions, field) {
                Some(RemovalFault::AlsoSuggested)
            } else {
                None
            };
            if let Some(fault) = fault {
                out.push(RemovalProblem {
                    target: RemovalTarget::Album { index },
                    field: field.clone(),
                    fault,
                });
            }
        }
        for (filename, fields) in &self.track_clear {
            let row = self
                .track_suggestions
                .iter()
                .find(|(f, _)| f == filename)
                .map_or(&[][..], |(_, r)| r.as_slice());
            for (index, field) in fields.iter().enumerate() {
                let fault = if musaic_services::TRACK_FIELDS_NOT_REMOVABLE.contains(&field.as_str())
                {
                    Some(RemovalFault::SetOnly)
                } else if !musaic_services::is_removable_track_field(field) {
                    Some(RemovalFault::Unknown {
                        allowed: musaic_services::removable_track_fields()
                            .into_iter()
                            .map(str::to_string)
                            .collect(),
                    })
                } else if suggested(row, field) {
                    Some(RemovalFault::AlsoSuggested)
                } else {
                    None
                };
                if let Some(fault) = fault {
                    out.push(RemovalProblem {
                        target: RemovalTarget::Track {
                            filename: filename.clone(),
                            index,
                        },
                        field: field.clone(),
                        fault,
                    });
                }
            }
        }
        out
    }
}

/// An apply whose checks passed: it holds the work guard and the album, and
/// writes nothing until [`run`](Self::run). Dropping it releases the guard.
#[must_use = "nothing is written until the run is run"]
pub struct ApplyRun {
    app: App,
    guard: musaic_services::gate::WorkGuard,
    req: ApplyRequest,
}

impl ApplyRun {
    /// Run the pipeline, reporting each step to `sink`. The guard is held until
    /// after the last event.
    pub fn run(
        self,
        sink: &mut dyn FnMut(ApplyEvent) -> Listening,
    ) -> Result<ApplyOutcome, OpError> {
        let ApplyRun { app, guard, req } = self;
        let _guard = guard;
        let mut listening = true;
        let mut forward = |event: Step| {
            if listening {
                // A sink that panics is treated as gone.
                listening = matches!(
                    catch_unwind(AssertUnwindSafe(|| sink(typed(event)))),
                    Ok(Listening::Yes)
                );
            }
            if listening {
                ControlFlow::Continue(())
            } else {
                // A run that writes finishes anyway; a dry run may stop.
                ControlFlow::Break(())
            }
        };
        let mut conn = match musaic_db::open_connection(app.db_path()) {
            Ok(c) => c,
            Err(_) => {
                let _ = forward(Step::Error {
                    detail: "database unavailable".into(),
                });
                return Ok(ApplyOutcome::Finished);
            }
        };
        Ok(app.with_apply_env(&mut conn, |env| {
            apply_album_changes(env, &req, &mut forward)
        }))
    }
}

/// A fetched remote cover: the image bytes and their type (a raster image the
/// fetch verified by its signature).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverPreview {
    pub bytes: Vec<u8>,
    pub mime: String,
}

/// The `apply` operations.
pub struct Apply<'a> {
    pub(super) app: &'a App,
}

fn sugg(pairs: &[(String, String)]) -> Vec<(String, Option<String>)> {
    pairs
        .iter()
        .map(|(k, v)| (k.clone(), Some(v.clone())))
        .collect()
}

impl Apply<'_> {
    /// Write `changes` into the album's files, reporting each step to `sink`:
    /// [`prepare_changes`](Self::prepare_changes), then [`ApplyRun::run`].
    pub fn apply_changes(
        &self,
        album_id: &str,
        changes: &ApplyChanges,
        sink: &mut dyn FnMut(ApplyEvent) -> Listening,
    ) -> Result<ApplyOutcome, OpError> {
        self.prepare_changes(album_id, changes)?.run(sink)
    }

    /// The checks of an apply, with nothing written: the returned run holds the
    /// work guard and the album found, and does the work when it is
    /// [run](ApplyRun::run). A face that may lose its caller between the two
    /// (a closed connection) drops the run instead, and nothing is written, as
    /// the HTTP route did when its client left during the album lookup.
    ///
    /// Refusals, all with nothing changed, in this order: a `clear` /
    /// `track_clear` entry that cannot be honoured (`InvalidInput`, see
    /// [`ApplyChanges::removal_problems`]); the work guard (`Busy` during a
    /// restore, a dry run included: it reads the database a restore swaps); no
    /// library folder (`NoLibrary`); an unknown album (`NotFound`); a Missing
    /// album (`Conflict`).
    pub fn prepare_changes(
        &self,
        album_id: &str,
        changes: &ApplyChanges,
    ) -> Result<ApplyRun, OpError> {
        if let Some(p) = changes.removal_problems().into_iter().next() {
            return Err(OpError::InvalidInput {
                field: match p.target {
                    RemovalTarget::Album { .. } => "clear".into(),
                    RemovalTarget::Track { .. } => "track_clear".into(),
                },
                message: p.message(),
            });
        }
        let guard = self.app.gate().begin(WorkKind::Apply)?;
        self.prepare(guard, album_id, |id, path, library_dir| ApplyRequest {
            album_id: id,
            album_path: path,
            library_dir: Some(library_dir),
            suggestions: sugg(&changes.suggestions),
            track_suggestions: changes
                .track_suggestions
                .iter()
                .map(|(k, v)| (k.clone(), sugg(v)))
                .collect(),
            removals: Removals {
                album: changes.clear.clone(),
                tracks: changes.track_clear.clone(),
            },
            cover_url: changes.cover_url.clone(),
            cover_bytes: None,
            cover_mime: None,
            dry_run: changes.dry_run,
        })
    }

    /// Embed an uploaded image as the album's cover (and save it as
    /// `cover.jpg`), reporting each step to `sink`:
    /// [`prepare_upload`](Self::prepare_upload), then [`ApplyRun::run`].
    pub fn upload_cover(
        &self,
        album_id: &str,
        content_type: Option<&str>,
        bytes: Vec<u8>,
        sink: &mut dyn FnMut(ApplyEvent) -> Listening,
    ) -> Result<ApplyOutcome, OpError> {
        self.prepare_upload(album_id, content_type, bytes)?
            .run(sink)
    }

    /// The checks of a cover upload, with nothing written, as
    /// [`prepare_changes`](Self::prepare_changes). `content_type` is the
    /// upload's declared type.
    ///
    /// Refusals, in this order: the work guard (`Busy`); a type that is not an
    /// image or an empty body (`Rejected`); a body over
    /// [`MAX_COVER_UPLOAD_BYTES`] (`TooLarge`); then the album's, as for
    /// [`prepare_changes`](Self::prepare_changes). A multipart request's own
    /// size cap is the face's.
    pub fn prepare_upload(
        &self,
        album_id: &str,
        content_type: Option<&str>,
        bytes: Vec<u8>,
    ) -> Result<ApplyRun, OpError> {
        // Not labelled `Apply`, as the route took it.
        let guard = self.app.gate().begin_work()?;
        let mime = validate_cover_upload(content_type, &bytes)?;
        self.prepare(guard, album_id, move |id, path, library_dir| ApplyRequest {
            album_id: id,
            album_path: path,
            library_dir: Some(library_dir),
            cover_bytes: Some(bytes),
            cover_mime: Some(mime),
            ..Default::default()
        })
    }

    /// With the guard held, find the album and build the request from it.
    fn prepare(
        &self,
        guard: musaic_services::gate::WorkGuard,
        album_id: &str,
        request: impl FnOnce(String, String, String) -> ApplyRequest,
    ) -> Result<ApplyRun, OpError> {
        let library = self.app.library_dir();
        let album = {
            let conn = musaic_db::open_connection(self.app.db_path())?;
            resolve_album(&conn, library.as_deref(), album_id, self.app.0.track_order)?
        };
        Ok(ApplyRun {
            app: self.app.clone(),
            guard,
            req: request(album.id, album.path, library.unwrap_or_default()),
        })
    }

    /// The cover a job suggested, fetched (`result.cover.cover_url` of the
    /// job's stored result). `NotFound` for an unknown job or one with no cover
    /// URL; `Upstream` when the fetch is refused or fails.
    pub fn job_cover_preview(&self, job_id: &str) -> Result<CoverPreview, OpError> {
        let url = {
            let conn = musaic_db::open_connection(self.app.db_path())?;
            let job =
                musaic_db::ai_jobs::get_job(&conn, job_id)?.ok_or_else(|| OpError::NotFound {
                    what: Subject::Job,
                    id: job_id.to_string(),
                    message: "Job not found".into(),
                })?;
            stored_cover_url(job.result_json.as_deref())
                .ok_or_else(|| no_cover(job_id, NO_COVER_URL))?
        };
        self.fetch(&url, &format!("job {job_id}"))
    }

    /// The cover the resolver proposes for the album, with no job: the Cover Art
    /// Archive front of the album's MusicBrainz anchor, which is derived here
    /// and never taken from the caller. `NotFound` for an unknown album and for
    /// one with no usable anchor or whose archive has no cover; `Conflict` for a
    /// Missing album (its cover cannot be changed, so a preview has no use);
    /// `Upstream` when the fetch is refused or fails otherwise.
    pub fn album_cover_preview(&self, album_id: &str) -> Result<CoverPreview, OpError> {
        let url = {
            let conn = musaic_db::open_connection(self.app.db_path())?;
            match musaic_db::albums::album_row_missing_since(&conn, album_id)? {
                None => {
                    return Err(OpError::NotFound {
                        what: Subject::Album,
                        id: album_id.to_string(),
                        message: ALBUM_NOT_FOUND.into(),
                    });
                }
                Some(Some(since)) if !since.is_empty() => {
                    return Err(OpError::Conflict {
                        message: missing_album_detail(&since),
                        details: None,
                    });
                }
                Some(_) => {}
            }
            let store = DbStore {
                conn: &conn,
                // Only stamped on writes; this reads.
                now_iso: String::new(),
                read: Default::default(),
            };
            anchored_cover_url(&store, album_id)
                .map_err(OpError::internal)?
                .ok_or_else(|| no_cover(album_id, NO_ALBUM_COVER))?
        };
        match fetch_preview(&self.app.0.transport, &url) {
            Ok(p) => Ok(CoverPreview {
                bytes: p.bytes,
                mime: p.mime,
            }),
            // The archive has no front cover for the group: what the
            // resolver's HEAD check reads as "Cover not in CAA".
            Err(PreviewError::Status(404)) => Err(no_cover(album_id, NO_ALBUM_COVER)),
            Err(e) => Err(self.unavailable(&format!("album {album_id}"), e)),
        }
    }

    fn fetch(&self, url: &str, subject: &str) -> Result<CoverPreview, OpError> {
        fetch_preview(&self.app.0.transport, url)
            .map(|p| CoverPreview {
                bytes: p.bytes,
                mime: p.mime,
            })
            .map_err(|e| self.unavailable(subject, e))
    }

    /// The refusal for a failed fetch, logged with what it was for.
    fn unavailable(&self, subject: &str, e: PreviewError) -> OpError {
        self.app.log().warn(
            PREVIEW_LOG_TARGET,
            &format!("cover preview for {subject} failed: {e}"),
        );
        OpError::Upstream {
            service: Service::CoverArtArchive,
            message: format!("Cover preview unavailable: {e}"),
        }
    }
}

fn no_cover(id: &str, message: &str) -> OpError {
    OpError::NotFound {
        what: Subject::Cover,
        id: id.to_string(),
        message: message.to_string(),
    }
}
