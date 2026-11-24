//! `musaic/api/settings.py`: settings read / patch (keys in the OS
//! keyring), prompts, UI preferences, database info, backup download, restore
//! upload and named snapshots.
//!
//! Every handler parses the request (a 422 before anything runs), calls one
//! operation of `ops().settings()` / `ops().database()` and renders the answer
//! (`crate::render::settings`). The guard, the order of the preconditions and
//! the events are the operations' (`docs/operations.md`); what stays here is
//! HTTP: the JSON parsing, receiving the restore upload to a scratch file
//! (with its cap) and framing the backup download.

use std::io::Write;

use axum::body::Bytes;
use axum::extract::multipart::MultipartRejection;
use axum::extract::{Multipart, Path as UrlPath, State};
use axum::http::{HeaderValue, header};
use musaic_app::ops::settings::{PrefValue, SettingsPatch, TEMPLATE_KEYS};
use serde_json::{Value, json};

use crate::http::{ApiError, ApiResult, blocking, ok_value};
use crate::lax::{Fields, err, parse_json_body, unprocessable};
use crate::render::settings as render;
use crate::state::AppState;

pub async fn get_settings(State(st): State<AppState>) -> ApiResult {
    blocking(move || ok_value(render::settings(&st.ops().settings().get()?))).await?
}

/// `GET /api/prompts/acoustid`: `{"dismissed": bool}`.
pub async fn acoustid_prompt(State(st): State<AppState>) -> ApiResult {
    blocking(move || {
        ok_value(render::acoustid_prompt(
            st.ops().settings().acoustid_prompt()?,
        ))
    })
    .await?
}

/// `POST /api/prompts/acoustid/dismiss`.
pub async fn dismiss_acoustid_prompt(State(st): State<AppState>) -> ApiResult {
    blocking(move || {
        ok_value(render::acoustid_prompt(
            st.ops().settings().dismiss_acoustid_prompt()?,
        ))
    })
    .await?
}

/// `GET /api/ui-prefs`.
pub async fn ui_prefs(State(st): State<AppState>) -> ApiResult {
    blocking(move || ok_value(render::ui_prefs(&st.ops().settings().ui_prefs()?))).await?
}

/// One body value as the operation tells them apart.
fn pref_value(v: &Value) -> PrefValue {
    match v {
        Value::Null => PrefValue::Null,
        Value::String(t) => PrefValue::Text(t.clone()),
        n => n.as_i64().map_or(PrefValue::Other, PrefValue::Whole),
    }
}

/// `PUT /api/ui-prefs`: the preferences named in the body (`theme`,
/// `tile_columns`); the operation refuses an unknown field or a value out of range.
pub async fn put_ui_prefs(State(st): State<AppState>, body: Bytes) -> ApiResult {
    let Some(Value::Object(map)) = parse_json_body(&body)? else {
        return Err(ApiError::new(
            400,
            "the body must be a JSON object".to_owned(),
        ));
    };
    let prefs: Vec<(String, PrefValue)> = map
        .iter()
        .map(|(k, v)| (k.clone(), pref_value(v)))
        .collect();
    blocking(move || ok_value(render::ui_prefs(&st.ops().settings().set_ui_prefs(&prefs)?))).await?
}

pub async fn patch_settings(State(st): State<AppState>, body: Bytes) -> ApiResult {
    let mut f = Fields::required(parse_json_body(&body)?)?;
    // The retired provider and key settings are not read: like any unknown
    // key, they are ignored.
    let acoustid = f.opt_str("acoustid_api_key");
    let retention = f.opt_int("snapshot_retention_count");
    let interval = f.opt_float("snapshot_min_interval_hours");
    let tpl = TEMPLATE_KEYS.map(|k| (k, f.opt_str(k)));
    f.finish()?;
    let patch = SettingsPatch {
        acoustid,
        retention,
        interval,
        tpl,
    };
    blocking(move || ok_value(render::settings(&st.ops().settings().patch(patch)?))).await?
}

// ------------------------------------------------------------- db

pub async fn db_info(State(st): State<AppState>) -> ApiResult {
    blocking(move || ok_value(render::db_info(&st.ops().database().info()?))).await?
}

pub async fn db_backup(State(st): State<AppState>) -> ApiResult {
    blocking(move || {
        let backup = st.ops().database().backup()?;
        let file = &backup.file;
        let mut r = crate::files::file_response(&file.path, "application/octet-stream")?;
        if let Ok(v) =
            HeaderValue::from_str(&format!("attachment; filename=\"{}\"", file.suggested_name))
        {
            r.headers_mut().insert(header::CONTENT_DISPOSITION, v);
        }
        Ok(r)
    })
    .await?
}

pub async fn db_restore(
    State(st): State<AppState>,
    mp: Result<Multipart, MultipartRejection>,
) -> ApiResult {
    let missing = || {
        unprocessable(vec![err(
            "missing",
            vec![json!("body"), json!("file")],
            "Field required",
            Value::Null,
        )])
    };
    let mut mp = mp.map_err(|_| missing())?;
    let limit = st.restore_upload_limit();
    let tmp = st.ops().database().scratch()?;
    let upload = tmp.path().join("upload.db");
    let mut found = false;
    while let Some(mut field) = mp.next_field().await.map_err(|_| missing())? {
        if field.name() != Some("file") || field.file_name().is_none() {
            continue;
        }
        // An early refusal before the upload is read (the Python app's
        // 422 for a missing file part still comes first). The guard the
        // operation takes is what counts.
        st.ops().database().restore_allowed()?;
        let mut out = std::fs::File::create(&upload).map_err(ApiError::internal)?;
        let mut written: u64 = 0;
        while let Some(chunk) = field.chunk().await.map_err(ApiError::internal)? {
            written += chunk.len() as u64;
            if written > limit {
                // Stop reading; `tmp` (and the partial file in it)
                // is removed when it drops on this return.
                drop(out);
                return Err(restore_too_large(limit));
            }
            out.write_all(&chunk).map_err(ApiError::internal)?;
        }
        found = true;
        break;
    }
    if !found {
        return Err(missing());
    }
    blocking(move || {
        ok_value(render::restore(
            &st.ops().database().restore_upload(tmp, &upload)?,
        ))
    })
    .await?
}

/// A restore upload over the cap. Same shape as every
/// other error (`{"detail": ...}`).
fn restore_too_large(limit: u64) -> ApiError {
    ApiError::new(
        413,
        format!("Database upload too large. Max accepted is {limit} bytes."),
    )
}

pub async fn list_snapshots(State(st): State<AppState>) -> ApiResult {
    blocking(move || ok_value(render::snapshots(&st.ops().database().snapshots()?))).await?
}

pub async fn create_snapshot(State(st): State<AppState>) -> ApiResult {
    blocking(move || {
        ok_value(render::created_snapshot(
            &st.ops().database().create_snapshot()?,
        ))
    })
    .await?
}

pub async fn delete_snapshot(
    State(st): State<AppState>,
    UrlPath(name): UrlPath<String>,
) -> ApiResult {
    blocking(move || {
        let d = st.ops().database().delete_snapshot(&name)?;
        ok_value(render::snapshot_deleted(d.deleted, &d.name))
    })
    .await?
}

pub async fn restore_snapshot(
    State(st): State<AppState>,
    UrlPath(name): UrlPath<String>,
) -> ApiResult {
    blocking(move || {
        ok_value(render::restore(
            &st.ops().database().restore_snapshot(&name)?,
        ))
    })
    .await?
}
