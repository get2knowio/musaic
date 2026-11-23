//! Immutable research context and revalidation against the current library.

pub(crate) mod legacy;
pub(crate) mod local;

use std::collections::BTreeMap;
use std::path::Path;

use musaic_commands::auth::hex_encode;
use musaic_commands::tools::fix::{CoverPatch, ProposedPatch, TagsPatch};
use musaic_commands::tools::work::{
    MusicBrainzKind, Recommendation, SCHEMA_VERSION, WorkContext, WorkResult, WorkTrack,
};
use musaic_commands::{ToolError, ToolErrorCode};
use musaic_db::delegated_work::{self, WorkRow};
use musaic_net::preview::{PreviewError, check_public_url, fetch_preview};
use musaic_net::{ClientError, MusicBrainzClient};
use musaic_services::agent_read;
use rusqlite::Connection;
use sha2::{Digest, Sha256};

use crate::App;

pub(crate) fn digest(text: &str) -> String {
    hex_encode(&Sha256::digest(text.as_bytes()))
}

pub(crate) fn track_id(album_id: &str, revision: &str, file_name: &str) -> String {
    digest(&format!("{album_id}\0{revision}\0{file_name}"))[..32].to_owned()
}

pub(crate) fn context(
    app: &App,
    conn: &Connection,
    album_id: &str,
    hint: Option<String>,
) -> Result<(String, WorkContext), ToolError> {
    let library = app.library_dir();
    let row = agent_read::cached_row(conn, library.as_deref(), album_id)?;
    if row.missing_since.is_some() || !Path::new(&row.album.path).is_dir() {
        return Err(ToolError::new(
            ToolErrorCode::MissingAlbum,
            "the album folder is missing",
        ));
    }
    let snapshot = agent_read::album_get(conn, library.as_deref(), album_id)?;
    let tracks = snapshot
        .tracks
        .into_iter()
        .take(500)
        .map(|t| WorkTrack {
            track_id: track_id(album_id, &snapshot.revision, &t.file_name),
            file_name: t.file_name,
            title: t.title.map(|title| title.chars().take(250).collect()),
        })
        .collect();
    let ctx = WorkContext {
        schema_version: SCHEMA_VERSION,
        album_id: album_id.to_owned(),
        title: snapshot.album.title.chars().take(500).collect(),
        artist: snapshot.album.artist.chars().take(500).collect(),
        hint: hint.map(|h| h.chars().take(1000).collect()),
        tracks,
    };
    if crate::agent::to_json_text(&ctx).len() > 256 * 1024 {
        return Err(ToolError::new(
            ToolErrorCode::InvalidInput,
            "the album exceeds the research context limit",
        ));
    }
    Ok((snapshot.revision, ctx))
}

fn stale() -> ToolError {
    ToolError::new(
        ToolErrorCode::StaleContext,
        "the library or album changed since the request was queued",
    )
}

pub(crate) fn current(app: &App, conn: &Connection, work: &WorkRow) -> Result<(), ToolError> {
    let epoch = delegated_work::epoch(conn).map_err(|_| {
        ToolError::new(
            ToolErrorCode::Internal,
            "the library epoch could not be read",
        )
    })?;
    if epoch != work.library_epoch {
        return Err(stale());
    }
    let row = agent_read::cached_row(conn, app.library_dir().as_deref(), &work.album_id).map_err(
        |e| match e.code {
            ToolErrorCode::NoLibrary | ToolErrorCode::NotFound => stale(),
            _ => e,
        },
    )?;
    if row.missing_since.is_some()
        || !Path::new(&row.album.path).is_dir()
        || agent_read::revision_of(&row) != work.base_revision
    {
        return Err(stale());
    }
    Ok(())
}

fn invalid(message: &str) -> ToolError {
    ToolError::new(ToolErrorCode::InvalidResult, message)
}

pub(crate) fn patch(work: &WorkRow, result: &WorkResult) -> Result<ProposedPatch, ToolError> {
    let mut patch = ProposedPatch::default();
    match &result.recommendation {
        Recommendation::Metadata(r) => {
            let snapshot: WorkContext = serde_json::from_str(&work.input_json)
                .map_err(|_| invalid("the request context needs to be refreshed"))?;
            let names: Vec<String> = snapshot
                .tracks
                .iter()
                .map(|t| t.file_name.clone())
                .collect();
            let changes = |fields: &BTreeMap<String, String>| {
                fields
                    .iter()
                    .map(|(field, value)| (field.clone(), Some(value.clone())))
                    .collect()
            };
            let mut tracks = BTreeMap::new();
            for track in &r.tracks {
                let entry = snapshot
                    .tracks
                    .iter()
                    .find(|t| t.track_id == track.track_id)
                    .ok_or_else(|| invalid("a track id does not belong to this album"))?;
                tracks.insert(entry.file_name.clone(), changes(&track.fields));
            }
            patch.tags = Some(TagsPatch {
                album: Some(changes(&r.album_fields)),
                tracks: Some(tracks),
            });
            musaic_services::agent_proposal::apply_parts(&patch, &names)
                .map_err(|_| invalid("the proposal includes an unsupported tag or value"))?;
        }
        Recommendation::Cover(r) => {
            check_public_url(&r.url)
                .map_err(|_| invalid("the cover requires a public HTTPS image URL"))?;
            check_public_url(&r.source_page)
                .map_err(|_| invalid("the cover requires a public HTTPS source page"))?;
            patch.cover = Some(CoverPatch::Url { url: r.url.clone() });
        }
        Recommendation::Unresolved(_) => {}
    }
    Ok(patch)
}

fn verify_musicbrainz(
    app: &App,
    proposal: &musaic_commands::tools::work::MetadataRecommendation,
) -> Result<bool, ToolError> {
    type Client = MusicBrainzClient<musaic_services::production::SharedTransport>;
    let client = Client::new(app.0.transport.clone());
    for identifier in &proposal.identifiers {
        let request = match identifier.kind {
            MusicBrainzKind::ReleaseGroup => Client::release_group_request(&identifier.id),
            MusicBrainzKind::Release => Client::release_request(&identifier.id),
            MusicBrainzKind::Recording => Client::recording_canonical_request(&identifier.id),
        };
        let body = match client.body(request) {
            Ok(body) => body,
            Err(ClientError::Transport(e)) if e.is_offline() => return Ok(false),
            Err(ClientError::Status { status: 404, .. }) => {
                return Err(invalid(
                    "MusicBrainz does not recognize a supplied identifier",
                ));
            }
            Err(_) => {
                return Err(ToolError::new(
                    ToolErrorCode::Unavailable,
                    "MusicBrainz verification could not finish",
                ));
            }
        };
        let advertised_id = serde_json::from_slice::<serde_json::Value>(&body)
            .ok()
            .and_then(|doc| doc.get("id").and_then(|id| id.as_str()).map(str::to_owned));
        if advertised_id.as_deref() != Some(&identifier.id) {
            return Err(invalid("MusicBrainz returned a different identifier"));
        }
    }
    Ok(true)
}

pub(crate) fn verify_result(
    app: &App,
    conn: &Connection,
    work: &WorkRow,
    result: &WorkResult,
) -> Result<&'static str, ToolError> {
    use musaic_commands::Validate;
    result.validate()?;
    if result.work_id != work.id
        || result.kind.as_str() != work.kind
        || result.input_digest != work.input_digest
    {
        return Err(invalid(
            "the result does not match the requested work or input",
        ));
    }
    current(app, conn, work)?;
    patch(work, result)?;
    let verified = match &result.recommendation {
        Recommendation::Metadata(r) => verify_musicbrainz(app, r)?,
        Recommendation::Cover(r) => match fetch_preview(&app.0.transport, &r.url) {
            Ok(image) => {
                let (width, height) =
                    musaic_tags::cover::verify_image(&image.bytes).ok_or_else(|| {
                        invalid("the cover exceeds decoded image limits or is unreadable")
                    })?;
                if r.width.is_some_and(|v| v != width) || r.height.is_some_and(|v| v != height) {
                    return Err(invalid(
                        "the cover dimensions differ from the recommendation",
                    ));
                }
                true
            }
            Err(PreviewError::Transport(e)) if e.is_offline() => false,
            Err(PreviewError::Transport(e))
                if !matches!(
                    e,
                    musaic_net::TransportError::Refused { .. }
                        | musaic_net::TransportError::BodyTooLarge { .. }
                ) =>
            {
                return Err(ToolError::new(
                    ToolErrorCode::Unavailable,
                    "cover verification could not finish",
                ));
            }
            Err(_) => {
                return Err(invalid(
                    "the cover cannot be used under the public image policy",
                ));
            }
        },
        Recommendation::Unresolved(_) => true,
    };
    current(app, conn, work)?;
    Ok(if verified { "verified" } else { "pending" })
}
