//! `musaic/api/tags.py`: the six album / track tag routes, the two
//! `/api/tags` routes and the seven `/api/vocabulary` routes, over `musaic_services::{track_tags,
//! vocabulary}`.
//!
//! Every route answers the Python app's 422 for its body / query first, then
//! (mutations only) takes the restore gate's [`crate::WorkGuard`]
//! (refused with 409 while a restore runs), then runs on a blocking task.
//! Reproduced from the Python app: a case-only vocabulary rename drops the
//! tag, a Missing album's tags can be changed, any filename is accepted,
//! and rename / merge / strip fold case for ASCII only.

use axum::body::Bytes;
use axum::extract::{Path as UrlPath, State};
use musaic_services::track_tags::{self, TagEnv};
use musaic_services::{HttpError, Json, vocabulary};
use rusqlite::Connection;
use serde_json::{Value, json};

use crate::http::{ApiError, ApiResult, blocking, ok_ordered};
use crate::lax::{Fields, KeyOrder, Params, err, parse_json_body, unprocessable};
use crate::routes::library::{Q, qp};
use crate::state::AppState;

/// A service refusal as the route's `HTTPException`; a database failure
/// (the services' 500) is FastAPI's plain `Internal Server Error`.
pub fn svc(e: HttpError) -> ApiError {
    if e.status == 500 {
        ApiError::internal(e.detail)
    } else {
        e.into()
    }
}

fn reply(r: Result<Json, HttpError>) -> ApiResult {
    match r {
        Ok(j) => ok_ordered(&j),
        Err(e) => Err(svc(e)),
    }
}

/// Run `f` with a [`TagEnv`] on a blocking task.
async fn with_tags<F>(st: AppState, guard: Option<crate::WorkGuard>, f: F) -> ApiResult
where
    F: FnOnce(&mut TagEnv<'_>) -> Result<Json, HttpError> + Send + 'static,
{
    blocking(move || {
        let _guard = guard;
        let mut conn = st.conn()?;
        reply(st.with_tag_env(&mut conn, f))
    })
    .await?
}

/// Run `f` on a plain connection on a blocking task.
async fn with_conn<F>(st: AppState, guard: Option<crate::WorkGuard>, f: F) -> ApiResult
where
    F: FnOnce(&mut Connection, &str) -> Result<Json, HttpError> + Send + 'static,
{
    blocking(move || {
        let _guard = guard;
        let mut conn = st.conn()?;
        let now = st.now_iso();
        reply(f(&mut conn, &now))
    })
    .await?
}

// ------------------------------------------------------------- album tags

pub async fn get_album_tags(
    State(st): State<AppState>,
    UrlPath(album_id): UrlPath<String>,
) -> ApiResult {
    with_tags(st, None, move |env| {
        track_tags::get_album_tags(env, &album_id)
    })
    .await
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
    let guard = st.begin_work()?;
    with_tags(st, Some(guard), move |env| {
        track_tags::replace_album_tags(env, &album_id, &map)
    })
    .await
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
    let guard = st.begin_work()?;
    with_tags(st, Some(guard), move |env| {
        track_tags::add_album_tag(env, &album_id, &tag, filenames.as_deref())
    })
    .await
}

pub async fn remove_album_tag(
    State(st): State<AppState>,
    UrlPath((album_id, tag)): UrlPath<(String, String)>,
) -> ApiResult {
    let guard = st.begin_work()?;
    with_tags(st, Some(guard), move |env| {
        track_tags::remove_album_tag(env, &album_id, &tag)
    })
    .await
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
    let guard = st.begin_work()?;
    with_tags(st, Some(guard), move |env| {
        track_tags::add_track_tag(env, &album_id, &filename, &tag)
    })
    .await
}

pub async fn remove_track_tag(
    State(st): State<AppState>,
    UrlPath((album_id, filename, tag)): UrlPath<(String, String, String)>,
) -> ApiResult {
    let guard = st.begin_work()?;
    with_tags(st, Some(guard), move |env| {
        track_tags::remove_track_tag(env, &album_id, &filename, &tag)
    })
    .await
}

// ------------------------------------------------------------- /api/tags

pub async fn list_tags(State(st): State<AppState>) -> ApiResult {
    with_conn(st, None, |conn, _| track_tags::list_tags(conn)).await
}

pub async fn tracks_for_tag(
    State(st): State<AppState>,
    UrlPath(tag): UrlPath<String>,
) -> ApiResult {
    with_conn(st, None, move |conn, _| {
        track_tags::tracks_for_tag(conn, &tag)
    })
    .await
}

// ------------------------------------------------------------- vocabulary

pub async fn list_vocabulary(State(st): State<AppState>) -> ApiResult {
    with_conn(st, None, |conn, _| vocabulary::list_vocabulary(conn)).await
}

pub async fn add_vocabulary_entry(State(st): State<AppState>, body: Bytes) -> ApiResult {
    let mut f = Fields::required(parse_json_body(&body)?)?;
    let tag = f.req_str("tag");
    let description = f.str_or("description", "");
    let color = f.opt_str("color");
    let sort_order = f.opt_int("sort_order");
    f.finish()?;
    let tag = tag.unwrap_or_default();
    let guard = st.begin_work()?;
    with_conn(st, Some(guard), move |conn, now| {
        vocabulary::add_vocabulary_entry(
            conn,
            now,
            &tag,
            Some(&description),
            color.as_deref(),
            sort_order,
        )
    })
    .await
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
    let guard = st.begin_work()?;
    with_conn(st, Some(guard), move |conn, _| {
        vocabulary::patch_vocabulary_entry(
            conn,
            &tag,
            description.as_deref(),
            color.as_deref(),
            sort_order,
        )
    })
    .await
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
    let guard = st.begin_work()?;
    with_conn(st, Some(guard), move |conn, _| {
        vocabulary::rename_vocabulary_entry(conn, &tag, &new_tag)
    })
    .await
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
    let guard = st.begin_work()?;
    with_conn(st, Some(guard), move |conn, _| {
        vocabulary::merge_vocabulary_entry(conn, &tag, &into_tag)
    })
    .await
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
    let guard = st.begin_work()?;
    with_conn(st, Some(guard), move |conn, _| {
        vocabulary::delete_vocabulary_entry(conn, &tag, strip)
    })
    .await
}

pub async fn promote_orphan_tag(
    State(st): State<AppState>,
    UrlPath(tag): UrlPath<String>,
) -> ApiResult {
    let guard = st.begin_work()?;
    with_conn(st, Some(guard), move |conn, now| {
        vocabulary::promote_orphan_tag(conn, now, &tag)
    })
    .await
}
