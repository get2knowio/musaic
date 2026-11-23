//! Typed review proposals built from deterministic resolver results.

use musaic_ai::parse::{CoverSuggestion, MetadataSuggestion};
use musaic_commands::Validate;
use musaic_commands::tools::work::{
    CoverRecommendation, Evidence, MetadataRecommendation, MusicBrainzId, MusicBrainzKind,
    Recommendation, SCHEMA_VERSION, TrackChange, UnresolvedRecommendation, WorkContext, WorkKind,
    WorkOutcome, WorkResult, is_uuid,
};
use musaic_db::delegated_work::{self, Completion, WorkRow};
use rusqlite::Transaction;

use super::{digest, patch};

pub(crate) fn retain(
    tx: &Transaction<'_>,
    work: &WorkRow,
    metadata: Option<&MetadataSuggestion>,
    cover: Option<&CoverSuggestion>,
    retrieved_at: &str,
    now: i64,
) -> musaic_db::Result<()> {
    let ctx = serde_json::from_str::<WorkContext>(&work.input_json).ok();
    let Some(kind) = WorkKind::parse(&work.kind) else {
        return Ok(());
    };
    let result = match kind {
        WorkKind::MetadataCorrect | WorkKind::MetadataIdentify => ctx
            .zip(metadata)
            .and_then(|(ctx, metadata)| metadata_proposal(metadata, &ctx)),
        WorkKind::CoverResearch => cover.and_then(cover_proposal),
    };
    let Some((recommendation, evidence, validation)) = result else {
        return Ok(());
    };
    let outcome = if matches!(&recommendation, Recommendation::Unresolved(_)) {
        WorkOutcome::NoMatch
    } else {
        WorkOutcome::Proposal
    };
    let result = WorkResult {
        schema_version: SCHEMA_VERSION,
        work_id: work.id.clone(),
        kind,
        input_digest: work.input_digest.clone(),
        outcome,
        confidence: musaic_ai::synth::SKIP_CONFIDENCE,
        evidence: vec![Evidence {
            source: evidence,
            retrieved_at: retrieved_at.to_owned(),
            claim: "The local resolver found an established match for this album.".into(),
            limitations: vec![],
        }],
        uncertainties: vec![],
        recommendation,
    };
    if result.validate().is_ok() && patch(work, &result).is_ok() {
        let json = crate::agent::to_json_text(&result);
        delegated_work::store_local_result(
            tx,
            work,
            Completion {
                key: "",
                request_digest: "",
                input_digest: &work.input_digest,
                id: &crate::agent::delegation::uuid(),
                digest: &digest(&json),
                result_json: &json,
                validation,
            },
            now,
        )?;
    }
    Ok(())
}

type LocalProposal = (Recommendation, String, &'static str);

fn metadata_proposal(metadata: &MetadataSuggestion, ctx: &WorkContext) -> Option<LocalProposal> {
    let id = metadata
        .mb_release_group_mbid
        .as_deref()?
        .to_ascii_lowercase();
    if !is_uuid(&id) {
        return None;
    }
    let tracks = metadata
        .tracks
        .iter()
        .map(|track| {
            let current = ctx
                .tracks
                .iter()
                .find(|current| current.file_name == track.filename)?;
            Some(TrackChange {
                track_id: current.track_id.clone(),
                fields: track.suggestions.iter().cloned().collect(),
            })
        })
        .collect::<Option<Vec<_>>>()?;
    let source = format!("https://musicbrainz.org/release-group/{id}");
    let recommendation = if metadata.suggestions.is_empty() && tracks.is_empty() {
        Recommendation::Unresolved(UnresolvedRecommendation {
            explanation: "The album tags already agree with the local metadata match.".into(),
            question: "Does the release require any additional metadata?".into(),
        })
    } else {
        Recommendation::Metadata(MetadataRecommendation {
            identifiers: vec![MusicBrainzId {
                kind: MusicBrainzKind::ReleaseGroup,
                id,
            }],
            album_fields: metadata.suggestions.iter().cloned().collect(),
            tracks,
        })
    };
    Some((recommendation, source, "verified"))
}

fn cover_proposal(cover: &CoverSuggestion) -> Option<LocalProposal> {
    let url = cover.cover_url.as_ref()?;
    let source = super::legacy::cover_archive_source(url)?;
    Some((
        Recommendation::Cover(CoverRecommendation {
            url: url.clone(),
            source_page: source.clone(),
            width: None,
            height: None,
            rationale: "The local resolver matched this cover to the MusicBrainz release group."
                .into(),
            rights_uncertainty: "The resolver did not verify usage rights.".into(),
        }),
        source,
        "pending",
    ))
}
