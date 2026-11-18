//! The per-file disc numbers and the tag disc total a scan stores
//! (`Album::track_discs`, `Album::tag_disc_total`), on hand-built
//! metadata-only FLAC files.

use std::path::Path;

use musaic_scan::{Album, TrackOrder, scan_album};

/// A FLAC file with STREAMINFO (44.1 kHz stereo 16-bit, 10 s) and the given
/// Vorbis comments, and no audio frames. The scanner reads metadata only.
fn write_flac(path: &Path, tags: &[(&str, &str)]) {
    let mut b = b"fLaC".to_vec();
    let mut si = vec![0u8; 34];
    si[0..2].copy_from_slice(&4096u16.to_be_bytes());
    si[2..4].copy_from_slice(&4096u16.to_be_bytes());
    let packed: u64 = (44100u64 << 44) | (1u64 << 41) | (15u64 << 36) | 441_000;
    si[10..18].copy_from_slice(&packed.to_be_bytes());
    b.push(0x00);
    b.extend_from_slice(&(si.len() as u32).to_be_bytes()[1..]);
    b.extend_from_slice(&si);
    let mut vc = Vec::new();
    vc.extend_from_slice(&4u32.to_le_bytes());
    vc.extend_from_slice(b"test");
    vc.extend_from_slice(&(tags.len() as u32).to_le_bytes());
    for (k, v) in tags {
        let e = format!("{k}={v}");
        vc.extend_from_slice(&(e.len() as u32).to_le_bytes());
        vc.extend_from_slice(e.as_bytes());
    }
    b.push(0x84);
    b.extend_from_slice(&(vc.len() as u32).to_be_bytes()[1..]);
    b.extend_from_slice(&vc);
    std::fs::write(path, b).unwrap();
}

/// An album folder whose files (named `01.flac`, `02.flac`, ...) carry the
/// given extra tags; scanned in the app's (disc, track) order.
fn scan(files: &[&[(&str, &str)]]) -> Album {
    let dir = tempfile::tempdir().unwrap();
    let album = dir.path().join("Artist - Album [FLAC]");
    std::fs::create_dir(&album).unwrap();
    for (i, tags) in files.iter().enumerate() {
        let mut all = vec![("ALBUM", "Album"), ("ARTIST", "Artist")];
        all.extend_from_slice(tags);
        write_flac(&album.join(format!("{:02}.flac", i + 1)), &all);
    }
    scan_album(&album, TrackOrder::DiscTrack).unwrap()
}

#[test]
fn discnumber_tags_give_one_disc_per_file_in_track_order() {
    let a = scan(&[
        &[("DISCNUMBER", "1/2"), ("TRACKNUMBER", "1")],
        &[("DISCNUMBER", "1/2"), ("TRACKNUMBER", "2")],
        &[("DISCNUMBER", "2/2"), ("TRACKNUMBER", "1")],
        &[("DISCNUMBER", "2/2"), ("TRACKNUMBER", "2")],
    ]);
    assert_eq!(a.track_numbers, [Some(1), Some(2), Some(1), Some(2)]);
    assert_eq!(
        a.track_discs,
        Some(vec![Some(1), Some(1), Some(2), Some(2)])
    );
    assert_eq!(a.tag_disc_total, Some(2));
}

/// The lists stay parallel when the (disc, track) order is not file order.
#[test]
fn the_disc_list_follows_the_track_list_order() {
    let a = scan(&[
        &[("DISCNUMBER", "2"), ("TRACKNUMBER", "1")],
        &[("DISCNUMBER", "1"), ("TRACKNUMBER", "1")],
        &[("DISCNUMBER", "1"), ("TRACKNUMBER", "2")],
    ]);
    assert_eq!(a.track_numbers, [Some(1), Some(2), Some(1)]);
    assert_eq!(a.track_discs, Some(vec![Some(1), Some(1), Some(2)]));
    assert_eq!(a.tag_disc_total, None);
}

/// A disc-prefixed TRACKNUMBER gives the disc; the track number stored is
/// only its track part.
#[test]
fn a_disc_prefixed_tracknumber_gives_the_disc() {
    let a = scan(&[&[("TRACKNUMBER", "1-05")], &[("TRACKNUMBER", "2-05")]]);
    assert_eq!(a.track_numbers, [Some(5), Some(5)]);
    assert_eq!(a.track_discs, Some(vec![Some(1), Some(2)]));
    assert_eq!(a.tag_disc_total, None);
}

/// No disc data: a list of nulls (read, nothing found), not "not read".
#[test]
fn no_disc_data_is_a_list_of_nulls() {
    let a = scan(&[&[("TRACKNUMBER", "1")], &[("TRACKNUMBER", "2")]]);
    assert_eq!(a.track_discs, Some(vec![None, None]));
    assert_eq!(a.tag_disc_total, None);
}

/// The total is the highest of DISCTOTAL, TOTALDISCS and DISCNUMBER's
/// "n/M" over every file.
#[test]
fn the_total_is_the_highest_any_file_names() {
    let a = scan(&[
        &[
            ("DISCNUMBER", "1"),
            ("DISCTOTAL", "2"),
            ("TRACKNUMBER", "1"),
        ],
        &[
            ("DISCNUMBER", "1"),
            ("TOTALDISCS", "4"),
            ("TRACKNUMBER", "2"),
        ],
        &[("DISCNUMBER", "1/3"), ("TRACKNUMBER", "3")],
    ]);
    assert_eq!(a.track_discs, Some(vec![Some(1), Some(1), Some(1)]));
    assert_eq!(a.tag_disc_total, Some(4));
    for (tags, want) in [
        (&[("DISCTOTAL", "2")][..], Some(2)),
        (&[("TOTALDISCS", "3")][..], Some(3)),
        (&[("DISCNUMBER", "1/3")][..], Some(3)),
        (&[("DISCTOTAL", "x")][..], None),
        (&[("DISCTOTAL", "1")][..], Some(1)),
    ] {
        assert_eq!(scan(&[tags]).tag_disc_total, want, "{tags:?}");
    }
}

/// An unreadable file is kept in the lists as an unknown, as its track
/// number already is.
#[test]
fn an_unreadable_file_has_no_disc() {
    let dir = tempfile::tempdir().unwrap();
    let album = dir.path().join("Artist - Album [FLAC]");
    std::fs::create_dir(&album).unwrap();
    write_flac(
        &album.join("01.flac"),
        &[("DISCNUMBER", "1/2"), ("TRACKNUMBER", "1")],
    );
    std::fs::write(album.join("02.flac"), b"not a flac file").unwrap();
    let a = scan_album(&album, TrackOrder::DiscTrack).unwrap();
    assert_eq!(a.track_count, 2);
    assert_eq!(a.track_numbers, [Some(1), None]);
    assert_eq!(a.track_discs, Some(vec![Some(1), None]));
    assert_eq!(a.tag_disc_total, Some(2));
}

/// The scanner's album dict (what the API serves) does not carry them.
#[test]
fn the_serialized_album_leaves_them_out() {
    let a = scan(&[&[("DISCNUMBER", "1/2"), ("TRACKNUMBER", "1")]]);
    let v = serde_json::to_value(&a).unwrap();
    assert!(v.get("track_discs").is_none(), "{v}");
    assert!(v.get("tag_disc_total").is_none(), "{v}");
}
