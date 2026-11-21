//! SC-005: the checked-in catalog is the generated one, and drift is caught.

use musaic_cli::catalog_cmd::{self, Mode};

fn docs() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../docs")
}

#[test]
fn checked_in_catalog_matches_the_code() {
    let r = catalog_cmd::run(Mode::Check, &docs()).unwrap();
    assert!(
        r.is_clean(),
        "{:?} differ; run `cargo run -p musaic-cli -- catalog --write --dir ../docs`",
        r.differing
    );
}

#[test]
fn a_modified_copy_fails_the_check_and_write_repairs_it() {
    let tmp = tempfile::tempdir().unwrap();
    for f in [catalog_cmd::JSON_FILE, catalog_cmd::MD_FILE] {
        std::fs::copy(docs().join(f), tmp.path().join(f)).unwrap();
    }
    assert!(
        catalog_cmd::run(Mode::Check, tmp.path())
            .unwrap()
            .is_clean()
    );
    let md = tmp.path().join(catalog_cmd::MD_FILE);
    let mut text = std::fs::read_to_string(&md).unwrap();
    text.push_str("drift\n");
    std::fs::write(&md, text).unwrap();
    let r = catalog_cmd::run(Mode::Check, tmp.path()).unwrap();
    assert_eq!(r.differing, vec![catalog_cmd::MD_FILE.to_string()]);
    // A missing file counts as drift too.
    std::fs::remove_file(tmp.path().join(catalog_cmd::JSON_FILE)).unwrap();
    assert_eq!(
        catalog_cmd::run(Mode::Check, tmp.path())
            .unwrap()
            .differing
            .len(),
        2
    );
    catalog_cmd::run(Mode::Write, tmp.path()).unwrap();
    assert!(
        catalog_cmd::run(Mode::Check, tmp.path())
            .unwrap()
            .is_clean()
    );
}
