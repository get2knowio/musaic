//! The check registry (`musaic/checks/__init__.py::REGISTRY`), in dispatch
//! and display order.

use std::sync::Arc;

use crate::base::{Check, CheckMeta};
use crate::checks::{
    DuplicateTracks, FilenameMismatch, InconsistentArtistSort, NoCoverArt, TagsOutOfSync,
    TrackGaps, UnidentifiedAlbum,
};

/// An ordered list of checks. [`Registry::default`] is the Python app's; tests
/// and hosts may build their own.
#[derive(Clone)]
pub struct Registry {
    checks: Vec<Arc<dyn Check>>,
}

impl Default for Registry {
    /// Foundations first: identification, then tags, track numbers, sort
    /// clusters, cover, duplicates, filenames.
    fn default() -> Self {
        Registry::new(vec![
            Arc::new(UnidentifiedAlbum),
            Arc::new(TagsOutOfSync),
            Arc::new(TrackGaps),
            Arc::new(InconsistentArtistSort),
            Arc::new(NoCoverArt),
            Arc::new(DuplicateTracks),
            Arc::new(FilenameMismatch),
        ])
    }
}

impl Registry {
    pub fn new(checks: Vec<Arc<dyn Check>>) -> Self {
        Registry { checks }
    }

    /// `checks.get(check_id)`.
    pub fn get(&self, check_id: &str) -> Option<&Arc<dyn Check>> {
        self.checks.iter().find(|c| c.meta().id == check_id)
    }

    /// `checks.all_meta()`.
    pub fn all_meta(&self) -> Vec<&CheckMeta> {
        self.checks.iter().map(|c| c.meta()).collect()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Arc<dyn Check>> {
        self.checks.iter()
    }
}
