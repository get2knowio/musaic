//! The `ChecksRerunFinished` event: one per batch of check runs, published
//! after the batch's findings are written.
//!
//! A batch is whatever one caller ran together: a post-scan sweep, a rename's
//! scoped rerun, a background sweep pass, a user-started run. Runs that did not
//! start (unknown check, database error, refused by the gate) are not
//! reported; a run that started and failed is, in `failed`, because it still
//! wrote what it found before failing and changed its run state. A batch
//! with no run that started publishes nothing.

use std::collections::BTreeSet;

use musaic_db::findings::RunRow;
use musaic_events::Event;

use crate::base::CheckEnv;

/// A batch limited to more albums than this is reported as library-wide
/// (empty `album_ids`), so a pass over a large library does not carry every
/// album id; the UI refreshes the same findings either way.
pub const RERUN_ALBUM_LIST_MAX: usize = 500;

/// The runs of one batch so far.
#[derive(Debug, Default)]
pub struct RerunReport {
    checks: Vec<String>,
    failed: Vec<String>,
    whole_library: bool,
    albums: BTreeSet<String>,
}

impl RerunReport {
    pub fn new() -> Self {
        Self::default()
    }

    /// A run of the batch that started, over `albums` (`None`: the whole
    /// library).
    pub fn record(&mut self, run: &RunRow, albums: Option<&[String]>) {
        if !self.checks.contains(&run.check_id) {
            self.checks.push(run.check_id.clone());
        }
        if run.status != "completed" && !self.failed.contains(&run.check_id) {
            self.failed.push(run.check_id.clone());
        }
        match albums {
            None => self.whole_library = true,
            Some(ids) => self.albums.extend(ids.iter().cloned()),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.checks.is_empty()
    }

    /// Publish the one event for the batch, unless no run started.
    pub fn publish(self, env: &CheckEnv) {
        if self.checks.is_empty() {
            return;
        }
        let album_ids = if self.whole_library || self.albums.len() > RERUN_ALBUM_LIST_MAX {
            Vec::new()
        } else {
            self.albums.into_iter().collect()
        };
        (env.publish)(Event::ChecksRerunFinished {
            checks: self.checks,
            failed: self.failed,
            album_ids,
        });
    }
}
