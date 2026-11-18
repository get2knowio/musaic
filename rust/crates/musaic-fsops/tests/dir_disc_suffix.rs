//! The folder name the filename-mismatch check proposes for one disc of a
//! set. Each disc of a set must get its own name, whether or not the folder
//! name gave a total.

use std::path::Path;

use musaic_db::renames::RenamePlanRow;
use musaic_fsops::plan::check_dir;
use musaic_scan::parse_album_dir;

const TEMPLATE: &str = "{albumartist} - {album} [FLAC]";

/// The proposal for a folder named `dir`: `None` when the name already is it.
fn proposal(dir: &str) -> Option<String> {
    let p = parse_album_dir(dir);
    let row = RenamePlanRow {
        id: "x".into(),
        path: None,
        dir_name: Some(dir.into()),
        album_artist: None,
        artist: None,
        dir_artist: p.dir_artist,
        title: p.dir_album,
        year: None,
        is_compilation: p.is_compilation,
        disc_number: p.disc_number,
        disc_total: p.disc_total,
    };
    check_dir(Path::new(&format!("/lib/{dir}")), TEMPLATE, &row)
        .unwrap()
        .map(|r| r.new)
}

#[test]
fn every_disc_of_a_set_keeps_its_own_name() {
    let cases = [
        // Number only: the marker is replaced, not dropped.
        (
            "Artist - Album CD1 [FLAC]",
            Some("Artist - Album (disc 1) [FLAC]"),
        ),
        (
            "Artist - Album (CD 2) [FLAC]",
            Some("Artist - Album (disc 2) [FLAC]"),
        ),
        // Already in the rendered shape: no finding.
        ("Artist - Album (disc 2) [FLAC]", None),
        ("Artist - Album (disc 1 of 2) [FLAC]", None),
        // Number and total in another spelling.
        (
            "Artist - Album Disc 1 of 2 [FLAC]",
            Some("Artist - Album (disc 1 of 2) [FLAC]"),
        ),
        // A set of one needs no suffix.
        (
            "Artist - Album CD1 of 1 [FLAC]",
            Some("Artist - Album [FLAC]"),
        ),
        // No marker: unchanged.
        ("Artist - Album [FLAC]", None),
    ];
    for (dir, want) in cases {
        assert_eq!(proposal(dir).as_deref(), want, "{dir}");
    }
    assert_ne!(
        proposal("Artist - Album CD1 [FLAC]"),
        proposal("Artist - Album CD2 [FLAC]")
    );
}
