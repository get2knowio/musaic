//! `musaic/api/settings.py`: settings read / patch (keys in the OS
//! keyring), database info, backup download, restore upload and named
//! snapshots.
//!
//! `PATCH /api/settings` and snapshot create / delete take the
//! restore gate's [`crate::WorkGuard`] (409 while a restore runs, before any
//! check of their own); the two restore routes take the
//! [`crate::RestoreGuard`] first, before the snapshot name is looked at.

use std::io::Write;

use axum::body::Bytes;
use axum::extract::multipart::MultipartRejection;
use axum::extract::{Multipart, Path as UrlPath, State};
use axum::http::{HeaderValue, header};
use musaic_db::backup::{self, Counts, RestoreError};
use musaic_services::db_admin::{self, ScratchError, SnapshotsDisabled};
use musaic_services::settings_patch::{self, PatchError, SettingsPatch, TEMPLATE_KEYS};
use serde_json::{Map, Value, json};

use crate::http::{ApiError, ApiResult, blocking, ok_value};
use crate::lax::{Fields, err, parse_json_body, unprocessable};
use crate::state::AppState;

/// The only provider the Rust build knows (there is no
/// provider selection; "claude" and "" are accepted so existing rows and
/// the unchanged frontend keep working).
const INSTALLED_PROVIDERS: &[&str] = &["claude"];

/// The settings body: musaic-secrets' view (the Python app's shape plus
/// `keyring_available`) plus `keys_pending_removal`, true while an
/// unfinished secret scrub leaves keys (or old copies of them) in the
/// database file or its pre-migration backup (a deliberate addition to the Python app's
/// shape; see `crate::keyscrub`).
fn settings_body(st: &AppState) -> Result<Value, ApiError> {
    let conn = st.conn()?;
    let m = musaic_secrets::all_settings(&conn, st.0.secrets.as_ref())?;
    let mut out = Map::new();
    for (k, v) in m {
        let after_keyring = k == "keyring_available";
        out.insert(k, v);
        if after_keyring {
            out.insert(
                KEYS_PENDING_REMOVAL.into(),
                Value::Bool(st.key_scrub_pending()),
            );
        }
    }
    out.entry(KEYS_PENDING_REMOVAL)
        .or_insert(Value::Bool(st.key_scrub_pending()));
    Ok(Value::Object(out))
}

/// The settings body's flag for an unfinished secret scrub.
pub const KEYS_PENDING_REMOVAL: &str = "keys_pending_removal";

pub async fn get_settings(State(st): State<AppState>) -> ApiResult {
    blocking(move || ok_value(settings_body(&st)?)).await?
}

/// The PATCH's refusals as HTTP answers: 400 (range, provider), 503
/// (keyring), 500 (database).
fn patch_error(e: PatchError) -> ApiError {
    match e {
        PatchError::Invalid(d) => ApiError::new(400, d),
        PatchError::Keyring(d) => ApiError::new(503, d),
        PatchError::Db(e) => ApiError::from(e),
        PatchError::Sqlite(e) => ApiError::internal(e),
    }
}

pub async fn patch_settings(State(st): State<AppState>, body: Bytes) -> ApiResult {
    let mut f = Fields::required(parse_json_body(&body)?)?;
    let max_parallel = f.opt_int("ai_max_parallel");
    let paused = f.opt_bool("ai_queue_paused");
    let provider = f.opt_str("ai_provider_id");
    let model = f.opt_str("ai_model_id");
    let ai_key = f.opt_str("ai_api_key");
    let acoustid = f.opt_str("acoustid_api_key");
    let firecrawl = f.opt_str("firecrawl_api_key");
    let retention = f.opt_int("snapshot_retention_count");
    let interval = f.opt_float("snapshot_min_interval_hours");
    let tpl = TEMPLATE_KEYS.map(|k| (k, f.opt_str(k)));
    f.finish()?;
    let patch = SettingsPatch {
        max_parallel,
        paused,
        provider,
        model,
        ai_key,
        acoustid,
        firecrawl,
        retention,
        interval,
        tpl,
    };
    // A new, non-empty AcoustID key: the stored fingerprints can now be
    // looked up (see `kick_fingerprint_pool`).
    let kick_pool = patch.sets_acoustid_key();
    let guard = st.begin_work()?;
    blocking(move || {
        let _guard = guard;
        let conn = st.conn()?;
        settings_patch::apply_patch(
            conn,
            st.0.secrets.as_ref(),
            patch,
            INSTALLED_PROVIDERS,
            st.log(),
        )
        .map_err(patch_error)?;
        // `notify_jobs_changed()` (api/settings.py:136): wake the AI
        // dispatcher (a max_parallel bump or an unpause) and the jobs streams.
        st.ai_jobs_changed();
        if kick_pool {
            settings_patch::kick_fingerprint_pool(&st.0.pool, st.library_dir(), || st.pool_deps());
        }
        ok_value(settings_body(&st)?)
    })
    .await?
}

// ------------------------------------------------------------- db

fn counts_json(c: &Counts) -> Value {
    Value::Object(c.iter().map(|(k, v)| (k.clone(), json!(v))).collect())
}

pub async fn db_info(State(st): State<AppState>) -> ApiResult {
    blocking(move || {
        let _g = st.0.db_gate.read().unwrap_or_else(|e| e.into_inner());
        let db = st.db()?;
        let i = backup::db_info(&db);
        ok_value(json!({
            "path": i.path.to_string_lossy(),
            "exists": i.exists,
            "size_bytes": i.size_bytes,
            "schema_version": i.schema_version,
            "current_schema": i.current_schema,
            "counts": counts_json(&i.counts),
            "modified_at": i.modified_at,
        }))
    })
    .await?
}

pub async fn db_backup(State(st): State<AppState>) -> ApiResult {
    blocking(move || {
        let (mut r, tag) = db_admin::backup_with(
            &st.0.db_gate,
            || st.db(),
            st.config_dir(),
            &|| st.now(),
            |path, tag| {
                let r = crate::files::file_response(path, "application/octet-stream")?;
                Ok::<_, ApiError>((r, tag.to_string()))
            },
        )?;
        let h = r.headers_mut();
        let name = backup::suggested_backup_filename(&tag);
        if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"{name}\"")) {
            h.insert(header::CONTENT_DISPOSITION, v);
        }
        Ok(r)
    })
    .await?
}

/// No scratch directory next to the database: a 500.
impl From<ScratchError> for ApiError {
    fn from(e: ScratchError) -> Self {
        ApiError::internal(e)
    }
}

/// A refused restore or a bad snapshot name: a 400 with its message.
impl From<RestoreError> for ApiError {
    fn from(e: RestoreError) -> Self {
        restore_error(&e)
    }
}

/// Snapshotting disabled is a 409.
impl From<SnapshotsDisabled> for ApiError {
    fn from(_: SnapshotsDisabled) -> Self {
        ApiError::new(409, db_admin::SNAPSHOTS_DISABLED)
    }
}

fn restore_body(info: &backup::RestoreInfo) -> Value {
    json!({
        "schema_version": info.schema_version,
        "size_bytes": info.size_bytes,
        "counts": counts_json(&info.counts),
        "restored_at": info.restored_at,
        "aside_paths": info.aside_paths.iter().map(|p| p.to_string_lossy().into_owned()).collect::<Vec<_>>(),
    })
}

/// Runs while the restore still holds the gate and the file lock, so no
/// writing work, pool, backup or snapshot of ours holds a connection that
/// could block the scrub's checkpoint (only a short read request can). An
/// outcome that is not clean is logged, surfaced as `keys_pending_removal`
/// and retried later (`crate::keyscrub`).
fn after_restore(st: &AppState, db: &musaic_db::Db) {
    st.scrub_keys(db, "after restore");
    st.0.resolver.invalidate(None);
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
    let limit = st.0.restore_upload_limit;
    let tmp = db_admin::tempdir_in_config(st.config_dir())?;
    let upload = tmp.path().join("upload.db");
    let mut found = false;
    while let Some(mut field) = mp.next_field().await.map_err(|_| missing())? {
        if field.name() != Some("file") || field.file_name().is_none() {
            continue;
        }
        // An early refusal before the upload is read (the Python app's
        // 422 for a missing file part still comes first). The guard taken
        // below is what counts.
        st.0.gate.restore_allowed()?;
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
    // Refused (409) while writing work holds the
    // gate; new ones are refused until this guard drops (end of the
    // blocking task, or its unwinding).
    let guard = st.0.gate.begin_restore()?;
    // Removed right after the swap; on an earlier failure, when the task
    // ends (after the guard is released), as before the move to services.
    let mut tmp = Some(tmp);
    blocking(move || {
        let _guard = guard;
        // The fingerprint pool never holds the gate; a restore stops it
        // before the file is swapped (see `state.rs`).
        let info = db_admin::restore_with(
            &st.0.pool,
            st.0.on_restore.as_deref(),
            &st.0.db_gate,
            || st.db(),
            &|| st.now(),
            |db, tag, now| {
                let r = backup::restore_from(db, &upload, tag, now);
                drop(tmp.take());
                r
            },
            |db| after_restore(&st, db),
        )?;
        ok_value(restore_body(&info))
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

fn restore_error(e: &RestoreError) -> ApiError {
    ApiError::new(400, e.to_string())
}

fn snapshot_entry(s: &backup::SnapshotEntry) -> Value {
    json!({
        "filename": s.filename,
        "size_bytes": s.size_bytes,
        "modified_at": s.modified_at,
        "schema_version": s.schema_version,
        "valid": s.valid,
        "counts": if s.valid { counts_json(&s.counts) } else { Value::Object(Map::new()) },
    })
}

pub async fn list_snapshots(State(st): State<AppState>) -> ApiResult {
    blocking(move || {
        let list =
            backup::list_snapshots(&st.0.config.snapshot_dir()).map_err(ApiError::internal)?;
        ok_value(json!({"snapshots": list.iter().map(snapshot_entry).collect::<Vec<_>>()}))
    })
    .await?
}

pub async fn create_snapshot(State(st): State<AppState>) -> ApiResult {
    let guard = st.begin_work()?;
    blocking(move || {
        let _guard = guard;
        let s = db_admin::create_snapshot_with::<ApiError>(
            &st.0.db_gate,
            || st.db(),
            &st.0.config.snapshot_dir(),
            &|| st.now(),
        )?;
        ok_value(json!({
            "filename": s.filename,
            "size_bytes": s.size_bytes,
            "modified_at": s.modified_at,
            "schema_version": s.schema_version,
            "pruned": s.pruned,
        }))
    })
    .await?
}

pub async fn delete_snapshot(
    State(st): State<AppState>,
    UrlPath(name): UrlPath<String>,
) -> ApiResult {
    let guard = st.begin_work()?;
    blocking(move || {
        let _guard = guard;
        match backup::delete_snapshot(&st.0.config.snapshot_dir(), &name) {
            Ok(deleted) => ok_value(json!({"deleted": deleted, "name": name})),
            Err(e) => Err(restore_error(&e)),
        }
    })
    .await?
}

pub async fn restore_snapshot(
    State(st): State<AppState>,
    UrlPath(name): UrlPath<String>,
) -> ApiResult {
    // As for the uploaded restore.
    let guard = st.0.gate.begin_restore()?;
    blocking(move || {
        let _guard = guard;
        // The fingerprint pool never holds the gate; a restore stops it
        // before the file is swapped (see `state.rs`).
        let info = db_admin::restore_with(
            &st.0.pool,
            st.0.on_restore.as_deref(),
            &st.0.db_gate,
            || st.db(),
            &|| st.now(),
            |db, tag, now| {
                backup::restore_from_snapshot(db, &st.0.config.snapshot_dir(), &name, tag, now)
            },
            |db| after_restore(&st, db),
        )?;
        ok_value(restore_body(&info))
    })
    .await?
}
