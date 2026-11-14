//! A scan (force) and cover requests may build the same album's thumbnail
//! at once. Every writer must succeed and no temp file may be left: with
//! the Python app's shared `<id>.jpg.tmp` the slower rename failed and the
//! cover route answered 404 for an album that has a cover.

use musaic_services::Json;
use musaic_services::covers::ensure_thumb;

#[test]
fn concurrent_writers_all_get_a_thumbnail() {
    let lib = musaic_testdata::synthetic_library();
    let album = lib.join("Aurora Fields - First Light [FLAC]");
    let tmp = tempfile::tempdir().unwrap();
    let thumbs = tmp.path().join("thumbs");
    let path = album.to_string_lossy().into_owned();
    let row = Json::obj([
        ("cover_file", Json::from("cover.jpg")),
        ("has_embedded_cover", Json::from(false)),
    ]);
    for _round in 0..20 {
        let failures: usize = std::thread::scope(|s| {
            let hs: Vec<_> = (0..8)
                .map(|_| s.spawn(|| ensure_thumb(&thumbs, "0123456789abcdef", &path, &row, true)))
                .collect();
            hs.into_iter()
                .map(|h| usize::from(h.join().unwrap().is_none()))
                .sum()
        });
        assert_eq!(failures, 0, "a writer lost the race");
    }
    let names: Vec<String> = std::fs::read_dir(&thumbs)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["0123456789abcdef.jpg"], "temp files left behind");
}
