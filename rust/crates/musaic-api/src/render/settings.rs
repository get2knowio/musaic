//! The `settings` and `database` groups as bytes: bodies for the settings, the
//! AcoustID prompt, the UI preferences, the database info, restores and the
//! snapshots.

use musaic_app::ops::database::{Counts, CreatedSnapshot, DbInfo, RestoreInfo, SnapshotEntry};
use musaic_app::ops::settings::{AcoustidPrompt, SettingsRecord, UiPrefs};
use serde_json::{Map, Value, json};

/// `GET` / `PATCH /api/settings`: the settings in their reported order.
pub fn settings(s: &SettingsRecord) -> Value {
    Value::Object(s.iter().map(|(k, v)| (k.to_owned(), v.clone())).collect())
}

/// `GET /api/prompts/acoustid`: `{"dismissed": bool}`.
pub fn acoustid_prompt(p: AcoustidPrompt) -> Value {
    json!({ "dismissed": p.dismissed })
}

/// `GET /api/ui-prefs`: `{"theme": "light"|"dark"|null, "tile_columns": 3..7|null}`
/// (null: never chosen).
pub fn ui_prefs(p: &UiPrefs) -> Value {
    json!({ "theme": p.theme, "tile_columns": p.tile_columns })
}

fn counts(c: &Counts) -> Value {
    Value::Object(c.iter().map(|(k, v)| (k.clone(), json!(v))).collect())
}

/// `GET /api/db/info`.
pub fn db_info(i: &DbInfo) -> Value {
    json!({
        "path": i.path.to_string_lossy(),
        "exists": i.exists,
        "size_bytes": i.size_bytes,
        "schema_version": i.schema_version,
        "current_schema": i.current_schema,
        "counts": counts(&i.counts),
        "modified_at": i.modified_at,
    })
}

/// `POST /api/db/restore` and `POST /api/db/snapshots/{name}/restore`.
pub fn restore(info: &RestoreInfo) -> Value {
    json!({
        "schema_version": info.schema_version,
        "size_bytes": info.size_bytes,
        "counts": counts(&info.counts),
        "restored_at": info.restored_at,
        "aside_paths": info.aside_paths.iter().map(|p| p.to_string_lossy().into_owned()).collect::<Vec<_>>(),
    })
}

fn snapshot_entry(s: &SnapshotEntry) -> Value {
    json!({
        "filename": s.filename,
        "size_bytes": s.size_bytes,
        "modified_at": s.modified_at,
        "schema_version": s.schema_version,
        "valid": s.valid,
        "counts": if s.valid { counts(&s.counts) } else { Value::Object(Map::new()) },
    })
}

/// `GET /api/db/snapshots`.
pub fn snapshots(list: &[SnapshotEntry]) -> Value {
    json!({"snapshots": list.iter().map(snapshot_entry).collect::<Vec<_>>()})
}

/// `POST /api/db/snapshots`.
pub fn created_snapshot(s: &CreatedSnapshot) -> Value {
    json!({
        "filename": s.filename,
        "size_bytes": s.size_bytes,
        "modified_at": s.modified_at,
        "schema_version": s.schema_version,
        "pruned": s.pruned,
    })
}

/// `DELETE /api/db/snapshots/{name}`.
pub fn snapshot_deleted(deleted: bool, name: &str) -> Value {
    json!({"deleted": deleted, "name": name})
}
