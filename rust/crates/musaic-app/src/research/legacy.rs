//! Convert legacy results where the recorded source and current track IDs can
//! be recovered. The legacy record stays intact and verification stays pending.

use std::collections::BTreeMap;

use musaic_commands::Validate;
use musaic_commands::auth::random_hex;
use musaic_commands::tools::work::{
    CoverRecommendation, Evidence, MetadataRecommendation, MusicBrainzId, MusicBrainzKind,
    Recommendation, SCHEMA_VERSION, TrackChange, WorkContext, WorkKind, WorkOutcome, WorkResult,
    is_uuid,
};
use musaic_db::delegated_work::{self, Completion, WorkRow};
use musaic_db::research_history::JobRow;
use rusqlite::Connection;
use serde::Deserialize;

#[derive(Deserialize)]
struct HistoricalMetadata {
    mb_release_group_mbid: String,
    suggestions: BTreeMap<String, String>,
    tracks: Vec<HistoricalTrack>,
}

#[derive(Deserialize)]
struct HistoricalTrack {
    filename: String,
    suggestions: BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct HistoricalCover {
    cover_url: String,
    source: String,
}

fn metadata(result: HistoricalMetadata, work: &WorkRow) -> Option<(Recommendation, String)> {
    let id = result.mb_release_group_mbid.to_ascii_lowercase();
    if !is_uuid(&id) {
        return None;
    }

    let context: WorkContext = serde_json::from_str(&work.input_json).ok()?;
    let tracks = result
        .tracks
        .into_iter()
        .filter(|t| !t.suggestions.is_empty())
        .map(|change| {
            // Legacy file names are references only, and never joined onto a
            // path. Every change must map to the current work-local track ID.
            let current = context
                .tracks
                .iter()
                .find(|t| t.file_name == change.filename)?;
            Some(TrackChange {
                track_id: current.track_id.clone(),
                fields: change.suggestions,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    let source = format!("https://musicbrainz.org/release-group/{id}");
    Some((
        Recommendation::Metadata(MetadataRecommendation {
            identifiers: vec![MusicBrainzId {
                kind: MusicBrainzKind::ReleaseGroup,
                id,
            }],
            album_fields: result.suggestions,
            tracks,
        }),
        source,
    ))
}

fn cover(result: HistoricalCover) -> Option<(Recommendation, String)> {
    let page = match musaic_net::preview::check_public_url(&result.source) {
        Ok(_) => result.source,
        Err(_) => cover_archive_source(&result.cover_url)?,
    };
    Some((
        Recommendation::Cover(CoverRecommendation {
            url: result.cover_url,
            source_page: page.clone(),
            width: None,
            height: None,
            rationale: "The earlier research suggested this cover.".into(),
            rights_uncertainty: "The earlier research did not record usage rights.".into(),
        }),
        page,
    ))
}

pub(super) fn cover_archive_source(url: &str) -> Option<String> {
    let (kind, rest) = url
        .strip_prefix("https://coverartarchive.org/")?
        .split_once('/')?;
    let (id, _) = rest.split_once('/')?;
    (matches!(kind, "release" | "release-group") && is_uuid(id))
        .then(|| format!("https://musicbrainz.org/{kind}/{id}/cover-art"))
}

fn converted(work: &WorkRow, job: &JobRow) -> Option<WorkResult> {
    // Parse only the requested child. A malformed sibling must not hide a
    // result whose own recorded source and fields can still be recovered.
    let raw: serde_json::Value = serde_json::from_str(job.result_json.as_ref()?).ok()?;
    let kind = WorkKind::parse(&work.kind)?;
    let (recommendation, source) = match kind {
        WorkKind::MetadataIdentify | WorkKind::MetadataCorrect => metadata(
            serde_json::from_value(raw.get("metadata")?.clone()).ok()?,
            work,
        )?,
        WorkKind::CoverResearch => cover(serde_json::from_value(raw.get("cover")?.clone()).ok()?)?,
    };
    let evidence = Evidence {
        source,
        retrieved_at: job.completed_at.clone()?,
        claim: "The earlier Musaic job recorded this reference at completion.".into(),
        limitations: vec![
            "The job did not record the underlying source response or its retrieval time.".into(),
        ],
    };
    let proposal = WorkResult {
        schema_version: SCHEMA_VERSION,
        work_id: work.id.clone(),
        kind,
        input_digest: work.input_digest.clone(),
        outcome: WorkOutcome::Proposal,
        evidence: vec![evidence],
        confidence: 0.0,
        uncertainties: vec![
            "The original match and evidence still need current verification.".into(),
        ],
        recommendation,
    };
    proposal.validate().ok()?;
    super::patch(work, &proposal).ok()?;
    Some(proposal)
}

pub(crate) fn convert(
    conn: &Connection,
    work: &WorkRow,
    job: &JobRow,
    now: i64,
) -> musaic_db::Result<()> {
    if work.state != "awaiting_review"
        || work.result_id.is_some()
        || work.base_revision.is_empty()
        || job.status != "completed"
    {
        return Ok(());
    }
    if let Some(proposal) = converted(work, job) {
        let json = crate::agent::to_json_text(&proposal);
        let result_id = random_hex(16);
        let _ = delegated_work::store_legacy_result(
            conn,
            &work.id,
            Completion {
                key: "",
                request_digest: "",
                input_digest: &work.input_digest,
                id: &result_id,
                digest: &super::digest(&json),
                result_json: &json,
                validation: "pending",
            },
            now,
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE_ID: &str = "988a745d-7bc8-4b08-9b6c-f6bc378805f7";

    fn historical(
        kind: WorkKind,
        result: serde_json::Value,
    ) -> (tempfile::TempDir, musaic_db::Db, WorkRow, JobRow) {
        let dir = tempfile::tempdir().unwrap();
        let (db, _) = musaic_db::Db::open_or_create(dir.path().join("library.db")).unwrap();
        let context = WorkContext {
            schema_version: SCHEMA_VERSION,
            album_id: "album".into(),
            title: "Fixture Album".into(),
            artist: "Fixture Artist".into(),
            hint: None,
            tracks: vec![musaic_commands::tools::work::WorkTrack {
                track_id: "a".repeat(32),
                file_name: "01.flac".into(),
                title: None,
            }],
        };
        let id = "4d126877-7876-495b-9cbe-d16136cb6d38";
        let new = delegated_work::NewWork {
            id,
            group_id: "group",
            kind: kind.as_str(),
            album_id: "album",
            base_revision: "first",
            input_json: &serde_json::to_string(&context).unwrap(),
            input_digest: &"a".repeat(64),
            max_attempts: delegated_work::DEFAULT_MAX_ATTEMPTS,
        };
        let now_iso = "2026-11-19T00:00:00Z";
        let job = JobRow {
            id: "legacy".into(),
            album_id: "album".into(),
            album_path: "/synthetic/album".into(),
            album_title: "Fixture Album".to_owned(),
            job_type: "metadata+cover".into(),
            hint: String::new(),
            status: "completed".into(),
            result_json: Some(result.to_string()),
            error: None,
            acted_on_action: None,
            created_at: now_iso.into(),
            started_at: None,
            completed_at: Some(now_iso.into()),
            acted_on_at: None,
            attach_cover: false,
        };
        delegated_work::enqueue(db.connection(), &[new], 1).unwrap();
        db.connection()
            .execute(
                "UPDATE delegated_work SET state='awaiting_review',legacy_job_id=?2 WHERE id=?1",
                [id, &job.id],
            )
            .unwrap();
        let row = delegated_work::get(db.connection(), id).unwrap().unwrap();
        (dir, db, row, job)
    }

    #[test]
    fn documented_legacy_metadata_becomes_single_pending_review() {
        let (_dir, db, row, job) = historical(
            WorkKind::MetadataIdentify,
            serde_json::json!({
                "metadata":{
                    "mb_release_group_mbid":SOURCE_ID,
                    "suggestions":{"title":"Correct album"},
                    "tracks":[{"filename":"01.flac","suggestions":{"title":"Correct track"}}]
                },
                "cover":false
            }),
        );
        let proposal = converted(&row, &job).unwrap();
        let Recommendation::Metadata(metadata) = &proposal.recommendation else {
            panic!("expected a metadata proposal");
        };
        assert_eq!(metadata.identifiers[0].kind, MusicBrainzKind::ReleaseGroup);
        assert_eq!(metadata.tracks[0].track_id, "a".repeat(32));
        convert(db.connection(), &row, &job, 2).unwrap();
        let saved = delegated_work::get(db.connection(), &row.id)
            .unwrap()
            .unwrap();
        let result = delegated_work::result(db.connection(), saved.result_id.as_deref().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(result.validation, "pending");
        assert_eq!(
            serde_json::from_str::<WorkResult>(&result.result_json).unwrap(),
            proposal
        );
        convert(db.connection(), &saved, &job, 3).unwrap();
        assert_eq!(
            delegated_work::get(db.connection(), &row.id)
                .unwrap()
                .unwrap(),
            saved
        );
    }

    #[test]
    fn a_foreign_track_or_nonpublic_cover_stays_history() {
        let (_dir, _db, row, job) = historical(
            WorkKind::MetadataIdentify,
            serde_json::json!({
                "metadata":{
                    "mb_release_group_mbid":SOURCE_ID,
                    "suggestions":{},
                    "tracks":[{"filename":"../foreign.flac","suggestions":{"title":"Foreign"}}]
                }
            }),
        );
        assert!(converted(&row, &job).is_none());

        let (_dir, _db, row, job) = historical(
            WorkKind::CoverResearch,
            serde_json::json!({
                "cover":{"cover_url":"https://127.0.0.1/front","source":"https://example.org/source"}
            }),
        );
        assert!(converted(&row, &job).is_none());
    }
}
