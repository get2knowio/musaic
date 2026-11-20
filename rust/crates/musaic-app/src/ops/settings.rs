//! The `settings` group: the settings (keys in the OS keyring), the one-time
//! AcoustID prompt, and the UI preferences.
//!
//! A write takes the work guard itself (a restore running makes it `Busy`, with
//! nothing changed), publishes `SettingsChanged` (names only, never a value) and,
//! for the settings, wakes the AI queue. No result holds a secret: a key is only
//! ever reported as set or not.

use musaic_events::Event;
pub use musaic_services::settings_patch::{SettingsPatch, TEMPLATE_KEYS};

use super::error::OpError;
pub use super::record::SettingsRecord;
use crate::App;

/// The settings row that remembers the one-time "add an AcoustID key"
/// prompt was dismissed. It is not part of the settings, and no check reads it.
const ACOUSTID_PROMPT_DISMISSED: &str = "acoustid_prompt_dismissed";

/// The settings rows holding the UI preferences. They are not part of the
/// settings, and no check reads them.
const UI_THEME: &str = "ui_theme";
const UI_TILE_COLUMNS: &str = "ui_tile_columns";

/// The Library tile-size slider's range.
const TILE_COLUMNS_MIN: i64 = 3;
const TILE_COLUMNS_MAX: i64 = 7;

/// Whether the AcoustID prompt was dismissed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AcoustidPrompt {
    pub dismissed: bool,
}

/// The UI preferences; `None`: never chosen (or a stored value out of range).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiPrefs {
    /// `"light"` or `"dark"`.
    pub theme: Option<String>,
    /// `3..=7`.
    pub tile_columns: Option<i64>,
}

/// One preference value as the caller gave it: the kinds an operation can tell
/// apart. A face maps its own document to this.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrefValue {
    /// Forget the preference.
    Null,
    Text(String),
    /// A number with no fraction that fits an `i64`.
    Whole(i64),
    /// Anything else (a fraction, a boolean, a list, a number too big).
    Other,
}

/// The `settings` operations.
pub struct Settings<'a> {
    pub(super) app: &'a App,
}

impl Settings<'_> {
    fn conn(&self) -> Result<rusqlite::Connection, OpError> {
        Ok(musaic_db::open_connection(self.app.db_path())?)
    }

    /// The settings, in the order the Python app reports them.
    pub fn get(&self) -> Result<SettingsRecord, OpError> {
        let conn = self.conn()?;
        let view = musaic_secrets::all_settings(&conn, self.app.0.secrets.as_ref())?;
        Ok(SettingsRecord::new(view, self.app.key_scrub_pending()))
    }

    /// Apply the fields of `patch` that are present, in the Python app's order.
    /// The guard first (`Busy` while a restore runs, before any value is
    /// looked at); a value out of range or an unknown provider is `Rejected`
    /// (fields before it stay written, as always), a keyring that cannot save
    /// a key is `Unavailable` with nothing written. Publishes `SettingsChanged`
    /// once with the names written, wakes the AI queue, and when a non-empty
    /// AcoustID key was saved restarts the fingerprint pool on the library so
    /// stored fingerprints get their lookups now. A refused patch that saved
    /// fields before the refused one does both for what it saved.
    pub fn patch(&self, patch: SettingsPatch) -> Result<SettingsRecord, OpError> {
        let kick_pool = patch.sets_acoustid_key();
        let _guard = self.app.gate().begin_work()?;
        let conn = self.conn()?;
        // The providers `jobs.providers` lists.
        let listed = self.app.ai_provider_ids();
        let listed: Vec<&str> = listed.iter().map(String::as_str).collect();
        let out = musaic_services::settings_patch::apply_patch(
            conn,
            self.app.0.secrets.as_ref(),
            patch,
            &listed,
            self.app.log(),
            self.app.bus(),
        );
        // Wake the AI dispatcher (a max_parallel bump or an unpause) and the
        // jobs streams: after a patch that went through, and after a refused
        // one that saved the fields before the refused one (they are in
        // effect), so an unpause saved that way is not left waiting for the
        // queue's next periodic wake.
        if out.result.is_ok() || !out.saved.is_empty() {
            self.app.ai_jobs_changed();
        }
        if kick_pool && out.saved.contains(&"acoustid_api_key") {
            musaic_services::settings_patch::kick_fingerprint_pool(
                &self.app.0.pool,
                self.app.library_dir(),
                || self.app.pool_deps(),
            );
        }
        out.result?;
        self.get()
    }

    /// Whether the AcoustID prompt was dismissed.
    pub fn acoustid_prompt(&self) -> Result<AcoustidPrompt, OpError> {
        let conn = self.conn()?;
        let dismissed = musaic_db::settings::get_bool(&conn, ACOUSTID_PROMPT_DISMISSED, false)?;
        Ok(AcoustidPrompt { dismissed })
    }

    /// Remember the dismissal (idempotent) and publish `SettingsChanged` so
    /// open windows refresh.
    pub fn dismiss_acoustid_prompt(&self) -> Result<AcoustidPrompt, OpError> {
        let _guard = self.app.gate().begin_work()?;
        let conn = self.conn()?;
        musaic_db::settings::set_raw(&conn, ACOUSTID_PROMPT_DISMISSED, "1")?;
        drop(conn);
        self.app.bus().publish(&Event::SettingsChanged {
            keys: vec![ACOUSTID_PROMPT_DISMISSED.to_owned()],
        });
        self.acoustid_prompt()
    }

    /// The UI preferences (a stored value out of range reads as unset).
    pub fn ui_prefs(&self) -> Result<UiPrefs, OpError> {
        let conn = self.conn()?;
        let theme =
            musaic_db::settings::get_raw(&conn, UI_THEME)?.filter(|t| t == "light" || t == "dark");
        let tile_columns = musaic_db::settings::get_raw(&conn, UI_TILE_COLUMNS)?
            .and_then(|v| v.parse::<i64>().ok())
            .filter(|n| (TILE_COLUMNS_MIN..=TILE_COLUMNS_MAX).contains(n));
        Ok(UiPrefs {
            theme,
            tile_columns,
        })
    }

    /// Set the preferences named in `prefs` (`theme`, `tile_columns`); a
    /// preference given as `Null` is forgotten, one left out is untouched.
    /// Meaning first: an unknown preference or a value out of range is
    /// `Rejected` (the first in `prefs` order) before the guard is looked at;
    /// then the guard (`Busy`), the writes, and `SettingsChanged` (names only)
    /// when anything was written.
    pub fn set_ui_prefs(&self, prefs: &[(String, PrefValue)]) -> Result<UiPrefs, OpError> {
        let rejected = |message: String| OpError::Rejected {
            message,
            details: None,
        };
        let mut writes: Vec<(&'static str, Option<String>)> = Vec::new();
        for (k, v) in prefs {
            match (k.as_str(), v) {
                ("theme", PrefValue::Null) => writes.push((UI_THEME, None)),
                ("theme", PrefValue::Text(t)) if t == "light" || t == "dark" => {
                    writes.push((UI_THEME, Some(t.clone())));
                }
                ("theme", _) => {
                    return Err(rejected(
                        "theme must be \"light\", \"dark\" or null".to_owned(),
                    ));
                }
                ("tile_columns", PrefValue::Null) => writes.push((UI_TILE_COLUMNS, None)),
                ("tile_columns", PrefValue::Whole(n))
                    if (TILE_COLUMNS_MIN..=TILE_COLUMNS_MAX).contains(n) =>
                {
                    writes.push((UI_TILE_COLUMNS, Some(n.to_string())));
                }
                ("tile_columns", _) => {
                    return Err(rejected(format!(
                        "tile_columns must be a whole number from {TILE_COLUMNS_MIN} to {TILE_COLUMNS_MAX} or null"
                    )));
                }
                (other, _) => {
                    return Err(rejected(format!("unknown preference {other:?}")));
                }
            }
        }
        let _guard = self.app.gate().begin_work()?;
        let conn = self.conn()?;
        for (key, value) in &writes {
            match value {
                Some(v) => musaic_db::settings::set_raw(&conn, key, v)?,
                None => musaic_db::settings::delete_raw(&conn, key)?,
            }
        }
        drop(conn);
        if !writes.is_empty() {
            self.app.bus().publish(&Event::SettingsChanged {
                keys: writes.iter().map(|(k, _)| (*k).to_owned()).collect(),
            });
        }
        self.ui_prefs()
    }
}
