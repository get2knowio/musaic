//! Primitives shared by every layer.

pub mod ai_wording;
mod album;
pub mod album_fields;
mod album_id;
pub mod config;
pub mod pycompat;
pub mod rename_reason;

pub use ai_wording::AiWording;
pub use album::{Album, TrackArtist};
pub use album_id::album_id;
