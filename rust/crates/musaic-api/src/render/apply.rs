//! The `apply` run as SSE items: each typed event is one `event:` / `data:`
//! frame with the Python app's name and payload keys.

use musaic_app::ops::apply::{ApplyEvent, TagEvent};
use musaic_app::ops::record::AlbumRecord;
use musaic_services::Json;

fn album_row(a: &AlbumRecord) -> Json {
    Json::obj(a.iter().map(|(k, v)| (k, v.clone())))
}

/// The SSE event name of `e`.
pub fn event_name(e: &ApplyEvent) -> &'static str {
    match e {
        ApplyEvent::CoverDownloading { .. } => "cover_downloading",
        ApplyEvent::CoverDownloaded { .. } => "cover_downloaded",
        ApplyEvent::Tags(t) => t.name(),
        ApplyEvent::CoverSaved { .. } => "cover_saved",
        ApplyEvent::UpdatedAlbum(_) => "updated_album",
        ApplyEvent::RemovedAlbum { .. } => "removed_album",
        ApplyEvent::Error { .. } => "error",
    }
}

fn path_str(p: &std::path::Path) -> String {
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

fn cover_downloaded(source_bytes: usize, source_mime: &str, bytes: usize, mime: &str) -> Json {
    Json::obj([
        ("source_bytes", Json::from(source_bytes)),
        ("source_mime", Json::from(source_mime)),
        ("bytes", Json::from(bytes)),
        ("mime", Json::from(mime)),
    ])
}

fn cover_saved(path: &str, filename: &str) -> Json {
    Json::obj([
        ("path", Json::from(path)),
        ("filename", Json::from(filename)),
    ])
}

/// The payload `tags.apply_to_album_iter` yields for each of its events.
pub fn tag_event_json(ev: &TagEvent) -> Json {
    match ev {
        TagEvent::CoverDownloaded {
            source_bytes,
            source_mime,
            bytes,
            mime,
        } => cover_downloaded(*source_bytes, source_mime, *bytes, mime),
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
            rename_failures,
        } => {
            let mut j = with_cleared(
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
            );
            // Only when there is something to say, so the event stays the
            // Python app's otherwise: files whose TRACKNUMBER was written
            // but whose rename did not happen.
            if !rename_failures.is_empty() {
                j.set(
                    "rename_failures",
                    Json::Arr(
                        rename_failures
                            .iter()
                            .map(|f| {
                                Json::obj([
                                    ("filename", Json::from(f.filename.as_str())),
                                    ("new_filename", Json::from(f.new_filename.as_str())),
                                    ("error", Json::from(f.error.as_str())),
                                    ("reason_code", Json::from(f.reason.code())),
                                    ("user_message", Json::from(f.user_message.as_str())),
                                ])
                            })
                            .collect(),
                    ),
                );
            }
            j
        }
        TagEvent::CoverSaved { path, filename } => cover_saved(&path_str(path), filename),
    }
}

/// The item's JSON payload.
pub fn payload(e: &ApplyEvent) -> Json {
    match e {
        ApplyEvent::CoverDownloading { url } => Json::obj([("url", Json::from(url.as_str()))]),
        ApplyEvent::CoverDownloaded {
            source_bytes,
            source_mime,
            bytes,
            mime,
        } => cover_downloaded(*source_bytes, source_mime, *bytes, mime),
        ApplyEvent::Tags(t) => tag_event_json(t),
        ApplyEvent::CoverSaved { path, filename } => cover_saved(path, filename),
        ApplyEvent::UpdatedAlbum(a) => album_row(a),
        ApplyEvent::RemovedAlbum { id, path } => Json::obj([
            ("id", Json::from(id.as_str())),
            ("path", Json::from(path.as_str())),
        ]),
        ApplyEvent::Error { detail } => Json::obj([("detail", Json::from(detail.as_str()))]),
    }
}

/// `services.sse.sse_event(event, data)` for one event.
pub fn frame(e: &ApplyEvent) -> String {
    format!("event: {}\ndata: {}\n\n", event_name(e), payload(e).dumps())
}
