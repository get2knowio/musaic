//! [`AnchorStore`] over the library database (`musaic_db::mb_anchor`).

use std::collections::BTreeSet;

use rusqlite::Connection;

use crate::source::{AnchorStore, FingerprintRow, StoreError, StoredAnchor};

/// The anchor store the app uses: the library database, with `now_iso`
/// (`YYYY-MM-DDTHH:MM:SSZ`, the Python app's `_now_iso()`) stamped on writes.
pub struct DbStore<'a> {
    pub conn: &'a Connection,
    pub now_iso: String,
}

fn err(e: musaic_db::Error) -> StoreError {
    StoreError(e.to_string())
}

impl AnchorStore for DbStore<'_> {
    fn anchor(&self, album_id: &str) -> Result<Option<StoredAnchor>, StoreError> {
        Ok(
            musaic_db::mb_anchor::get_album_mb_anchor(self.conn, album_id)
                .map_err(err)?
                .map(|r| StoredAnchor {
                    release_group_mbid: r.release_group_mbid,
                    release_mbid: r.release_mbid,
                    score: r.score,
                    last_checked_at: r.last_checked_at,
                }),
        )
    }

    fn set_anchor(
        &self,
        album_id: &str,
        release_group_mbid: &str,
        release_mbid: Option<&str>,
        score: i64,
    ) -> Result<(), StoreError> {
        musaic_db::mb_anchor::set_album_mb_anchor(
            self.conn,
            album_id,
            release_group_mbid,
            release_mbid,
            score,
            &self.now_iso,
        )
        .map(|_| ())
        .map_err(err)
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
