//! A directory the filesystem lists twice within one listing (seen on an SMB
//! share: the same folder name returned twice, same inode) is discovered
//! once, so its album is scanned and counted once. Invented folders and
//! empty `.flac` names only (discovery reads names, not audio).

use std::ffi::OsString;
use std::path::Path;

use musaic_services::scan::{discover_album_dirs, discover_album_dirs_with};

fn touch(p: &Path) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, b"").unwrap();
}

fn names(dir: &Path) -> Option<Vec<OsString>> {
    std::fs::read_dir(dir)
        .ok()
        .map(|rd| rd.flatten().map(|e| e.file_name()).collect())
}

#[test]
fn a_folder_listed_twice_is_discovered_once() {
    let t = tempfile::tempdir().unwrap();
    let root = musaic_testdata::canonical(t.path());
    touch(&root.join("Invented Band - First [FLAC]/01 - One.flac"));
    touch(&root.join("Invented Band - Second [FLAC]/01 - One.flac"));
    touch(&root.join("Nested/Other Band - Third [FLAC]/01 - One.flac"));
    let plain = discover_album_dirs(&root);
    assert_eq!(plain.len(), 3, "{plain:?}");

    // Every directory's listing repeats each of its entries, as the share
    // did for one folder (and a FLAC listed twice changes nothing either).
    let twice = |dir: &Path| {
        names(dir).map(|v| {
            let mut out = Vec::new();
            for n in v {
                out.push(n.clone());
                out.push(n);
            }
            out
        })
    };
    let found = discover_album_dirs_with(&root, &twice);
    assert_eq!(found, plain);

    // Only the first album's folder repeated, at the end of the root's
    // listing: still once, in first-seen order.
    let first = OsString::from("Invented Band - First [FLAC]");
    let root2 = root.clone();
    let one_repeat = move |dir: &Path| {
        names(dir).map(|mut v| {
            if dir == root2 {
                v.push(first.clone());
            }
            v
        })
    };
    let found = discover_album_dirs_with(&root, &one_repeat);
    assert_eq!(found, plain);
}
