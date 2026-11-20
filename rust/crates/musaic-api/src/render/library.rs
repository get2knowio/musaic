//! The `library` and `scan` groups as bytes: bodies and SSE frames for the
//! library folder, albums, tracks, the anchor, the resolution and the scan.

use musaic_app::ops::library::{
    AlbumList, ClearedAnchor, DeletedAlbum, LibraryChoice, LibraryConfig, Resolution, TrackList,
};
use musaic_app::ops::record::{AlbumRecord, TrackRecord};
use musaic_app::ops::scan::{AlbumSource, CancelOutcome, RescanStarted, ScanEvent, ScanState};
use musaic_services::Json;
use musaic_services::scan::{ABORT_REASON, CANCEL_REASON};

/// A record's fields in stored order.
fn fields<'a>(it: impl Iterator<Item = (&'a str, &'a Json)>) -> Json {
    Json::obj(it.map(|(k, v)| (k, v.clone())))
}

fn album_dict(a: &AlbumRecord) -> Json {
    fields(a.iter())
}

fn track_dict(t: &TrackRecord) -> Json {
    fields(t.iter())
}

/// `GET /api/config`.
pub fn config(c: &LibraryConfig) -> Json {
    Json::obj([
        ("library_dir", Json::from(c.library_dir.clone())),
        ("exists", Json::Bool(c.exists)),
    ])
}

/// `POST /api/config`.
pub fn library_choice(choice: LibraryChoice) -> Json {
    let mut body = vec![
        ("library_dir", Json::from(choice.library_dir)),
        ("exists", Json::Bool(true)),
    ];
    // Present only when it happened, so the usual answer keeps its shape.
    if choice.kept_stored_spelling {
        body.push(("kept_stored_spelling", Json::Bool(true)));
    }
    Json::obj(body)
}

/// `GET /api/albums`.
pub fn album_list(out: AlbumList) -> Json {
    Json::obj([
        ("library_dir", Json::from(out.library_dir.clone())),
        ("count", Json::from(out.count())),
        ("scanned_at", Json::from(out.scanned_at)),
        (
            "albums",
            Json::Arr(out.albums.iter().map(album_dict).collect()),
        ),
    ])
}

/// `GET /api/albums/{id}`.
pub fn album(a: &AlbumRecord) -> Json {
    album_dict(a)
}

/// `DELETE /api/albums/{id}`.
pub fn deleted_album(d: DeletedAlbum) -> Json {
    Json::obj([
        ("ok", Json::Bool(true)),
        ("deleted_album_id", Json::from(d.album_id)),
    ])
}

/// `GET /api/albums/{id}/tracks`.
pub fn tracks(t: TrackList) -> Json {
    Json::obj([
        ("album_id", Json::from(t.album_id)),
        // Deliberate change: each track carries its ALBUM tag.
        (
            "tracks",
            Json::Arr(t.tracks.iter().map(track_dict).collect()),
        ),
    ])
}

/// `POST /api/albums/{id}/mb-anchor/clear`.
pub fn cleared_anchor(c: ClearedAnchor) -> Json {
    Json::obj([
        ("ok", Json::Bool(true)),
        ("blocked_mbid", Json::from(c.blocked_mbid)),
    ])
}

/// `GET /api/albums/{id}/resolution`: the Python app's `to_dict()`.
pub fn resolution(r: &Resolution) -> serde_json::Value {
    r.to_dict()
}

/// `GET /api/scan/status` body (key order of `to_dict`). The cancel is
/// visible only through `status` (`cancelling`, then `cancelled`), so
/// the body keeps the Python app's keys.
///
/// `skipped_non_utf8_count` appears only when something was skipped, so the usual body is unchanged.
pub fn scan_state(s: &ScanState) -> Json {
    let mut pairs = vec![
        ("status", Json::from(s.status_str())),
        ("library_dir", Json::from(s.library_dir.clone())),
        ("force", Json::from(s.force)),
        ("total", Json::from(s.total)),
        ("scanned", Json::from(s.scanned)),
        ("cached_count", Json::from(s.cached_count)),
        ("removed_count", Json::from(s.removed_count)),
        ("started_at", Json::from(s.started_at.clone())),
        ("completed_at", Json::from(s.completed_at.clone())),
        ("error", Json::from(s.error.clone())),
    ];
    if s.skipped_non_utf8 > 0 {
        pairs.push(("skipped_non_utf8_count", Json::from(s.skipped_non_utf8)));
    }
    Json::obj(pairs)
}

/// `POST /api/scan/cancel`.
pub fn cancel(out: &CancelOutcome) -> Json {
    Json::obj([
        ("outcome", Json::from(out.as_str())),
        ("scan", scan_state(out.state())),
    ])
}

/// `POST /api/scan/rescan`.
pub fn rescan(r: RescanStarted) -> Json {
    Json::obj([
        ("status", Json::from("scan started")),
        ("library_dir", Json::from(r.library_dir)),
        ("scan", scan_state(&r.scan)),
    ])
}

/// The scan event's name on the stream.
pub fn scan_event_name(e: &ScanEvent) -> &'static str {
    e.name()
}

/// The scan event's payload in the wire's key order.
pub fn scan_event_data(e: &ScanEvent) -> Json {
    match e {
        ScanEvent::Start {
            library_dir,
            total,
            cached_count,
            force,
        } => Json::obj([
            ("library_dir", Json::from(library_dir.as_str())),
            ("total", Json::from(*total)),
            ("cached_count", Json::from(*cached_count)),
            ("force", Json::from(*force)),
        ]),
        ScanEvent::Album { album, source } => {
            let mut dict = album_dict(album);
            dict.set(
                "_status",
                Json::from(match source {
                    AlbumSource::Fresh => "fresh",
                    AlbumSource::Cached => "cached",
                }),
            );
            dict
        }
        ScanEvent::Progress { scanned, total } | ScanEvent::Cancelling { scanned, total } => {
            Json::obj([
                ("scanned", Json::from(*scanned)),
                ("total", Json::from(*total)),
            ])
        }
        ScanEvent::MarkedMissing { id, path } => Json::obj([
            ("id", Json::from(id.as_str())),
            ("path", Json::from(path.as_str())),
        ]),
        ScanEvent::Adopted {
            old_id,
            id,
            old_path,
            path,
        } => Json::obj([
            ("old_id", Json::from(old_id.as_str())),
            ("id", Json::from(id.as_str())),
            ("old_path", Json::from(old_path.as_str())),
            ("path", Json::from(path.as_str())),
        ]),
        ScanEvent::Done(d) => {
            let mut done = vec![
                ("count", Json::from(d.count)),
                ("marked_missing_count", Json::from(d.marked_missing_count)),
                ("newly_missing_count", Json::from(d.newly_missing_count)),
                ("returned_count", Json::from(d.returned_count)),
                ("scanned_at", Json::from(d.scanned_at.as_str())),
            ];
            if let Some(n) = d.skipped_non_utf8_count {
                done.push(("skipped_non_utf8_count", Json::from(n)));
            }
            Json::obj(done)
        }
        ScanEvent::Aborted(a) => Json::obj([
            ("reason", Json::from(ABORT_REASON)),
            ("vanish_ratio", Json::Float(a.vanish_ratio)),
            ("vanish_count", Json::from(a.vanish_count)),
            ("cached_count", Json::from(a.cached_count)),
            ("threshold", Json::Float(a.threshold)),
            ("detail", Json::from(a.detail.as_str())),
        ]),
        ScanEvent::Cancelled(c) => Json::obj([
            ("reason", Json::from(CANCEL_REASON)),
            ("phase", Json::from(c.phase.as_str())),
            ("scanned", Json::from(c.scanned)),
            ("total", Json::from(c.total)),
            ("detail", Json::from(c.detail.as_str())),
        ]),
        ScanEvent::Error { detail } => Json::obj([("detail", Json::from(detail.as_str()))]),
    }
}

/// One frame of `GET /api/scan/stream`: `sse_event(name, payload)`.
pub fn scan_frame(e: &ScanEvent) -> String {
    format!(
        "event: {}\ndata: {}\n\n",
        scan_event_name(e),
        scan_event_data(e).dumps()
    )
}
