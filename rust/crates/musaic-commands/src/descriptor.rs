//! `agent-instance.json`: where the running app listens (contracts/descriptor.md).
//! It holds no credential. The writer is the app; the reader is the CLI, which
//! trusts nothing in it until the handshake proves the instance.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::errors::{ToolError, ToolErrorCode};

pub const FILE_NAME: &str = "agent-instance.json";
pub const MAX_BYTES: u64 = 4096;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Descriptor {
    pub format_version: u32,
    pub instance_id: String,
    pub app_version: String,
    pub protocol_profiles: Vec<String>,
    pub contract_version: String,
    pub host: String,
    pub port: u32,
    pub pid: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DescriptorError {
    #[error("there is no descriptor")]
    Absent,
    #[error("the descriptor is not acceptable: {0}")]
    Invalid(String),
}

impl DescriptorError {
    /// The error the CLI reports for it.
    pub fn to_tool_error(&self) -> ToolError {
        match self {
            Self::Absent => ToolError::new(ToolErrorCode::AppNotRunning, "Musaic is not running"),
            Self::Invalid(why) => ToolError::new(ToolErrorCode::DescriptorInvalid, why.clone()),
        }
    }
}

pub fn path(dir: &Path) -> PathBuf {
    dir.join(FILE_NAME)
}

/// Write the descriptor atomically: a temp file (mode 0600 on Unix), fsync,
/// rename, and fsync of the directory on Unix.
pub fn write_atomic_owner_only(dir: &Path, d: &Descriptor) -> std::io::Result<()> {
    let tmp = dir.join(format!("{FILE_NAME}.tmp-{}", std::process::id()));
    let body = serde_json::to_vec_pretty(d).map_err(std::io::Error::other)?;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let result = (|| {
        let mut f = opts.open(&tmp)?;
        f.write_all(&body)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path(dir))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
        return result;
    }
    #[cfg(unix)]
    {
        if let Ok(d) = std::fs::File::open(dir) {
            let _ = d.sync_all();
        }
    }
    Ok(())
}

/// Remove the descriptor only when it still names this instance (a newer app
/// run may have replaced it). Returns whether it was removed.
pub fn remove_if_own(dir: &Path, instance_id: &str) -> bool {
    let p = path(dir);
    let Ok(text) = std::fs::read_to_string(&p) else {
        return false;
    };
    match serde_json::from_str::<Descriptor>(&text) {
        Ok(d) if d.instance_id == instance_id => std::fs::remove_file(&p).is_ok(),
        _ => false,
    }
}

/// The effective uid, without libc or unsafe: the owner of a probe file
/// created next to the descriptor (the reader's own config dir), else of
/// `/proc/self` on Linux, else of a probe in the system temp dir. `None` only
/// when none of these works; the caller then refuses the descriptor.
#[cfg(unix)]
fn current_uid(dir: &Path) -> Option<u32> {
    current_uid_with_temp(dir, &std::env::temp_dir())
}

#[cfg(unix)]
fn current_uid_with_temp(dir: &Path, temp: &Path) -> Option<u32> {
    probe_uid(dir)
        .or_else(proc_self_uid)
        .or_else(|| probe_uid(temp))
}

/// The owner of a uniquely named file this process creates in `dir`, which
/// is removed again.
#[cfg(unix)]
fn probe_uid(dir: &Path) -> Option<u32> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let mut name = [0u8; 8];
    getrandom::fill(&mut name).ok()?;
    let p = dir.join(format!(".musaic-uid-{}", crate::auth::hex_encode(&name)));
    let f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&p)
        .ok()?;
    let uid = f.metadata().ok().map(|m| m.uid());
    drop(f);
    let _ = std::fs::remove_file(&p);
    uid
}

/// `/proc/self` is owned by the process's effective uid.
#[cfg(target_os = "linux")]
fn proc_self_uid() -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata("/proc/self").ok().map(|m| m.uid())
}

#[cfg(all(unix, not(target_os = "linux")))]
fn proc_self_uid() -> Option<u32> {
    None
}

/// The owner check: a descriptor owned by another user is refused, and so
/// is one checked by a process that cannot tell who it is (fail closed).
#[cfg(unix)]
fn check_owner(owner: u32, current: Option<u32>) -> Result<(), DescriptorError> {
    match current {
        None => Err(DescriptorError::Invalid(
            "cannot tell which user is reading it".into(),
        )),
        Some(uid) if uid != owner => Err(DescriptorError::Invalid(
            "it belongs to another user".into(),
        )),
        Some(_) => Ok(()),
    }
}

/// Read and validate the descriptor, applying the reader table of the contract
/// in order.
pub fn read_validated(dir: &Path) -> Result<Descriptor, DescriptorError> {
    let p = path(dir);
    let meta = match std::fs::symlink_metadata(&p) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(DescriptorError::Absent),
        Err(e) => {
            return Err(DescriptorError::Invalid(format!(
                "cannot inspect it: {}",
                e.kind()
            )));
        }
    };
    if meta.file_type().is_symlink() || !meta.is_file() {
        return Err(DescriptorError::Invalid("it is not a regular file".into()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        check_owner(meta.uid(), current_uid(dir))?;
        if meta.mode() & 0o077 != 0 {
            return Err(DescriptorError::Invalid(
                "its permissions allow other users".into(),
            ));
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes() & 0x400 != 0 {
            return Err(DescriptorError::Invalid("it is a reparse point".into()));
        }
    }
    if meta.len() > MAX_BYTES {
        return Err(DescriptorError::Invalid("it is too large".into()));
    }
    let mut text = String::new();
    std::fs::File::open(&p)
        .and_then(|f| f.take(MAX_BYTES + 1).read_to_string(&mut text))
        .map_err(|e| DescriptorError::Invalid(format!("cannot read it: {}", e.kind())))?;
    if text.len() as u64 > MAX_BYTES {
        return Err(DescriptorError::Invalid("it is too large".into()));
    }
    let d: Descriptor = serde_json::from_str(&text)
        .map_err(|_| DescriptorError::Invalid("it is not a valid descriptor".into()))?;
    if d.format_version != 1 {
        return Err(DescriptorError::Invalid(
            "its format version is not 1".into(),
        ));
    }
    if d.host != "127.0.0.1" {
        return Err(DescriptorError::Invalid("its host is not 127.0.0.1".into()));
    }
    if !(1..=65535).contains(&d.port) {
        return Err(DescriptorError::Invalid("its port is out of range".into()));
    }
    Ok(d)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    fn own_uid(dir: &Path) -> u32 {
        let p = dir.join("mine");
        std::fs::write(&p, b"").unwrap();
        let uid = std::fs::metadata(&p).unwrap().uid();
        std::fs::remove_file(&p).unwrap();
        uid
    }

    #[test]
    fn the_uid_is_learned_without_the_temp_dir_and_leaves_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let unusable = dir.path().join("no").join("such").join("temp");
        assert_eq!(
            current_uid_with_temp(dir.path(), &unusable),
            Some(own_uid(dir.path()))
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn the_probe_answers_none_where_it_cannot_create_a_file() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(probe_uid(&dir.path().join("absent")), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn proc_self_names_the_same_user_as_a_probe() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(proc_self_uid(), probe_uid(dir.path()));
    }

    #[test]
    fn an_unknown_reader_fails_closed() {
        let why = |r: Result<(), DescriptorError>| match r {
            Err(DescriptorError::Invalid(w)) => w,
            other => panic!("expected invalid, got {other:?}"),
        };
        assert!(why(check_owner(501, None)).contains("cannot tell"));
        assert!(why(check_owner(501, Some(0))).contains("another user"));
        assert_eq!(check_owner(501, Some(501)), Ok(()));
        assert_eq!(
            DescriptorError::Invalid("x".into()).to_tool_error().code,
            crate::errors::ToolErrorCode::DescriptorInvalid
        );
    }
}
