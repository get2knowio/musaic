//! Config directory and `config.json`.
//!
//! Ports the Python app's `config.py`:
//!
//! * the config dir is `$MUSAIC_CONFIG_DIR` when set and non-empty, else
//!   `~/.musaic`;
//! * `config.json` in it holds `{"library_dir": <path or null>}`, written by
//!   `json.dumps(..., indent=2)` (ASCII-escaped, no trailing newline);
//! * `set_library_dir` stores the path after `expanduser()` and a non-strict
//!   `resolve()` (symlinks followed, `.`/`..` removed, missing tail kept).
//!
//! Deliberate change: `MEDIA_MANAGER_CONFIG_DIR`
//! and the one-time rename of `~/.media_manager` are not ported. An install
//! from before that rename must run the Python app once first.

use std::ffi::OsStr;
use std::io;
use std::path::{Component, Path, PathBuf};

use crate::pycompat::json_string;

/// Environment variable that overrides the config dir.
pub const CONFIG_DIR_ENV: &str = "MUSAIC_CONFIG_DIR";
/// Name of the config dir under the home directory.
pub const DEFAULT_DIR_NAME: &str = ".musaic";
/// File holding the selected library directory.
pub const CONFIG_FILE_NAME: &str = "config.json";
/// The library database inside the config dir.
pub const DB_FILE_NAME: &str = "library.db";
/// Sub-directory holding automatic snapshots.
pub const SNAPSHOT_DIR_NAME: &str = "backups";

/// Pure resolution: `env_value` is the value of `MUSAIC_CONFIG_DIR` (if any),
/// `home` the user's home directory. An empty variable counts as unset, as in
/// Python's `if env := os.environ.get(...)`. A relative value is returned as is.
pub fn resolve_config_dir(env_value: Option<&OsStr>, home: Option<&Path>) -> Option<PathBuf> {
    match env_value {
        Some(v) if !v.is_empty() => Some(PathBuf::from(v)),
        _ => home.map(|h| h.join(DEFAULT_DIR_NAME)),
    }
}

/// The user's home directory as Python's `Path.home()` sees it on POSIX:
/// `$HOME` when set, else the password database.
pub fn home_dir() -> Option<PathBuf> {
    #[allow(deprecated)] // un-deprecated in later Rust releases; behaviour is what we want
    std::env::home_dir()
}

/// [`resolve_config_dir`] against the real environment.
pub fn config_dir() -> Option<PathBuf> {
    resolve_config_dir(
        std::env::var_os(CONFIG_DIR_ENV).as_deref(),
        home_dir().as_deref(),
    )
}

/// The contents of `config.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AppConfig {
    pub library_dir: Option<String>,
}

impl AppConfig {
    /// Exactly what `json.dumps(asdict(config), indent=2)` writes.
    pub fn to_json(&self) -> String {
        let value = match &self.library_dir {
            Some(s) => json_string(s),
            None => "null".to_owned(),
        };
        format!("{{\n  \"library_dir\": {value}\n}}")
    }

    /// Parse `config.json` text. Anything unreadable gives the default config.
    ///
    /// Two deliberate differences from the Python app, both on files it never
    /// writes itself: a JSON value that is not an object gives the default
    /// (Python raises `AttributeError`), and a non-string `library_dir` gives
    /// `None` (Python passes the number through).
    pub fn parse(bytes: &[u8]) -> Self {
        let Ok(text) = std::str::from_utf8(bytes) else {
            return Self::default();
        };
        match serde_json::from_str::<serde_json::Value>(text) {
            Ok(serde_json::Value::Object(map)) => Self {
                library_dir: map
                    .get("library_dir")
                    .and_then(|v| v.as_str())
                    .map(str::to_owned),
            },
            _ => Self::default(),
        }
    }
}

/// A config directory and the files in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigDir {
    dir: PathBuf,
}

impl ConfigDir {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// From the real environment; `None` only when there is no home directory.
    pub fn from_env() -> Option<Self> {
        config_dir().map(Self::new)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn config_file(&self) -> PathBuf {
        self.dir.join(CONFIG_FILE_NAME)
    }

    pub fn db_path(&self) -> PathBuf {
        self.dir.join(DB_FILE_NAME)
    }

    pub fn snapshot_dir(&self) -> PathBuf {
        self.dir.join(SNAPSHOT_DIR_NAME)
    }

    /// `load_config()`: the default config when the file is absent or unreadable.
    pub fn load(&self) -> AppConfig {
        let file = self.config_file();
        if !file.is_file() {
            return AppConfig::default();
        }
        match std::fs::read(&file) {
            Ok(bytes) => AppConfig::parse(&bytes),
            Err(_) => AppConfig::default(),
        }
    }

    /// `save_config()`: creates the dir, then writes the file. Written to a
    /// temporary name and renamed into place, so a crash never leaves a
    /// truncated file (the Python app writes in place; the bytes are the same).
    pub fn save(&self, config: &AppConfig) -> io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let file = self.config_file();
        let tmp = self.dir.join(format!("{CONFIG_FILE_NAME}.tmp"));
        std::fs::write(&tmp, config.to_json())?;
        std::fs::rename(&tmp, &file)
    }

    /// `set_library_dir()`: expand `~`, resolve, store. Other keys an older
    /// file carried are dropped, as in the Python app (it rewrites the dataclass).
    pub fn set_library_dir(&self, path: &str, home: Option<&Path>) -> io::Result<AppConfig> {
        let resolved = resolve_path(&expand_user(path, home))?;
        let Some(s) = resolved.to_str() else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "library path is not valid UTF-8",
            ));
        };
        let mut config = self.load();
        config.library_dir = Some(s.to_owned());
        self.save(&config)?;
        Ok(config)
    }
}

/// `Path.expanduser()` for `~` and `~/...`. `~user` forms are left as they are
/// (Python looks the user up in the password database). On Windows `~\...`
/// is expanded too, as `ntpath.expanduser` does.
pub fn expand_user(path: &str, home: Option<&Path>) -> PathBuf {
    if let Some(home) = home {
        if path == "~" {
            return home.to_path_buf();
        }
        if let Some(rest) = path.strip_prefix("~/") {
            return home.join(rest.trim_start_matches('/'));
        }
        if cfg!(windows)
            && let Some(rest) = path.strip_prefix("~\\")
        {
            return home.join(rest.trim_start_matches(['/', '\\']));
        }
    }
    PathBuf::from(path)
}

/// `Path.resolve()` (non-strict), following `posixpath.realpath`: make the
/// path absolute against the current directory, then walk it component by
/// component, applying `..` lexically and replacing each existing symlink by
/// its target. Components that do not exist are kept as written. Unlike
/// `std::fs::canonicalize`, names keep the spelling given (macOS realpath(3)
/// would return the on-disk case).
///
/// On Windows it follows `ntpath.realpath` instead, as `Path.resolve()` does
/// there: see [`windows_realpath`].
pub fn resolve_path(path: &Path) -> io::Result<PathBuf> {
    let path = if path.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        path.to_path_buf()
    };
    let abs = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()?.join(path)
    };
    if cfg!(windows) {
        return Ok(windows_realpath(&abs));
    }
    let mut out = PathBuf::new();
    let mut links_followed = 0usize;
    walk(&mut out, &abs, &mut links_followed);
    if out.as_os_str().is_empty() {
        out.push(Component::RootDir.as_os_str());
    }
    Ok(out)
}

/// `ntpath.realpath(abs)` (non-strict), which is what `Path.resolve()` does on
/// Windows: `.` and `..` are removed lexically first, then the longest
/// existing leading part is replaced by the name the OS reports for it (the
/// final path: links and junctions followed, 8.3 short names expanded, the
/// on-disk case of every name), and a missing tail is kept as written.
///
/// Unlike `posixpath.realpath`, this asks the OS for the spelling. That is
/// what keeps an album id (which hashes this string) the same however the
/// library folder was typed: NTFS is case-insensitive, and `C:\Users\RUNNER~1`
/// and `C:\Users\runneradmin` are one folder. The `\\?\` prefix
/// `std::fs::canonicalize` adds is dropped again, as Python drops it, so the
/// result is an ordinary path that may be joined with `/` and shown to people.
///
/// On other systems this is the lexical normalisation only (it is not called
/// there outside tests).
pub fn windows_realpath(abs: &Path) -> PathBuf {
    let mut norm = PathBuf::new();
    for c in abs.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                // Never pops the prefix or root: `C:\..` is `C:\`.
                if matches!(norm.components().next_back(), Some(Component::Normal(_))) {
                    norm.pop();
                }
            }
            other => norm.push(other.as_os_str()),
        }
    }
    if !cfg!(windows) {
        return norm;
    }
    let mut head = norm.clone();
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    loop {
        if let Ok(real) = std::fs::canonicalize(&head) {
            let mut out = strip_verbatim(real);
            for name in tail.iter().rev() {
                out.push(name);
            }
            return out;
        }
        match head.file_name() {
            Some(name) => {
                tail.push(name.to_owned());
                head.pop();
            }
            None => return norm,
        }
    }
}

/// `\\?\C:\x` → `C:\x` and `\\?\UNC\server\share\x` → `\\server\share\x`,
/// when the plain form is a valid path (shorter than `MAX_PATH`); anything
/// else is returned unchanged.
fn strip_verbatim(p: PathBuf) -> PathBuf {
    const MAX_PATH: usize = 260;
    let Some(s) = p.to_str() else { return p };
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        if rest.len() + 2 < MAX_PATH {
            return PathBuf::from(format!(r"\\{rest}"));
        }
        return p;
    }
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        let b = rest.as_bytes();
        if b.len() >= 3
            && b[0].is_ascii_alphabetic()
            && b[1] == b':'
            && b[2] == b'\\'
            && rest.len() < MAX_PATH
        {
            return PathBuf::from(rest);
        }
    }
    p
}

/// Symlink hops after which a path is treated as a loop and kept unresolved
/// (Python's non-strict realpath likewise returns the looping path as is).
const MAX_LINKS: usize = 40;

fn walk(out: &mut PathBuf, rest: &Path, links_followed: &mut usize) {
    for comp in rest.components() {
        match comp {
            Component::Prefix(p) => {
                *out = PathBuf::from(p.as_os_str());
            }
            Component::RootDir => {
                let prefix: PathBuf = out
                    .components()
                    .take_while(|c| matches!(c, Component::Prefix(_)))
                    .collect();
                *out = prefix;
                out.push(Component::RootDir.as_os_str());
            }
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(name) => {
                let candidate = out.join(name);
                let is_link = std::fs::symlink_metadata(&candidate)
                    .map(|m| m.file_type().is_symlink())
                    .unwrap_or(false);
                if is_link
                    && *links_followed < MAX_LINKS
                    && let Ok(target) = std::fs::read_link(&candidate)
                {
                    *links_followed += 1;
                    // A relative target is relative to the link's directory (`out`);
                    // an absolute one restarts from the root.
                    walk(out, &target, links_followed);
                    continue;
                }
                *out = candidate;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A trivial structural check; the Python app comparisons live in
    // tests/config_python_app.rs.
    #[test]
    fn verbatim_prefix_is_dropped_when_the_plain_path_is_valid() {
        let p = |s: &str| PathBuf::from(s);
        assert_eq!(strip_verbatim(p(r"\\?\C:\Music\A")), p(r"C:\Music\A"));
        assert_eq!(
            strip_verbatim(p(r"\\?\UNC\nas\share\Music")),
            p(r"\\nas\share\Music")
        );
        // Not a drive path: kept.
        assert_eq!(
            strip_verbatim(p(r"\\?\Volume{0}\Music")),
            p(r"\\?\Volume{0}\Music")
        );
        // Too long for a plain path: kept.
        let long = format!(r"\\?\C:\{}", "x".repeat(300));
        assert_eq!(strip_verbatim(p(&long)), p(&long));
        assert_eq!(strip_verbatim(p("/plain/path")), p("/plain/path"));
    }

    // On Windows the result also depends on the disk (tests/windows_paths.rs);
    // the lexical part is the same everywhere.
    #[cfg(not(windows))]
    #[test]
    fn windows_realpath_removes_dots_lexically() {
        assert_eq!(
            windows_realpath(Path::new("/a/./b/../c/..")),
            PathBuf::from("/a")
        );
        assert_eq!(windows_realpath(Path::new("/..")), PathBuf::from("/"));
    }

    #[test]
    fn file_locations_sit_in_the_dir() {
        let c = ConfigDir::new("/x/cfg");
        assert_eq!(c.config_file(), Path::new("/x/cfg/config.json"));
        assert_eq!(c.db_path(), Path::new("/x/cfg/library.db"));
        assert_eq!(c.snapshot_dir(), Path::new("/x/cfg/backups"));
    }
}
