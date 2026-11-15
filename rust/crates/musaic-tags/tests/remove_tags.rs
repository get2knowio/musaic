//! Removing a tag through apply (issue #210). The Python app could not: an
//! empty or null suggestion was skipped, and a whitespace-only one was
//! written as an empty tag. Removal is now an explicit request
//! ([`Removals`]); a suggestion never removes or blanks a tag.
//!
//! Invented albums built from minimal FLACs (STREAMINFO + VORBIS_COMMENT +
//! padding, no audio frames).

use std::fs;
use std::path::Path;

use musaic_tags::{Diff, Event, FileChange, FlacFile, Removals, apply_to_album_iter};

/// Minimal FLAC (the recipe of this crate's logging_seams test).
fn flac_bytes(tags: &[(&str, &str)]) -> Vec<u8> {
    let mut si = Vec::new();
    si.extend_from_slice(&[0x10, 0, 0x10, 0, 0, 0, 0, 0, 0, 0]);
    let packed: u64 = (44100u64 << 44) | (1 << 41) | (15 << 36) | 44100;
    si.extend_from_slice(&packed.to_be_bytes());
    si.extend_from_slice(&[0; 16]);
    let mut vc = Vec::new();
    vc.extend_from_slice(&4u32.to_le_bytes());
    vc.extend_from_slice(b"test");
    vc.extend_from_slice(&(tags.len() as u32).to_le_bytes());
    for (k, v) in tags {
        let e = format!("{k}={v}");
        vc.extend_from_slice(&(e.len() as u32).to_le_bytes());
        vc.extend_from_slice(e.as_bytes());
    }
    let mut out = b"fLaC".to_vec();
    out.push(0);
    out.extend_from_slice(&(si.len() as u32).to_be_bytes()[1..]);
    out.extend_from_slice(&si);
    out.push(0x04);
    out.extend_from_slice(&(vc.len() as u32).to_be_bytes()[1..]);
    out.extend_from_slice(&vc);
    out.push(0x81);
    out.extend_from_slice(&1024u32.to_be_bytes()[1..]);
    out.extend_from_slice(&[0; 1024]);
    out
}

/// Two tracks of an invented album, with a stray sort tag spelled two ways,
/// a two-valued genre, an alias spelling of the album artist and a year
/// under both DATE and YEAR.
fn album(dir: &Path) {
    for (name, n, title) in [
        ("01 - Glass.flac", "1", "Glass"),
        ("02 - Tin.flac", "2", "Tin"),
    ] {
        fs::write(
            dir.join(name),
            flac_bytes(&[
                ("TITLE", title),
                ("ARTIST", "Velvet Quarry"),
                ("ALBUM", "Lanterns"),
                ("ALBUMARTIST", "Velvet Quarry"),
                ("album_artist", "Velvet Quarry"),
                ("DATE", "2011-05-02"),
                ("YEAR", "2011"),
                ("ORIGINALDATE", "2009"),
                ("GENRE", "Shoegaze"),
                ("genre", "Dream Pop"),
                ("TRACKNUMBER", n),
                ("ALBUMARTISTSORT", "Wrong, Sort"),
                ("albumartistsort", "Also Wrong"),
                ("COMMENT", ""),
            ]),
        )
        .unwrap();
    }
}

fn comments(path: &Path) -> Vec<(String, String)> {
    FlacFile::open(path)
        .unwrap()
        .tags()
        .unwrap()
        .comments
        .clone()
}

fn keys(path: &Path) -> Vec<String> {
    comments(path).into_iter().map(|(k, _)| k).collect()
}

/// `(field, value)` suggestions as written in a test.
type Pairs<'a> = &'a [(&'a str, Option<&'a str>)];

fn run(
    dir: &Path,
    suggestions: &[(&str, Option<&str>)],
    tracks: &[(&str, Pairs)],
    removals: &Removals,
    dry_run: bool,
) -> Vec<Event> {
    let own = |s: &[(&str, Option<&str>)]| -> Vec<(String, Option<String>)> {
        s.iter()
            .map(|(k, v)| (k.to_string(), v.map(str::to_string)))
            .collect()
    };
    let tracks = tracks
        .iter()
        .map(|(f, s)| (f.to_string(), own(s)))
        .collect();
    let mut events = Vec::new();
    apply_to_album_iter(
        dir,
        &own(suggestions),
        &tracks,
        removals,
        None,
        dry_run,
        &mut |e| events.push(e),
    )
    .unwrap();
    events
}

fn file_done(events: &[Event], filename: &str) -> FileChange {
    events
        .iter()
        .find_map(|e| match e {
            Event::FileDone(c) if c.filename == filename => Some(c.clone()),
            _ => None,
        })
        .unwrap()
}

fn removed(old: &str) -> Diff {
    Diff {
        old: old.into(),
        new: None,
    }
}

fn album_removals(fields: &[&str]) -> Removals {
    Removals {
        album: fields.iter().map(|f| f.to_string()).collect(),
        tracks: Vec::new(),
    }
}

/// Every value under every spelling of the key goes; the diff names the
/// removal (`new: None`) and shows every value removed.
#[test]
fn album_level_removal_takes_every_value_and_spelling() {
    let tmp = tempfile::tempdir().unwrap();
    album(tmp.path());
    let events = run(
        tmp.path(),
        &[],
        &[],
        &album_removals(&["album_artist_sort", "genre"]),
        false,
    );
    for name in ["01 - Glass.flac", "02 - Tin.flac"] {
        let c = file_done(&events, name);
        assert_eq!(
            c.diffs,
            vec![
                ("albumartistsort".into(), removed("Wrong, Sort; Also Wrong")),
                ("genre".into(), removed("Shoegaze; Dream Pop")),
            ],
            "{name}"
        );
        assert!(c.saved && c.error.is_none(), "{name}");
        let k = keys(&tmp.path().join(name));
        assert!(
            !k.iter()
                .any(|k| k.eq_ignore_ascii_case("albumartistsort")
                    || k.eq_ignore_ascii_case("genre")),
            "{name}: {k:?}"
        );
        // Everything else is untouched, in place.
        assert!(k.starts_with(&["TITLE".to_string(), "ARTIST".to_string()]));
    }
    let Event::Start {
        cleared,
        suggestions_applied,
        track_change_count,
        ..
    } = &events[0]
    else {
        panic!("{:?}", events[0]);
    };
    assert_eq!(cleared, &["album_artist_sort", "genre"]);
    assert!(suggestions_applied.is_empty());
    assert_eq!(*track_change_count, 0);
    let Some(Event::Done {
        cleared,
        saved_count,
        ..
    }) = events.last()
    else {
        panic!()
    };
    assert_eq!(cleared, &["album_artist_sort", "genre"]);
    assert_eq!(*saved_count, 2);
}

/// A dry run shows the same removal diffs and leaves the files alone.
#[test]
fn dry_run_shows_removals_and_writes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    album(tmp.path());
    let before = fs::read(tmp.path().join("01 - Glass.flac")).unwrap();
    let events = run(
        tmp.path(),
        &[],
        &[],
        &album_removals(&["album_artist_sort"]),
        true,
    );
    let c = file_done(&events, "01 - Glass.flac");
    assert_eq!(
        c.diffs,
        vec![("albumartistsort".into(), removed("Wrong, Sort; Also Wrong"))]
    );
    assert!(!c.saved);
    assert_eq!(
        fs::read(tmp.path().join("01 - Glass.flac")).unwrap(),
        before
    );
}

/// The spellings the scanner reads as the same field go too
/// (ALBUM_ARTIST, YEAR); ORIGINALDATE is another field and stays.
#[test]
fn removal_takes_the_scanner_aliases() {
    let tmp = tempfile::tempdir().unwrap();
    album(tmp.path());
    let events = run(
        tmp.path(),
        &[],
        &[],
        &album_removals(&["album_artist", "year"]),
        false,
    );
    assert_eq!(
        file_done(&events, "02 - Tin.flac").diffs,
        vec![
            ("albumartist".into(), removed("Velvet Quarry")),
            ("album_artist".into(), removed("Velvet Quarry")),
            ("date".into(), removed("2011-05-02")),
            ("year".into(), removed("2011")),
        ]
    );
    let c = comments(&tmp.path().join("02 - Tin.flac"));
    assert!(c.contains(&("ORIGINALDATE".into(), "2009".into())), "{c:?}");
    let meta = musaic_scan::scan_album(tmp.path(), musaic_scan::TrackOrder::Filename).unwrap();
    assert_eq!(meta.album_artist, None);
    // The scanner's year falls back to ORIGINALDATE, a different tag.
    assert_eq!(meta.year.as_deref(), Some("2009"));
}

/// A tag the file does not have: no diff, no write. A tag present with an
/// empty value is removed (and shown).
#[test]
fn absent_tag_is_no_change_and_empty_tag_is_removed() {
    let tmp = tempfile::tempdir().unwrap();
    album(tmp.path());
    let before = fs::read(tmp.path().join("01 - Glass.flac")).unwrap();
    let events = run(
        tmp.path(),
        &[],
        &[],
        &album_removals(&["album_sort"]),
        false,
    );
    let c = file_done(&events, "01 - Glass.flac");
    assert!(c.diffs.is_empty() && !c.saved);
    assert_eq!(
        fs::read(tmp.path().join("01 - Glass.flac")).unwrap(),
        before
    );

    // COMMENT is not a field apply knows; it stays whatever is asked.
    let events = run(tmp.path(), &[], &[], &album_removals(&["comment"]), false);
    assert!(file_done(&events, "01 - Glass.flac").diffs.is_empty());

    fs::write(
        tmp.path().join("01 - Glass.flac"),
        flac_bytes(&[("TITLE", "Glass"), ("artistsort", "")]),
    )
    .unwrap();
    let events = run(
        tmp.path(),
        &[],
        &[],
        &album_removals(&["artist_sort"]),
        false,
    );
    assert_eq!(
        file_done(&events, "01 - Glass.flac").diffs,
        vec![("artistsort".into(), removed(""))]
    );
    assert_eq!(keys(&tmp.path().join("01 - Glass.flac")), ["TITLE"]);
}

/// Per-track removal touches only its file; for one file a per-track
/// suggestion overrides an album-level removal of the same key, as
/// per-track suggestions override album-level ones.
#[test]
fn per_track_removal_and_precedence() {
    let tmp = tempfile::tempdir().unwrap();
    album(tmp.path());
    let removals = Removals {
        album: vec!["artist".into()],
        tracks: vec![("01 - Glass.flac".into(), vec!["title".into()])],
    };
    let events = run(
        tmp.path(),
        &[],
        &[("02 - Tin.flac", &[("artist", Some("Guest Lark"))])],
        &removals,
        false,
    );
    assert_eq!(
        file_done(&events, "01 - Glass.flac").diffs,
        vec![
            ("artist".into(), removed("Velvet Quarry")),
            ("title".into(), removed("Glass")),
        ]
    );
    assert_eq!(
        file_done(&events, "02 - Tin.flac").diffs,
        vec![(
            "artist".into(),
            Diff {
                old: "Velvet Quarry".into(),
                new: Some("Guest Lark".into())
            }
        )]
    );
    let Event::Start {
        track_change_count, ..
    } = &events[0]
    else {
        panic!()
    };
    assert_eq!(*track_change_count, 2);
    assert!(
        !keys(&tmp.path().join("01 - Glass.flac"))
            .iter()
            .any(|k| k == "TITLE" || k == "ARTIST")
    );
}

/// TRACKNUMBER cannot be removed (the scanner orders tracks by it), so a
/// removal of it is ignored here; the HTTP layer refuses it.
#[test]
fn tracknumber_is_not_removable() {
    let tmp = tempfile::tempdir().unwrap();
    album(tmp.path());
    let before = fs::read(tmp.path().join("01 - Glass.flac")).unwrap();
    let removals = Removals {
        album: vec!["tracknumber".into()],
        tracks: vec![("01 - Glass.flac".into(), vec!["tracknumber".into()])],
    };
    let events = run(tmp.path(), &[], &[], &removals, false);
    assert!(file_done(&events, "01 - Glass.flac").diffs.is_empty());
    assert_eq!(
        fs::read(tmp.path().join("01 - Glass.flac")).unwrap(),
        before
    );
    assert!(!musaic_tags::is_removable_track_field("tracknumber"));
    assert!(musaic_tags::is_removable_track_field("artist_sort"));
}

/// A suggestion never removes or blanks a tag: null, "" and whitespace-only
/// values are all "no change". (The Python app wrote the whitespace-only
/// one as an empty tag.)
#[test]
fn empty_and_whitespace_suggestions_change_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    album(tmp.path());
    let before = fs::read(tmp.path().join("01 - Glass.flac")).unwrap();
    let events = run(
        tmp.path(),
        &[
            ("genre", Some("")),
            ("album_artist_sort", Some("  \t ")),
            ("artist", None),
        ],
        &[(
            "01 - Glass.flac",
            &[("title", Some(" ")), ("artist", Some(""))],
        )],
        &Removals::default(),
        false,
    );
    let c = file_done(&events, "01 - Glass.flac");
    assert!(c.diffs.is_empty() && !c.saved, "{c:?}");
    assert_eq!(
        fs::read(tmp.path().join("01 - Glass.flac")).unwrap(),
        before
    );
    let Event::Start {
        suggestions_applied,
        cleared,
        track_change_count,
        ..
    } = &events[0]
    else {
        panic!()
    };
    assert!(suggestions_applied.is_empty() && cleared.is_empty());
    assert_eq!(*track_change_count, 0);
}
