//! The `tags` group as bodies: the album / track tag state, the tags in use and
//! the vocabulary with its change results, in the keys and order the Python app
//! answered.

use musaic_app::ops::record::TagSet;
use musaic_app::ops::tags::{
    AlbumTags, TagCounts, TagMerge, TagRewrite, TagTracks, Vocabulary, VocabularyDeleted,
    VocabularyEntry, VocabularyMerged, VocabularyRenamed,
};
use musaic_services::Json;

fn names<'a>(it: impl Iterator<Item = &'a str>) -> Json {
    Json::Arr(it.map(Json::from).collect())
}

fn tag_set(t: &TagSet) -> Json {
    names(t.iter())
}

fn strings(v: &[String]) -> Json {
    names(v.iter().map(String::as_str))
}

fn tag_count(tag: &str, count: i64) -> Json {
    Json::obj([("tag", Json::from(tag)), ("count", Json::Int(count))])
}

/// Every album / track tag route: `{album_id, track_tags, album_tags}`.
pub fn album_tags(t: &AlbumTags) -> Json {
    Json::obj([
        ("album_id", Json::from(t.album_id.as_str())),
        (
            "track_tags",
            Json::Obj(
                t.tracks
                    .iter()
                    .map(|(f, tags)| (f.clone(), tag_set(tags)))
                    .collect(),
            ),
        ),
        ("album_tags", tag_set(&t.album_tags)),
    ])
}

/// `GET /api/tags`.
pub fn tag_counts(t: &TagCounts) -> Json {
    Json::obj([(
        "tags",
        Json::Arr(t.tags.iter().map(|(t, n)| tag_count(t, *n)).collect()),
    )])
}

/// `GET /api/tags/{tag}/tracks`.
pub fn tag_tracks(t: &TagTracks) -> Json {
    Json::obj([
        ("tag", Json::from(t.tag.as_str())),
        (
            "tracks",
            Json::Arr(
                t.tracks
                    .iter()
                    .map(|x| {
                        Json::obj([
                            ("album_id", Json::from(x.album_id.as_str())),
                            ("filename", Json::from(x.file_name.as_str())),
                        ])
                    })
                    .collect(),
            ),
        ),
    ])
}

fn entry(e: &VocabularyEntry) -> Json {
    Json::obj([
        ("tag", Json::from(e.tag.as_str())),
        ("description", Json::from(e.description.as_str())),
        ("color", Json::from(e.color.clone())),
        ("sort_order", Json::Int(e.sort_order)),
        ("created_at", Json::from(e.created_at.as_str())),
        ("count", Json::Int(e.count)),
    ])
}

/// `GET /api/vocabulary` and the body every vocabulary write answers with:
/// `{vocabulary, orphans}`, plus `case_conflicts` when there is one.
pub fn vocabulary(v: &Vocabulary) -> Json {
    let mut body = Json::obj([
        (
            "vocabulary",
            Json::Arr(v.entries.iter().map(entry).collect()),
        ),
        (
            "orphans",
            Json::Arr(v.orphans.iter().map(|(t, n)| tag_count(t, *n)).collect()),
        ),
    ]);
    if !v.case_conflicts.is_empty() {
        body.set(
            "case_conflicts",
            Json::Arr(v.case_conflicts.iter().map(|g| strings(g)).collect()),
        );
    }
    body
}

/// `sidecars_not_written` appears only when a sidecar was not written.
fn with_not_written(mut result: Json, not_written: &[String]) -> Json {
    if !not_written.is_empty() {
        result.set("sidecars_not_written", strings(not_written));
    }
    result
}

fn rewrite(r: &TagRewrite) -> Json {
    with_not_written(
        Json::obj([
            ("vocab", Json::Bool(r.vocab)),
            ("tag_rows", Json::from(r.tag_rows)),
            ("albums", strings(&r.albums)),
        ]),
        &r.sidecars_not_written,
    )
}

fn merge(m: &TagMerge) -> Json {
    with_not_written(
        Json::obj([
            ("moved", Json::from(m.moved)),
            ("albums", strings(&m.albums)),
        ]),
        &m.sidecars_not_written,
    )
}

fn with(mut body: Json, key: &str, value: Json) -> Json {
    body.set(key, value);
    body
}

/// `POST /api/vocabulary/{tag}/rename`.
pub fn renamed(r: &VocabularyRenamed) -> Json {
    with(
        vocabulary(&r.vocabulary),
        "rename_result",
        rewrite(&r.renamed),
    )
}

/// `POST /api/vocabulary/{tag}/merge`.
pub fn merged(m: &VocabularyMerged) -> Json {
    with(vocabulary(&m.vocabulary), "merge_result", merge(&m.merged))
}

/// `DELETE /api/vocabulary/{tag}`.
pub fn deleted(d: &VocabularyDeleted) -> Json {
    with(
        vocabulary(&d.vocabulary),
        "delete_result",
        rewrite(&d.deleted),
    )
}
