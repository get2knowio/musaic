//! The per-call user message: `ai._album_payload` (the album JSON
//! with the resolver's output embedded) and `ai._format_user_message`
//! (`<hint>` then `<album>` blocks, the payload as `json.dumps(indent=2)`).

use musaic_resolve::AlbumResolution;

use crate::pyjson::{Py, py_head, py_len, py_strip};

/// `_format_user_message`: hints over 2000 characters are cut.
pub const HINT_MAX_CHARS: usize = 2000;

fn get(d: &Py, k: &str) -> Py {
    d.get(k).cloned().unwrap_or(Py::Null)
}

fn opt_str(f: Option<&musaic_resolve::FieldFact>) -> Py {
    f.map_or(Py::Null, |f| Py::str(f.value.clone()))
}

/// `ai._album_payload(album, tracks, resolution)`.
pub fn album_payload(album: &Py, tracks: &Py, resolution: Option<&AlbumResolution>) -> Py {
    let matched = !matches!(get(album, "dir_artist"), Py::Null)
        || !matches!(get(album, "dir_album"), Py::Null)
        || get(album, "is_compilation").truthy();
    let track_list: Vec<Py> = match tracks {
        Py::List(items) => items
            .iter()
            .map(|t| {
                Py::dict([
                    ("filename", get(t, "filename")),
                    ("tracknumber", get(t, "tracknumber")),
                    ("title", get(t, "title")),
                    ("artist", get(t, "artist")),
                    ("duration", get(t, "duration")),
                ])
            })
            .collect(),
        _ => Vec::new(),
    };
    let mut payload = Py::dict([
        (
            "directory",
            Py::dict([
                ("dir_name", get(album, "dir_name")),
                ("matched", Py::Bool(matched)),
                ("dir_artist", get(album, "dir_artist")),
                ("dir_album", get(album, "dir_album")),
                (
                    "is_compilation",
                    Py::Bool(get(album, "is_compilation").truthy()),
                ),
                ("disc_number", get(album, "disc_number")),
                ("disc_total", get(album, "disc_total")),
            ]),
        ),
        (
            "current_tags",
            Py::dict([
                ("title", get(album, "title")),
                ("album_artist", get(album, "album_artist")),
                ("artist", get(album, "artist")),
                ("year", get(album, "year")),
                ("genre", get(album, "genre")),
            ]),
        ),
        ("track_count", get(album, "track_count")),
        ("total_seconds", get(album, "total_seconds")),
        ("tracks", Py::List(track_list)),
    ]);
    let Some(r) = resolution else {
        return payload;
    };

    // filename -> fingerprint entry (a later duplicate replaces an earlier one).
    let mut fp: Vec<(String, Py)> = Vec::new();
    for tr in &r.tracks {
        let Some(rec) = tr.recording_mbid.as_deref().filter(|m| !m.is_empty()) else {
            continue;
        };
        let mut entry = Py::dict([
            ("recording_mbid", Py::str(rec)),
            ("score", tr.fingerprint_score.map_or(Py::Null, Py::Float)),
        ]);
        if let Some(t) = &tr.canonical_title {
            entry.set("mb_canonical_title", Py::str(t.value.clone()));
        }
        if let Some(a) = &tr.canonical_artist {
            entry.set("mb_canonical_artist", Py::str(a.value.clone()));
        }
        match fp.iter_mut().find(|(f, _)| *f == tr.filename) {
            Some(slot) => slot.1 = entry,
            None => fp.push((tr.filename.clone(), entry)),
        }
    }
    let unresolved: Vec<Py> = r
        .tracks
        .iter()
        .filter(|t| t.canonical_title.is_none())
        .map(|t| Py::str(t.filename.clone()))
        .collect();

    payload.set(
        "anchor",
        match &r.anchor {
            Some(a) => Py::dict([
                ("release_group_mbid", Py::str(a.value.clone())),
                ("strategy", Py::str(r.anchor_strategy.clone())),
                ("confidence", Py::Float(a.confidence)),
                (
                    "notes",
                    Py::List(r.notes.iter().map(|n| Py::str(n.clone())).collect()),
                ),
            ]),
            None => Py::Null,
        },
    );
    payload.set(
        "resolver_canonical",
        Py::dict([
            ("title", opt_str(r.canonical_title.as_ref())),
            ("album_artist", opt_str(r.canonical_album_artist.as_ref())),
            ("year", opt_str(r.canonical_year.as_ref())),
            (
                "album_artist_sort",
                opt_str(r.canonical_album_artist_sort.as_ref()),
            ),
        ]),
    );
    if !fp.is_empty()
        && let Py::Dict(pairs) = &mut payload
        && let Some((_, Py::List(entries))) = pairs.iter_mut().find(|(k, _)| k == "tracks")
    {
        for e in entries.iter_mut() {
            let name = e.get("filename").and_then(Py::as_str).map(str::to_string);
            if let Some(name) = name
                && let Some((_, f)) = fp.iter().find(|(n, _)| *n == name)
            {
                e.set("fingerprint", f.clone());
            }
        }
    }
    if !unresolved.is_empty() {
        payload.set("unresolved_filenames", Py::List(unresolved));
    }
    payload
}

/// `ai._format_user_message(payload, user_hint)`.
pub fn format_user_message(payload: &Py, user_hint: &str) -> String {
    let mut hint = py_strip(user_hint).to_string();
    if py_len(&hint) > HINT_MAX_CHARS {
        hint = format!(
            "{}\n[\u{2026}hint truncated to 2000 characters\u{2026}]",
            py_head(&hint, HINT_MAX_CHARS)
        );
    }
    let mut parts = Vec::new();
    if !hint.is_empty() {
        parts.push(format!("<hint>\n{hint}\n</hint>"));
    }
    parts.push(format!("<album>\n{}\n</album>", payload.dumps_indent2()));
    parts.join("\n\n")
}
