//! Shared accessors for the album-artist and sort cascades (port of the
//! Python app's `album_fields.py`). Every consumer that shows or renders "the
//! album's artist" goes through these, so the fallback chain is defined once.

use crate::Album;
use crate::pycompat::strip;

/// `display_artist(album, include_dir_artist=...)`: the first of
/// `album_artist`, `artist` (and, when `include_dir_artist`, `dir_artist`)
/// that is non-empty after Python `str.strip()`, stripped; `""` otherwise.
///
/// `include_dir_artist = true` is for callers rendering a path or tag, where
/// an empty artist token would be destructive (the Mansell bug).
pub fn display_artist(
    album_artist: Option<&str>,
    artist: Option<&str>,
    dir_artist: Option<&str>,
    include_dir_artist: bool,
) -> String {
    let dir = if include_dir_artist { dir_artist } else { None };
    [album_artist, artist, dir]
        .into_iter()
        .flatten()
        .map(strip)
        .find(|s| !s.is_empty())
        .unwrap_or("")
        .to_string()
}

/// `sort_key(album)`: the first non-empty (after strip) of
/// `album_artist_sort`, `artist_sort`, `album_artist`, `artist`, lower-cased
/// like Python's `str.lower()`; `""` otherwise.
pub fn sort_key(
    album_artist_sort: Option<&str>,
    artist_sort: Option<&str>,
    album_artist: Option<&str>,
    artist: Option<&str>,
) -> String {
    [album_artist_sort, artist_sort, album_artist, artist]
        .into_iter()
        .flatten()
        .map(strip)
        .find(|s| !s.is_empty())
        .map(str::to_lowercase)
        .unwrap_or_default()
}

impl Album {
    /// [`display_artist`] over this row's fields.
    pub fn display_artist(&self, include_dir_artist: bool) -> String {
        display_artist(
            self.album_artist.as_deref(),
            self.artist.as_deref(),
            self.dir_artist.as_deref(),
            include_dir_artist,
        )
    }

    /// [`sort_key`] over this row's fields.
    pub fn sort_key(&self) -> String {
        sort_key(
            self.album_artist_sort.as_deref(),
            self.artist_sort.as_deref(),
            self.album_artist.as_deref(),
            self.artist.as_deref(),
        )
    }
}
