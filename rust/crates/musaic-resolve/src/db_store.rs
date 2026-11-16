//! [`AnchorStore`] over the library database (`musaic_db::mb_anchor`).

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

use rusqlite::Connection;

use crate::source::{AnchorStore, FingerprintRow, StoreError, StoredAnchor};

/// The anchor store the app uses: the library database, with `now_iso`
/// (`YYYY-MM-DDTHH:MM:SSZ`, the Python app's `_now_iso()`) stamped on writes.
pub struct DbStore<'a> {
    pub conn: &'a Connection,
    pub now_iso: String,
    /// The release group each album had when this store last read (or
    /// wrote) it. A write is a compare-and-set against it
    /// (`set_album_mb_anchor_if`), so a user's change made while the
    /// resolver was waiting on MusicBrainz is never overwritten.
    pub read: RefCell<BTreeMap<String, Option<String>>>,
}

impl<'a> DbStore<'a> {
    pub fn new(conn: &'a Connection, now_iso: String) -> Self {
        DbStore {
            conn,
            now_iso,
            read: RefCell::default(),
        }
    }
}

fn err(e: musaic_db::Error) -> StoreError {
    StoreError(e.to_string())
}

impl AnchorStore for DbStore<'_> {
    fn anchor(&self, album_id: &str) -> Result<Option<StoredAnchor>, StoreError> {
        let row = musaic_db::mb_anchor::get_album_mb_anchor(self.conn, album_id).map_err(err)?;
        self.read.borrow_mut().insert(
            album_id.to_string(),
            row.as_ref().map(|r| r.release_group_mbid.clone()),
        );
        Ok(row.map(|r| StoredAnchor {
            release_group_mbid: r.release_group_mbid,
            release_mbid: r.release_mbid,
            score: r.score,
            last_checked_at: r.last_checked_at,
        }))
    }

    fn set_anchor(
        &self,
        album_id: &str,
        release_group_mbid: &str,
        release_mbid: Option<&str>,
        score: i64,
    ) -> Result<(), StoreError> {
        // Not read yet by this store: what is there now.
        let expected = match self.read.borrow().get(album_id) {
            Some(e) => e.clone(),
            None => musaic_db::mb_anchor::get_album_mb_anchor(self.conn, album_id)
                .map_err(err)?
                .map(|r| r.release_group_mbid),
        };
        let written = musaic_db::mb_anchor::set_album_mb_anchor_if(
            self.conn,
            album_id,
            expected.as_deref(),
            release_group_mbid,
            release_mbid,
            score,
            &self.now_iso,
        )
        .map_err(err)?;
        if !written {
            return Err(StoreError(
                "the album's MusicBrainz match changed or was blocked meanwhile; not overwritten"
                    .into(),
            ));
        }
        self.read
            .borrow_mut()
            .insert(album_id.to_string(), Some(release_group_mbid.to_string()));
        Ok(())
    }

    fn blocked(&self, album_id: &str) -> Result<BTreeSet<String>, StoreError> {
        musaic_db::mb_anchor::get_blocked_mb_release_groups(self.conn, album_id).map_err(err)
    }

    fn fingerprints(&self, album_id: &str) -> Result<Vec<FingerprintRow>, StoreError> {
        Ok(
            musaic_db::mb_anchor::get_track_fingerprints(self.conn, album_id)
                .map_err(err)?
                .into_iter()
                .map(|r| FingerprintRow {
                    filename: r.filename,
                    recording_mbid: r.acoustid_recording_mbid,
                    score: r.acoustid_score,
                })
                .collect(),
        )
    }
}
