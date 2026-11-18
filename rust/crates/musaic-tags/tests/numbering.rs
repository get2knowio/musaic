//! A renumber writes the file's numeric prefix through the caller's
//! numbering (the configured filename pattern's number part), with the
//! file's own disc. The default numbering is the track alone, as ever.

use std::fs;
use std::path::Path;

use musaic_tags::{
    Event, Numbering, Removals, apply_to_album_iter, apply_to_album_iter_numbered,
    default_numbering, proposed_filename_with,
};

/// Minimal FLAC (STREAMINFO + VORBIS_COMMENT + padding).
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

/// `{tracknumber:02}`: the track alone.
fn track_only(track: i64, _disc: Option<i64>) -> Option<String> {
    Some(format!("{track:02}"))
}

/// `{disc}-{tracknumber:02}` with a disc, `{tracknumber:02}` without one.
fn disc_track(track: i64, disc: Option<i64>) -> Option<String> {
    Some(match disc {
        Some(d) => format!("{d}-{track:02}"),
        None => format!("{track:02}"),
    })
}

/// A pattern whose number does not come first.
fn no_number_part(_track: i64, _disc: Option<i64>) -> Option<String> {
    None
}

fn renumber(dir: &Path, files: &[(&str, &str)], numbering: Numbering) -> Vec<(String, String)> {
    let tracks = files
        .iter()
        .map(|(f, n)| {
            (
                f.to_string(),
                vec![("tracknumber".to_string(), Some(n.to_string()))],
            )
        })
        .collect();
    let mut renamed = Vec::new();
    apply_to_album_iter_numbered(
        dir,
        &Vec::new(),
        &tracks,
        &Removals::default(),
        None,
        false,
        &mut |e| {
            if let Event::FileRenamed {
                old_filename,
                new_filename,
                ..
            } = e
            {
                renamed.push((old_filename, new_filename));
            }
        },
        numbering,
    )
    .unwrap();
    renamed
}

fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

fn pair(a: &str, b: &str) -> (String, String) {
    (a.to_owned(), b.to_owned())
}

#[test]
fn a_track_only_pattern_renumbers_a_disc_prefixed_name_with_the_track_alone() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(
        tmp.path().join("1-05 - Song.flac"),
        flac_bytes(&[("TRACKNUMBER", "5"), ("DISCNUMBER", "1")]),
    )
    .unwrap();
    let renamed = renumber(tmp.path(), &[("1-05 - Song.flac", "6")], &track_only);
    assert_eq!(renamed, [pair("1-05 - Song.flac", "06 - Song.flac")]);
    assert_eq!(names(tmp.path()), ["06 - Song.flac"]);
}

#[test]
fn a_disc_track_pattern_keeps_each_files_own_disc() {
    let tmp = tempfile::tempdir().unwrap();
    for (name, disc) in [("1-05 - A.flac", "1"), ("2-05 - B.flac", "2/2")] {
        fs::write(
            tmp.path().join(name),
            flac_bytes(&[("TRACKNUMBER", "5"), ("DISCNUMBER", disc)]),
        )
        .unwrap();
    }
    let mut renamed = renumber(
        tmp.path(),
        &[("1-05 - A.flac", "6"), ("2-05 - B.flac", "6")],
        &disc_track,
    );
    renamed.sort();
    assert_eq!(
        renamed,
        [
            pair("1-05 - A.flac", "1-06 - A.flac"),
            pair("2-05 - B.flac", "2-06 - B.flac")
        ]
    );
}

#[test]
fn a_disc_taken_from_a_disc_prefixed_tracknumber_is_used_when_there_is_no_disc_tag() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(
        tmp.path().join("2-05 - B.flac"),
        flac_bytes(&[("TRACKNUMBER", "2-05")]),
    )
    .unwrap();
    let renamed = renumber(tmp.path(), &[("2-05 - B.flac", "6")], &disc_track);
    assert_eq!(renamed, [pair("2-05 - B.flac", "2-06 - B.flac")]);
}

#[test]
fn a_file_with_no_disc_gets_the_numbering_without_one() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(
        tmp.path().join("05 - A.flac"),
        flac_bytes(&[("TRACKNUMBER", "5")]),
    )
    .unwrap();
    let renamed = renumber(tmp.path(), &[("05 - A.flac", "6")], &disc_track);
    assert_eq!(renamed, [pair("05 - A.flac", "06 - A.flac")]);
}

#[test]
fn a_pattern_without_a_number_part_renames_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(
        tmp.path().join("05 - A.flac"),
        flac_bytes(&[("TRACKNUMBER", "5")]),
    )
    .unwrap();
    let renamed = renumber(tmp.path(), &[("05 - A.flac", "6")], &no_number_part);
    assert!(renamed.is_empty());
    assert_eq!(names(tmp.path()), ["05 - A.flac"]);
}

#[test]
fn only_the_prefix_changes_whatever_the_rest_of_the_name_says() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(
        tmp.path().join("1-05 Whatever you like.flac"),
        flac_bytes(&[("TRACKNUMBER", "5"), ("DISCNUMBER", "1")]),
    )
    .unwrap();
    let renamed = renumber(
        tmp.path(),
        &[("1-05 Whatever you like.flac", "6")],
        &disc_track,
    );
    assert_eq!(
        renamed,
        [pair(
            "1-05 Whatever you like.flac",
            "1-06 Whatever you like.flac"
        )]
    );
}

#[test]
fn a_name_without_a_numeric_prefix_is_left_alone_whatever_the_numbering() {
    assert_eq!(
        proposed_filename_with("Song.flac", "5", Some(1), &disc_track),
        None
    );
}

#[test]
fn the_default_numbering_is_the_track_alone() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(
        tmp.path().join("2-05 - B.flac"),
        flac_bytes(&[("TRACKNUMBER", "5"), ("DISCNUMBER", "2")]),
    )
    .unwrap();
    let tracks = vec![(
        "2-05 - B.flac".to_owned(),
        vec![("tracknumber".to_owned(), Some("6".to_owned()))],
    )];
    let mut renamed = Vec::new();
    apply_to_album_iter(
        tmp.path(),
        &Vec::new(),
        &tracks,
        &Removals::default(),
        None,
        false,
        &mut |e| {
            if let Event::FileRenamed { new_filename, .. } = e {
                renamed.push(new_filename);
            }
        },
    )
    .unwrap();
    assert_eq!(renamed, ["06 - B.flac"]);
    assert_eq!(default_numbering(6, Some(2)).as_deref(), Some("06"));
}
