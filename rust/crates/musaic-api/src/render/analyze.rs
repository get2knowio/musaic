//! The `analyze` run as SSE items: each typed event is one `event:` / `data:`
//! frame with the Python app's name and payload keys.

use musaic_app::ops::analyze::{AnalyzeEvent, FileResult};
use musaic_services::Json;
use musaic_services::albums::album_to_json;

/// The SSE event name of `e`.
pub fn event_name(e: &AnalyzeEvent) -> &'static str {
    match e {
        AnalyzeEvent::Started { .. } => "start",
        AnalyzeEvent::FileStarted { .. } => "file_started",
        AnalyzeEvent::FileDone { .. } => "file_done",
        AnalyzeEvent::AlbumDone { .. } => "album_done",
        AnalyzeEvent::UpdatedAlbum(_) => "updated_album",
        AnalyzeEvent::Done(_) => "done",
    }
}

/// `warnings` is present only when there is one, so every item the Python app
/// recorded keeps its keys.
fn with_warnings(mut item: Json, warnings: &[String]) -> Json {
    if !warnings.is_empty() {
        item.set(
            "warnings",
            Json::Arr(warnings.iter().map(|w| Json::from(w.as_str())).collect()),
        );
    }
    item
}

/// The item's JSON payload.
pub fn payload(e: &AnalyzeEvent) -> Json {
    match e {
        AnalyzeEvent::Started { what, file_count } => Json::obj([
            ("what", Json::from(what.as_str())),
            ("file_count", Json::from(*file_count)),
        ]),
        AnalyzeEvent::FileStarted {
            index,
            file_name,
            path,
        } => Json::obj([
            ("index", Json::from(*index)),
            ("filename", Json::from(file_name.as_str())),
            ("path", Json::from(path.as_str())),
        ]),
        AnalyzeEvent::FileDone {
            index,
            file_name,
            result,
        } => {
            let head = |extra: Vec<(&'static str, Json)>| {
                let mut pairs = vec![
                    ("index", Json::from(*index)),
                    ("filename", Json::from(file_name.as_str())),
                ];
                pairs.extend(extra);
                Json::obj(pairs)
            };
            match result {
                FileResult::Bpm { bpm, warnings } => {
                    with_warnings(head(vec![("bpm", Json::from(*bpm))]), warnings)
                }
                FileResult::ReplayGain {
                    track_gain,
                    track_peak,
                    warnings,
                } => with_warnings(
                    head(vec![
                        ("replaygain_track_gain", Json::from(track_gain.as_str())),
                        ("replaygain_track_peak", Json::from(track_peak.as_str())),
                    ]),
                    warnings,
                ),
                FileResult::Failed { error } => head(vec![("error", Json::from(error.as_str()))]),
            }
        }
        AnalyzeEvent::AlbumDone {
            album_gain,
            album_peak,
            warnings,
        } => with_warnings(
            Json::obj([
                ("replaygain_album_gain", Json::from(album_gain.as_str())),
                ("replaygain_album_peak", Json::from(album_peak.as_str())),
            ]),
            warnings,
        ),
        AnalyzeEvent::UpdatedAlbum(album) => album_to_json(album),
        AnalyzeEvent::Done(s) => Json::obj([
            ("what", Json::from(s.what.as_str())),
            ("analyzed_count", Json::from(s.analyzed_count)),
            ("errors", Json::from(s.errors)),
        ]),
    }
}

/// `services.sse.sse_event(event, data)` for one event.
pub fn frame(e: &AnalyzeEvent) -> String {
    format!("event: {}\ndata: {}\n\n", event_name(e), payload(e).dumps())
}
