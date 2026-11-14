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
pub use listing::{list_flacs, py_resolve};
pub use scan::{Album, TrackArtist, TrackOrder, parse_tracknum, scan_album, scan_library};
