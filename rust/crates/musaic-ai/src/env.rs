//! What the resolver synthesis needs from the host.

use std::sync::Arc;

use musaic_resolve::AlbumResolution;
use rusqlite::Connection;

use crate::pyjson::Py;

/// The host's resolver and track reader, as the synthesis of a research
/// request uses them.
pub trait ResolverEnv {
    /// `services.resolver.resolve_album(album_id, user_hint=hint)`.
    fn resolve(
        &self,
        conn: &Connection,
        album_id: &str,
        hint: &str,
    ) -> Result<Arc<AlbumResolution>, String>;
    /// `services.albums.read_tracks(album["path"])`.
    fn read_tracks(&self, album_path: &str) -> Py;
}
