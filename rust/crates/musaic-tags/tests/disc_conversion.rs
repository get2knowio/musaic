//! The tag writes of a multi-disc conversion
//! ([`musaic_tags::apply_disc_conversion`]) and the fence around them: the
//! general apply ([`apply_to_album_iter`]) can neither set nor remove
//! TRACKTOTAL, TOTALTRACKS or the disc tags, whatever it is asked.
//!
//! Invented albums built from minimal FLACs (STREAMINFO + VORBIS_COMMENT +
//! padding, no audio frames).

use std::fs;
use std::path::Path;

use musaic_tags::{
    ConvertedNumbers, DiscConversion, Event, FlacFile, Removals, apply_disc_conversion,
    apply_to_album_iter, is_removable_album_field, is_removable_track_field,
};

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

const FILE: &str = "2-01 - Ember.flac";

fn album(dir: &Path) {
    fs::write(
        dir.join(FILE),
        flac_bytes(&[
            ("TITLE", "Ember"),
            ("ALBUM", "Lanterns"),
            ("TRACKNUMBER", "2-01"),
            ("TRACKTOTAL", "4"),
            ("TOTALTRACKS", "4"),
            ("DiscNumber", "2"),
            ("discnumber", "2/2"),
            ("DISCTOTAL", "2"),
            ("TOTALDISCS", "2"),
        ]),
    )
    .unwrap();
}

/// Every comment of `name` exactly as the file has it: keys in their own
/// spelling, in file order.
fn raw_comments(dir: &Path, name: &str) -> Vec<(String, String)> {
    FlacFile::open(&dir.join(name))
        .unwrap()
        .tags()
        .unwrap()
        .comments
        .clone()
}

fn pairs(v: &[(&str, &str)]) -> Vec<(String, String)> {
    v.iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn convert(dir: &Path, name: &str, numbers: ConvertedNumbers) {
    let conversion = DiscConversion {
        files: vec![(name.to_string(), numbers)],
    };
    apply_disc_conversion(dir, &conversion, &mut |_| {}).unwrap();
}

/// Every comment of the file, in order.
fn comments(dir: &Path) -> Vec<(String, String)> {
    let f = FlacFile::open(&dir.join(FILE)).unwrap();
    let mut v: Vec<(String, String)> = Vec::new();
    for k in [
        "TITLE",
        "ALBUM",
        "TRACKNUMBER",
        "TRACKTOTAL",
        "TOTALTRACKS",
        "DISCNUMBER",
        "DISCTOTAL",
        "TOTALDISCS",
    ] {
        for val in f.tags().unwrap().values(k) {
            v.push((k.to_string(), val.to_string()));
        }
    }
    v
}

#[test]
fn a_conversion_writes_exactly_its_numbers_and_removes_every_disc_tag() {
    let tmp = tempfile::tempdir().unwrap();
    album(tmp.path());
    let conversion = DiscConversion {
        files: vec![(
            FILE.to_string(),
            ConvertedNumbers {
                tracknumber: "5".into(),
                tracktotal: Some("8".into()),
                totaltracks: Some("8".into()),
            },
        )],
    };
    let mut renamed = 0;
    let mut saved = 0;
    apply_disc_conversion(tmp.path(), &conversion, &mut |e| match e {
        Event::FileRenamed { .. } => renamed += 1,
        Event::FileDone(c) if c.saved => saved += 1,
        _ => {}
    })
    .unwrap();
    assert_eq!(saved, 1);
    // No rename: the conversion renames through the rename code.
    assert_eq!(renamed, 0);
    assert!(tmp.path().join(FILE).exists());
    assert_eq!(
        comments(tmp.path()),
        [
            ("TITLE".to_string(), "Ember".to_string()),
            ("ALBUM".to_string(), "Lanterns".to_string()),
            ("TRACKNUMBER".to_string(), "5".to_string()),
            ("TRACKTOTAL".to_string(), "8".to_string()),
            ("TOTALTRACKS".to_string(), "8".to_string()),
        ]
    );
}

/// A total is written only where the conversion names one: a file without
/// TRACKTOTAL or TOTALTRACKS gets neither, and one with only one of them
/// keeps only that one.
#[test]
fn a_file_without_a_track_total_gets_none() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    fs::write(
        dir.join("a.flac"),
        flac_bytes(&[("TITLE", "A"), ("TRACKNUMBER", "1"), ("DISCNUMBER", "2")]),
    )
    .unwrap();
    fs::write(
        dir.join("b.flac"),
        flac_bytes(&[("TOTALTRACKS", "4"), ("TRACKNUMBER", "2"), ("TITLE", "B")]),
    )
    .unwrap();
    convert(
        dir,
        "a.flac",
        ConvertedNumbers {
            tracknumber: "5".into(),
            tracktotal: None,
            totaltracks: None,
        },
    );
    convert(
        dir,
        "b.flac",
        ConvertedNumbers {
            tracknumber: "6".into(),
            tracktotal: None,
            totaltracks: Some("8".into()),
        },
    );
    assert_eq!(
        raw_comments(dir, "a.flac"),
        pairs(&[("TITLE", "A"), ("TRACKNUMBER", "5")])
    );
    assert_eq!(
        raw_comments(dir, "b.flac"),
        pairs(&[("TOTALTRACKS", "8"), ("TRACKNUMBER", "6"), ("TITLE", "B")])
    );
}

/// A rewritten comment keeps the file's spelling of its key and its place
/// among the others; a key written twice is left once.
#[test]
fn a_rewritten_key_keeps_its_spelling_and_its_place() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    fs::write(
        dir.join("a.flac"),
        flac_bytes(&[
            ("TRACKNUMBER", "3"),
            ("Title", "A"),
            ("TrackTotal", "4"),
            ("tracktotal", "4"),
            ("totaltracks", "4"),
            ("ALBUM", "Lanterns"),
        ]),
    )
    .unwrap();
    fs::write(
        dir.join("b.flac"),
        flac_bytes(&[("tracknumber", "2/4"), ("TITLE", "B")]),
    )
    .unwrap();
    convert(
        dir,
        "a.flac",
        ConvertedNumbers {
            tracknumber: "7".into(),
            tracktotal: Some("8".into()),
            totaltracks: Some("8".into()),
        },
    );
    convert(
        dir,
        "b.flac",
        ConvertedNumbers {
            tracknumber: "6/8".into(),
            tracktotal: None,
            totaltracks: None,
        },
    );
    assert_eq!(
        raw_comments(dir, "a.flac"),
        pairs(&[
            ("TRACKNUMBER", "7"),
            ("Title", "A"),
            ("TrackTotal", "8"),
            ("totaltracks", "8"),
            ("ALBUM", "Lanterns"),
        ])
    );
    assert_eq!(
        raw_comments(dir, "b.flac"),
        pairs(&[("tracknumber", "6/8"), ("TITLE", "B")])
    );
}

#[test]
fn the_general_apply_cannot_set_or_remove_the_disc_fields() {
    let tmp = tempfile::tempdir().unwrap();
    album(tmp.path());
    let before = comments(tmp.path());
    let keys = [
        "tracktotal",
        "totaltracks",
        "discnumber",
        "disctotal",
        "totaldiscs",
    ];
    for k in keys {
        assert!(!is_removable_album_field(k), "{k}");
        assert!(!is_removable_track_field(k), "{k}");
    }
    let suggestions: Vec<(String, Option<String>)> = keys
        .iter()
        .map(|k| (k.to_string(), Some("9".to_string())))
        .collect();
    let removals = Removals {
        album: keys.iter().map(|k| k.to_string()).collect(),
        tracks: vec![(
            FILE.to_string(),
            keys.iter().map(|k| k.to_string()).collect(),
        )],
    };
    apply_to_album_iter(
        tmp.path(),
        &suggestions,
        &vec![(FILE.to_string(), suggestions.clone())],
        &removals,
        None,
        false,
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(comments(tmp.path()), before);
}
