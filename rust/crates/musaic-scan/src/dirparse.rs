//! Directory-name convention: `Artist - Album [FLAC]`, optional disc marker.
//!
//! Disc markers recognised at the end of the album part (before the `FLAC`
//! tag), with one or two digits and an optional ` of N`:
//!
//! - `Album disc 2`, `Album CD2`, `Album CD 2`, `Album Disk 2` (any case)
//! - the same in `( )` or `[ ]`, or after ` - `
//! - `Album (2 of 3)`
//!
//! `Vol.` / `Volume` / `Part` are not discs. Words that merely contain the
//! letters (`ABCD1`, `Disco 2000`), three-digit numbers (`CD101`) and a
//! marker with nothing before it (an album titled `Disc 1`) are left alone.

use std::sync::LazyLock;

use musaic_core::pycompat::strip;
use regex::Regex;

/// Artist-position names that mark a compilation.
const COLLECTION_NAMES: [&str; 4] = ["compilation", "compilations", "various artists", "various"];

/// The Python app's `_DIR_PATTERN` without its disc group: the marker is read
/// from the album part by [`DISC_MARKER`], so every form is handled in one
/// place. Python's `re` and the `regex` crate agree on everything these
/// patterns use (lazy quantifiers, leftmost-first alternation, Unicode `\s` /
/// `\d`, case-insensitivity), except that Python's `re.I` also lets `i` match
/// U+0130 (İ) and U+0131 (ı), which Unicode simple case folding does not:
/// `disc` is spelled with an explicit class for that
/// (testdata/golden/unicode-names.json).
static DIR_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?P<artist>.+?)\s*-\s*(?P<album>.+?)\s+\[?FLAC\]?\s*$")
        .expect("static pattern")
});

/// A disc marker ending the album part. Narrow on purpose: the number ends the
/// name (so `CD101` is not a marker), the marker is a separate token (preceded
/// by whitespace, ` - ` or an opening bracket that it closes, so `ABCD1` is
/// not one) and something precedes it (`base` is not empty, so an album called
/// `Disc 1` stays itself).
///
/// Parentheses around `disc N` need not pair (`Album (disc 2`): the Python
/// app's pattern allowed that, and a recorded name may rely on it. The new
/// words need a matched pair.
static DISC_MARKER: LazyLock<Regex> = LazyLock::new(|| {
    // `{i}` makes the group names unique: the regex crate has no duplicate names.
    let mark = r"(?:cd|d[i\x{130}\x{131}]s[ck])\s?(?P<n{i}>\d{1,2})(?:\s+of\s+(?P<t{i}>\d{1,2}))?";
    let at = |i: u8| mark.replace("{i}", &i.to_string());
    Regex::new(&format!(
        r"(?i)^(?P<base>.+?)(?:\s+-\s+{}|\s+\(\s*{}\s*\)|\s+\[\s*{}\s*\]|\s+{}|{}|{})$",
        at(1),
        at(2),
        at(3),
        at(4),
        // Python-app forms: `(disc 2` / `disc 2)` with unpaired parentheses.
        r"\s+\(?d[i\x{130}\x{131}]sc\s+(?P<n5>\d{1,2})(?:\s+of\s+(?P<t5>\d{1,2}))?\)?",
        r"\s+\((?P<n6>\d{1,2})\s+of\s+(?P<t6>\d{1,2})\)",
    ))
    .expect("static pattern")
});

/// `(album without the marker, disc number, disc total)` when the album part
/// ends in a marker that makes sense: a number of at least 1, no larger than
/// the total when there is one.
fn split_disc_marker(album: &str) -> Option<(String, i64, Option<i64>)> {
    let m = DISC_MARKER.captures(album)?;
    let group = |p: &str| (1..=6).find_map(|i| m.name(&format!("{p}{i}")));
    let number = group("n").and_then(|g| musaic_core::pycompat::int(g.as_str()))?;
    let total = group("t").and_then(|g| musaic_core::pycompat::int(g.as_str()));
    if number < 1 || total.is_some_and(|t| t < number) {
        return None;
    }
    Some((strip(&m["base"]).to_string(), number, total))
}

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
    let album = strip(&m["album"]);
    match split_disc_marker(album) {
        Some((base, number, total)) => {
            parsed.dir_album = Some(base);
            parsed.disc_number = Some(number);
            parsed.disc_total = total;
        }
        None => parsed.dir_album = Some(album.to_string()),
    }
    if COLLECTION_NAMES.contains(&artist.to_lowercase().as_str()) {
        parsed.is_compilation = true;
    } else {
        parsed.dir_artist = Some(artist.to_string());
    }
    parsed
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `(folder name, album, disc, total)` for names that carry a marker.
    #[test]
    fn markers_are_recognised() {
        let cases: &[(&str, &str, i64, Option<i64>)] = &[
            ("Artist - Album CD1 [FLAC]", "Album", 1, None),
            ("Artist - Album cd1 [FLAC]", "Album", 1, None),
            ("Artist - Album CD 2 [FLAC]", "Album", 2, None),
            ("Artist - Album (CD 2) [FLAC]", "Album", 2, None),
            ("Artist - Album (CD2) [FLAC]", "Album", 2, None),
            ("Artist - Album [CD 2] [FLAC]", "Album", 2, None),
            ("Artist - Album [Disc 1] [FLAC]", "Album", 1, None),
            ("Artist - Album - Disc 3 [FLAC]", "Album", 3, None),
            ("Artist - Album - CD3 FLAC", "Album", 3, None),
            ("Artist - Album Disk 2 of 4 [FLAC]", "Album", 2, Some(4)),
            ("Artist - Album (Disk 2 of 4) [FLAC]", "Album", 2, Some(4)),
            ("Artist - Album [CD 1 of 2] [FLAC]", "Album", 1, Some(2)),
            ("Artist - Album DISC12 [FLAC]", "Album", 12, None),
            (
                "Artist - Album (Deluxe) CD2 [FLAC]",
                "Album (Deluxe)",
                2,
                None,
            ),
            // The forms the Python app already read.
            ("Artist - Album (disc 1 of 3) [FLAC]", "Album", 1, Some(3)),
            ("Artist - Album Disc 1 [FLAC]", "Album", 1, None),
            ("Artist - Album (disc 2) [FLAC]", "Album", 2, None),
            ("Artist - Album (disc 2 [FLAC]", "Album", 2, None),
            ("Artist - Album disc 2) [FLAC]", "Album", 2, None),
            ("Artist - Album (10 of 12) [FLAC]", "Album", 10, Some(12)),
            ("Artist - Album (Disc 3 Of 5) [FLAC]", "Album", 3, Some(5)),
        ];
        for (name, album, disc, total) in cases {
            let p = parse_album_dir(name);
            assert_eq!(
                (p.dir_album.as_deref(), p.disc_number, p.disc_total),
                (Some(*album), Some(*disc), *total),
                "{name}"
            );
        }
    }

    /// The marker words and numbers of real titles stay in the album.
    #[test]
    fn titles_are_left_alone() {
        let names = [
            ("Artist - ABCD1 [FLAC]", "ABCD1"),
            ("Artist - Album ABCD1 [FLAC]", "Album ABCD1"),
            ("Artist - Album CD101 [FLAC]", "Album CD101"),
            ("Artist - Album Disc 101 [FLAC]", "Album Disc 101"),
            ("Artist - Disco 2000 [FLAC]", "Disco 2000"),
            ("Artist - Album Disco 2 [FLAC]", "Album Disco 2"),
            ("Artist - Discovery [FLAC]", "Discovery"),
            ("Artist - Album CD Single [FLAC]", "Album CD Single"),
            ("Artist - Greatest Hits 2 [FLAC]", "Greatest Hits 2"),
            ("Artist - Album Volume 2 [FLAC]", "Album Volume 2"),
            ("Artist - Album Vol. 2 [FLAC]", "Album Vol. 2"),
            ("Artist - Album Part 2 [FLAC]", "Album Part 2"),
            ("Artist - Live at Studio 54 [FLAC]", "Live at Studio 54"),
            ("Artist - Disc 1 [FLAC]", "Disc 1"),
            ("Artist - CD1 [FLAC]", "CD1"),
            ("Artist - (CD 2) [FLAC]", "(CD 2)"),
            ("Artist - Album-CD1 [FLAC]", "Album-CD1"),
            ("Artist - Album (CD1] [FLAC]", "Album (CD1]"),
            ("Artist - Album [CD1) [FLAC]", "Album [CD1)"),
            ("Artist - Album CD1 (2005) [FLAC]", "Album CD1 (2005)"),
            ("Artist - Album CD0 [FLAC]", "Album CD0"),
            ("Artist - Album CD 3 of 2 [FLAC]", "Album CD 3 of 2"),
            (
                "Artist - Album (disc one of two) [FLAC]",
                "Album (disc one of two)",
            ),
        ];
        for (name, album) in names {
            let p = parse_album_dir(name);
            assert_eq!(
                (p.dir_album.as_deref(), p.disc_number, p.disc_total),
                (Some(album), None, None),
                "{name}"
            );
        }
    }

    /// The folders of a set fold to one album, which the multi-disc
    /// grouping relies on.
    #[test]
    fn discs_of_a_set_share_an_album() {
        let a = parse_album_dir("Artist - Album CD1 [FLAC]");
        let b = parse_album_dir("Artist - Album (CD 2) [FLAC]");
        assert_eq!(a.dir_album, b.dir_album);
        assert_eq!((a.disc_number, b.disc_number), (Some(1), Some(2)));
    }
}
