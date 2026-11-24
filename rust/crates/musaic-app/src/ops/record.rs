//! The allow-list of record newtypes: documents whose shape is owned by stored
//! data and which keep their stored key order. Nothing else under `ops/` may name
//! `serde_json`; adding a record type here is a reviewed change to the allow-list
//! in `tests/ops_boundary.rs`.

use musaic_services::Json;

/// The text of a string value, `None` for null and for anything that is not a
/// string.
fn text<'a>(doc: &'a Json, key: &str) -> Option<&'a str> {
    doc.get(key).and_then(Json::as_str)
}

fn pairs(doc: &Json) -> impl Iterator<Item = (&str, &Json)> {
    match doc {
        Json::Obj(p) => p.as_slice(),
        _ => &[],
    }
    .iter()
    .map(|(k, v)| (k.as_str(), v))
}

/// One album as the library reports it: the stored row's fields in stored
/// order. A face other than HTTP reads the typed accessors; the ordered
/// iteration is for a renderer.
#[derive(Debug, Clone, PartialEq)]
pub struct AlbumRecord(Json);

impl AlbumRecord {
    pub(crate) fn new(doc: Json) -> Self {
        AlbumRecord(doc)
    }

    pub fn id(&self) -> &str {
        text(&self.0, "id").unwrap_or("")
    }

    pub fn path(&self) -> &str {
        text(&self.0, "path").unwrap_or("")
    }

    /// When the album's folder was found gone; `None` while it is present.
    pub fn missing_since(&self) -> Option<&str> {
        text(&self.0, "missing_since").filter(|s| !s.is_empty())
    }

    pub fn is_missing(&self) -> bool {
        self.missing_since().is_some()
    }

    /// The album's artist as the one shared cascade names it
    /// ([`musaic_core::album_fields::display_artist`]).
    pub fn display_artist(&self, include_dir_artist: bool) -> String {
        musaic_core::album_fields::display_artist(
            text(&self.0, "album_artist"),
            text(&self.0, "artist"),
            text(&self.0, "dir_artist"),
            include_dir_artist,
        )
    }

    pub fn title(&self) -> Option<&str> {
        text(&self.0, "title")
    }

    /// The fields in stored order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Json)> {
        pairs(&self.0)
    }
}

/// One track of an album: what the files say, in the order the listing keeps.
/// A file that could not be read has `error()` and no tags.
#[derive(Debug, Clone, PartialEq)]
pub struct TrackRecord(Json);

impl TrackRecord {
    pub(crate) fn new(doc: Json) -> Self {
        TrackRecord(doc)
    }

    pub fn file_name(&self) -> &str {
        text(&self.0, "filename").unwrap_or("")
    }

    /// The `TRACKNUMBER` tag as written (`"3"`, `"3/12"`), `""` when absent.
    pub fn track_number(&self) -> &str {
        text(&self.0, "tracknumber").unwrap_or("")
    }

    pub fn title(&self) -> &str {
        text(&self.0, "title").unwrap_or("")
    }

    pub fn artist(&self) -> &str {
        text(&self.0, "artist").unwrap_or("")
    }

    /// Length in seconds, one decimal.
    pub fn duration(&self) -> Option<f64> {
        match self.0.get("duration") {
            Some(Json::Float(f)) => Some(*f),
            _ => None,
        }
    }

    /// Why the file could not be read.
    pub fn error(&self) -> Option<&str> {
        text(&self.0, "error")
    }

    /// The fields in listing order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Json)> {
        pairs(&self.0)
    }
}

/// The settings as `GET /api/settings` reports them: the secrets store's view
/// (key presence as `<name>_set` flags, never a value) in its order, plus
/// `keys_pending_removal`. Read through the typed accessors; the ordered
/// iteration is for a renderer.
#[derive(Debug, Clone, PartialEq)]
pub struct SettingsRecord(serde_json::Map<String, serde_json::Value>);

/// The settings body's flag for an unfinished secret scrub.
pub const KEYS_PENDING_REMOVAL: &str = "keys_pending_removal";

impl SettingsRecord {
    /// `view` is the secrets store's view; `keys_pending_removal` goes right
    /// after `keyring_available` (a deliberate addition to the Python app's
    /// shape; see `musaic_services::keyscrub`).
    pub(crate) fn new(
        view: serde_json::Map<String, serde_json::Value>,
        keys_pending_removal: bool,
    ) -> Self {
        let mut out = serde_json::Map::new();
        for (k, v) in view {
            let after_keyring = k == "keyring_available";
            out.insert(k, v);
            if after_keyring {
                out.insert(
                    KEYS_PENDING_REMOVAL.into(),
                    serde_json::Value::Bool(keys_pending_removal),
                );
            }
        }
        out.entry(KEYS_PENDING_REMOVAL)
            .or_insert(serde_json::Value::Bool(keys_pending_removal));
        SettingsRecord(out)
    }

    pub fn bool(&self, name: &str) -> Option<bool> {
        self.0.get(name).and_then(serde_json::Value::as_bool)
    }

    pub fn int(&self, name: &str) -> Option<i64> {
        self.0.get(name).and_then(serde_json::Value::as_i64)
    }

    pub fn float(&self, name: &str) -> Option<f64> {
        self.0.get(name).and_then(serde_json::Value::as_f64)
    }

    pub fn str(&self, name: &str) -> Option<&str> {
        self.0.get(name).and_then(serde_json::Value::as_str)
    }

    /// True while an unfinished secret scrub leaves keys in the database file.
    pub fn keys_pending_removal(&self) -> bool {
        self.bool(KEYS_PENDING_REMOVAL).unwrap_or(false)
    }

    /// The settings in reported order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &serde_json::Value)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v))
    }
}

/// The tags of one track, or the tags every track of an album has: a set of
/// plain names in the order the library keeps them (sorted).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TagSet(Vec<String>);

impl TagSet {
    pub(crate) fn new(tags: Vec<String>) -> Self {
        TagSet(tags)
    }

    pub fn contains(&self, tag: &str) -> bool {
        self.0.iter().any(|t| t == tag)
    }

    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.0.iter().map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// What a check's documents have in common: a parsed stored document (a
/// finding's detail is stored text of any shape, the check summaries and the
/// previews are built by the checks crate in the key order its tests pin). A
/// renderer reads [`document`](Self::document); everyone else the accessors.
macro_rules! document_accessors {
    () => {
        /// The document as the checks crate built it, for a renderer.
        pub fn document(&self) -> &serde_json::Value {
            &self.0
        }

        #[allow(dead_code)]
        fn str_at(&self, key: &str) -> Option<&str> {
            self.0.get(key).and_then(serde_json::Value::as_str)
        }
    };
}

/// One finding of a check, in the shape the checks API reports it (the summary
/// as shown, [`musaic_checks::wording::finding_summary`]).
#[derive(Debug, Clone, PartialEq)]
pub struct FindingRecord(serde_json::Value);

impl FindingRecord {
    pub(crate) fn new(doc: serde_json::Value) -> Self {
        FindingRecord(doc)
    }

    document_accessors!();

    pub fn id(&self) -> i64 {
        self.0
            .get("id")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0)
    }

    pub fn check_id(&self) -> &str {
        self.str_at("check_id").unwrap_or("")
    }

    /// The album the finding is about, `None` for a library-wide finding.
    pub fn album_id(&self) -> Option<&str> {
        self.str_at("album_id")
    }

    pub fn summary(&self) -> &str {
        self.str_at("summary").unwrap_or("")
    }

    pub fn severity(&self) -> &str {
        self.str_at("severity").unwrap_or("")
    }

    /// When the user ignored it, `None` while it is not ignored.
    pub fn ignored_at(&self) -> Option<&str> {
        self.str_at("ignored_at")
    }

    /// When the check stopped finding it, `None` while it is open.
    pub fn resolved_at(&self) -> Option<&str> {
        self.str_at("resolved_at")
    }
}

/// One check as the list reports it: what it is, how many findings are open
/// and how its last run went.
#[derive(Debug, Clone, PartialEq)]
pub struct CheckSummaryRecord(serde_json::Value);

impl CheckSummaryRecord {
    pub(crate) fn new(doc: serde_json::Value) -> Self {
        CheckSummaryRecord(doc)
    }

    document_accessors!();

    pub fn check_id(&self) -> &str {
        self.0
            .get("meta")
            .and_then(|m| m.get("id"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
    }

    pub fn open_count(&self) -> i64 {
        self.0
            .get("open_count")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0)
    }

    /// The last run's status (`running`, `completed`, `failed`), `None` for a
    /// check that never ran.
    pub fn last_run_status(&self) -> Option<&str> {
        self.0
            .get("last_run")
            .and_then(|r| r.get("status"))
            .and_then(serde_json::Value::as_str)
    }
}

/// One check with its recent runs and its open findings.
#[derive(Debug, Clone, PartialEq)]
pub struct CheckDetailRecord(serde_json::Value);

impl CheckDetailRecord {
    pub(crate) fn new(doc: serde_json::Value) -> Self {
        CheckDetailRecord(doc)
    }

    document_accessors!();

    pub fn check_id(&self) -> &str {
        self.0
            .get("meta")
            .and_then(|m| m.get("id"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
    }

    pub fn run_count(&self) -> usize {
        self.0
            .get("runs")
            .and_then(serde_json::Value::as_array)
            .map_or(0, Vec::len)
    }

    pub fn finding_count(&self) -> usize {
        self.0
            .get("findings")
            .and_then(serde_json::Value::as_array)
            .map_or(0, Vec::len)
    }
}

/// The duplicate-tracks finding's cluster as it is now: the files still
/// there, the durations, the file recommended to keep, and the names that are
/// gone since the finding was made.
#[derive(Debug, Clone, PartialEq)]
pub struct ClusterPreview(serde_json::Value);

impl ClusterPreview {
    pub(crate) fn new(doc: serde_json::Value) -> Self {
        ClusterPreview(doc)
    }

    document_accessors!();

    pub fn album_id(&self) -> &str {
        self.str_at("album_id").unwrap_or("")
    }

    /// True when no file of the cluster is left.
    pub fn is_empty(&self) -> bool {
        self.0
            .get("is_empty")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    }

    /// The files of the cluster that are still there.
    pub fn filenames(&self) -> Vec<&str> {
        self.names("filenames")
    }

    /// The finding's files that are no longer in the cluster.
    pub fn removed_filenames(&self) -> Vec<&str> {
        self.names("removed_filenames")
    }

    fn names(&self, key: &str) -> Vec<&str> {
        self.0
            .get(key)
            .and_then(serde_json::Value::as_array)
            .map(|a| a.iter().filter_map(serde_json::Value::as_str).collect())
            .unwrap_or_default()
    }
}

/// The answer of a multi-disc conversion that ran (also when a step failed:
/// `ok` is then false and each file says why): the document the conversion
/// code builds, with the record file it kept.
#[derive(Debug, Clone, PartialEq)]
pub struct ConversionRecord(serde_json::Value);

impl ConversionRecord {
    pub(crate) fn new(doc: serde_json::Value) -> Self {
        ConversionRecord(doc)
    }

    document_accessors!();

    /// Every step finished.
    pub fn ok(&self) -> bool {
        self.0
            .get("ok")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    }

    /// The conversion stopped part way (a test seam); the next run resumes it.
    pub fn interrupted(&self) -> bool {
        self.0
            .get("interrupted")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    }

    /// The album's id once the conversion is done.
    pub fn album_id(&self) -> &str {
        self.str_at("album_id").unwrap_or("")
    }

    /// A sentence for the user.
    pub fn user_message(&self) -> &str {
        self.str_at("user_message").unwrap_or("")
    }
}

/// The answer of abandoning an unfinished conversion: the records closed and
/// where every file is now.
#[derive(Debug, Clone, PartialEq)]
pub struct AbandonRecord(serde_json::Value);

impl AbandonRecord {
    pub(crate) fn new(doc: serde_json::Value) -> Self {
        AbandonRecord(doc)
    }

    document_accessors!();

    /// The record files that were closed.
    pub fn records(&self) -> Vec<&str> {
        self.0
            .get("records")
            .and_then(serde_json::Value::as_array)
            .map(|a| a.iter().filter_map(serde_json::Value::as_str).collect())
            .unwrap_or_default()
    }
}
