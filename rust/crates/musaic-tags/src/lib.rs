//! FLAC mutation primitives ported from the Python app's `musaic/tags.py`
//! (Vorbis comment writes, cover embed, `cover.jpg`
//! sidecar).
//!
//! - [`flac`] - metadata model + writer with mutagen's block layout;
//! - [`cover`] - `normalize_to_jpeg` and `_build_picture`;
//! - [`sidecar`] - `save_cover_to_album_dir`;
//! - [`preserve`] - what a rewritten FLAC keeps of the file it replaces;
//! - [`apply`] - `apply_to_album_iter` and the file steps of
//!   `services/apply.apply_album_changes_iter`.

pub mod apply;
pub mod cover;
pub mod flac;
pub mod preserve;
pub mod sidecar;

pub use apply::{
    CoverInput, Diff, Event, FileChange, Removals, Suggestions, TRACK_FIELDS_NOT_REMOVABLE,
    TrackSuggestions, apply_album_files, apply_to_album_iter, is_removable_album_field,
    is_removable_track_field, proposed_filename, removable_album_fields, removable_track_fields,
};
pub use cover::{build_picture, normalize_to_jpeg};
pub use flac::{FlacFile, Picture, SaveMode, SaveReport, VorbisComment};
pub use sidecar::save_cover_to_album_dir;
