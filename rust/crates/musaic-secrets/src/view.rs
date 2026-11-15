//! Settings reads and secret writes as the API sees them.
//!
//! `all_settings` returns exactly the keys and value types of the Python
//! app's `settings.all_settings()` (the `GET /api/settings` body): the three
//! secrets appear only as `<key>_set` booleans, now answered by the store.
//! One field is added: `keyring_available`, false when the store
//! could not be reached, so the UI can say why keys read as not set.

use rusqlite::Connection;
use serde_json::{Map, Value, json};

use musaic_core::pycompat;
use musaic_db::settings::{get_bool, get_int, get_raw};

use crate::migrate::write_verified;
use crate::store::{SecretStore, StoreError};
use crate::{SECRET_KEYS, is_secret_key};

const DEFAULT_AI_PROVIDER_ID: &str = "claude";
const DEFAULT_AI_MODEL_ID: &str = "claude-sonnet-4-6";

/// A secret's value, stripped; `""` when not set. Mirrors the Python
/// accessors (`settings.ai_api_key()` etc.), reading the store.
pub fn secret(store: &dyn SecretStore, key: &str) -> Result<String, StoreError> {
    debug_assert!(is_secret_key(key), "{key} is not a secret key");
    Ok(store
        .get(key)?
        .map(|v| pycompat::strip(&v).to_owned())
        .unwrap_or_default())
}

/// `PATCH` semantics for one secret: the value is stripped; empty clears it.
/// Written to the store only, and confirmed by reading it back.
pub fn set_secret(store: &dyn SecretStore, key: &str, value: &str) -> Result<(), StoreError> {
    if !is_secret_key(key) {
        return Err(StoreError::Backend(format!("{key} is not a secret key")));
    }
    let v = pycompat::strip(value);
    if v.is_empty() {
        store.delete(key)
    } else {
        write_verified(store, key, v)
    }
}

/// Remove every entry this app owns.
pub fn clear_all(store: &dyn SecretStore) -> Result<(), StoreError> {
    for key in SECRET_KEYS {
        store.delete(key)?;
    }
    Ok(())
}

/// Whether the store answered, and which secrets are set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretStatus {
    /// `None` when the keyring answered; the error otherwise.
    pub unavailable: Option<StoreError>,
    pub set: [(&'static str, bool); 3],
}

pub fn secret_status(store: &dyn SecretStore) -> SecretStatus {
    let mut unavailable = None;
    let set = SECRET_KEYS.map(|k| match secret(store, k) {
        Ok(v) => (k, !v.is_empty()),
        Err(e) => {
            unavailable.get_or_insert(e);
            (k, false)
        }
    });
    SecretStatus { unavailable, set }
}

fn stripped_or(conn: &Connection, key: &str, default: &str) -> musaic_db::Result<String> {
    let raw = get_raw(conn, key)?.unwrap_or_default();
    let s = pycompat::strip(&raw);
    Ok(if s.is_empty() { default } else { s }.to_owned())
}

fn template(conn: &Connection, key: &str) -> musaic_db::Result<String> {
    let default = musaic_db::settings::default_for(key).unwrap_or_default();
    stripped_or(conn, key, default)
}

/// Python's `float(s)` for a settings string: surrounding whitespace, PEP 515
/// underscores between digits, Unicode decimal digits, `inf`/`nan` spellings.
fn py_float(s: &str) -> Option<f64> {
    let t = pycompat::strip(s);
    let chars: Vec<char> = t.chars().collect();
    let mut ascii = String::with_capacity(chars.len());
    for (i, &c) in chars.iter().enumerate() {
        if c == '_' {
            let digit =
                |j: Option<&char>| j.is_some_and(|c| pycompat::decimal_digit_value(*c).is_some());
            if !(i > 0 && digit(chars.get(i - 1)) && digit(chars.get(i + 1))) {
                return None;
            }
            continue;
        }
        match pycompat::decimal_digit_value(c) {
            Some(d) if !c.is_ascii() => ascii.push(char::from(b'0' + d as u8)),
            _ => ascii.push(c),
        }
    }
    let lower = ascii.to_ascii_lowercase();
    let body = lower.trim_start_matches(['+', '-']);
    if lower.len() - body.len() > 1 {
        return None;
    }
    if matches!(body, "inf" | "infinity" | "nan") {
        return lower.parse().ok().or_else(|| {
            let neg = lower.starts_with('-');
            Some(match (body, neg) {
                ("nan", _) => f64::NAN,
                (_, true) => f64::NEG_INFINITY,
                _ => f64::INFINITY,
            })
        });
    }
    if !body
        .chars()
        .all(|c| c.is_ascii_digit() || matches!(c, '.' | 'e' | '+' | '-'))
    {
        return None;
    }
    lower.parse().ok()
}

/// `snapshot_min_interval_hours` as the settings body and the startup
/// snapshot read it: 24 when unset, unreadable or not finite, negative
/// values (and -0.0) as 0. A non-finite value is treated as unreadable
/// (issue #213): the Python app turned NaN into 0 (snapshot on every
/// start) and kept infinity (never snapshot again), which JSON cannot
/// even carry; the PATCH now refuses both.
pub fn snapshot_min_interval_hours(conn: &Connection) -> musaic_db::Result<f64> {
    Ok(match get_raw(conn, "snapshot_min_interval_hours")? {
        None => 24.0,
        Some(raw) => match py_float(&raw) {
            Some(v) if !v.is_finite() => 24.0,
            None => 24.0,
            Some(v) if v > 0.0 => v,
            Some(_) => 0.0,
        },
    })
}

/// The `GET /api/settings` body.
pub fn all_settings(
    conn: &Connection,
    store: &dyn SecretStore,
) -> musaic_db::Result<Map<String, Value>> {
    let status = secret_status(store);
    let mut m = Map::new();
    m.insert(
        "ai_max_parallel".into(),
        json!(get_int(conn, "ai_max_parallel", 1)?.clamp(1, 8)),
    );
    m.insert(
        "ai_queue_paused".into(),
        json!(get_bool(conn, "ai_queue_paused", false)?),
    );
    m.insert(
        "ai_provider_id".into(),
        json!(stripped_or(conn, "ai_provider_id", DEFAULT_AI_PROVIDER_ID)?),
    );
    m.insert(
        "ai_model_id".into(),
        json!(stripped_or(conn, "ai_model_id", DEFAULT_AI_MODEL_ID)?),
    );
    for (key, set) in status.set {
        m.insert(format!("{key}_set"), json!(set));
    }
    m.insert(
        "keyring_available".into(),
        json!(status.unavailable.is_none()),
    );
    m.insert(
        "snapshot_retention_count".into(),
        json!(get_int(conn, "snapshot_retention_count", 10)?.clamp(0, 100)),
    );
    m.insert(
        "snapshot_min_interval_hours".into(),
        json!(snapshot_min_interval_hours(conn)?),
    );
    for key in [
        "filename_template_single_artist",
        "filename_template_compilation",
        "dir_template_single_artist",
        "dir_template_compilation",
    ] {
        m.insert(key.into(), json!(template(conn, key)?));
    }
    Ok(m)
}
