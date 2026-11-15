//! The seven checks, one module each.

pub mod duplicate_tracks;
pub mod filename_mismatch;
pub mod inconsistent_artist_sort;
pub mod no_cover_art;
pub mod tags_out_of_sync;
pub mod track_gaps;
pub mod unidentified_album;

pub use duplicate_tracks::DuplicateTracks;
pub use filename_mismatch::FilenameMismatch;
pub use inconsistent_artist_sort::InconsistentArtistSort;
pub use no_cover_art::NoCoverArt;
pub use tags_out_of_sync::TagsOutOfSync;
pub use track_gaps::TrackGaps;
pub use unidentified_album::UnidentifiedAlbum;
