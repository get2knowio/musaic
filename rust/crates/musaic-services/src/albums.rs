//! Album dicts as the Python app hands them around:
//!
//! - [`album_to_json`]: `scanner.album_to_dict` (dataclass `asdict`, field
//!   order of `scanner.Album`);
//! - [`get_album_by_id`] / [`load_snapshot`]: `cache._row_to_album` over the
//!   `albums` table (the "merged" dict the apply pipeline emits and the
//!   scan's `cached` album events carry);
//! - [`find_album_by_id`]: `scanner.find_album_by_id` (live walk).
//!
//! Each row also comes back as a [`musaic_core::Album`] so a scan can feed
//! cached rows to `musaic_db::albums::save_snapshot` the way the Python app feeds
//! the dicts back through `_album_to_row`.

use std::path::Path;

use musaic_core::{Album, TrackArtist};
use musaic_scan::TrackOrder;
use rusqlite::types::ValueRef;
use rusqlite::{Connection, OptionalExtension, Row};

use crate::json::Json;

/// `db.ALBUM_COLUMNS` (schema v16; v17 to v19 add nothing to `albums`),
/// then the two v20 columns, which are read into [`Album`] only: the album
/// dict keeps the shape the API has always served.
pub const ALBUM_COLUMNS: [&str; 36] = [
    "id",
    "library_path",
    "path",
    "dir_name",
    "title",
    "artist",
    "album_artist",
    "year",
    "genre",
    "track_count",
    "total_seconds",
    "has_embedded_cover",
    "cover_file",
    "sample_track",
    "dir_mtime",
    "dir_artist",
    "dir_album",
    "is_compilation",
    "disc_number",
    "disc_total",
    "sample_rate",
    "bits_per_sample",
    "channels",
    "track_numbers_json",
    "mb_release_group_mbid",
    "mb_release_mbid",
    "mb_match_score",
    "mb_last_checked_at",
    "album_artist_sort",
    "artist_sort",
    "album_sort",
    "track_artists_json",
    "metadata_ai_acted_on_at",
    "missing_since",
    "track_discs_json",
    "tag_disc_total",
];

/// One `albums` row: the Python app's dict and the scanner-owned columns.
#[derive(Debug, Clone, PartialEq)]
pub struct AlbumRow {
    pub json: Json,
    pub album: Album,
    pub missing_since: Option<String>,
}

impl AlbumRow {
    pub fn id(&self) -> &str {
        &self.album.id
    }
    pub fn path(&self) -> &str {
        &self.album.path
    }
}

fn opt_str(s: &Option<String>) -> Json {
    s.clone().map_or(Json::Null, Json::Str)
}

fn opt_int<T: Into<i64> + Copy>(n: Option<T>) -> Json {
    n.map_or(Json::Null, |n| Json::Int(n.into()))
}

fn track_artists_json(list: &[TrackArtist]) -> Json {
    Json::Arr(
        list.iter()
            .map(|t| {
                Json::obj([
                    ("filename", Json::from(t.filename.as_str())),
                    ("artist", Json::from(t.artist.as_str())),
                    ("artist_sort", Json::from(t.artist_sort.as_str())),
                ])
            })
            .collect(),
    )
}

/// `scanner.album_to_dict(album)`.
pub fn album_to_json(a: &Album) -> Json {
    Json::obj([
        ("id", Json::from(a.id.as_str())),
        ("path", Json::from(a.path.as_str())),
        ("dir_name", Json::from(a.dir_name.as_str())),
        ("title", opt_str(&a.title)),
        ("artist", opt_str(&a.artist)),
        ("album_artist", opt_str(&a.album_artist)),
        ("year", opt_str(&a.year)),
        ("genre", opt_str(&a.genre)),
        ("track_count", Json::from(a.track_count)),
        ("total_seconds", Json::Float(a.total_seconds)),
        ("has_embedded_cover", Json::from(a.has_embedded_cover)),
        ("cover_file", opt_str(&a.cover_file)),
        ("sample_track", Json::from(a.sample_track.as_str())),
        ("dir_mtime", Json::Float(a.dir_mtime)),
        ("dir_artist", opt_str(&a.dir_artist)),
        ("dir_album", opt_str(&a.dir_album)),
        ("is_compilation", Json::from(a.is_compilation)),
        ("disc_number", opt_int(a.disc_number)),
        ("disc_total", opt_int(a.disc_total)),
        ("sample_rate", opt_int(a.sample_rate)),
        ("bits_per_sample", opt_int(a.bits_per_sample)),
        ("channels", opt_int(a.channels)),
        (
            "track_numbers",
            Json::Arr(a.track_numbers.iter().map(|n| opt_int(*n)).collect()),
        ),
        ("album_artist_sort", opt_str(&a.album_artist_sort)),
        ("artist_sort", opt_str(&a.artist_sort)),
        ("album_sort", opt_str(&a.album_sort)),
        ("track_artists", track_artists_json(&a.track_artists)),
    ])
}

/// A column as Python's sqlite3 returns it.
fn raw(r: &Row<'_>, col: &str) -> rusqlite::Result<Json> {
    Ok(match r.get_ref(col)? {
        ValueRef::Null => Json::Null,
        ValueRef::Integer(n) => Json::Int(n),
        ValueRef::Real(f) => Json::Float(f),
        ValueRef::Text(t) => Json::Str(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(b) => Json::Str(String::from_utf8_lossy(b).into_owned()),
    })
}

/// Python truthiness of a column value.
fn truthy(v: &Json) -> bool {
    match v {
        Json::Null => false,
        Json::Bool(b) => *b,
        Json::Int(n) => *n != 0,
        Json::Float(f) => *f != 0.0,
        Json::Str(s) => !s.is_empty(),
        Json::Arr(a) => !a.is_empty(),
        Json::Obj(o) => !o.is_empty(),
    }
}

/// `_parse_track_numbers_json`.
fn parse_track_numbers(raw: Option<&str>) -> Vec<Option<i64>> {
    let Some(raw) = raw.filter(|s| !s.is_empty()) else {
        return Vec::new();
    };
    let Ok(serde_json::Value::Array(items)) = serde_json::from_str::<serde_json::Value>(raw) else {
        return Vec::new();
    };
    items
        .iter()
        .map(|x| match x {
            serde_json::Value::Bool(b) => Some(i64::from(*b)),
            serde_json::Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
            _ => None,
        })
        .collect()
}

/// `track_discs_json`: SQL NULL (or anything that is not a JSON list) is
/// "not read yet", so a scan reads the files again rather than trust it.
fn parse_track_discs(raw: Option<&str>) -> Option<Vec<Option<i64>>> {
    let serde_json::Value::Array(items) = serde_json::from_str(raw?).ok()? else {
        return None;
    };
    Some(items.iter().map(serde_json::Value::as_i64).collect())
}

/// `str(item.get(key) or "")`.
fn py_str_or_empty(v: Option<&serde_json::Value>) -> String {
    match v {
        None | Some(serde_json::Value::Null) => String::new(),
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Bool(false)) => String::new(),
        Some(serde_json::Value::Bool(true)) => "True".into(),
        Some(serde_json::Value::Number(n)) => {
            if n.as_f64() == Some(0.0) {
                String::new()
            } else {
                n.to_string()
            }
        }
        Some(other) => other.to_string(),
    }
}

/// `_parse_track_artists_json`.
fn parse_track_artists(raw: Option<&str>) -> Vec<TrackArtist> {
    let Some(raw) = raw.filter(|s| !s.is_empty()) else {
        return Vec::new();
    };
    let Ok(serde_json::Value::Array(items)) = serde_json::from_str::<serde_json::Value>(raw) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let obj = item.as_object()?;
            let filename = py_str_or_empty(obj.get("filename"));
            if filename.is_empty() {
                return None;
            }
            Some(TrackArtist {
                filename,
                artist: py_str_or_empty(obj.get("artist")),
                artist_sort: py_str_or_empty(obj.get("artist_sort")),
            })
        })
        .collect()
}

fn as_opt_string(v: &Json) -> Option<String> {
    match v {
        Json::Null => None,
        Json::Str(s) => Some(s.clone()),
        Json::Int(n) => Some(n.to_string()),
        Json::Float(f) => Some(f.to_string()),
        other => Some(other.dumps()),
    }
}

fn as_opt_i64(v: &Json) -> Option<i64> {
    match v {
        Json::Int(n) => Some(*n),
        Json::Float(f) => Some(*f as i64),
        Json::Bool(b) => Some(i64::from(*b)),
        _ => None,
    }
}

fn as_f64(v: &Json) -> f64 {
    match v {
        Json::Int(n) => *n as f64,
        Json::Float(f) => *f,
        _ => 0.0,
    }
}

/// `cache._row_to_album(row)` plus the scanner-owned [`Album`].
fn row_to_album(r: &Row<'_>) -> rusqlite::Result<AlbumRow> {
    let col = |c: &str| raw(r, c);
    let text = |c: &str| -> rusqlite::Result<Option<String>> { Ok(as_opt_string(&raw(r, c)?)) };
    let track_count = col("track_count")?;
    let track_count = if truthy(&track_count) {
        track_count
    } else {
        Json::Int(0)
    };
    let total_seconds = col("total_seconds")?;
    let total_seconds = if truthy(&total_seconds) {
        total_seconds
    } else {
        Json::Float(0.0)
    };
    let sample_track = col("sample_track")?;
    let sample_track = if truthy(&sample_track) {
        sample_track
    } else {
        Json::from("")
    };
    let dir_mtime = col("dir_mtime")?;
    let dir_mtime = if truthy(&dir_mtime) {
        dir_mtime
    } else {
        Json::Float(0.0)
    };
    let track_numbers_raw = text("track_numbers_json")?;
    let track_numbers = parse_track_numbers(track_numbers_raw.as_deref());
    let track_artists_raw = text("track_artists_json")?;
    let track_artists = parse_track_artists(track_artists_raw.as_deref());
    let has_cover = truthy(&col("has_embedded_cover")?);
    let is_compilation = truthy(&col("is_compilation")?);
    let missing_since = text("missing_since")?;

    let json = Json::obj([
        ("id", col("id")?),
        ("path", col("path")?),
        ("dir_name", col("dir_name")?),
        ("title", col("title")?),
        ("artist", col("artist")?),
        ("album_artist", col("album_artist")?),
        ("year", col("year")?),
        ("genre", col("genre")?),
        ("track_count", track_count.clone()),
        ("total_seconds", total_seconds.clone()),
        ("has_embedded_cover", Json::from(has_cover)),
        ("cover_file", col("cover_file")?),
        ("sample_track", sample_track.clone()),
        ("dir_mtime", dir_mtime.clone()),
        ("dir_artist", col("dir_artist")?),
        ("dir_album", col("dir_album")?),
        ("is_compilation", Json::from(is_compilation)),
        ("disc_number", col("disc_number")?),
        ("disc_total", col("disc_total")?),
        ("sample_rate", col("sample_rate")?),
        ("bits_per_sample", col("bits_per_sample")?),
        ("channels", col("channels")?),
        (
            "track_numbers",
            Json::Arr(track_numbers.iter().map(|n| opt_int(*n)).collect()),
        ),
        ("mb_release_group_mbid", col("mb_release_group_mbid")?),
        ("mb_release_mbid", col("mb_release_mbid")?),
        ("mb_match_score", col("mb_match_score")?),
        ("mb_last_checked_at", col("mb_last_checked_at")?),
        ("album_artist_sort", col("album_artist_sort")?),
        ("artist_sort", col("artist_sort")?),
        ("album_sort", col("album_sort")?),
        ("track_artists", track_artists_json(&track_artists)),
        ("metadata_ai_acted_on_at", col("metadata_ai_acted_on_at")?),
        ("missing_since", col("missing_since")?),
    ]);
    let album = Album {
        id: text("id")?.unwrap_or_default(),
        path: text("path")?.unwrap_or_default(),
        dir_name: text("dir_name")?.unwrap_or_default(),
        title: text("title")?,
        artist: text("artist")?,
        album_artist: text("album_artist")?,
        year: text("year")?,
        genre: text("genre")?,
        track_count: as_opt_i64(&track_count).unwrap_or(0).max(0) as usize,
        total_seconds: as_f64(&total_seconds),
        has_embedded_cover: has_cover,
        cover_file: text("cover_file")?,
        sample_track: sample_track.as_str().unwrap_or_default().to_string(),
        dir_mtime: as_f64(&dir_mtime),
        dir_artist: text("dir_artist")?,
        dir_album: text("dir_album")?,
        is_compilation,
        disc_number: as_opt_i64(&col("disc_number")?),
        disc_total: as_opt_i64(&col("disc_total")?),
        sample_rate: as_opt_i64(&col("sample_rate")?).map(|n| n as u32),
        bits_per_sample: as_opt_i64(&col("bits_per_sample")?).map(|n| n as u8),
        channels: as_opt_i64(&col("channels")?).map(|n| n as u8),
        track_numbers,
        album_artist_sort: text("album_artist_sort")?,
        artist_sort: text("artist_sort")?,
        album_sort: text("album_sort")?,
        track_artists,
        track_discs: parse_track_discs(text("track_discs_json")?.as_deref()),
        tag_disc_total: as_opt_i64(&col("tag_disc_total")?),
    };
    Ok(AlbumRow {
        json,
        album,
        missing_since,
    })
}

fn select_sql(filter: &str) -> String {
    format!("SELECT {} FROM albums {filter}", ALBUM_COLUMNS.join(", "))
}

/// `cache.get_album_by_id(album_id)`.
pub fn get_album_by_id(conn: &Connection, album_id: &str) -> rusqlite::Result<Option<AlbumRow>> {
    conn.query_row(&select_sql("WHERE id = ?1"), [album_id], row_to_album)
        .optional()
}

/// `cache.load_snapshot(library_dir).albums_by_path` (Missing albums
/// included), in the order the Python app's query returns them (rowid order, via
/// `idx_albums_library`). `library_dir` must already be resolved.
pub fn load_snapshot(conn: &Connection, library_dir: &str) -> rusqlite::Result<Vec<AlbumRow>> {
    let has_library: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM libraries WHERE path = ?1)",
        [library_dir],
        |r| r.get(0),
    )?;
    if !has_library {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(&select_sql("WHERE library_path = ?1 ORDER BY rowid"))?;
    let rows = stmt.query_map([library_dir], row_to_album)?;
    rows.collect()
}

/// `scanner.find_album_by_id(root, album_id)`: walk the library and scan
/// until an album with that id turns up.
pub fn find_album_by_id(root: &Path, album_id: &str, order: TrackOrder) -> Option<Album> {
    musaic_scan::scan_library(root, order)
        .into_iter()
        .find(|a| a.id == album_id)
}
