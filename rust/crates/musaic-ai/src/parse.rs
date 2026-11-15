//! A provider's final text into a suggestion: `ai._extract_json`,
//! the response handling of `suggest_metadata` / `suggest_cover`, and
//! `_validate_mbid`. Same whitelists, same coercions (`str(v)`), same
//! failure when a field has a shape the Python app's code trips over (the
//! exception text it would raise becomes the job's error).

use std::sync::OnceLock;

use regex::Regex;

use crate::prompts::{SUGGESTABLE_FIELDS, TRACK_SUGGESTABLE_FIELDS};
use crate::pyjson::{Py, loads, py_strip};

/// `TrackSuggestion`.
#[derive(Debug, Clone, PartialEq)]
pub struct TrackSuggestion {
    pub filename: String,
    pub suggestions: Vec<(String, String)>,
}

/// `MetadataSuggestion` (without `raw`).
#[derive(Debug, Clone, PartialEq)]
pub struct MetadataSuggestion {
    pub suggestions: Vec<(String, String)>,
    pub tracks: Vec<TrackSuggestion>,
    pub notes: String,
    pub confidence: String,
    pub mb_release_group_mbid: Option<String>,
}

impl MetadataSuggestion {
    /// `result["metadata"]` as the worker stores it.
    pub fn to_py(&self) -> Py {
        let pairs = |p: &[(String, String)]| {
            Py::Dict(
                p.iter()
                    .map(|(k, v)| (k.clone(), Py::str(v.clone())))
                    .collect(),
            )
        };
        Py::dict([
            ("suggestions", pairs(&self.suggestions)),
            (
                "tracks",
                Py::List(
                    self.tracks
                        .iter()
                        .map(|t| {
                            Py::dict([
                                ("filename", Py::str(t.filename.clone())),
                                ("suggestions", pairs(&t.suggestions)),
                            ])
                        })
                        .collect(),
                ),
            ),
            ("notes", Py::str(self.notes.clone())),
            ("confidence", Py::str(self.confidence.clone())),
            (
                "mb_release_group_mbid",
                Py::opt_str(self.mb_release_group_mbid.clone()),
            ),
        ])
    }
}

/// `CoverSuggestion` (without `raw`).
#[derive(Debug, Clone, PartialEq)]
pub struct CoverSuggestion {
    pub cover_url: Option<String>,
    pub source: String,
    pub notes: String,
    pub confidence: String,
}

impl CoverSuggestion {
    /// `result["cover"]` as the worker stores it.
    pub fn to_py(&self) -> Py {
        Py::dict([
            ("cover_url", Py::opt_str(self.cover_url.clone())),
            ("source", Py::str(self.source.clone())),
            ("notes", Py::str(self.notes.clone())),
            ("confidence", Py::str(self.confidence.clone())),
        ])
    }
}

fn fence_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // `re.compile(r"^```(?:json)?\s*|\s*```\s*$", re.IGNORECASE |
    // re.MULTILINE)`; Python's `\s` also matches U+001C..U+001F.
    RE.get_or_init(|| {
        Regex::new(r"(?im)^```(?:json)?[\s\x1c-\x1f]*|[\s\x1c-\x1f]*```[\s\x1c-\x1f]*$")
            .expect("fence regex")
    })
}

/// `ai._extract_json(text)`: fences stripped, then the span from the first
/// `{` to the last `}` parsed; `None` when there is none or it does not
/// parse. (The span starts with `{`, so what parses is always a dict.)
pub fn extract_json(text: &str) -> Option<Py> {
    if text.is_empty() {
        return None;
    }
    let cleaned = fence_re().replace_all(py_strip(text), "").into_owned();
    let cleaned = py_strip(&cleaned);
    let first = cleaned.find('{')?;
    let last = cleaned.rfind('}')?;
    if last < first {
        // `cleaned[first:last + 1]` is empty: json.loads("") fails.
        return None;
    }
    loads(&cleaned[first..=last]).ok()
}

/// `parsed.get(k) or {}` / `or []` followed by `.items()` or iteration: the
/// error Python raises for a truthy value of the wrong type.
fn no_items(v: &Py) -> String {
    format!("'{}' object has no attribute 'items'", v.type_name())
}

fn not_iterable(v: &Py) -> String {
    format!("'{}' object is not iterable", v.type_name())
}

/// `str(v or default)`.
fn str_or(v: Option<&Py>, default: &str) -> String {
    match v {
        Some(x) if x.truthy() => x.py_str(),
        _ => default.to_string(),
    }
}

/// The values `for x in v` visits for a truthy `v` (a dict yields its keys,
/// a str its characters); `Err` for a value Python cannot iterate.
fn iterate(v: &Py) -> Result<Vec<Py>, String> {
    match v {
        Py::List(items) => Ok(items.clone()),
        Py::Dict(pairs) => Ok(pairs.iter().map(|(k, _)| Py::str(k.clone())).collect()),
        Py::Str(s) => Ok(s.chars().map(|c| Py::str(c.to_string())).collect()),
        other => Err(not_iterable(other)),
    }
}

/// The response handling of `ai.suggest_metadata` for the provider's final
/// text `raw`. `Err` is the text of the exception the Python app would raise
/// (a truthy `suggestions` that is not a dict, a truthy `tracks` that cannot
/// be iterated).
pub fn parse_metadata(raw: &str) -> Result<MetadataSuggestion, String> {
    let parsed = extract_json(raw)
        .filter(Py::truthy)
        .unwrap_or(Py::Dict(Vec::new()));
    let mut suggestions = Vec::new();
    match parsed.get("suggestions") {
        Some(s) if s.truthy() => {
            let Py::Dict(pairs) = s else {
                return Err(no_items(s));
            };
            for (k, v) in pairs {
                if SUGGESTABLE_FIELDS.contains(&k.as_str()) && !v.is_none_or_empty_str() {
                    suggestions.push((k.clone(), v.py_str()));
                }
            }
        }
        _ => {}
    }
    let mut tracks = Vec::new();
    if let Some(t) = parsed.get("tracks").filter(|t| t.truthy()) {
        for raw_track in iterate(t)? {
            let Py::Dict(_) = raw_track else { continue };
            let filename = raw_track.get("filename").cloned().unwrap_or(Py::Null);
            let track_sug = match raw_track.get("suggestions") {
                Some(s) if s.truthy() => s.clone(),
                _ => Py::Dict(Vec::new()),
            };
            if !filename.truthy() {
                continue;
            }
            let Py::Dict(pairs) = &track_sug else {
                continue;
            };
            let cleaned: Vec<(String, String)> = pairs
                .iter()
                .filter(|(k, v)| {
                    TRACK_SUGGESTABLE_FIELDS.contains(&k.as_str()) && !v.is_none_or_empty_str()
                })
                .map(|(k, v)| (k.clone(), py_strip(&v.py_str()).to_string()))
                .collect();
            if !cleaned.is_empty() {
                tracks.push(TrackSuggestion {
                    filename: filename.py_str(),
                    suggestions: cleaned,
                });
            }
        }
    }
    let mbid = match parsed.get("mb_release_group_mbid") {
        Some(v) if v.truthy() => validate_mbid(v),
        _ => None,
    };
    Ok(MetadataSuggestion {
        suggestions,
        tracks,
        notes: str_or(parsed.get("notes"), ""),
        confidence: str_or(parsed.get("confidence"), "unknown"),
        mb_release_group_mbid: mbid,
    })
}

/// The response handling of `ai.suggest_cover` before URL verification:
/// `cover_url` is `str(v) or None` for any non-null value.
pub fn parse_cover(raw: &str) -> CoverSuggestion {
    let parsed = extract_json(raw)
        .filter(Py::truthy)
        .unwrap_or(Py::Dict(Vec::new()));
    let cover_url = match parsed.get("cover_url") {
        None | Some(Py::Null) => None,
        Some(v) => Some(v.py_str()).filter(|s| !s.is_empty()),
    };
    CoverSuggestion {
        cover_url,
        source: str_or(parsed.get("source"), ""),
        notes: str_or(parsed.get("notes"), ""),
        confidence: str_or(parsed.get("confidence"), "unknown"),
    }
}

fn mbid_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$")
            .expect("mbid regex")
    })
}

/// `ai._validate_mbid`: a str that, stripped and lower-cased, is a
/// lowercase 8-4-4-4-12 UUID.
pub fn validate_mbid(value: &Py) -> Option<String> {
    let Py::Str(s) = value else { return None };
    let s = py_strip(s).to_lowercase();
    // Python's `$` also matches before one trailing "\n"; strip() has
    // removed any.
    mbid_re().is_match(&s).then_some(s)
}
