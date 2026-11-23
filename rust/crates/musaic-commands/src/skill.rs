//! The work-delegation skill bundle (spec 005): the files, their digests and
//! the exact-match lookup every face uses.
//!
//! The hand-written files live under `skills/musaic-work-delegation/` and are
//! embedded at build time, so nothing reads a skill from the filesystem when
//! the app or the CLI runs. Two reference files are generated from the tool
//! registry and the event catalog. Digest and size are computed from exactly
//! the bytes served, and the committed `skill.lock.json` plus the committed
//! copies of the generated files are what `musaic-cli catalog --check` holds
//! against the code.

use std::sync::LazyLock;

use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::{REGISTRY, auth::hex_encode};

/// The skill's name: its directory, its frontmatter `name` and its URI host.
pub const SKILL_NAME: &str = "musaic-work-delegation";
/// `skill://musaic-work-delegation/`
pub const URI_PREFIX: &str = "skill://musaic-work-delegation/";
/// The entry point.
pub const ENTRY: &str = "SKILL.md";
/// The lock file name (not served).
pub const LOCK_FILE: &str = "skill.lock.json";
/// The published listing may be cached for an hour.
pub const TTL_MS: u64 = 3_600_000;
/// The message every refusal carries; it never echoes the request.
pub const UNKNOWN_RESOURCE: &str = "unknown resource";

macro_rules! file {
    ($p:literal) => {
        (
            $p,
            include_bytes!(concat!("../../../../skills/musaic-work-delegation/", $p)) as &[u8],
        )
    };
}

/// The hand-written files, in manifest order.
const EMBEDDED: [(&str, &[u8]); 7] = [
    file!("SKILL.md"),
    file!("references/work-lifecycle.md"),
    file!("references/metadata-evidence.md"),
    file!("references/cover-research.md"),
    file!("references/result-schemas.md"),
    file!("references/async-operations.md"),
    file!("references/recovery.md"),
];

/// The generated files, after the embedded ones.
pub const GENERATED: [&str; 2] = ["references/tools.md", "references/events.md"];

/// One served file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub path: &'static str,
    pub uri: String,
    pub bytes: Vec<u8>,
    /// `sha256:<64 lowercase hex>` over `bytes`.
    pub digest: String,
    pub size: usize,
}

impl Entry {
    fn new(path: &'static str, bytes: Vec<u8>) -> Entry {
        Entry {
            path,
            uri: format!("{URI_PREFIX}{path}"),
            digest: digest_of(&bytes),
            size: bytes.len(),
            bytes,
        }
    }

    /// The text of the file (all of them are UTF-8 Markdown).
    pub fn text(&self) -> &str {
        std::str::from_utf8(&self.bytes).expect("skill files are UTF-8")
    }
}

/// `sha256:<hex>` of some bytes.
pub fn digest_of(bytes: &[u8]) -> String {
    format!("sha256:{}", hex_encode(&Sha256::digest(bytes)))
}

/// The bundle: every file and the parsed frontmatter of `SKILL.md`.
#[derive(Debug)]
pub struct Bundle {
    pub entries: Vec<Entry>,
    /// The frontmatter as a JSON object, every key passed through.
    pub frontmatter: Value,
}

/// A skill lookup the bundle refuses. Carries no detail by design.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("unknown resource")]
pub struct Unknown;

impl Bundle {
    /// The entry whose URI is exactly `uri`.
    pub fn lookup(&self, uri: &str) -> Result<&Entry, Unknown> {
        if refused(uri) {
            return Err(Unknown);
        }
        self.entries.iter().find(|e| e.uri == uri).ok_or(Unknown)
    }

    pub fn skill_md(&self) -> &Entry {
        &self.entries[0]
    }

    pub fn name(&self) -> &str {
        self.frontmatter["name"].as_str().unwrap_or_default()
    }

    pub fn description(&self) -> &str {
        self.frontmatter["description"].as_str().unwrap_or_default()
    }

    /// The skill's semantic version (`metadata.version`).
    pub fn version(&self) -> &str {
        self.frontmatter["metadata"]["version"]
            .as_str()
            .unwrap_or_default()
    }

    /// The body of `SKILL.md` after its frontmatter.
    pub fn skill_body(&self) -> &str {
        body_of(self.skill_md().text())
    }

    /// The lock document for these bytes.
    pub fn lock_json(&self) -> String {
        let mut files: Vec<&Entry> = self.entries.iter().collect();
        files.sort_by_key(|e| e.path);
        let doc = json!({
            "version": self.version(),
            "files": files.iter().map(|e| json!({
                "path": e.path, "digest": e.digest, "size": e.size,
            })).collect::<Vec<_>>(),
        });
        let mut s = serde_json::to_string_pretty(&doc).expect("lock is JSON");
        s.push('\n');
        s
    }
}

/// The reasons a URI is refused before any comparison. Every one of them
/// also fails the exact match; they are named so the tests can list them.
pub fn refused(uri: &str) -> bool {
    uri.contains("..")
        || uri.contains('%')
        || uri.contains('\\')
        || uri.contains('?')
        || uri.contains('#')
        || uri.ends_with('/')
        || uri.chars().any(char::is_control)
        || !uri.starts_with("skill://")
}

static BUNDLE: LazyLock<Bundle> = LazyLock::new(build);

/// The bundle of this build.
pub fn bundle() -> &'static Bundle {
    &BUNDLE
}

fn build() -> Bundle {
    let mut entries: Vec<Entry> = EMBEDDED
        .iter()
        .map(|(path, bytes)| Entry::new(path, bytes.to_vec()))
        .collect();
    entries.push(Entry::new(GENERATED[0], tools_markdown().into_bytes()));
    entries.push(Entry::new(
        GENERATED[1],
        crate::events::events_catalog_markdown().into_bytes(),
    ));
    let frontmatter = parse_frontmatter(entries[0].text()).expect("SKILL.md has frontmatter");
    Bundle {
        entries,
        frontmatter,
    }
}

/// The text after the closing `---` of a frontmatter block.
pub fn body_of(text: &str) -> &str {
    let mut lines = text.split_inclusive('\n');
    if lines.next().map(str::trim_end) != Some("---") {
        return text;
    }
    let mut consumed = 4;
    for line in lines {
        consumed += line.len();
        if line.trim_end() == "---" {
            return text[consumed..].trim_start_matches('\n');
        }
    }
    text
}

/// Parse the YAML subset the skill's frontmatter uses: `key: value` lines and
/// one level of nested keys under a bare `key:` line. Every key is passed
/// through; values are strings.
pub fn parse_frontmatter(text: &str) -> Result<Value, String> {
    let mut lines = text.lines();
    if lines.next().map(str::trim_end) != Some("---") {
        return Err("no frontmatter".into());
    }
    let mut root = Map::new();
    let mut open: Option<(String, Map<String, Value>)> = None;
    let flush = |root: &mut Map<String, Value>, open: &mut Option<(String, Map<String, Value>)>| {
        if let Some((k, m)) = open.take() {
            root.insert(k, Value::Object(m));
        }
    };
    let mut closed = false;
    for line in lines {
        if line.trim_end() == "---" {
            closed = true;
            break;
        }
        if line.trim().is_empty() {
            continue;
        }
        let indented = line.starts_with(' ') || line.starts_with('\t');
        let (key, value) = line
            .trim()
            .split_once(':')
            .ok_or_else(|| "a frontmatter line has no colon".to_string())?;
        let value = unquote(value.trim());
        if indented {
            let (_, map) = open
                .as_mut()
                .ok_or_else(|| "an indented key has no parent".to_string())?;
            map.insert(key.trim().to_string(), Value::String(value));
        } else {
            flush(&mut root, &mut open);
            if value.is_empty() {
                open = Some((key.trim().to_string(), Map::new()));
            } else {
                root.insert(key.trim().to_string(), Value::String(value));
            }
        }
    }
    if !closed {
        return Err("the frontmatter is not closed".into());
    }
    flush(&mut root, &mut open);
    Ok(Value::Object(root))
}

fn unquote(v: &str) -> String {
    let b = v.as_bytes();
    if b.len() >= 2 && (b[0] == b'"' || b[0] == b'\'') && b[b.len() - 1] == b[0] {
        v[1..v.len() - 1].to_string()
    } else {
        v.to_string()
    }
}

/// `references/tools.md`: the tool reference, generated from the registry.
pub fn tools_markdown() -> String {
    let mut m = String::from(
        "# Tool reference\n\nGenerated from the tool registry; do not edit by hand. \
         Your pairing may allow fewer tools than are listed: call `musaic_capabilities` for what you may do. \
         A tool result never contains a filesystem path.\n\n",
    );
    for t in &REGISTRY {
        let kinds = t
            .kinds
            .iter()
            .map(|k| format!("`{k}`"))
            .collect::<Vec<_>>()
            .join(", ");
        m.push_str(&format!(
            "## `{}`\n\n{}\n\nNeeds: {}.\n\nInput:\n\n",
            t.id, t.description, kinds
        ));
        let (input, _) = crate::catalog::tool_fields(t);
        if input.is_empty() {
            m.push_str("- (no fields)\n");
        }
        for l in input {
            m.push_str(&l);
            m.push('\n');
        }
        m.push('\n');
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_name_is_the_directory_and_the_uri_host() {
        let b = bundle();
        assert_eq!(b.name(), SKILL_NAME);
        assert!(URI_PREFIX.ends_with(&format!("{SKILL_NAME}/")));
        assert!(!b.description().is_empty());
        let v = b.version();
        assert_eq!(v.split('.').count(), 3);
        assert!(v.split('.').all(|p| p.parse::<u32>().is_ok()));
    }

    #[test]
    fn the_manifest_is_complete_and_unique() {
        let b = bundle();
        let mut seen = std::collections::BTreeSet::new();
        for e in &b.entries {
            assert!(seen.insert(e.uri.clone()), "{}", e.uri);
            assert!(e.digest.starts_with("sha256:") && e.digest.len() == 7 + 64);
            assert_eq!(e.size, e.bytes.len());
            assert!(
                e.digest[7..]
                    .bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
            );
        }
        assert_eq!(b.entries[0].path, ENTRY);
        for g in GENERATED {
            assert!(seen.contains(&format!("{URI_PREFIX}{g}")));
        }
        assert_eq!(b.entries.len(), EMBEDDED.len() + GENERATED.len());
        // The lock itself is not served.
        assert!(b.lookup(&format!("{URI_PREFIX}{LOCK_FILE}")).is_err());
    }

    #[test]
    fn digests_are_stable_and_come_from_the_served_bytes() {
        let a = build();
        let b = build();
        for (x, y) in a.entries.iter().zip(&b.entries) {
            assert_eq!(x.digest, y.digest);
            assert_eq!(x.digest, digest_of(&x.bytes));
        }
        assert_eq!(a.lock_json(), b.lock_json());
    }

    #[test]
    fn frontmatter_passes_every_key_through() {
        let v = parse_frontmatter(
            "---\nname: a\ndescription: \"b c\"\nmetadata:\n  version: \"1.2.3\"\n  io.example/x: y\nextra: z\n---\nbody\n",
        )
        .unwrap();
        assert_eq!(v["name"], "a");
        assert_eq!(v["description"], "b c");
        assert_eq!(v["metadata"]["version"], "1.2.3");
        assert_eq!(v["metadata"]["io.example/x"], "y");
        assert_eq!(v["extra"], "z");
        assert!(parse_frontmatter("no frontmatter").is_err());
        assert!(parse_frontmatter("---\nname: a\n").is_err());
    }

    #[test]
    fn the_served_frontmatter_equals_the_parsed_bytes() {
        let b = bundle();
        assert_eq!(
            b.frontmatter,
            parse_frontmatter(b.skill_md().text()).unwrap()
        );
        assert!(b.skill_body().starts_with("# Musaic work delegation"));
    }

    #[test]
    fn every_listed_uri_resolves_to_its_own_bytes() {
        let b = bundle();
        for e in &b.entries {
            assert_eq!(b.lookup(&e.uri).unwrap(), e);
        }
    }

    #[test]
    fn every_refusal_case_is_the_same_error() {
        let b = bundle();
        let ok = format!("{URI_PREFIX}SKILL.md");
        let cases = [
            format!("{URI_PREFIX}../SKILL.md"),
            format!("{URI_PREFIX}references/../SKILL.md"),
            format!("{URI_PREFIX}%2e%2e/SKILL.md"),
            format!("{URI_PREFIX}references%2fwork-lifecycle.md"),
            format!("{URI_PREFIX}references\\work-lifecycle.md"),
            format!("{URI_PREFIX}SKILL.md\0"),
            format!("{URI_PREFIX}SKILL.md\n"),
            format!("{ok}?x=1"),
            format!("{ok}#frag"),
            format!("{URI_PREFIX}references/"),
            "/etc/passwd".to_string(),
            "file:///etc/passwd".to_string(),
            "https://example.com/SKILL.md".to_string(),
            "http://example.com/SKILL.md".to_string(),
            "skill://other-skill/SKILL.md".to_string(),
            "skill://musaic-work-delegation".to_string(),
            "skill://musaic-work-delegation/unlisted.md".to_string(),
            "SKILL.md".to_string(),
            "skill://MUSAIC-WORK-DELEGATION/SKILL.md".to_string(),
            String::new(),
        ];
        for case in cases {
            assert_eq!(b.lookup(&case), Err(Unknown), "{case:?}");
        }
        assert_eq!(Unknown.to_string(), UNKNOWN_RESOURCE);
        assert!(b.lookup(&ok).is_ok());
    }

    #[test]
    fn the_generated_references_name_the_registry_and_the_events() {
        let b = bundle();
        let tools = b
            .lookup(&format!("{URI_PREFIX}references/tools.md"))
            .unwrap();
        for t in &REGISTRY {
            assert!(tools.text().contains(&format!("`{}`", t.id)), "{}", t.id);
        }
        let events = b
            .lookup(&format!("{URI_PREFIX}references/events.md"))
            .unwrap();
        assert!(events.text().contains(crate::events::EVENT_REQUESTED));
    }

    #[test]
    fn the_lock_lists_every_file_sorted() {
        let lock: Value = serde_json::from_str(&bundle().lock_json()).unwrap();
        assert_eq!(lock["version"], bundle().version());
        let paths: Vec<&str> = lock["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["path"].as_str().unwrap())
            .collect();
        let mut sorted = paths.clone();
        sorted.sort();
        assert_eq!(paths, sorted);
        assert_eq!(paths.len(), bundle().entries.len());
    }
}
