//! The `tags` group: the tags on an album's tracks, the tags in use, and the
//! tag vocabulary.
//!
//! Every write takes the work guard itself (a restore running makes it `Busy`,
//! with nothing changed) and holds it until its work ends. A write to a Missing
//! album's tags is refused (`Conflict`, right after the album lookup); reads
//! still answer. A tag write publishes no event: nothing subscribes to a tag, the
//! sidecar is rewritten from the database in the same call, and the recorded
//! behaviour (`testdata/golden/services/tags_vocab.json`) has none.

use musaic_services::track_tags::{self, TagEnv};
pub use musaic_services::track_tags::{TagCounts, TagTracks, TaggedTrack};
use musaic_services::vocabulary;
pub use musaic_services::vocabulary::{
    TagMerge, TagRewrite, VocabEntry as VocabularyEntry, Vocabulary, VocabularyDeleted,
    VocabularyMerged, VocabularyRenamed,
};
use rusqlite::Connection;

use super::error::OpError;
use super::record::TagSet;
use crate::App;

/// An album's tags: each track's tags by file name (in file-name order), and
/// the tags every track has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlbumTags {
    pub album_id: String,
    pub tracks: Vec<(String, TagSet)>,
    pub album_tags: TagSet,
}

impl AlbumTags {
    /// One track's tags, `None` when the album's rows name no such file.
    pub fn track(&self, file_name: &str) -> Option<&TagSet> {
        self.tracks
            .iter()
            .find(|(f, _)| f == file_name)
            .map(|(_, t)| t)
    }
}

impl From<track_tags::AlbumTags> for AlbumTags {
    fn from(t: track_tags::AlbumTags) -> Self {
        AlbumTags {
            album_id: t.album_id,
            tracks: t
                .track_tags
                .into_iter()
                .map(|(f, tags)| (f, TagSet::new(tags)))
                .collect(),
            album_tags: TagSet::new(t.album_tags),
        }
    }
}

/// The `tags` operations.
pub struct Tags<'a> {
    pub(super) app: &'a App,
}

impl Tags<'_> {
    fn conn(&self) -> Result<Connection, OpError> {
        Ok(musaic_db::open_connection(self.app.db_path())?)
    }

    /// Run `f` over the tag environment (the library as configured now).
    fn with_env<R>(
        &self,
        f: impl FnOnce(&mut TagEnv<'_>) -> Result<R, OpError>,
    ) -> Result<R, OpError> {
        let mut conn = self.conn()?;
        self.app.with_tag_env(&mut conn, f)
    }

    /// The same under the work guard, taken first.
    fn writing<R>(
        &self,
        f: impl FnOnce(&mut TagEnv<'_>) -> Result<R, OpError>,
    ) -> Result<R, OpError> {
        let _guard = self.app.gate().begin_work()?;
        self.with_env(f)
    }

    /// The same on a plain connection with the clock, under the guard.
    fn editing<R>(
        &self,
        f: impl FnOnce(&mut Connection, &str) -> Result<R, OpError>,
    ) -> Result<R, OpError> {
        let _guard = self.app.gate().begin_work()?;
        let mut conn = self.conn()?;
        let now = self.app.now_iso();
        f(&mut conn, &now)
    }

    /// An album's tags. `NotFound` for an unknown album, `NoLibrary` without a
    /// library folder; a Missing album's tags can be read.
    pub fn album_tags(&self, album_id: &str) -> Result<AlbumTags, OpError> {
        self.with_env(|env| track_tags::get_album_tags(env, album_id))
            .map(Into::into)
    }

    /// Replace the tags of the named tracks wholesale (a track not named keeps
    /// its tags). `track_tags` is the request's map in its order; every name
    /// must be a FLAC of the album (`Rejected`), the empty name is skipped.
    pub fn replace_album_tags(
        &self,
        album_id: &str,
        track_tags: &[(String, Vec<String>)],
    ) -> Result<AlbumTags, OpError> {
        self.writing(|env| track_tags::replace_album_tags(env, album_id, track_tags))
            .map(Into::into)
    }

    /// Add `tag` to the named tracks, or to every track when `filenames` is
    /// `None` or empty. A blank tag or a name that is not a FLAC of the album is
    /// `Rejected`; an album with no tracks is a `Conflict`.
    pub fn add_album_tag(
        &self,
        album_id: &str,
        tag: &str,
        filenames: Option<&[String]>,
    ) -> Result<AlbumTags, OpError> {
        self.writing(|env| track_tags::add_album_tag(env, album_id, tag, filenames))
            .map(Into::into)
    }

    /// Remove `tag` from every track of the album.
    pub fn remove_album_tag(&self, album_id: &str, tag: &str) -> Result<AlbumTags, OpError> {
        self.writing(|env| track_tags::remove_album_tag(env, album_id, tag))
            .map(Into::into)
    }

    /// Add `tag` to one track. The answer is the album's whole tag state.
    pub fn add_track_tag(
        &self,
        album_id: &str,
        filename: &str,
        tag: &str,
    ) -> Result<AlbumTags, OpError> {
        self.writing(|env| track_tags::add_track_tag(env, album_id, filename, tag))
            .map(Into::into)
    }

    /// Remove `tag` from one track (a file the album only has rows for is
    /// accepted, so legacy data stays removable).
    pub fn remove_track_tag(
        &self,
        album_id: &str,
        filename: &str,
        tag: &str,
    ) -> Result<AlbumTags, OpError> {
        self.writing(|env| track_tags::remove_track_tag(env, album_id, filename, tag))
            .map(Into::into)
    }

    /// Every tag in use, with its track count.
    pub fn list_tags(&self) -> Result<TagCounts, OpError> {
        track_tags::list_tags(&self.conn()?)
    }

    /// The tracks carrying `tag`; a blank tag is `Rejected`.
    pub fn tracks_for_tag(&self, tag: &str) -> Result<TagTracks, OpError> {
        track_tags::tracks_for_tag(&self.conn()?, tag)
    }

    /// The vocabulary with the orphan tags (in use, no entry).
    pub fn list_vocabulary(&self) -> Result<Vocabulary, OpError> {
        vocabulary::list_vocabulary(&self.conn()?)
    }

    /// Add an entry (`description` `None` is empty). A blank tag is `Rejected`;
    /// a case-insensitive duplicate is not an error.
    pub fn add_vocabulary_entry(
        &self,
        tag: &str,
        description: Option<&str>,
        color: Option<&str>,
        sort_order: Option<i64>,
    ) -> Result<Vocabulary, OpError> {
        self.editing(|conn, now| {
            vocabulary::add_vocabulary_entry(conn, now, tag, description, color, sort_order)
        })
    }

    /// Add an entry with defaults for a tag already in use.
    pub fn promote_orphan_tag(&self, tag: &str) -> Result<Vocabulary, OpError> {
        self.editing(|conn, now| vocabulary::promote_orphan_tag(conn, now, tag))
    }

    /// Delete an entry; `strip_from_tracks` also removes the tag from every
    /// track. Refused as `Rejected` when the entry cannot be addressed.
    pub fn delete_vocabulary_entry(
        &self,
        tag: &str,
        strip_from_tracks: bool,
    ) -> Result<VocabularyDeleted, OpError> {
        self.editing(|conn, _| vocabulary::delete_vocabulary_entry(conn, tag, strip_from_tracks))
    }

    /// Change an entry's fields; an unknown tag changes nothing, a tag several
    /// entries fold to is a `Conflict`.
    pub fn patch_vocabulary_entry(
        &self,
        tag: &str,
        description: Option<&str>,
        color: Option<&str>,
        sort_order: Option<i64>,
    ) -> Result<Vocabulary, OpError> {
        self.editing(|conn, _| {
            vocabulary::patch_vocabulary_entry(conn, tag, description, color, sort_order)
        })
    }

    /// Fold `tag` into the entry `into_tag`; every refusal is `Rejected`.
    pub fn merge_vocabulary_entry(
        &self,
        tag: &str,
        into_tag: &str,
    ) -> Result<VocabularyMerged, OpError> {
        self.editing(|conn, _| vocabulary::merge_vocabulary_entry(conn, tag, into_tag))
    }

    /// Rename an entry and its track rows; the new name taken by another entry
    /// is a `Conflict`.
    pub fn rename_vocabulary_entry(
        &self,
        tag: &str,
        new_tag: &str,
    ) -> Result<VocabularyRenamed, OpError> {
        self.editing(|conn, _| vocabulary::rename_vocabulary_entry(conn, tag, new_tag))
    }
}
