//! The `library` group: the library folder, the album list, one album, its
//! tracks and cover, removing an album from the library, clearing its
//! MusicBrainz anchor, and its resolution.
//!
//! Each operation that writes (and the resolution, which can store
//! fingerprints and an anchor) takes the work guard itself and holds it until
//! its work ends; a restore running makes it `Busy` with nothing changed.

use std::path::{Path, PathBuf};

pub use musaic_resolve::AlbumResolution as Resolution;
use musaic_services::ResolvedAlbum;
use musaic_services::covers::{self, FullCover};
use musaic_services::library_ops;
pub use musaic_services::library_ops::LibraryChoice;

use super::error::OpError;
use super::record::{AlbumRecord, TrackRecord};
use crate::App;

/// The configured library folder and whether it is there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryConfig {
    pub library_dir: Option<String>,
    pub exists: bool,
}

/// The library's albums in the app's order.
#[derive(Debug, Clone, PartialEq)]
pub struct AlbumList {
    pub library_dir: String,
    /// `""` when the library was never scanned.
    pub scanned_at: String,
    pub albums: Vec<AlbumRecord>,
}

impl AlbumList {
    pub fn count(&self) -> usize {
        self.albums.len()
    }
}

/// An album's tracks as the files read.
#[derive(Debug, Clone, PartialEq)]
pub struct TrackList {
    pub album_id: String,
    pub tracks: Vec<TrackRecord>,
}

/// Which cover is asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoverSize {
    /// The cached thumbnail (made now when missing).
    Thumb,
    /// The full-size image.
    Full,
}

/// A cover to serve. A face that serves files serves the path; one that
/// cannot reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoverImage {
    /// The cover file kept next to the tracks.
    File(PathBuf),
    /// The cached thumbnail (always JPEG).
    Thumbnail(PathBuf),
    /// The largest picture embedded in the album's sample track.
    Embedded { data: Vec<u8>, mime: String },
}

/// What [`Library::remove_album`] removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeletedAlbum {
    pub album_id: String,
}

/// What [`Library::clear_mb_anchor`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClearedAnchor {
    /// The release group now blocked so the resolver does not pick it again.
    pub blocked_mbid: Option<String>,
}

/// The `library` operations.
pub struct Library<'a> {
    pub(super) app: &'a App,
}

impl Library<'_> {
    fn conn(&self) -> Result<rusqlite::Connection, OpError> {
        Ok(musaic_db::open_connection(self.app.db_path())?)
    }

    /// The configured library folder, and whether it is a directory.
    pub fn config(&self) -> LibraryConfig {
        let library_dir = self.app.0.config.load().library_dir;
        let exists = library_dir
            .as_deref()
            .is_some_and(|d| !d.is_empty() && Path::new(d).is_dir());
        LibraryConfig {
            library_dir,
            exists,
        }
    }

    /// Choose the library folder: the guard first (a restore running is
    /// `Busy` even for a bad path, as `POST /api/config` answers today:
    /// recorded as a deviation from FR-008 in `docs/operations.md`), then a
    /// path that is not a readable directory is `Rejected`, then the folder
    /// is stored and its snapshot dropped (the next scan re-reads it).
    pub fn set_library_dir(&self, dir: &str) -> Result<LibraryChoice, OpError> {
        let _guard = self.app.gate().begin_work()?;
        library_ops::set_library_dir(&self.app.0.config, self.app.0.home.as_deref(), dir, || {
            self.conn()
        })
    }

    /// The albums of the configured library. `refresh` drops the library's
    /// snapshot first and so takes the guard; a plain list does not.
    pub fn list_albums(&self, refresh: bool) -> Result<AlbumList, OpError> {
        let _guard = if refresh {
            Some(self.app.gate().begin_work()?)
        } else {
            None
        };
        let lib = library_ops::configured_library(self.app.library_dir())?;
        let mut conn = self.conn()?;
        let listing = library_ops::list_albums(&mut conn, &lib, refresh)?;
        Ok(AlbumList {
            library_dir: listing.library_dir,
            scanned_at: listing.scanned_at,
            albums: listing.albums.into_iter().map(AlbumRecord::new).collect(),
        })
    }

    /// One album: the library's cached row, else a live walk of the library.
    pub fn album(&self, id: &str) -> Result<AlbumRecord, OpError> {
        let lib = self
            .app
            .library_dir()
            .filter(|l| !l.is_empty())
            .ok_or_else(library_ops::no_library)?;
        let conn = self.conn()?;
        let album = library_ops::get_album(&conn, &lib, id, self.app.0.track_order)?;
        Ok(AlbumRecord::new(album))
    }

    /// An album's tracks, read from its files.
    pub fn tracks(&self, id: &str) -> Result<TrackList, OpError> {
        let album = self.resolve(id)?;
        Ok(TrackList {
            album_id: id.to_string(),
            tracks: library_ops::panel_track_rows(&album.path)
                .into_iter()
                .map(TrackRecord::new)
                .collect(),
        })
    }

    /// The folder an album lives in, found as [`tracks`](Self::tracks) finds
    /// it (the configured library's row, else a walk of the library). It may
    /// no longer be on disk. Refusals: `NoLibrary`, `NotFound`.
    pub fn album_folder(&self, id: &str) -> Result<std::path::PathBuf, OpError> {
        Ok(self.resolve(id)?.path.into())
    }

    fn resolve(&self, id: &str) -> Result<ResolvedAlbum, OpError> {
        let conn = self.conn()?;
        let (id, path, json) = library_ops::resolve_album_dict(
            &conn,
            self.app.library_dir().as_deref(),
            id,
            self.app.0.track_order,
        )?;
        Ok(ResolvedAlbum { id, path, json })
    }

    /// An album's cover; `NotFound` (subject `Cover`) when it has none.
    pub fn cover(&self, id: &str, size: CoverSize) -> Result<CoverImage, OpError> {
        let ResolvedAlbum {
            id: album_id,
            path,
            json: album,
        } = self.resolve(id)?;
        match size {
            CoverSize::Full => Ok(
                match covers::full_cover_or_none(&album_id, &path, &album)? {
                    FullCover::Disk(p) => CoverImage::File(p),
                    FullCover::Embedded(data, mime) => CoverImage::Embedded { data, mime },
                },
            ),
            CoverSize::Thumb => Ok(CoverImage::Thumbnail(covers::thumb_or_none(
                &self.app.thumbs_dir(),
                &album_id,
                &path,
                &album,
            )?)),
        }
    }

    /// Remove an album from the library (one of the two ways a row is
    /// dropped); publishes `AlbumDeleted`. `NotFound` unless the library
    /// holds it.
    pub fn remove_album(&self, id: &str) -> Result<DeletedAlbum, OpError> {
        let _guard = self.app.gate().begin_work()?;
        let conn = self.conn()?;
        library_ops::delete_album(conn, self.app.bus(), self.app.library_dir().as_deref(), id)?;
        Ok(DeletedAlbum {
            album_id: id.to_string(),
        })
    }

    /// Block the album's current release group, clear its anchor and tell the
    /// bus (`AlbumTagsChanged` of `mb_release_group_mbid`).
    pub fn clear_mb_anchor(&self, id: &str) -> Result<ClearedAnchor, OpError> {
        let _guard = self.app.gate().begin_work()?;
        let conn = self.conn()?;
        let blocked = library_ops::clear_mb_anchor(
            conn,
            || self.app.library_dir(),
            self.app.0.track_order,
            &self.app.0.resolver,
            self.app.bus(),
            || self.app.now_iso(),
            id,
        )?;
        Ok(ClearedAnchor {
            blocked_mbid: blocked,
        })
    }

    /// The album's resolution. A read that can store fingerprints, AcoustID
    /// matches and an anchor, so it holds the guard for its whole run: a
    /// restore asked for meanwhile is refused instead of waiting.
    pub fn resolution(&self, id: &str, force: bool, hint: &str) -> Result<Resolution, OpError> {
        let _guard = self.app.gate().begin_work()?;
        let conn = self.conn()?;
        let mut r = library_ops::album_resolution(
            &conn,
            &|| self.app.library_dir(),
            self.app.0.track_order,
            &|| self.app.parts(),
            id,
            hint,
            force,
        )?;
        // The trace note names AI only when a provider is ready.
        r.reword_notes(self.app.ai_wording());
        Ok(r)
    }
}
