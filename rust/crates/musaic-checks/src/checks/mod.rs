//! The checks, one module each: the Python app's seven, then
//! multi-disc-layout.

pub mod duplicate_tracks;
pub mod filename_mismatch;
pub mod inconsistent_artist_sort;
pub mod multi_disc_layout;
pub mod no_cover_art;
pub mod tags_out_of_sync;
pub mod track_gaps;
pub mod unidentified_album;

pub use duplicate_tracks::DuplicateTracks;
pub use filename_mismatch::FilenameMismatch;
pub use inconsistent_artist_sort::InconsistentArtistSort;
pub use multi_disc_layout::MultiDiscLayout;
pub use no_cover_art::NoCoverArt;
pub use tags_out_of_sync::TagsOutOfSync;
pub use track_gaps::TrackGaps;
pub use unidentified_album::UnidentifiedAlbum;
