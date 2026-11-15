//! Directory-name convention: `Artist - Album [FLAC]`, optional disc marker.

use std::sync::LazyLock;

use musaic_core::pycompat::strip;
use regex::Regex;

/// Artist-position names that mark a compilation.
const COLLECTION_NAMES: [&str; 4] = ["compilation", "compilations", "various artists", "various"];

/// Same pattern as the Python app's `_DIR_PATTERN`. Python's `re` and the `regex`
/// crate agree on everything it uses (lazy quantifiers, leftmost-first
/// alternation, Unicode `\s` / `\d`, case-insensitivity), except that
/// Python's `re.I` also lets `i` match U+0130 (İ) and U+0131 (ı), which
/// Unicode simple case folding does not: `disc` is spelled with an explicit
/// class for that (testdata/golden/unicode-names.json).
static DIR_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?i)^(?P<artist>.+?)\s*-\s*(?P<album>.+?)",
        r"(?:",
        r"\s+\(?d[i\x{130}\x{131}]sc\s+(?P<disc>\d+)(?:\s+of\s+(?P<disc_total>\d+))?\)?",
        r"|\s+\((?P<disc2>\d+)\s+of\s+(?P<disc_total2>\d+)\)",
        r")?",
        r"\s+\[?FLAC\]?\s*$",
    ))
    .expect("static pattern")
});

/// Result of parsing an album directory name. Everything is `None` / `false`
/// when the convention did not match.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DirectoryParse {
    pub matched: bool,
    /// `None` when `is_compilation`.
    pub dir_artist: Option<String>,
    pub dir_album: Option<String>,
    pub is_compilation: bool,
    pub disc_number: Option<i64>,
    pub disc_total: Option<i64>,
}

pub fn parse_album_dir(dirname: &str) -> DirectoryParse {
    let mut parsed = DirectoryParse::default();
    let Some(m) = DIR_PATTERN.captures(dirname) else {
        return parsed;
    };
    parsed.matched = true;
    let artist = strip(&m["artist"]);
    parsed.dir_album = Some(strip(&m["album"]).to_string());
    if COLLECTION_NAMES.contains(&artist.to_lowercase().as_str()) {
        parsed.is_compilation = true;
    } else {
        parsed.dir_artist = Some(artist.to_string());
    }
    let group = |a: &str, b: &str| m.name(a).or_else(|| m.name(b)).map(|g| g.as_str());
    parsed.disc_number = group("disc", "disc2").and_then(musaic_core::pycompat::int);
    parsed.disc_total = group("disc_total", "disc_total2").and_then(musaic_core::pycompat::int);
    parsed
}
