//! FR-022 and SC-002: every group accessor on `Ops` has at least one operation
//! test of its own. The accessors are read from `src/ops/mod.rs`, so a group added
//! later without a test file that calls it fails here. (Whether those tests pass
//! is `cargo test`'s verdict on the files this one names.)

use std::path::PathBuf;

fn manifest() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The group name on a line `pub fn <name>(&self) -> <name>::<Type><'_> {`.
fn accessor(line: &str) -> Option<String> {
    let rest = line.trim().strip_prefix("pub fn ")?;
    let (name, tail) = rest.split_once("(&self) -> ")?;
    let module = tail.split_once("::")?.0;
    (module == name && tail.contains("<'_>")).then(|| name.to_string())
}

/// The names of the group accessors declared on `impl Ops`.
fn accessors() -> Vec<String> {
    let src = std::fs::read_to_string(manifest().join("src/ops/mod.rs")).unwrap();
    src.lines().filter_map(accessor).collect()
}

/// The `tests/ops_*.rs` files (this one excluded) with their sources.
fn ops_tests() -> Vec<(String, String)> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(manifest().join("tests")).unwrap() {
        let p = e.unwrap().path();
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        if name.starts_with("ops_")
            && name.ends_with(".rs")
            && name != "ops_all_groups.rs"
            && name != "ops_boundary.rs"
            // only reaches each accessor; it exercises no operation
            && name != "ops_handle.rs"
        {
            out.push((name, std::fs::read_to_string(&p).unwrap()));
        }
    }
    out
}

/// A group's operations may be tested in the file of the group that owns the
/// subject (the database operations in `ops_settings.rs`), so any ops test file
/// that has a `#[test]` and calls `.<group>()` counts.
#[test]
fn every_group_accessor_has_an_ops_test() {
    let groups = accessors();
    assert!(
        groups.len() >= 11,
        "found only {groups:?}; did the accessor pattern change?"
    );
    let files = ops_tests();
    let missing: Vec<&String> = groups
        .iter()
        .filter(|g| {
            let call = format!(".{g}()");
            !files
                .iter()
                .any(|(_, src)| src.contains("#[test]") && src.contains(&call))
        })
        .collect();
    assert!(
        missing.is_empty(),
        "groups with no ops test calling them: {missing:?}"
    );
}

#[test]
fn the_check_reads_accessors() {
    assert_eq!(
        accessor("    pub fn widgets(&self) -> widgets::Widgets<'_> {"),
        Some("widgets".to_string())
    );
    assert_eq!(accessor("    pub fn ops(&self) -> Ops {"), None);
}
