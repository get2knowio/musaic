//! Descriptor writer and reader (T012).

use musaic_commands::descriptor::{self, Descriptor, DescriptorError};

fn sample() -> Descriptor {
    Descriptor {
        format_version: 1,
        instance_id: "i_3f9a0c1d2e4b5a69".into(),
        app_version: "0.2.0".into(),
        protocol_profiles: vec!["mcp-2026-07-28".into(), "mcp-2025-11-25".into()],
        contract_version: "1".into(),
        host: "127.0.0.1".into(),
        port: 53817,
        pid: 4242,
    }
}

fn invalid(r: Result<Descriptor, DescriptorError>) -> String {
    match r {
        Err(DescriptorError::Invalid(why)) => why,
        other => panic!("expected invalid, got {other:?}"),
    }
}

fn write_raw(dir: &std::path::Path, body: &str) {
    let p = descriptor::path(dir);
    std::fs::write(&p, body).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
}

#[test]
fn round_trip_and_own_removal() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        descriptor::read_validated(dir.path()),
        Err(DescriptorError::Absent)
    );
    descriptor::write_atomic_owner_only(dir.path(), &sample()).unwrap();
    assert_eq!(descriptor::read_validated(dir.path()).unwrap(), sample());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(descriptor::path(dir.path()))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    // No temp file is left behind.
    let names: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names.len(), 1);
    // Another instance's removal leaves it; its own removes it.
    assert!(!descriptor::remove_if_own(dir.path(), "i_other"));
    assert!(descriptor::path(dir.path()).exists());
    assert!(descriptor::remove_if_own(dir.path(), &sample().instance_id));
    assert!(!descriptor::path(dir.path()).exists());
}

#[cfg(unix)]
#[test]
fn symlink_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real.json");
    write_raw_to(&real, &serde_json::to_string(&sample()).unwrap());
    std::os::unix::fs::symlink(&real, descriptor::path(dir.path())).unwrap();
    assert!(invalid(descriptor::read_validated(dir.path())).contains("regular file"));
}

#[cfg(unix)]
fn write_raw_to(p: &std::path::Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(p, body).unwrap();
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o600)).unwrap();
}

#[cfg(unix)]
#[test]
fn group_or_other_bits_are_refused() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    write_raw(dir.path(), &serde_json::to_string(&sample()).unwrap());
    std::fs::set_permissions(
        descriptor::path(dir.path()),
        std::fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    assert!(invalid(descriptor::read_validated(dir.path())).contains("permissions"));
}

// A file owned by another user cannot be created without privilege, so the
// other-owner case runs only as root (where chown works) and is skipped
// otherwise; the owner comparison itself is the same code path the mode
// check above sits next to.
#[cfg(unix)]
#[test]
fn other_owner_is_refused_when_root_can_set_it() {
    let out = std::process::Command::new("id").arg("-u").output().unwrap();
    if String::from_utf8_lossy(&out.stdout).trim() != "0" {
        eprintln!("skipped: other-owner case needs root");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    write_raw(dir.path(), &serde_json::to_string(&sample()).unwrap());
    let st = std::process::Command::new("chown")
        .arg("65534")
        .arg(descriptor::path(dir.path()))
        .status()
        .unwrap();
    assert!(st.success());
    assert!(invalid(descriptor::read_validated(dir.path())).contains("another user"));
}

// The reader learns its uid from a probe next to the descriptor; where it may
// not write there it falls back (/proc/self on Linux, the temp dir) instead of
// skipping the owner check, and leaves nothing behind.
#[cfg(unix)]
#[test]
fn a_descriptor_in_a_read_only_dir_is_still_owner_checked_and_read() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    descriptor::write_atomic_owner_only(dir.path(), &sample()).unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
    let read = descriptor::read_validated(dir.path());
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(read.unwrap(), sample());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn oversize_bad_version_host_port_and_unknown_fields_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    write_raw(dir.path(), &" ".repeat(5000));
    assert!(invalid(descriptor::read_validated(dir.path())).contains("large"));

    let mut cases: Vec<(Descriptor, &str)> = Vec::new();
    let mut d = sample();
    d.format_version = 2;
    cases.push((d, "format version"));
    let mut d = sample();
    d.host = "0.0.0.0".into();
    cases.push((d, "host"));
    let mut d = sample();
    d.host = "localhost".into();
    cases.push((d, "host"));
    let mut d = sample();
    d.port = 0;
    cases.push((d, "port"));
    let mut d = sample();
    d.port = 70000;
    cases.push((d, "port"));
    for (d, why) in cases {
        write_raw(dir.path(), &serde_json::to_string(&d).unwrap());
        assert!(
            invalid(descriptor::read_validated(dir.path())).contains(why),
            "{why}"
        );
    }
    let mut v = serde_json::to_value(sample()).unwrap();
    v["focus"] = "x".into();
    write_raw(dir.path(), &v.to_string());
    assert!(invalid(descriptor::read_validated(dir.path())).contains("valid descriptor"));
    write_raw(dir.path(), "not json");
    assert!(invalid(descriptor::read_validated(dir.path())).contains("valid descriptor"));
}

#[test]
fn errors_map_to_the_contract_codes() {
    use musaic_commands::ToolErrorCode as C;
    assert_eq!(
        DescriptorError::Absent.to_tool_error().code,
        C::AppNotRunning
    );
    assert_eq!(
        DescriptorError::Invalid("x".into()).to_tool_error().code,
        C::DescriptorInvalid
    );
}
