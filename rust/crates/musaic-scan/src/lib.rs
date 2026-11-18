//! Library scanner.
//!
//! A port of the Python app's `scanner.py`. An album is any directory that
//! directly contains at least one FLAC file. Album-level fields come from the
//! directory name and from the tags of one "sample" track.

mod dirparse;
pub mod flacmeta;
mod listing;
mod scan;

pub use dirparse::{DirectoryParse, parse_album_dir};
pub use listing::{dir_names, list_flacs, listed_spelling, py_resolve};
pub use scan::{
    Album, COVER_FILENAMES, TrackArtist, TrackOrder, parse_tracknum, scan_album, scan_library,
};
