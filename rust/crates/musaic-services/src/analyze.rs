//! Per-album audio analysis: port of
//! `musaic/services/audio_analysis.py::analyze_album_iter` and the album
//! lookup of `musaic/api/analyze.py` (`POST /api/albums/{id}/analyze/stream`).
//!
//! # Shape
//!
//! [`analyze_album`] hands the sink one [`ApplyItem`] per item the Python app's
//! generator yields, in its order and with its payloads:
//!
//! ```text
//! start         {what, file_count}
//! file_started  {index, filename, path}                       per FLAC
//! file_done     {index, filename, bpm}                        BPM
//!               {index, filename, replaygain_track_gain, replaygain_track_peak}
//!               {index, filename, error}                      on any per-file failure
//!               + `warnings: [str]` when a write rewrote the file and could not
//!               keep some of its attributes (not in the Python app)
//! album_done    {replaygain_album_gain, replaygain_album_peak} ReplayGain, when the
//!                                                              aggregate exists
//! updated_album <scanner dict of the rescanned album>          (not the merged row)
//! done          {what, analyzed_count, errors}
//! ```
//!
//! With no FLAC in the directory: `start {file_count: 0}`, `done`, and
//! nothing else (no rescan, no upsert, no event). `AlbumTagsChanged(album_id,
//! fields=["bpm"] | ["replaygain"])` is published after `updated_album` has
//! been handed out and before `done`, whenever a library dir is set, also
//! when every file failed.
//!
//! Tags are written straight to the FLAC (`_write_tags`: `audio[key] = value`
//! then save), through musaic-tags: `bpm` = `"%.2f"`; per track
//! `replaygain_track_gain`, `replaygain_track_peak`,
//! `replaygain_reference_loudness`; then `replaygain_album_gain` and
//! `replaygain_album_peak` on every listed FLAC whose track step succeeded.
//! A read-only FLAC is refused by the tag writer (a tag save checks the
//! file is writable first), which is what the Python app's
//! mutagen save does too.
//!
//! # Deliberate changes from the Python app
//!
//! - A run goes to completion whatever the consumer does, as an
//!   apply does: once the sink returns [`ControlFlow::Break`] (or
//!   panics) it is never called again, but every remaining track is
//!   analysed and written, the album gains are written, the album is
//!   rescanned and upserted and `AlbumTagsChanged` is published. Only the
//!   forwarding stops ([`AnalyzeOutcome::Stopped`]). In the Python app the
//!   closed generator stopped at its suspended yield, leaving new track
//!   gains beside old (or no) album gains, or skipping the publish.
//! - An album whose row is Missing is refused with 409 and the same text
//!   apply uses, before anything is read: [`resolve_album`] answers it, and
//!   [`analyze_album`] itself hands out nothing and returns
//!   [`AnalyzeOutcome::Refused`]. The Python app analysed it when the directory
//!   was there (the row stayed Missing) and raised inside the stream when it
//!   was not.
//! - A file whose track step failed (analysis or tag write) is
//!   counted once in `errors` and is skipped by the album-tag loop: it gets
//!   no album gain (its track gain is not part of the album measurement
//!   either). The Python app wrote album tags to it anyway and, when that write
//!   failed too, counted the file a second time without an item. A failed
//!   album-tag write on a file whose track step succeeded still counts once.
//! - An album whose row is Present but whose directory cannot be
//!   listed (gone, unmounted) is refused with 409 before streaming
//!   ([`unreachable_album_detail`]; the status and wording of
//!   delete-duplicate's "unreachable album directory" answer):
//!   [`resolve_album`] answers it and [`analyze_album`] refuses too. The
//!   Python app answered 200 and its stream
//!   ended with no frame. Only a directory that vanishes between that check
//!   and the listing still ends as [`AnalyzeOutcome::Aborted`].
//!
//! # Per-file error text
//!
//! An OS error renders as Python's `OSError.__str__`
//! (`[Errno 13] Permission denied: '<path>'`); a file without a FLAC
//! signature as libsndfile's `Error opening '<path>': Format not recognised.`
//! Any other decode failure carries this build's own message (the Python
//! app's came from libsndfile / librosa and is not reproducible: only the
//! presence of the error is compared, except where its text is
//! reproducible, like permission denied).

use std::io;
use std::ops::ControlFlow;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};

use musaic_dsp::loudness::{
    REPLAYGAIN_REFERENCE_LUFS, aggregate_album, format_gain, format_peak, reference_loudness_tag,
};
use musaic_events::{Bus, Event as BusEvent};
use musaic_scan::{TrackOrder, py_resolve};
use rusqlite::Connection;

use crate::albums::{self, album_to_json};
use crate::apply::{ApplyItem, HttpError, ResolvedAlbum, missing_album_detail, refuse_missing};
use crate::json::Json;

/// `AnalysisKind`: the request body's `what`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnalysisKind {
    Bpm,
    ReplayGain,
}

impl AnalysisKind {
    /// The wire value (`"bpm"` / `"replaygain"`).
    pub fn as_str(self) -> &'static str {
        match self {
            AnalysisKind::Bpm => "bpm",
            AnalysisKind::ReplayGain => "replaygain",
        }
    }

    /// Parse the wire value; anything else is rejected by the router (422).
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "bpm" => Some(AnalysisKind::Bpm),
            "replaygain" => Some(AnalysisKind::ReplayGain),
            _ => None,
        }
    }
}

/// The two measurements, behind a seam (the Python app's `_detect_bpm` /
/// `_measure_track_loudness`, which its own tests stub the same way).
/// Errors are the text that ends up in `file_done.error`.
pub trait Analyzer {
    /// Tempo in BPM, unrounded.
    fn bpm(&self, path: &Path) -> Result<f64, String>;
    /// `(integrated LUFS, sample peak)`.
    fn loudness(&self, path: &Path) -> Result<(f64, f64), String>;
}

/// The real analyzer: musaic-bpm (librosa port) and musaic-dsp (pyloudnorm port).
pub struct DspAnalyzer;

impl Analyzer for DspAnalyzer {
    fn bpm(&self, path: &Path) -> Result<f64, String> {
        musaic_bpm::detect_bpm(path).map_err(|e| decode_error_text(path, &e.to_string()))
    }

    fn loudness(&self, path: &Path) -> Result<(f64, f64), String> {
        musaic_dsp::loudness::track_replaygain_file(path)
            .map(|t| (t.lufs, t.peak))
            .map_err(|e| match e {
                musaic_dsp::loudness::LoudnessError::Decode(d) => {
                    decode_error_text(path, &d.to_string())
                }
                other => other.to_string(),
            })
    }
}

/// libsndfile's text for a file it cannot identify, else `ours`.
fn decode_error_text(path: &Path, ours: &str) -> String {
    let mut magic = [0u8; 4];
    let is_flac = std::fs::File::open(path)
        .and_then(|mut f| io::Read::read_exact(&mut f, &mut magic))
        .is_ok()
        && &magic == b"fLaC";
    if is_flac {
        ours.to_string()
    } else {
        format!(
            "Error opening {}: Format not recognised.",
            musaic_fsops::py_repr(&path.to_string_lossy())
        )
    }
}

/// `str(OSError)` for an error about `path`: `[Errno N] <strerror>: '<path>'`.
///
/// On Windows the OS reports Win32 codes; Python's `open()` there goes
/// through the C runtime, which turns the common ones into errno values, so
/// those are translated the same way (a read-only file is
/// `[Errno 13] Permission denied`, as on every other platform).
pub fn py_os_error_text(e: &io::Error, path: &Path) -> String {
    if cfg!(windows) {
        let crt = match e.raw_os_error() {
            // ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND
            Some(2 | 3) => Some((2, "No such file or directory")),
            // ERROR_ACCESS_DENIED, ERROR_SHARING_VIOLATION, ERROR_LOCK_VIOLATION
            Some(5 | 32 | 33) => Some((13, "Permission denied")),
            _ => None,
        };
        if let Some((code, strerror)) = crt {
            return format!(
                "[Errno {code}] {strerror}: {}",
                musaic_fsops::py_repr(&path.to_string_lossy())
            );
        }
    }
    match e.raw_os_error() {
        Some(code) => {
            let full = e.to_string();
            let suffix = format!(" (os error {code})");
            let strerror = full.strip_suffix(&suffix).unwrap_or(&full);
            format!(
                "[Errno {code}] {strerror}: {}",
                musaic_fsops::py_repr(&path.to_string_lossy())
            )
        }
        None => e.to_string(),
    }
}

/// What to analyse (`analyze_album_iter`'s keyword arguments).
#[derive(Debug, Clone)]
pub struct AnalyzeRequest {
    pub album_id: String,
    pub album_path: String,
    /// The configured library root; `None` or `""` skips the upsert and the
    /// publish (the Python app's `""`).
    pub library_dir: Option<String>,
    pub what: AnalysisKind,
}

/// Everything the pipeline touches besides the album directory.
pub struct AnalyzeEnv<'a> {
    pub conn: &'a mut Connection,
    pub bus: &'a Bus,
    pub analyzer: &'a dyn Analyzer,
    /// `_now_iso()` for the libraries row the upsert may create.
    pub now: &'a dyn Fn() -> String,
    pub track_order: TrackOrder,
}

/// How a run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnalyzeOutcome {
    /// The sink was handed every item, `done` last.
    Finished,
    /// The sink returned `Break` (or panicked) and was not called again; the
    /// run itself went to completion.
    Stopped,
    /// The album's row is Missing; nothing was read, written or
    /// handed out. The router answers this before streaming.
    Refused(HttpError),
    /// The Python app raised inside the stream (the album directory cannot be
    /// listed); no item was produced. The text is the exception's `str()`,
    /// for the log: the client sees the response end without a final frame.
    Aborted(String),
}

/// `services.albums.resolve_album_dict` as the analyze router uses it: 409
/// without a configured library; the cached row, refused with 409 when it is
/// Missing or its directory cannot be listed; else a
/// live walk of the library; else 404.
pub fn resolve_album(
    conn: &Connection,
    library_dir: Option<&str>,
    album_id: &str,
    order: TrackOrder,
) -> Result<ResolvedAlbum, HttpError> {
    let Some(lib) = library_dir.filter(|l| !l.is_empty()) else {
        return Err(HttpError {
            status: 409,
            detail: "No library directory configured".into(),
        });
    };
    let resolved = py_resolve(Path::new(lib)).to_string_lossy().into_owned();
    let cached = albums::load_snapshot(conn, &resolved).map_err(|e| HttpError {
        status: 500,
        detail: e.to_string(),
    })?;
    if let Some(row) = cached.into_iter().find(|r| r.id() == album_id) {
        if let Some(since) = row.missing_since.as_deref().filter(|s| !s.is_empty()) {
            return Err(HttpError {
                status: 409,
                detail: missing_album_detail(since),
            });
        }
        refuse_unreachable(Path::new(&row.album.path))?;
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
        None => Err(HttpError {
            status: 404,
            detail: "Album not found (try rescanning the library)".into(),
        }),
    }
}

/// The 409 for a Present row whose album
/// directory cannot be listed (gone, not a directory, an unmounted drive, a
/// permission flicker). Same status and wording as the refusal of
/// delete-duplicate for an unreachable album directory. The Python app answered
/// 200 and its stream ended with no frame (`iterdir()` raised).
pub fn unreachable_album_detail(album_dir: &Path, why: &dyn std::fmt::Display) -> String {
    format!(
        "Album directory {} cannot be reached ({why}); nothing was analyzed. \
         Reconnect the drive and try again.",
        musaic_fsops::py_repr(&album_dir.to_string_lossy())
    )
}

fn refuse_unreachable(album_dir: &Path) -> Result<(), HttpError> {
    std::fs::read_dir(album_dir)
        .map(|_| ())
        .map_err(|e| HttpError {
            status: 409,
            detail: unreachable_album_detail(album_dir, &e),
        })
}

/// Hands items to the sink until it stops reading (the run goes on either
/// way). A panic in the sink counts as `Break`.
struct Forward<'s> {
    sink: &'s mut dyn FnMut(ApplyItem) -> ControlFlow<()>,
    gone: bool,
}

impl Forward<'_> {
    fn emit(&mut self, event: &'static str, data: Json) {
        if self.gone {
            return;
        }
        let sink = &mut *self.sink;
        self.gone = !matches!(
            catch_unwind(AssertUnwindSafe(|| sink(ApplyItem { event, data }))),
            Ok(ControlFlow::Continue(()))
        );
    }
}

/// `_write_tags`: set each key (removing every case variant, appending at
/// the end, as `audio[key] = value`) and save.
///
/// `Ok` carries what the save could not carry over from a rewritten file
/// ([`musaic_tags::SaveReport::warnings`]).
fn write_tags(path: &Path, updates: &[(&str, &str)]) -> Result<Vec<String>, String> {
    let mut f = musaic_tags::FlacFile::open(path).map_err(|e| tag_error_text(&e, path))?;
    for (k, v) in updates {
        f.tags_mut().set(k, v);
    }
    f.save()
        .map(|r| r.warnings)
        .map_err(|e| tag_error_text(&e, path))
}

/// Adds `warnings` to a `file_done` / `album_done` payload, only when there
/// are any (the event is the Python app's otherwise).
fn with_warnings(mut item: Json, warnings: Vec<String>) -> Json {
    if !warnings.is_empty() {
        item.set(
            "warnings",
            Json::Arr(warnings.into_iter().map(Json::from).collect()),
        );
    }
    item
}

fn tag_error_text(e: &io::Error, path: &Path) -> String {
    py_os_error_text(e, path)
}

/// `round(x, 2)`: correctly rounded on the exact binary value.
fn round2(x: f64) -> f64 {
    format!("{x:.2}").parse().unwrap_or(x)
}

fn file_done_error(index: usize, filename: &str, error: String) -> Json {
    Json::obj([
        ("index", Json::from(index)),
        ("filename", Json::from(filename)),
        ("error", Json::from(error)),
    ])
}

/// Run one analysis, handing every item to `sink` as it happens. See the
/// module docs for the sequence, the stop rule and the Missing-album case.
pub fn analyze_album(
    env: &mut AnalyzeEnv<'_>,
    req: &AnalyzeRequest,
    sink: &mut dyn FnMut(ApplyItem) -> ControlFlow<()>,
) -> AnalyzeOutcome {
    if let Err(refusal) = refuse_missing(env.conn, &req.album_id) {
        return AnalyzeOutcome::Refused(refusal);
    }
    if let Err(refusal) = refuse_unreachable(Path::new(&req.album_path)) {
        return AnalyzeOutcome::Refused(refusal);
    }
    let mut fwd = Forward { sink, gone: false };
    match run(env, req, &mut fwd) {
        Ok(()) if fwd.gone => AnalyzeOutcome::Stopped,
        Ok(()) => AnalyzeOutcome::Finished,
        Err(detail) => AnalyzeOutcome::Aborted(detail),
    }
}

/// `Err` = the Python app would have raised inside the stream (the text, for
/// the log).
fn run(
    env: &mut AnalyzeEnv<'_>,
    req: &AnalyzeRequest,
    fwd: &mut Forward<'_>,
) -> Result<(), String> {
    let album_dir = Path::new(&req.album_path);
    let what = req.what.as_str();

    // `album_path.iterdir()`: raises before anything is yielded.
    if let Err(e) = std::fs::read_dir(album_dir) {
        return Err(py_os_error_text(&e, album_dir));
    }
    let flacs: Vec<PathBuf> = musaic_scan::list_flacs(album_dir);

    fwd.emit(
        "start",
        Json::obj([
            ("what", Json::from(what)),
            ("file_count", Json::from(flacs.len())),
        ]),
    );
    if flacs.is_empty() {
        fwd.emit(
            "done",
            Json::obj([
                ("what", Json::from(what)),
                ("analyzed_count", Json::from(0usize)),
                ("errors", Json::from(0usize)),
            ]),
        );
        return Ok(());
    }

    let mut analyzed_count = 0usize;
    let mut errors = 0usize;
    // (duration, lufs, peak) of every track whose step succeeded.
    let mut rg_tracks: Vec<(f64, f64, f64)> = Vec::new();
    // The files whose track step failed (no album tags for them).
    let mut failed = vec![false; flacs.len()];

    for (index, path) in flacs.iter().enumerate() {
        let filename = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        fwd.emit(
            "file_started",
            Json::obj([
                ("index", Json::from(index)),
                ("filename", Json::from(filename.as_str())),
                ("path", Json::from(path.to_string_lossy().into_owned())),
            ]),
        );
        let done = match req.what {
            AnalysisKind::Bpm => env.analyzer.bpm(path).and_then(|bpm| {
                let warnings = write_tags(path, &[("bpm", &musaic_bpm::format_bpm_tag(bpm))])?;
                Ok(with_warnings(
                    Json::obj([
                        ("index", Json::from(index)),
                        ("filename", Json::from(filename.as_str())),
                        ("bpm", Json::from(round2(bpm))),
                    ]),
                    warnings,
                ))
            }),
            AnalysisKind::ReplayGain => env.analyzer.loudness(path).and_then(|(lufs, peak)| {
                let gain = format_gain(REPLAYGAIN_REFERENCE_LUFS, lufs);
                let peak_str = format_peak(peak);
                let warnings = write_tags(
                    path,
                    &[
                        ("replaygain_track_gain", &gain),
                        ("replaygain_track_peak", &peak_str),
                        ("replaygain_reference_loudness", &reference_loudness_tag()),
                    ],
                )?;
                // `FLAC(path).info.length or 0.0`, re-read after the write.
                let duration = musaic_scan::flacmeta::read(path)
                    .map_err(|e| tag_error_text(&e, path))?
                    .info
                    .length();
                rg_tracks.push((duration, lufs, peak));
                Ok(with_warnings(
                    Json::obj([
                        ("index", Json::from(index)),
                        ("filename", Json::from(filename.as_str())),
                        ("replaygain_track_gain", Json::from(gain)),
                        ("replaygain_track_peak", Json::from(peak_str)),
                    ]),
                    warnings,
                ))
            }),
        };
        let item = match done {
            Ok(j) => {
                analyzed_count += 1;
                j
            }
            Err(e) => {
                errors += 1;
                failed[index] = true;
                file_done_error(index, &filename, e)
            }
        };
        fwd.emit("file_done", item);
    }

    // Album-level ReplayGain, written to every listed FLAC whose track step
    // succeeded.
    if req.what == AnalysisKind::ReplayGain
        && !rg_tracks.is_empty()
        && let Some(agg) = aggregate_album(&rg_tracks)
    {
        let mut album_warnings = Vec::new();
        for (path, _) in flacs.iter().zip(&failed).filter(|(_, f)| !**f) {
            match write_tags(
                path,
                &[
                    ("replaygain_album_gain", &agg.album_gain),
                    ("replaygain_album_peak", &agg.album_peak),
                ],
            ) {
                Ok(warnings) => {
                    let name = path.file_name().unwrap_or_default().to_string_lossy();
                    album_warnings.extend(warnings.into_iter().map(|w| format!("{name}: {w}")));
                }
                Err(_) => errors += 1,
            }
        }
        fwd.emit(
            "album_done",
            with_warnings(
                Json::obj([
                    ("replaygain_album_gain", Json::from(agg.album_gain.as_str())),
                    ("replaygain_album_peak", Json::from(agg.album_peak.as_str())),
                ]),
                album_warnings,
            ),
        );
    }

    // Rescan this album, upsert, hand out the scanner dict.
    let library_dir = req.library_dir.as_deref().filter(|l| !l.is_empty());
    if let Some(fresh) = musaic_scan::scan_album(album_dir, env.track_order) {
        if let Some(lib) = library_dir {
            let lib = py_resolve(Path::new(lib)).to_string_lossy().into_owned();
            // The Python app logs nothing here: a failing upsert raises out of
            // the stream after the 200 (same as the iterdir case).
            musaic_db::albums::upsert_album(env.conn, &lib, &(env.now)(), &fresh)
                .map_err(|e| e.to_string())?;
        }
        fwd.emit("updated_album", album_to_json(&fresh));
    }

    // Published only once the generator is resumed after updated_album.
    if library_dir.is_some() {
        env.bus.publish(&BusEvent::AlbumTagsChanged {
            album_id: req.album_id.clone(),
            fields: vec![what.to_string()],
        });
    }

    fwd.emit(
        "done",
        Json::obj([
            ("what", Json::from(what)),
            ("analyzed_count", Json::from(analyzed_count)),
            ("errors", Json::from(errors)),
        ]),
    );
    Ok(())
}
