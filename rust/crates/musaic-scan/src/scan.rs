//! Album row derivation.

use std::path::{Path, PathBuf};

use musaic_core::pycompat::{decimal_digit_value, int, round1, strip};
pub use musaic_core::{Album, TrackArtist};

use crate::dirparse::parse_album_dir;
use crate::flacmeta::{self, FlacMeta};
use crate::listing::{list_flacs, py_resolve};

const COVER_FILENAMES: [&str; 9] = [
    "cover.jpg",
    "cover.jpeg",
    "cover.png",
    "folder.jpg",
    "folder.jpeg",
    "folder.png",
    "front.jpg",
    "front.jpeg",
    "front.png",
];

/// Which track supplies the album-level tags, and the order of per-track lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TrackOrder {
    /// By (DISCNUMBER, TRACKNUMBER). A missing disc number counts as disc 1;
    /// tracks without a readable track number sort after the numbered ones,
    /// in file name order; equal positions keep file name order.
    /// This is the Rust app's behaviour (a deliberate change; the Python app
    /// took the first file in code-point order).
    #[default]
    DiscTrack,
    /// File name code point order: what the Python app does. Kept so the port
    /// can be compared with the Python app field for field.
    Filename,
}

/// Track number from a tag value: "5", "05", "5/12", and disc-prefixed forms
/// "1-05" / "1.05" (the last number wins). `None` when unparseable.
pub fn parse_tracknum(raw: Option<&str>) -> Option<i64> {
    let mut s = strip(raw?);
    if s.is_empty() {
        return None;
    }
    if let Some((head, _)) = s.split_once('/') {
        s = strip(head);
    }
    for sep in ['-', '.'] {
        if let Some((_, tail)) = s.rsplit_once(sep) {
            let tail = strip(tail);
            // Python's str.isdigit() also accepts a few non-decimal digit
            // characters (superscripts); for those int() then fails, and so
            // does int() of the whole string, so the result is None either way.
            if !tail.is_empty() && tail.chars().all(|c| decimal_digit_value(c).is_some()) {
                s = tail;
                break;
            }
        }
    }
    int(s)
}

/// First non-empty value among `keys`, stripped. Mirrors `_first_tag`: for
/// each key only its FIRST value is considered; an empty first value moves on
/// to the next key; a whitespace-only value is returned as an empty string.
fn first_tag(meta: &FlacMeta, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|k| {
        meta.values(k)
            .next()
            .filter(|v| !v.is_empty())
            .map(|v| strip(v).to_string())
    })
}

fn find_cover_file(album_dir: &Path) -> Option<String> {
    for name in COVER_FILENAMES {
        if std::fs::metadata(album_dir.join(name)).is_ok_and(|m| m.is_file()) {
            return Some(name.to_string());
        }
    }
    // Case-insensitive fallback. When two names fold to the same lower-case
    // form the Python app keeps whichever the directory listing yields last;
    // deliberate change: names are taken in code point order, so the last in
    // that order wins on every filesystem.
    let mut names: Vec<std::ffi::OsString> = std::fs::read_dir(album_dir)
        .ok()?
        .flatten()
        .filter(|e| std::fs::metadata(e.path()).is_ok_and(|m| m.is_file()))
        .map(|e| e.file_name())
        .collect();
    names.sort();
    let mut existing = std::collections::HashMap::new();
    for n in names {
        let name = n.to_string_lossy().into_owned();
        existing.insert(name.to_lowercase(), name);
    }
    COVER_FILENAMES
        .iter()
        .find_map(|n| existing.get(*n).cloned())
}

fn mtime(path: &Path) -> Option<f64> {
    let m = std::fs::metadata(path).ok()?.modified().ok()?;
    let d = m.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(d.as_secs() as f64 + 1e-9 * d.subsec_nanos() as f64)
}

/// Max of the directory's mtime and every FLAC's mtime: tag edits rewrite a
/// file in place and do not bump the directory.
fn effective_mtime(album_dir: &Path, flacs: &[PathBuf]) -> f64 {
    flacs
        .iter()
        .filter_map(|f| mtime(f))
        .fold(mtime(album_dir).unwrap_or(0.0), f64::max)
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Read one album directory. `None` when it holds no FLACs or the sample
/// track cannot be read.
pub fn scan_album(album_dir: &Path, order: TrackOrder) -> Option<Album> {
    let flacs = list_flacs(album_dir);
    if flacs.is_empty() {
        return None;
    }
    // (path, metadata or None when unreadable), in file name order.
    let mut tracks: Vec<(PathBuf, Option<FlacMeta>)> = flacs
        .iter()
        .map(|p| (p.clone(), flacmeta::read(p).ok()))
        .collect();
    if order == TrackOrder::DiscTrack {
        let key = |m: &Option<FlacMeta>| {
            let m = m.as_ref();
            let disc = m
                .and_then(|m| first_tag(m, &["discnumber"]))
                .and_then(|v| int(v.split('/').next().unwrap_or("")))
                .unwrap_or(1);
            let track = m.and_then(|m| parse_tracknum(first_tag(m, &["tracknumber"]).as_deref()));
            (disc, track.is_none(), track.unwrap_or(0))
        };
        tracks.sort_by_key(|(_, m)| key(m)); // stable: ties keep file name order
    }

    let (sample_path, sample) = tracks.first()?;
    let sample = sample.as_ref()?;

    let title = first_tag(sample, &["album"]);
    let artist = first_tag(sample, &["albumartist", "album_artist", "artist"]);
    let album_artist = first_tag(sample, &["albumartist", "album_artist"]);
    let mut year = first_tag(sample, &["date", "year", "originaldate", "originalyear"]);
    if let Some(y) = &year {
        // "1973-09-15" -> "1973"
        let head: String = y.chars().take(4).collect();
        if head.chars().count() == 4 && head.chars().all(|c| decimal_digit_value(c).is_some()) {
            year = Some(head);
        }
    }

    let mut genre_seen = std::collections::HashSet::new();
    let mut genres: Vec<String> = Vec::new();
    let mut total_seconds = 0.0f64;
    let mut track_numbers = Vec::with_capacity(tracks.len());
    let mut track_artists = Vec::new();
    for (path, meta) in &tracks {
        let Some(meta) = meta else {
            track_numbers.push(None);
            continue;
        };
        total_seconds += meta.info.length();
        track_numbers.push(parse_tracknum(first_tag(meta, &["tracknumber"]).as_deref()));
        for raw in meta.values("genre") {
            let g = strip(raw);
            if !g.is_empty() && genre_seen.insert(g.to_lowercase()) {
                genres.push(g.to_string());
            }
        }
        let t_artist = first_tag(meta, &["artist"]).unwrap_or_default();
        let t_artist_sort = first_tag(meta, &["artistsort"]).unwrap_or_default();
        if !t_artist.is_empty() || !t_artist_sort.is_empty() {
            track_artists.push(TrackArtist {
                filename: file_name(path),
                artist: t_artist,
                artist_sort: t_artist_sort,
            });
        }
    }

    let dir_name = file_name(album_dir);
    let parsed = parse_album_dir(&dir_name);
    let resolved = py_resolve(album_dir).to_string_lossy().into_owned();
    Some(Album {
        id: musaic_core::album_id(&resolved),
        path: resolved,
        // Prefer the directory-parsed title: tags can be stale or wrong.
        title: parsed
            .dir_album
            .clone()
            .or(title)
            .or_else(|| Some(dir_name.clone())),
        dir_name,
        artist,
        album_artist,
        year,
        genre: (!genres.is_empty()).then(|| genres.join("; ")),
        track_count: tracks.len(),
        total_seconds: round1(total_seconds),
        has_embedded_cover: sample.picture_count > 0,
        cover_file: find_cover_file(album_dir),
        sample_track: py_resolve(sample_path).to_string_lossy().into_owned(),
        dir_mtime: effective_mtime(album_dir, &flacs),
        dir_artist: parsed.dir_artist,
        dir_album: parsed.dir_album,
        is_compilation: parsed.is_compilation,
        disc_number: parsed.disc_number,
        disc_total: parsed.disc_total,
        sample_rate: Some(sample.info.sample_rate),
        bits_per_sample: Some(sample.info.bits_per_sample),
        channels: Some(sample.info.channels),
        track_numbers,
        album_artist_sort: first_tag(sample, &["albumartistsort"]),
        artist_sort: first_tag(sample, &["artistsort"]),
        album_sort: first_tag(sample, &["albumsort"]),
        track_artists,
    })
}

/// Every album under `root`, sorted by path. Directories whose name starts
/// with "." are not entered, and symlinked directories are not followed
/// (`os.walk` defaults), though `root` itself may be either. A name listed
/// twice in one directory is entered once.
pub fn scan_library(root: &Path, order: TrackOrder) -> Vec<Album> {
    let mut dirs = vec![root.to_path_buf()];
    let mut albums = Vec::new();
    while let Some(dir) = dirs.pop() {
        if let Ok(rd) = std::fs::read_dir(&dir) {
            // A name listed twice (some SMB shares do) is entered once.
            let mut seen = std::collections::HashSet::new();
            for e in rd.flatten() {
                if !seen.insert(e.file_name()) {
                    continue;
                }
                let is_real_dir = e.file_type().is_ok_and(|t| t.is_dir());
                if is_real_dir && !e.file_name().to_string_lossy().starts_with('.') {
                    dirs.push(e.path());
                }
            }
        }
        albums.extend(scan_album(&dir, order));
    }
    albums.sort_by(|a, b| a.path.cmp(&b.path));
    albums
}
