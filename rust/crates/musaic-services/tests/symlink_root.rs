//! A symlinked library root: album ids and paths when the
//! library is reached through a symlink, replayed against
//! testdata/golden/scan-symlink-root.json, which the Python generator
//! produced by running the Python app's `scan_albums`, `discover_album_dirs` and
//! `incremental_scan` over the same symlink layout.
//!
//! Paths in the golden are relative to the recording checkout, whose library
//! was `testdata/synthetic-library/library`; the absolute root (private half) only matters for
//! the ids, which hash the absolute resolved path: with the private half
//! the ids are checked against that root, otherwise as sha1(path)[:16] of
//! the paths seen here.
//!
//! Non-UTF-8 names (the other malformed path case) cannot be created
//! on APFS, so the Python app has nothing to record for them here.

// Creates Unix symlinks; Windows builds this file out.
#![cfg(unix)]

use std::path::{Path, PathBuf};

use musaic_scan::{TrackOrder, py_resolve, scan_album, scan_library};
use musaic_services::scan::discover_album_dirs;

/// Where the recording checkout had the library, relative to its root.
const RECORDED_LIBRARY: &str = "fixtures/library";

fn golden() -> musaic_testdata::Json {
    musaic_testdata::json("golden/scan-symlink-root.json")
}

/// The generator's layout, rebuilt in a temp dir: `abs-link` -> library,
/// `chain` -> `abs-link` (relative), `real-root/linked-album` -> one album.
fn build_tree(dir: &Path, linked_album: &str) -> Vec<(String, PathBuf)> {
    let lib = musaic_testdata::synthetic_library();
    let abs_link = dir.join("abs-link");
    std::os::unix::fs::symlink(&lib, &abs_link).unwrap();
    let chain = dir.join("chain");
    std::os::unix::fs::symlink("abs-link", &chain).unwrap();
    let real_root = dir.join("real-root");
    std::fs::create_dir(&real_root).unwrap();
    std::os::unix::fs::symlink(lib.join(linked_album), real_root.join("linked-album")).unwrap();
    vec![
        ("abs-link".into(), abs_link),
        ("chain".into(), chain),
        ("real-root".into(), real_root),
    ]
}

/// As the golden stores it: the library-relative part under the recording
/// checkout's library directory.
fn rel(p: &str) -> String {
    let lib = musaic_testdata::synthetic_library().canonicalize().unwrap();
    let under = Path::new(p)
        .strip_prefix(&lib)
        .unwrap_or_else(|_| panic!("{p} is not under the synthetic library"));
    if under.as_os_str().is_empty() {
        RECORDED_LIBRARY.to_string()
    } else {
        format!("{RECORDED_LIBRARY}/{}", under.to_string_lossy())
    }
}

#[test]
fn symlinked_library_root_gives_the_python_app_ids() {
    let gj = golden();
    let g = &gj.value;
    // Private half: the recording checkout's absolute root (the ids hash it).
    let python_root = g["python_repo_root"].as_str().map(str::to_string);
    let tmp = tempfile::tempdir().unwrap();
    let roots = build_tree(tmp.path(), g["linked_album"].as_str().unwrap());
    let want_roots = g["roots"].as_array().unwrap();
    assert_eq!(want_roots.len(), roots.len());
    let mut exact = 0;
    for ((name, root), want) in roots.iter().zip(want_roots) {
        assert_eq!(want["root"].as_str().unwrap(), name);
        let want_albums = want["scan_albums"].as_array().unwrap();

        // scanner.scan_albums(root)
        let scanned = scan_library(root, TrackOrder::Filename);
        assert_eq!(scanned.len(), want_albums.len(), "{name}");
        for (a, w) in scanned.iter().zip(want_albums) {
            assert_eq!(a.dir_name, w["dir_name"].as_str().unwrap(), "{name}");
            assert_eq!(rel(&a.path), w["path"].as_str().unwrap(), "{name}");
            assert_eq!(rel(&a.sample_track), w["sample_track"].as_str().unwrap());
            assert_eq!(a.id, musaic_core::album_id(&a.path), "{name}: {}", a.path);
            if let Some(root) = &python_root {
                let python_path = format!("{root}/{}", w["path"].as_str().unwrap());
                assert_eq!(
                    musaic_core::album_id(&python_path),
                    w["id"].as_str().unwrap(),
                    "{name}: {python_path}"
                );
                exact += 1;
            }
        }

        // scanner.discover_album_dirs(root), then one album scan per
        // candidate as the incremental scan does it.
        let mut discovered = discover_album_dirs(root);
        discovered.sort();
        let want_discovered: Vec<&str> = want["discover_album_dirs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        let have: Vec<String> = discovered.iter().map(|p| rel(p)).collect();
        assert_eq!(have, want_discovered, "{name}");
        assert_eq!(
            discovered.len(),
            want["incremental_discovered"]["total"].as_u64().unwrap() as usize
        );
        for (p, w) in discovered.iter().zip(want_albums) {
            let a = scan_album(Path::new(p), TrackOrder::Filename).unwrap();
            assert_eq!(&a.path, p);
            assert_eq!(rel(p), w["path"].as_str().unwrap(), "{name}: {p}");
        }

        // incremental_scan's "discovered" library_dir is the resolved root.
        let lib_dir = py_resolve(root).to_string_lossy().into_owned();
        if want["root_is_symlink"].as_bool().unwrap() {
            assert_eq!(
                rel(&lib_dir),
                want["incremental_discovered"]["library_dir"]
                    .as_str()
                    .unwrap()
            );
        } else {
            assert_eq!(
                lib_dir,
                py_resolve(&tmp.path().join(name)).to_string_lossy()
            );
        }
    }
    if python_root.is_some() {
        assert_eq!(exact, 26, "13 albums under each of the two symlinked roots");
    } else {
        musaic_testdata::skip_private_cases();
    }
}
