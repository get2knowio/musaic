//! Where Claude Desktop keeps its configuration, and whose `musaic` entry is in
//! it. One definition for the bundled CLI (`setup`, `uninstall`, `doctor`) and
//! the app (the readiness report), so the two cannot disagree about whether an
//! entry is Musaic's. Pure functions over text and paths: callers do the reading.

use std::path::{Path, PathBuf};

use serde_json::Value;

/// The entry's name in `mcpServers`.
pub const ENTRY_NAME: &str = "musaic";

/// The operating system families Claude Desktop is configured on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Platform {
    Darwin,
    Win32,
    Linux,
}

impl Platform {
    /// The platform this program was built for.
    pub const fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::Darwin
        } else if cfg!(windows) {
            Self::Win32
        } else {
            Self::Linux
        }
    }

    /// The name used on the wire and in a bundle's `compatibility.platforms`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Darwin => "darwin",
            Self::Win32 => "win32",
            Self::Linux => "linux",
        }
    }
}

/// Claude Desktop's configuration file under `home` (`appdata` is `%APPDATA%`
/// on Windows; without it the conventional spot under `home` is used).
pub fn config_file(platform: Platform, home: &Path, appdata: Option<&Path>) -> PathBuf {
    match platform {
        Platform::Darwin => {
            home.join("Library/Application Support/Claude/claude_desktop_config.json")
        }
        Platform::Win32 => appdata
            .map(Path::to_path_buf)
            .unwrap_or_else(|| home.join("AppData/Roaming"))
            .join("Claude/claude_desktop_config.json"),
        Platform::Linux => home.join(".config/Claude/claude_desktop_config.json"),
    }
}

/// The bundle's author name and name. Claude Desktop unpacks an installed
/// extension into a folder built from both, so the manifest and
/// [`extension_dir`] share these and cannot drift apart.
pub const BUNDLE_AUTHOR: &str = "Musaic";
pub const BUNDLE_NAME: &str = "musaic";

/// Claude Desktop's support directory (the one holding its configuration file).
fn support_dir(platform: Platform, home: &Path, appdata: Option<&Path>) -> PathBuf {
    let file = config_file(platform, home, appdata);
    file.parent().map(Path::to_path_buf).unwrap_or(file)
}

/// Where Claude Desktop unpacks the installed Musaic extension: `Claude
/// Extensions/local.mcpb.<author>.<name>` in its support directory. Observed
/// on macOS (Claude Desktop 6.28899.6); the same relative folder is assumed on
/// Windows (unverified) and Linux. Not a documented location: a missing folder
/// means "not detected", never "not installed".
pub fn extension_dir(platform: Platform, home: &Path, appdata: Option<&Path>) -> PathBuf {
    extension_dir_in(&support_dir(platform, home, appdata))
}

/// [`extension_dir`] for a known support directory.
pub fn extension_dir_in(support: &Path) -> PathBuf {
    support.join("Claude Extensions").join(format!(
        "local.mcpb.{}.{}",
        BUNDLE_AUTHOR.to_lowercase(),
        BUNDLE_NAME
    ))
}

/// Whether the Musaic extension's folder was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtensionState {
    Detected,
    NotDetected,
    /// Claude's support directory exists but cannot be read.
    Unknown,
}

impl ExtensionState {
    /// The wire word.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Detected => "detected",
            Self::NotDetected => "not-detected",
            Self::Unknown => "unknown",
        }
    }
}

/// Look, read-only, for the extension's folder under `home`.
pub fn detect_extension(platform: Platform, home: &Path, appdata: Option<&Path>) -> ExtensionState {
    detect_extension_in(&support_dir(platform, home, appdata))
}

/// [`detect_extension`] for a known support directory.
pub fn detect_extension_in(support: &Path) -> ExtensionState {
    match std::fs::metadata(extension_dir_in(support)) {
        Ok(m) if m.is_dir() => ExtensionState::Detected,
        Ok(_) => ExtensionState::NotDetected,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // A missing support directory is "not detected"; one that exists
            // but cannot be read is "unknown".
            match std::fs::read_dir(support) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => ExtensionState::Unknown,
                _ => ExtensionState::NotDetected,
            }
        }
        Err(_) => ExtensionState::Unknown,
    }
}

/// Whose `musaic` entry, if any, a configuration file holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryOwner {
    /// No file, or no `musaic` entry in it.
    Absent,
    /// Written by `musaic-cli setup`.
    Ours,
    /// An entry named `musaic` that Musaic did not write.
    Foreign,
    /// The file cannot be read as JSON, or `mcpServers` is not an object.
    Unreadable,
}

/// Whether an existing entry is one setup wrote: its command's file name is
/// `musaic-cli[.exe]` and its args start with `mcp`.
pub fn is_ours(entry: &Value) -> bool {
    let command = entry["command"].as_str().unwrap_or("");
    let name = command.rsplit(['/', '\\']).next().unwrap_or("");
    let args_ok = entry["args"]
        .as_array()
        .and_then(|a| a.first())
        .and_then(Value::as_str)
        == Some("mcp");
    (name == "musaic-cli" || name == "musaic-cli.exe") && args_ok
}

/// The owner of the `musaic` entry in a configuration file's text (`None` when
/// the file does not exist).
pub fn entry_owner(config_text: Option<&str>) -> EntryOwner {
    let Some(text) = config_text else {
        return EntryOwner::Absent;
    };
    let Ok(root) = serde_json::from_str::<Value>(text) else {
        return EntryOwner::Unreadable;
    };
    let Some(root) = root.as_object() else {
        return EntryOwner::Unreadable;
    };
    let servers = match root.get("mcpServers") {
        None => return EntryOwner::Absent,
        Some(Value::Object(m)) => m,
        Some(_) => return EntryOwner::Unreadable,
    };
    match servers.get(ENTRY_NAME) {
        None => EntryOwner::Absent,
        Some(e) if is_ours(e) => EntryOwner::Ours,
        Some(_) => EntryOwner::Foreign,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn config_file_per_platform() {
        let home = Path::new("/h");
        assert_eq!(
            config_file(Platform::Darwin, home, None),
            Path::new("/h/Library/Application Support/Claude/claude_desktop_config.json")
        );
        assert_eq!(
            config_file(Platform::Linux, home, None),
            Path::new("/h/.config/Claude/claude_desktop_config.json")
        );
        assert_eq!(
            config_file(Platform::Win32, home, Some(Path::new("/ad"))),
            Path::new("/ad/Claude/claude_desktop_config.json")
        );
        assert_eq!(
            config_file(Platform::Win32, home, None),
            Path::new("/h/AppData/Roaming/Claude/claude_desktop_config.json")
        );
    }

    #[test]
    fn the_extension_folder_follows_the_bundle_identity() {
        let home = Path::new("/h");
        assert_eq!(
            extension_dir(Platform::Darwin, home, None),
            Path::new(
                "/h/Library/Application Support/Claude/Claude Extensions/local.mcpb.musaic.musaic"
            )
        );
        assert_eq!(
            extension_dir(Platform::Linux, home, None),
            Path::new("/h/.config/Claude/Claude Extensions/local.mcpb.musaic.musaic")
        );
        assert_eq!(
            extension_dir(Platform::Win32, home, Some(Path::new("/ad"))),
            Path::new("/ad/Claude/Claude Extensions/local.mcpb.musaic.musaic")
        );
        assert_eq!(BUNDLE_AUTHOR, "Musaic");
        assert_eq!(BUNDLE_NAME, "musaic");
    }

    #[test]
    fn each_extension_state() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let platform = Platform::Linux;
        // No support directory at all.
        assert_eq!(
            detect_extension(platform, home, None),
            ExtensionState::NotDetected
        );
        // Support directory, no extension.
        let support = config_file(platform, home, None)
            .parent()
            .unwrap()
            .to_path_buf();
        std::fs::create_dir_all(&support).unwrap();
        assert_eq!(
            detect_extension(platform, home, None),
            ExtensionState::NotDetected
        );
        // A plain file where the folder should be is not an extension.
        let dir = extension_dir(platform, home, None);
        std::fs::create_dir_all(dir.parent().unwrap()).unwrap();
        std::fs::write(&dir, b"x").unwrap();
        assert_eq!(
            detect_extension(platform, home, None),
            ExtensionState::NotDetected
        );
        std::fs::remove_file(&dir).unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(
            detect_extension(platform, home, None),
            ExtensionState::Detected
        );
        std::fs::remove_dir_all(&dir).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let ext = dir.parent().unwrap();
            std::fs::set_permissions(&support, std::fs::Permissions::from_mode(0o000)).unwrap();
            let got = detect_extension(platform, home, None);
            std::fs::set_permissions(&support, std::fs::Permissions::from_mode(0o755)).unwrap();
            let _ = ext;
            // Running as root can read anything; otherwise it is unknown.
            assert!(matches!(
                got,
                ExtensionState::Unknown | ExtensionState::NotDetected
            ));
        }
    }

    #[test]
    fn ownership_is_the_binary_name_and_the_mcp_argument() {
        assert!(is_ours(
            &json!({"command": "/a/b/musaic-cli", "args": ["mcp"]})
        ));
        assert!(is_ours(
            &json!({"command": "C:\\x\\musaic-cli.exe", "args": ["mcp", "--config-dir", "d"]})
        ));
        assert!(!is_ours(&json!({"command": "/a/b/other", "args": ["mcp"]})));
        assert!(!is_ours(
            &json!({"command": "/a/b/musaic-cli", "args": ["serve"]})
        ));
        assert!(!is_ours(&json!({"command": "/a/b/musaic-cli"})));
    }

    #[test]
    fn each_owner_state() {
        assert_eq!(entry_owner(None), EntryOwner::Absent);
        assert_eq!(entry_owner(Some("{}")), EntryOwner::Absent);
        assert_eq!(
            entry_owner(Some(r#"{"mcpServers":{"other":{}}}"#)),
            EntryOwner::Absent
        );
        assert_eq!(
            entry_owner(Some(
                r#"{"mcpServers":{"musaic":{"command":"/x/musaic-cli","args":["mcp"]}}}"#
            )),
            EntryOwner::Ours
        );
        assert_eq!(
            entry_owner(Some(
                r#"{"mcpServers":{"musaic":{"command":"/x/other","args":["mcp"]}}}"#
            )),
            EntryOwner::Foreign
        );
        assert_eq!(entry_owner(Some("not json")), EntryOwner::Unreadable);
        assert_eq!(entry_owner(Some("[1]")), EntryOwner::Unreadable);
        assert_eq!(
            entry_owner(Some(r#"{"mcpServers":[1]}"#)),
            EntryOwner::Unreadable
        );
    }
}
