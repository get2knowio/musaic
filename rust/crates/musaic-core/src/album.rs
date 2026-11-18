//! The album row as the scanner produces it (scanner-owned columns only).

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TrackArtist {
    pub filename: String,
    pub artist: String,
    pub artist_sort: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Album {
    pub id: String,
    pub path: String,
    pub dir_name: String,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album_artist: Option<String>,
    pub year: Option<String>,
    pub genre: Option<String>,
    pub track_count: usize,
    pub total_seconds: f64,
    pub has_embedded_cover: bool,
    pub cover_file: Option<String>,
    pub sample_track: String,
    pub dir_mtime: f64,
    pub dir_artist: Option<String>,
    pub dir_album: Option<String>,
    pub is_compilation: bool,
    pub disc_number: Option<i64>,
    pub disc_total: Option<i64>,
    pub sample_rate: Option<u32>,
    pub bits_per_sample: Option<u8>,
    pub channels: Option<u8>,
    pub track_numbers: Vec<Option<i64>>,
    pub album_artist_sort: Option<String>,
    pub artist_sort: Option<String>,
    pub album_sort: Option<String>,
    pub track_artists: Vec<TrackArtist>,
    /// Each file's disc number from its tags, parallel to `track_numbers`;
    /// `None` when the files have not been read for it yet (a row written
    /// before schema v20). Not part of the album dict the API serves.
    #[serde(skip_serializing)]
    pub track_discs: Option<Vec<Option<i64>>>,
    /// The highest disc total the files' tags name (DISCTOTAL, TOTALDISCS,
    /// DISCNUMBER "n/M"); `None` when none does, or not read yet.
    #[serde(skip_serializing)]
    pub tag_disc_total: Option<i64>,
}
