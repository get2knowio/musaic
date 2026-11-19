//! File-moving operations that are not tag writes.
//!
//! - [`plan`] / [`rename`]: the filename-mismatch check's plan and its
//!   execution (the database side lives in
//!   `musaic_db::renames`);
//! - [`template`]: filename / directory templates and sanitising;
//! - [`trash`]: soft delete into `<config>/trash`;
//! - [`cover`]: the on-disk cover path and MIME (discovery order
//!   is in `musaic-scan`);
//! - [`sidecar`]: the track-tag sidecar rewrite after renames;
//! - [`multi_disc`]: multi-disc sets not in the one-folder layout, and the
//!   read-only plan for converting one.
//!
//! The tracknumber-driven filename prefix rewrite belongs to the
//! tag apply path and lives in `musaic-tags`.
//!
//! Nothing here publishes events: operations return what happened (e.g.
//! [`rename::AlbumRenamed`]) and the caller publishes.

pub mod conversion_log;
pub mod cover;
mod fsutil;
pub mod multi_disc;
pub mod plan;
mod pyrepr;
pub mod rename;
pub mod sidecar;
pub mod template;
pub mod trash;

pub use pyrepr::py_repr;

/// Move a regular file to another folder of the same volume without ever
/// replacing anything at `dst` (a hard link to the new name, then the old
/// name removed; where the volume has no hard links, a rename after
/// checking the name is free). Never a copy: across volumes it fails, and
/// the file stays where it was.
pub fn move_file_noclobber(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    fsutil::rename_noclobber(src, dst)
}
