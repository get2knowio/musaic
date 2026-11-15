//! Where the workspace's tests find their data. Dev-only: every crate lists
//! it under `[dev-dependencies]`.
//!
//! Two tiers:
//!
//! * **Public** data lives in `rust/testdata/` and is always present:
//!   self-authored, synthetic inputs and the expected results recorded for
//!   them. [`public`] returns a path inside it.
//! * **Private** data (recorded third-party answers, anything derived from a
//!   real music library) lives outside the public tree. [`private`] returns
//!   a path inside it, or `None` when it is not available; a test that needs
//!   it calls [`require_private!`] (or [`skip`]) and returns early, printing
//!   one `skipped: private test data not available (<test>)` line.
//!
//! The private root is, in order:
//!
//! 1. `$MUSAIC_PRIVATE_TESTDATA` when the variable is set. Setting it turns
//!    the fallback below off; an empty value means "no private data".
//! 2. `<workspace>/../private-testdata` when that directory exists (a
//!    separate checkout of the private test data placed next to `rust/`).
//!
//! Mixed files are stored as two halves with the same relative path: the
//! public half under `rust/testdata/`, the private half under the private
//! root. [`json`] merges them (see [`merge`]); a test that also has cases
//! only the private half carries runs those when [`Json::private`] is true.
//!
//! The synthetic audio library is generated, never committed:
//! [`synthetic_library`] fails with the generator command when it is absent.

use std::path::{Path, PathBuf};

use serde_json::Value;

/// Environment variable naming the private test-data directory.
pub const PRIVATE_ENV: &str = "MUSAIC_PRIVATE_TESTDATA";

/// Name of the directory, next to the workspace, that holds the private data
/// when [`PRIVATE_ENV`] is not set.
pub const PRIVATE_FALLBACK_DIR: &str = "private-testdata";

/// The command that generates the synthetic library, run from `rust/`.
pub const SYNTHETIC_LIBRARY_COMMAND: &str =
    "python3 testdata/synthetic-library/gen_fixture_library.py";

/// The Rust workspace root (`rust/`).
pub fn workspace_root() -> PathBuf {
    // This crate is `rust/crates/musaic-testdata`.
    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = here.join("../..");
    if root.exists() { canonical(root) } else { root }
}

/// `std::fs::canonicalize`, without the `\\?\` prefix it adds on Windows: the
/// spelling `musaic_scan::py_resolve` gives an existing directory (on-disk
/// names, links followed, 8.3 short names expanded on Windows), so a temp dir
/// made with this matches the paths the app stores and reports. Panics when
/// the path does not exist.
pub fn canonical(p: impl AsRef<Path>) -> PathBuf {
    let p = p.as_ref();
    let c = p
        .canonicalize()
        .unwrap_or_else(|e| panic!("canonicalize {}: {e}", p.display()));
    match c.to_str() {
        Some(s) if s.starts_with(r"\\?\UNC\") => PathBuf::from(format!(r"\\{}", &s[8..])),
        Some(s) if s.starts_with(r"\\?\") && s.as_bytes().get(5) == Some(&b':') => {
            PathBuf::from(&s[4..])
        }
        _ => c,
    }
}

/// `s.replace(root, marker)`, for masking a temp root in output compared
/// with a golden recorded on macOS. On Windows the path that follows each
/// masked root is also spelled with `/`, as the golden spells it. The root
/// is looked for with its separators escaped once (JSON, or a Python-style
/// repr: `C:\\Users\\…`) or twice (a repr inside JSON), most-escaped
/// first, then as written; the separators of the same width that follow it
/// become `/`, up to the next `'`, `"`, `\"` or line end, and any other
/// escape (such as `\u00e9`) is kept. Elsewhere this is exactly
/// `str::replace`.
pub fn replace_root(s: &str, root: &str, marker: &str) -> String {
    if !cfg!(windows) || root.is_empty() {
        return s.replace(root, marker);
    }
    let mut out = s.to_owned();
    for width in [4, 2, 1] {
        let sep = "\\".repeat(width);
        out = mask_runs(&out, &root.replace('\\', &sep), marker, &sep);
    }
    out
}

fn mask_runs(s: &str, root: &str, marker: &str, sep: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find(root) {
        out.push_str(&rest[..at]);
        out.push_str(marker);
        rest = &rest[at + root.len()..];
        let mut i = 0;
        while i < rest.len() {
            let tail = &rest[i..];
            if tail.starts_with(sep) {
                out.push('/');
                i += sep.len();
                continue;
            }
            let c = tail.chars().next().unwrap();
            if matches!(c, '\'' | '"' | '\n' | '\r') {
                break;
            }
            if c == '\\' {
                // An escaped quote ends the string; another escape is kept.
                let after = tail.trim_start_matches('\\');
                if after.starts_with('"') {
                    break;
                }
                let run = tail.len() - after.len();
                out.push_str(&tail[..run]);
                i += run;
                continue;
            }
            out.push(c);
            i += c.len_utf8();
        }
        rest = &rest[i..];
    }
    out.push_str(rest);
    out
}

/// A message recorded on macOS with the C library's text for ENOENT, in
/// this OS's words: on Windows `No such file or directory (os error 2)`
/// becomes `The system cannot find the file specified. (os error 2)`, the
/// text `std::io::Error` gives there. Elsewhere unchanged.
pub fn os_error_text(s: &str) -> String {
    if !cfg!(windows) {
        return s.to_owned();
    }
    s.replace(
        "No such file or directory (os error 2)",
        &std::io::Error::from_raw_os_error(2).to_string(),
    )
}

/// `rel` with its components joined by `/`, the way goldens recorded on
/// macOS spell relative paths.
pub fn slash_path(rel: impl AsRef<Path>) -> String {
    let mut out = String::new();
    for c in rel.as_ref().components() {
        if !out.is_empty() {
            out.push('/');
        }
        out.push_str(&c.as_os_str().to_string_lossy());
    }
    out
}

/// The public test-data root (`rust/testdata`).
pub fn public_root() -> PathBuf {
    workspace_root().join("testdata")
}

/// A path inside the public test data. Panics when it does not exist, so a
/// missing file names itself instead of failing later as an I/O error.
pub fn public(rel: impl AsRef<Path>) -> PathBuf {
    let p = public_root().join(rel.as_ref());
    assert!(
        p.exists(),
        "public test data {} is missing (rust/testdata is part of the repository)",
        p.display()
    );
    p
}

/// The private test-data root, if one is configured and present.
pub fn private_root() -> Option<PathBuf> {
    match std::env::var_os(PRIVATE_ENV) {
        Some(v) if v.is_empty() => None,
        Some(v) => {
            let p = PathBuf::from(v);
            p.is_dir().then_some(p)
        }
        None => {
            let p = workspace_root().join("..").join(PRIVATE_FALLBACK_DIR);
            if p.is_dir() { Some(canonical(p)) } else { None }
        }
    }
}

/// A path inside the private test data, or `None` when that file or
/// directory is not available.
pub fn private(rel: impl AsRef<Path>) -> Option<PathBuf> {
    let p = private_root()?.join(rel.as_ref());
    p.exists().then_some(p)
}

/// A path inside the private test data that the calling test cannot do
/// without. Panics when it is not available: a test that reaches this must
/// start with [`require_private!`] naming the same path.
pub fn private_required(rel: impl AsRef<Path>) -> PathBuf {
    let rel = rel.as_ref();
    private(rel).unwrap_or_else(|| {
        panic!(
            "private test data {} is not available; this test needs \
             `musaic_testdata::require_private!(\"{}\")` at its start",
            rel.display(),
            rel.display()
        )
    })
}

/// True when the private test data itself is present (its `README.md`
/// marks it; an empty directory named by the variable does not count).
pub fn private_present() -> bool {
    private("README.md").is_some()
}

/// True when every one of `rels` is available as private data.
pub fn private_available(rels: &[&str]) -> bool {
    rels.iter().all(|r| private(r).is_some())
}

/// The name of the running test (libtest names the test thread after it).
pub fn test_name() -> String {
    std::thread::current()
        .name()
        .unwrap_or("<unnamed test>")
        .to_string()
}

/// Print the one-line skip notice for the running test. The caller returns
/// right after.
pub fn skip() {
    println!("skipped: private test data not available ({})", test_name());
}

/// Print the notice for a test that ran its public cases but not the cases
/// that only the private data carries.
pub fn skip_private_cases() {
    println!(
        "skipped: private test data not available ({}: private cases only)",
        test_name()
    );
}

/// Return from the calling test (after printing the skip line) unless every
/// named private path is available. With no arguments, only the private
/// root itself is required.
///
/// ```text
/// #[test]
/// fn replays_recorded_answers() {
///     musaic_testdata::require_private!("replay");
///     // ...
/// }
/// ```
#[macro_export]
macro_rules! require_private {
    () => {
        if !$crate::private_present() {
            $crate::skip();
            return;
        }
    };
    ($($rel:expr),+ $(,)?) => {
        if !$crate::private_available(&[$($rel),+]) {
            $crate::skip();
            return;
        }
    };
}

/// The generated synthetic audio library (`rust/testdata/synthetic-library/library`).
/// Panics, naming the generator command, when it has not been generated.
pub fn synthetic_library() -> PathBuf {
    let p = public_root().join("synthetic-library/library");
    assert!(
        p.is_dir(),
        "the synthetic audio library {} has not been generated (audio is never committed); \
         generate it with `{SYNTHETIC_LIBRARY_COMMAND}` from rust/ (needs Python 3 with numpy, \
         soundfile, mutagen and Pillow)",
        p.display()
    );
    p
}

/// The local copy of the real-audio library (private, never committed), if present.
pub fn real_library() -> Option<PathBuf> {
    private("real-library/library").filter(|p| p.is_dir())
}

/// Read and parse a JSON file, naming it on failure.
pub fn read_json(path: impl AsRef<Path>) -> Value {
    let path = path.as_ref();
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("bad JSON in {}: {e}", path.display()))
}

/// A public JSON file.
pub fn public_json(rel: impl AsRef<Path>) -> Value {
    read_json(public(rel))
}

/// A private JSON file, if available.
pub fn private_json(rel: impl AsRef<Path>) -> Option<Value> {
    private(rel).map(read_json)
}

/// A JSON document stored as a public half and, optionally, a private half.
pub struct Json {
    /// The public half merged with the private half when that was available.
    pub value: Value,
    /// Whether the private half was available and merged in.
    pub private: bool,
}

/// The JSON document at `rel`: the public half (which must exist) merged
/// with the private half at the same relative path when available.
pub fn json(rel: impl AsRef<Path>) -> Json {
    let rel = rel.as_ref();
    let public = public_json(rel);
    match private_json(rel) {
        Some(private) => Json {
            value: merge(public, private),
            private: true,
        },
        None => Json {
            value: public,
            private: false,
        },
    }
}

/// A golden under `golden/` as one document: [`json`]`("golden/<rel>").value`.
pub fn golden(rel: &str) -> Value {
    json(Path::new("golden").join(rel)).value
}

/// Merge the private half of a split JSON document into its public half.
///
/// The private half has the same shape as the public one and holds only the
/// parts that were moved out of it. Objects merge key by key (recursively
/// where both halves have the key). Arrays named in the private half's
/// top-level `"_split"` object (JSON pointer -> ascending indices) get the
/// private elements inserted back at those indices of the merged array;
/// arrays not named there must have equal length and merge element by
/// element. Anything else present in both halves is a conflict and panics.
pub fn merge(public: Value, mut private: Value) -> Value {
    let splits = match private.as_object_mut().and_then(|o| o.remove("_split")) {
        Some(Value::Object(m)) => m
            .into_iter()
            .map(|(k, v)| {
                let idx: Vec<usize> = v
                    .as_array()
                    .unwrap_or_else(|| panic!("_split {k}: not an array"))
                    .iter()
                    .map(|i| i.as_u64().expect("_split index") as usize)
                    .collect();
                (k, idx)
            })
            .collect(),
        Some(other) => panic!("_split must be an object, not {other}"),
        None => Vec::new(),
    };
    merge_at(public, private, "", &splits)
}

fn escape(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}

fn merge_at(public: Value, private: Value, ptr: &str, splits: &[(String, Vec<usize>)]) -> Value {
    match (public, private) {
        (Value::Object(mut a), Value::Object(b)) => {
            for (k, v) in b {
                let child = format!("{ptr}/{}", escape(&k));
                let merged = match a.remove(&k) {
                    Some(old) => merge_at(old, v, &child, splits),
                    None => v,
                };
                a.insert(k, merged);
            }
            Value::Object(a)
        }
        (Value::Array(a), Value::Array(b)) => {
            if let Some((_, idx)) = splits.iter().find(|(p, _)| p == ptr) {
                assert_eq!(
                    idx.len(),
                    b.len(),
                    "{ptr}: _split indices vs private elements"
                );
                let mut out = a;
                for (i, v) in idx.iter().zip(b) {
                    assert!(*i <= out.len(), "{ptr}: _split index {i} out of range");
                    out.insert(*i, v);
                }
                Value::Array(out)
            } else {
                assert_eq!(
                    a.len(),
                    b.len(),
                    "{ptr}: arrays of different length and no _split entry"
                );
                Value::Array(
                    a.into_iter()
                        .zip(b)
                        .enumerate()
                        .map(|(i, (x, y))| merge_at(x, y, &format!("{ptr}/{i}"), splits))
                        .collect(),
                )
            }
        }
        (a, b) => panic!("{ptr}: both halves hold a value ({a} vs {b})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn merges_split_arrays_objects_and_elementwise() {
        let public = json!({
            "a": 1,
            "cases": [0, 2, 4],
            "libs": {"synthetic": {"n": 1}},
            "roots": [{"x": 1}, {"x": 2}],
        });
        let private = json!({
            "_split": {"/cases": [1, 3]},
            "cases": [1, 3],
            "libs": {"real": {"n": 2}},
            "roots": [{"t": "p"}, {}],
            "extra": true,
        });
        assert_eq!(
            merge(public, private),
            json!({
                "a": 1,
                "cases": [0, 1, 2, 3, 4],
                "libs": {"synthetic": {"n": 1}, "real": {"n": 2}},
                "roots": [{"x": 1, "t": "p"}, {"x": 2}],
                "extra": true,
            })
        );
    }

    #[test]
    fn escapes_pointer_keys() {
        let public = json!({"a/b": [1]});
        let private = json!({"_split": {"/a~1b": [0]}, "a/b": [0]});
        assert_eq!(merge(public, private), json!({"a/b": [0, 1]}));
    }

    #[test]
    #[should_panic(expected = "both halves hold a value")]
    fn conflicting_scalars_panic() {
        merge(json!({"a": 1}), json!({"a": 2}));
    }

    #[test]
    fn mask_runs_spells_the_masked_path_with_slashes() {
        let root = r"C:\t\lib";
        assert_eq!(
            mask_runs(
                r"Album directory 'C:\t\lib\A\B' is gone",
                root,
                "<LIB>",
                "\\"
            ),
            "Album directory '<LIB>/A/B' is gone"
        );
        let escaped = r"C:\\t\\lib";
        assert_eq!(
            mask_runs(
                r#"{"path": "C:\\t\\lib\\Caf\u00e9\\01.flac", "x": "a\\b"}"#,
                escaped,
                "<LIB>",
                r"\\"
            ),
            r#"{"path": "<LIB>/Caf\u00e9/01.flac", "x": "a\\b"}"#
        );
        assert_eq!(
            mask_runs(
                r#"data: {\"p\": \"C:\\t\\lib\\x\"}"#,
                escaped,
                "<LIB>",
                r"\\"
            ),
            r#"data: {\"p\": \"<LIB>/x\"}"#
        );
        // A Python repr inside JSON: separators escaped twice.
        let twice = r"C:\\\\t\\\\lib";
        assert_eq!(
            mask_runs(
                r#""[Errno 13] Permission denied: 'C:\\\\t\\\\lib\\\\A\\\\01.flac'""#,
                twice,
                "<LIB>",
                r"\\\\"
            ),
            r#""[Errno 13] Permission denied: '<LIB>/A/01.flac'""#
        );
        assert_eq!(slash_path(Path::new("a").join("b")), "a/b");
    }
}
