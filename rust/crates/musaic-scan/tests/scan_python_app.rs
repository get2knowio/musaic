//! The scanner compared with goldens recorded from the Python app.

use std::path::{Path, PathBuf};

use musaic_scan::{
    Album, TrackOrder, parse_album_dir, parse_tracknum, py_resolve, scan_album, scan_library,
};
use serde_json::{Value, json};

fn golden(name: &str) -> Value {
    musaic_testdata::golden(name)
}

fn library() -> PathBuf {
    musaic_testdata::synthetic_library()
}

/// The album as the golden stores it: no id/path, sample track by file name.
fn as_golden(a: &Album) -> Value {
    let mut v = serde_json::to_value(a).unwrap();
    let o = v.as_object_mut().unwrap();
    o.remove("id");
    o.remove("path");
    let sample = Path::new(a.sample_track.as_str())
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    o.insert("sample_track".into(), json!(sample));
    v
}

/// Directory-name convention, 40 names through the Python app's parser.
#[test]
fn directory_names_parse_like_the_python_app() {
    let g = musaic_testdata::json("golden/dir-parse.json");
    let cases = g.value["cases"].as_array().unwrap();
    // 4 of the 40 names are real-library directories (private half).
    if g.private {
        assert!(cases.len() >= 40);
    } else {
        assert!(cases.len() >= 36);
        musaic_testdata::skip_private_cases();
    }
    for c in cases {
        let name = c["name"].as_str().unwrap();
        let p = parse_album_dir(name);
        let have = json!({
            "matched": p.matched, "dir_artist": p.dir_artist, "dir_album": p.dir_album,
            "is_compilation": p.is_compilation, "disc_number": p.disc_number, "disc_total": p.disc_total,
        });
        let mut want = c.clone();
        want.as_object_mut().unwrap().remove("name");
        assert_eq!(have, want, "{name:?}");
    }
}

/// Track-number tag values through the Python app's parser.
#[test]
fn track_numbers_parse_like_the_python_app() {
    let g = golden("tracknum.json");
    let cases = g["cases"].as_array().unwrap();
    assert!(cases.len() >= 37);
    for c in cases {
        let raw = c["raw"].as_str();
        assert_eq!(json!(parse_tracknum(raw)), c["value"], "{raw:?}");
    }
}

/// In the Python app's own track order, every album row field
/// equals what the Python scanner produced for the synthetic library.
#[test]
fn filename_order_scan_equals_the_python_app() {
    let g = golden("scan-synthetic.json");
    let want = g["albums"].as_array().unwrap();
    let have = scan_library(&library(), TrackOrder::Filename);
    assert_eq!(have.len(), want.len());
    assert_eq!(want.len(), 13);
    for a in &have {
        let w = want
            .iter()
            .find(|w| w["dir_name"] == json!(a.dir_name))
            .unwrap_or_else(|| panic!("{}", a.dir_name));
        assert_eq!(&as_golden(a), w, "{}", a.dir_name);
        // The id is the hash of the path we report.
        assert_eq!(a.id, musaic_core::album_id(&a.path));
        assert_eq!(
            a.path,
            py_resolve(&library().join(&a.dir_name)).to_string_lossy()
        );
    }
}

/// The album row, with tracks in (disc, track) order (a deliberate change
/// from the Python app's file-name order): it equals the Python app's wherever the two orders agree, and differs in the decided way
/// where they do not.
#[test]
fn disc_track_order_differs_only_where_decided() {
    let g = golden("scan-synthetic.json");
    let want = g["albums"].as_array().unwrap();
    let have = scan_library(&library(), TrackOrder::DiscTrack);
    let mut reordered = Vec::new();
    for a in &have {
        let w = want
            .iter()
            .find(|w| w["dir_name"] == json!(a.dir_name))
            .unwrap();
        let python_numbers: Vec<Option<i64>> = w["track_numbers"]
            .as_array()
            .unwrap()
            .iter()
            .map(Value::as_i64)
            .collect();
        let already_ordered = python_numbers.windows(2).all(|p| p[0] <= p[1])
            && python_numbers.iter().all(Option::is_some);
        if already_ordered {
            assert_eq!(&as_golden(a), w, "{}", a.dir_name);
            continue;
        }
        reordered.push(a.dir_name.clone());
        // Same tracks, now in track order.
        let mut sorted = python_numbers.clone();
        sorted.sort();
        assert_eq!(a.track_numbers, sorted, "{}", a.dir_name);
        // The sample is the lowest-numbered track: the file the Python app listed
        // at the position of its smallest track number.
        let names: Vec<String> = musaic_scan::list_flacs(&library().join(&a.dir_name))
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        let first = python_numbers
            .iter()
            .enumerate()
            .min_by_key(|(_, n)| **n)
            .unwrap()
            .0;
        assert!(
            a.sample_track.ends_with(&names[first]),
            "{}: {}",
            a.dir_name,
            a.sample_track
        );
        // Per-track artist credits are the Python app's, permuted into track order.
        let mut order: Vec<usize> = (0..names.len()).collect();
        order.sort_by_key(|i| python_numbers[*i]);
        let want_artists: Vec<Value> = order
            .iter()
            .filter_map(|i| {
                w["track_artists"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|t| t["filename"] == json!(names[*i]))
            })
            .cloned()
            .collect();
        assert_eq!(
            serde_json::to_value(&a.track_artists).unwrap(),
            json!(want_artists),
            "{}",
            a.dir_name
        );
        // Order-independent fields are untouched.
        for k in [
            "track_count",
            "total_seconds",
            "cover_file",
            "dir_album",
            "dir_artist",
            "is_compilation",
            "dir_mtime",
            "title",
        ] {
            assert_eq!(as_golden(a)[k], w[k], "{} {k}", a.dir_name);
        }
        // The genre union has the same members; only its order may change.
        let split = |v: &Value| {
            let mut g: Vec<String> = v
                .as_str()
                .unwrap_or("")
                .split("; ")
                .map(str::to_string)
                .collect();
            g.sort();
            g
        };
        assert_eq!(
            split(&as_golden(a)["genre"]),
            split(&w["genre"]),
            "{}",
            a.dir_name
        );
    }
    // The fixture library has exactly one album whose file order is not its track order.
    assert_eq!(reordered, ["Various Artists - Night Mix [FLAC]"]);
}

// ---------- (disc, track) order tie-breaks, on hand-built metadata-only FLAC files ----------

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

/// Missing disc number = disc 1; unnumbered tracks last, by file name;
/// equal positions keep file name order.
#[test]
fn disc_track_order_tie_breaks() {
    let dir = tempfile::tempdir().unwrap();
    let album = dir.path().join("Artist - Album [FLAC]");
    std::fs::create_dir(&album).unwrap();
    write_flac(
        &album.join("a.flac"),
        &[("TITLE", "no number"), ("ALBUM", "from a")],
    );
    write_flac(
        &album.join("b.flac"),
        &[
            ("TRACKNUMBER", "2"),
            ("DISCNUMBER", "2/2"),
            ("ALBUM", "from b"),
        ],
    );
    write_flac(
        &album.join("c.flac"),
        &[("TRACKNUMBER", "2"), ("ALBUM", "from c"), ("ARTIST", "C")],
    );
    write_flac(
        &album.join("d.flac"),
        &[("tracknumber", "1/9"), ("Album", "from d"), ("artist", "D")],
    );
    write_flac(
        &album.join("e.flac"),
        &[("TRACKNUMBER", "2"), ("DISCNUMBER", "1"), ("ARTIST", "E")],
    );
    write_flac(&album.join("f.flac"), &[("TRACKNUMBER", "x")]);

    let a = scan_album(&album, TrackOrder::DiscTrack).unwrap();
    // disc 1: d(1), c(2), e(2), then unnumbered a, f; disc 2: b(2)
    assert_eq!(
        a.track_numbers,
        [Some(1), Some(2), Some(2), None, None, Some(2)]
    );
    assert!(a.sample_track.ends_with("d.flac"));
    assert_eq!(a.artist.as_deref(), Some("D"));
    let credited: Vec<&str> = a
        .track_artists
        .iter()
        .map(|t| t.filename.as_str())
        .collect();
    assert_eq!(credited, ["d.flac", "c.flac", "e.flac"]);
    assert_eq!(a.title.as_deref(), Some("Album")); // directory name wins over the ALBUM tag
    assert_eq!(a.total_seconds, 60.0);

    let o = scan_album(&album, TrackOrder::Filename).unwrap();
    assert_eq!(
        o.track_numbers,
        [None, Some(2), Some(2), Some(1), Some(2), None]
    );
    assert!(o.sample_track.ends_with("a.flac"));
    assert_eq!(o.artist, None);
}

/// One listing rule. AppleDouble files, other
/// extensions and same-inode duplicates are dropped; dot-directories are not entered.
#[cfg(unix)]
#[test]
fn listing_skips_junk_and_collapses_hard_links() {
    let dir = tempfile::tempdir().unwrap();
    let album = dir.path().join("lib/Artist - Album [FLAC]");
    std::fs::create_dir_all(&album).unwrap();
    write_flac(&album.join("02 b.FLAC"), &[("TRACKNUMBER", "2")]);
    write_flac(&album.join("01 a.flac"), &[("TRACKNUMBER", "1")]);
    std::fs::hard_link(
        album.join("01 a.flac"),
        album.join("01 a (same inode).flac"),
    )
    .unwrap();
    std::fs::write(album.join("._01 a.flac"), b"appledouble").unwrap();
    std::fs::write(album.join("notes.txt"), b"x").unwrap();
    std::fs::write(album.join(".flac"), b"x").unwrap();
    std::fs::create_dir(album.join("sub.flac")).unwrap();
    let hidden = dir.path().join("lib/.hidden/Artist - Other [FLAC]");
    std::fs::create_dir_all(&hidden).unwrap();
    write_flac(&hidden.join("01.flac"), &[]);
    let junk_only = dir.path().join("lib/Junk Only");
    std::fs::create_dir(&junk_only).unwrap();
    std::fs::write(junk_only.join("._01.flac"), b"appledouble").unwrap();

    let names: Vec<String> = musaic_scan::list_flacs(&album)
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["01 a (same inode).flac", "02 b.FLAC"]);

    let albums = scan_library(&dir.path().join("lib"), TrackOrder::DiscTrack);
    assert_eq!(albums.len(), 1);
    assert_eq!(albums[0].track_count, 2);
}
