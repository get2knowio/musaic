//! Windows: one folder, one resolved path (and so one album id), however the
//! path was spelled. Python's `Path.resolve()` asks the OS for the final
//! path there; `py_resolve` does the same.
#![cfg(windows)]

use std::path::{Path, PathBuf};

use musaic_scan::py_resolve;

fn lib() -> (tempfile::TempDir, PathBuf) {
    let td = tempfile::tempdir().unwrap();
    let lib = td
        .path()
        .join("Music Library")
        .join("Artist - Album [FLAC]");
    std::fs::create_dir_all(&lib).unwrap();
    (td, lib)
}

#[test]
fn case_and_verbatim_spellings_resolve_alike() {
    let (_td, album) = lib();
    let plain = py_resolve(&album);
    let s = plain.to_str().unwrap();
    assert!(!s.starts_with(r"\\?\"), "{s}");
    assert!(plain.is_dir(), "{s}");

    let upper = PathBuf::from(album.to_str().unwrap().to_uppercase());
    assert_eq!(py_resolve(&upper), plain, "upper-case spelling");
    let lower = PathBuf::from(album.to_str().unwrap().to_lowercase());
    assert_eq!(py_resolve(&lower), plain, "lower-case spelling");

    let verbatim = std::fs::canonicalize(&album).unwrap();
    assert!(verbatim.to_str().unwrap().starts_with(r"\\?\"));
    assert_eq!(py_resolve(&verbatim), plain, "\\\\?\\ spelling");

    let dotted = album.join("..").join("Artist - Album [FLAC]").join(".");
    assert_eq!(py_resolve(&dotted), plain, "dots");

    let slashed = PathBuf::from(album.to_str().unwrap().replace('\\', "/"));
    assert_eq!(py_resolve(&slashed), plain, "forward slashes");

    assert_eq!(
        musaic_core::album_id(&py_resolve(&upper).to_string_lossy()),
        musaic_core::album_id(s)
    );
}

#[test]
fn missing_tail_is_kept_as_written() {
    let (_td, album) = lib();
    let real = py_resolve(&album);
    let missing = PathBuf::from(album.to_str().unwrap().to_uppercase())
        .join("Not There")
        .join("x.flac");
    assert_eq!(py_resolve(&missing), real.join("Not There").join("x.flac"));
}

/// The runner's temp dir is usually spelled with an 8.3 name
/// (`C:\Users\RUNNER~1\...`); its resolved form is the long one.
#[test]
fn short_names_are_expanded() {
    let (td, _) = lib();
    let given = td.path().to_str().unwrap().to_owned();
    let resolved = py_resolve(Path::new(&given));
    let r = resolved.to_str().unwrap();
    if given.contains('~') {
        assert!(!r.contains('~'), "{given} -> {r}");
    } else {
        println!("note: temp dir {given} has no 8.3 component; only the round trip is checked");
    }
    assert_eq!(py_resolve(&resolved), resolved);
}

#[test]
fn tilde_with_a_backslash_is_the_home_dir() {
    let home = Path::new(r"C:\Users\someone");
    assert_eq!(
        musaic_core::config::expand_user(r"~\Music\FLAC", Some(home)),
        home.join("Music").join("FLAC")
    );
    assert_eq!(
        musaic_core::config::expand_user("~/Music", Some(home)),
        home.join("Music")
    );
}
