//! `musaic/api/tags.py`: the six album / track tag routes, the two
//! `/api/tags` routes and the seven `/api/vocabulary` routes, over
//! `ops().tags()`.
//!
//! Every handler answers the Python app's 422 for its body / query first, then
//! calls one operation and renders the answer (`crate::render::tags`). The
//! restore gate's guard, the Missing-album refusal and the order of the
//! preconditions are the operations' (`docs/operations.md`). Reproduced from the
//! Python app: a case-only vocabulary rename drops the tag, any filename the
//! album lists is accepted, and rename / merge / strip fold case for ASCII only.

use axum::body::Bytes;
use axum::extract::{Path as UrlPath, State};
use serde_json::{Value, json};

use crate::http::{ApiResult, blocking, ok_ordered};
use crate::lax::{Fields, KeyOrder, Params, err, parse_json_body, unprocessable};
use crate::render::tags as render;
use crate::routes::library::{Q, qp};
use crate::state::AppState;

// ------------------------------------------------------------- album tags

pub async fn get_album_tags(
    State(st): State<AppState>,
    UrlPath(album_id): UrlPath<String>,
) -> ApiResult {
    blocking(move || {
        let tags = st.ops().tags().album_tags(&album_id)?;
        ok_ordered(&render::album_tags(&tags))
    })
    .await?
}

/// `dict[str, list[str]]` (required): entries in body order (`order`, the
/// body's [`KeyOrder`]; the Python app inserts the rows, and
/// reports a bad entry, in that order).
fn str_list_map(
    body: &Value,
    order: &KeyOrder,
    name: &str,
    errors: &mut Vec<Value>,
) -> Vec<(String, Vec<String>)> {
    let loc = |extra: Vec<Value>| {
        let mut l = vec![json!("body"), json!(name)];
        l.extend(extra);
        l
    };
    match body.get(name) {
        None => {
            errors.push(err("missing", loc(vec![]), "Field required", body.clone()));
            Vec::new()
        }
        Some(Value::Object(m)) => {
            let mut out = Vec::new();
            for (k, v) in &order.child(name).entries(m.clone()) {
                match v {
                    Value::Array(items) => {
                        let mut tags = Vec::new();
                        for (i, x) in items.iter().enumerate() {
                            match x {
                                Value::String(s) => tags.push(s.clone()),
                                other => errors.push(err(
                                    "string_type",
                                    loc(vec![json!(k), json!(i)]),
                                    "Input should be a valid string",
                                    other.clone(),
                                )),
                            }
                        }
                        out.push((k.clone(), tags));
                    }
                    other => errors.push(err(
                        "list_type",
                        loc(vec![json!(k)]),
                        "Input should be a valid list",
                        other.clone(),
                    )),
                }
            }
            out
        }
        Some(other) => {
            errors.push(err(
                "dict_type",
                loc(vec![]),
                "Input should be a valid dictionary",
                other.clone(),
            ));
            Vec::new()
        }
    }
}

pub async fn replace_album_tags(
    State(st): State<AppState>,
    UrlPath(album_id): UrlPath<String>,
    body: Bytes,
) -> ApiResult {
    let f = Fields::required(parse_json_body(&body)?)?;
    f.finish()?;
    let v: Value = parse_json_body(&body)?.unwrap_or(Value::Null);
    let mut errors = Vec::new();
    let map = str_list_map(&v, &KeyOrder::of_body(&body), "track_tags", &mut errors);
    if !errors.is_empty() {
        return Err(unprocessable(errors));
    }
    blocking(move || {
        let tags = st.ops().tags().replace_album_tags(&album_id, &map)?;
        ok_ordered(&render::album_tags(&tags))
    })
    .await?
}

pub async fn add_album_tag(
    State(st): State<AppState>,
    UrlPath(album_id): UrlPath<String>,
    body: Bytes,
) -> ApiResult {
    let mut f = Fields::required(parse_json_body(&body)?)?;
    let tag = f.req_str("tag");
    let filenames = f.opt_str_list("filenames");
    f.finish()?;
    let tag = tag.unwrap_or_default();
    blocking(move || {
        let tags = st
            .ops()
            .tags()
            .add_album_tag(&album_id, &tag, filenames.as_deref())?;
        ok_ordered(&render::album_tags(&tags))
    })
    .await?
}

pub async fn remove_album_tag(
    State(st): State<AppState>,
    UrlPath((album_id, tag)): UrlPath<(String, String)>,
) -> ApiResult {
    blocking(move || {
        let tags = st.ops().tags().remove_album_tag(&album_id, &tag)?;
        ok_ordered(&render::album_tags(&tags))
    })
    .await?
}

pub async fn add_track_tag(
    State(st): State<AppState>,
    UrlPath((album_id, filename)): UrlPath<(String, String)>,
    body: Bytes,
) -> ApiResult {
    let mut f = Fields::required(parse_json_body(&body)?)?;
    let tag = f.req_str("tag");
    f.finish()?;
    let tag = tag.unwrap_or_default();
    blocking(move || {
        let tags = st.ops().tags().add_track_tag(&album_id, &filename, &tag)?;
        ok_ordered(&render::album_tags(&tags))
    })
    .await?
}

pub async fn remove_track_tag(
    State(st): State<AppState>,
    UrlPath((album_id, filename, tag)): UrlPath<(String, String, String)>,
) -> ApiResult {
    blocking(move || {
        let tags = st
            .ops()
            .tags()
            .remove_track_tag(&album_id, &filename, &tag)?;
        ok_ordered(&render::album_tags(&tags))
    })
    .await?
}

// ------------------------------------------------------------- /api/tags

pub async fn list_tags(State(st): State<AppState>) -> ApiResult {
    blocking(move || {
        let tags = st.ops().tags().list_tags()?;
        ok_ordered(&render::tag_counts(&tags))
    })
    .await?
}

pub async fn tracks_for_tag(
    State(st): State<AppState>,
    UrlPath(tag): UrlPath<String>,
) -> ApiResult {
    blocking(move || {
        let tracks = st.ops().tags().tracks_for_tag(&tag)?;
        ok_ordered(&render::tag_tracks(&tracks))
    })
    .await?
}

// ------------------------------------------------------------- vocabulary

pub async fn list_vocabulary(State(st): State<AppState>) -> ApiResult {
    blocking(move || {
        let v = st.ops().tags().list_vocabulary()?;
        ok_ordered(&render::vocabulary(&v))
    })
    .await?
}

pub async fn add_vocabulary_entry(State(st): State<AppState>, body: Bytes) -> ApiResult {
    let mut f = Fields::required(parse_json_body(&body)?)?;
    let tag = f.req_str("tag");
    let description = f.str_or("description", "");
    let color = f.opt_str("color");
    let sort_order = f.opt_int("sort_order");
    f.finish()?;
    let tag = tag.unwrap_or_default();
    blocking(move || {
        let v = st.ops().tags().add_vocabulary_entry(
            &tag,
            Some(&description),
            color.as_deref(),
            sort_order,
        )?;
        ok_ordered(&render::vocabulary(&v))
    })
    .await?
}

pub async fn patch_vocabulary_entry(
    State(st): State<AppState>,
    UrlPath(tag): UrlPath<String>,
    body: Bytes,
) -> ApiResult {
    let mut f = Fields::required(parse_json_body(&body)?)?;
    let description = f.opt_str("description");
    let color = f.opt_str("color");
    let sort_order = f.opt_int("sort_order");
    f.finish()?;
    blocking(move || {
        let v = st.ops().tags().patch_vocabulary_entry(
            &tag,
            description.as_deref(),
            color.as_deref(),
            sort_order,
        )?;
        ok_ordered(&render::vocabulary(&v))
    })
    .await?
}

pub async fn rename_vocabulary_entry(
    State(st): State<AppState>,
    UrlPath(tag): UrlPath<String>,
    body: Bytes,
) -> ApiResult {
    let mut f = Fields::required(parse_json_body(&body)?)?;
    let new_tag = f.req_str("new_tag");
    f.finish()?;
    let new_tag = new_tag.unwrap_or_default();
    blocking(move || {
        let r = st.ops().tags().rename_vocabulary_entry(&tag, &new_tag)?;
        ok_ordered(&render::renamed(&r))
    })
    .await?
}

pub async fn merge_vocabulary_entry(
    State(st): State<AppState>,
    UrlPath(tag): UrlPath<String>,
    body: Bytes,
) -> ApiResult {
    let mut f = Fields::required(parse_json_body(&body)?)?;
    let into_tag = f.req_str("into_tag");
    f.finish()?;
    let into_tag = into_tag.unwrap_or_default();
    blocking(move || {
        let m = st.ops().tags().merge_vocabulary_entry(&tag, &into_tag)?;
        ok_ordered(&render::merged(&m))
    })
    .await?
}

pub async fn delete_vocabulary_entry(
    State(st): State<AppState>,
    UrlPath(tag): UrlPath<String>,
    q: Q,
) -> ApiResult {
    let q = qp(q);
    let mut p = Params::default();
    let strip = p.query_bool(&q, "strip_from_tracks", false);
    p.finish()?;
    blocking(move || {
        let d = st.ops().tags().delete_vocabulary_entry(&tag, strip)?;
        ok_ordered(&render::deleted(&d))
    })
    .await?
}

pub async fn promote_orphan_tag(
    State(st): State<AppState>,
    UrlPath(tag): UrlPath<String>,
) -> ApiResult {
    blocking(move || {
        let v = st.ops().tags().promote_orphan_tag(&tag)?;
        ok_ordered(&render::vocabulary(&v))
    })
    .await?
}
