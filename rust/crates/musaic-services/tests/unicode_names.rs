//! Non-ASCII names (NFC and NFD; CJK, emoji, combining marks,
//! names whose lower-casing changes byte length such as "İ", Python `re.I`
//! equivalences, non-ASCII decimal digits) through every public name-parsing
//! and name-matching function the port shares with the Python app. No call may
//! panic, and every result must equal the Python app's
//! (testdata/golden/unicode-names.json, recorded from the Python app's own
//! functions and the reference regex).
//!
//! Covered: the directory parse, the disc marker and title similarity,
//! filename sanitising, the article / sort-key cascades (CLAUDE.md
//! pattern 4). Deliberate differences: none.

use std::panic::catch_unwind;

use musaic_checks::checks::tags_out_of_sync::{rotate_article, starts_with_article};
use musaic_core::album_fields::{display_artist, sort_key};
use musaic_fsops::template::sanitize_for_filename;
use musaic_resolve::similarity::{norm_title, title_similarity_hundredths};
use musaic_resolve::source::AlbumRecord;
use serde_json::{Value, json};

fn golden() -> Value {
    musaic_testdata::golden("unicode-names.json")
}

/// Everything the Rust side computes for one name, in the golden's shape.
fn ours(name: &str, other: &str) -> Value {
    let p = musaic_scan::parse_album_dir(name);
    let rec = AlbumRecord {
        dir_name: Some(name.to_string()),
        ..Default::default()
    };
    json!({
        "parse_album_dir": {
            "matched": p.matched, "dir_artist": p.dir_artist, "dir_album": p.dir_album,
            "is_compilation": p.is_compilation, "disc_number": p.disc_number,
            "disc_total": p.disc_total,
        },
        "disc_marker": rec.is_disc_directory(),
        "norm_title": norm_title(name),
        "similarity_other_form_hundredths": title_similarity_hundredths(name, other),
        "starts_with_article": starts_with_article(Some(name)),
        "rotate_article": rotate_article(name),
        "sort_key": sort_key(Some(name), None, None, Some("x")),
        "display_artist": display_artist(Some(name), Some("x"), None, false),
        "sanitize_for_filename": sanitize_for_filename(name),
    })
}

#[test]
fn non_ascii_names_never_panic_and_match_the_python_app() {
    let g = golden();
    let cases = g["cases"].as_array().unwrap();
    assert!(cases.len() >= 40, "golden too small");
    let (mut nfd, mut mismatches) = (0, Vec::new());
    for c in cases {
        let name = c["name"].as_str().unwrap();
        let other = c["other_form"].as_str().unwrap();
        nfd += usize::from(c["form"] == "NFD");
        let got =
            catch_unwind(|| ours(name, other)).unwrap_or_else(|_| panic!("panicked on {name:?}"));
        for (k, v) in got.as_object().unwrap() {
            if &c[k] != v {
                mismatches.push(format!("{name:?} {k}: recorded {} rust {v}", c[k]));
            }
        }
    }
    assert!(nfd >= 15, "golden must carry NFD forms");
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}
