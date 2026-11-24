//! Connecting Claude Desktop (spec 007): the readiness report, the MCP bundle
//! the app hands to Claude Desktop, and the private file it lives in.
//!
//! The command a bundle runs and the command the report shows are one value
//! ([`Command`]); nothing the page, a tool or a setting says reaches either.
//! Claude's own files are only read. The environment (home, platform,
//! executable, temporary root) is injected through [`HostEnv`], so tests never
//! touch a real Claude directory or the real temporary directory.
//!
//! Which bundle shape a platform uses is [`shape_for`]; see `docs/mcp.md` for
//! the observation that decides it.

use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use musaic_commands::claude_desktop::{
    BUNDLE_AUTHOR, BUNDLE_NAME, EntryOwner, ExtensionState, Platform, config_file,
    detect_extension, entry_owner,
};

/// The file name of the bundle, inside its private directory.
pub const BUNDLE_FILE: &str = "Musaic.mcpb";
/// The private directory's name starts with this.
pub const DIR_PREFIX: &str = "musaic-connect-";
/// How long a generated file may stay before the periodic cleanup removes it.
pub const MAX_AGE: Duration = Duration::from_secs(60 * 60);

const ICON: &[u8] = include_bytes!("../../assets/musaic-128.png");
const DESCRIPTION: &str = "Browse and look after your FLAC collection from Claude. Musaic runs on your computer; this extension only starts its command-line program.";

/// A program and its arguments: the single source for what is shown and what
/// is written into the bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub program: String,
    pub args: Vec<String>,
}

/// Whether the bundle runs the app's installed program or carries a copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BundleShape {
    Installed,
    SelfContained,
}

/// The shape a platform uses. Whether Claude Desktop runs a program outside
/// the bundle's own directory is undocumented, so both shapes are built and
/// tested; flipping a platform is a change here plus a line in `docs/mcp.md`
/// naming the observation that justifies it. Observed on macOS (Claude
/// Desktop 6.28899.6): Claude ran `musaic-cli mcp` at its location outside the
/// bundle, so macOS uses the installed program. Windows is unverified and
/// uses the same shape until an observation says otherwise.
pub const fn shape_for(platform: Platform) -> BundleShape {
    match platform {
        Platform::Darwin | Platform::Win32 | Platform::Linux => BundleShape::Installed,
    }
}

/// Why the app's program cannot be put in a bundle although it exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unstable {
    /// Running from a mounted disk image: a volume under `/Volumes/` that is
    /// read-only, or whose mount could not be read.
    DiskImage,
    /// Running from inside an AppImage.
    AppImage,
    /// macOS App Translocation: a randomised read-only copy.
    Translocated,
    /// Under the temporary directory.
    Temporary,
}

impl Unstable {
    /// The wire word.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DiskImage => "disk-image",
            Self::AppImage => "app-image",
            Self::Translocated => "translocated",
            Self::Temporary => "temporary",
        }
    }
}

/// How the volume holding a path is mounted, as far as it decides whether a
/// program there will last.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MountKind {
    /// Mounted read-only: what a downloaded disk image always is.
    ReadOnly,
    /// Mounted writable: an external drive (or a writable image), which lasts
    /// as long as it stays attached.
    Writable,
    /// The mount could not be read.
    Unknown,
}

/// The real probe: asks the filesystem whether the volume holding `path` is
/// mounted read-only. Only macOS is asked, the one platform where a volume
/// path is classified; elsewhere the answer is `Unknown`.
pub fn probe_mount(path: &Path) -> MountKind {
    #[cfg(target_os = "macos")]
    {
        use nix::sys::statvfs::{FsFlags, statvfs};
        match statvfs(path) {
            Ok(s) if s.flags().contains(FsFlags::ST_RDONLY) => MountKind::ReadOnly,
            Ok(_) => MountKind::Writable,
            Err(_) => MountKind::Unknown,
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        MountKind::Unknown
    }
}

/// The app's own command-line program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliLocation {
    /// A regular file beside the running executable, canonical.
    Found(PathBuf),
    Missing,
    Unstable(Unstable),
}

/// What the report knows about the program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliState {
    /// With the command the bundle would run.
    Found(Command),
    Missing,
    Unstable(Unstable),
}

/// The one-click route, or why not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    Bundle,
    ConfigFileOnly,
    Blocked,
}

/// Everything the dialog reports before a file is created.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeDesktopStatus {
    pub platform: Platform,
    pub bundle_supported: bool,
    pub claude_found: bool,
    pub config_entry: EntryOwner,
    /// Whether the Musaic extension's folder is in Claude Desktop's support
    /// directory (observed on macOS, not documented). The route ignores it:
    /// an installed extension does not block installing again.
    pub extension: ExtensionState,
    pub cli: CliState,
    pub shape: BundleShape,
    pub route: Route,
    pub setup_command: Option<Command>,
    pub uninstall_command: Option<Command>,
    pub app_version: String,
}

/// A bundle on disk. The path never reaches the page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedBundle {
    pub path: PathBuf,
    pub command: Command,
    pub shape: BundleShape,
}

/// The environment the generator reads. Real for the app, injected in tests.
#[derive(Debug, Clone)]
pub struct HostEnv {
    pub platform: Platform,
    pub home: Option<PathBuf>,
    pub appdata: Option<PathBuf>,
    pub local_appdata: Option<PathBuf>,
    /// Where Claude Desktop's application may be installed (macOS).
    pub application_dirs: Vec<PathBuf>,
    /// The running executable; the program sits beside it.
    pub executable: Option<PathBuf>,
    pub temp_root: PathBuf,
    /// A program under the temporary directory is stable (tests only).
    pub allow_temporary_program: bool,
    /// The app's configuration directory when it is not the default one; it
    /// becomes `--config-dir` in the command.
    pub non_default_config_dir: Option<PathBuf>,
    /// Remove leftover bundles at startup and on the periodic tick.
    pub cleanup: bool,
    /// How the volume holding a path is mounted ([`probe_mount`] for the
    /// app; a fixed answer in tests).
    pub mount: fn(&Path) -> MountKind,
}

impl HostEnv {
    /// The running machine. `config_dir` is the app's configuration directory.
    pub fn real(config_dir: &Path) -> Self {
        let home = musaic_core::config::home_dir();
        // The default the program will see when Claude Desktop starts it: a
        // `MUSAIC_CONFIG_DIR` this app was started with is not passed on, so
        // only `~/.musaic` may go without `--config-dir`.
        let default = musaic_core::config::resolve_config_dir(None, home.as_deref())
            .and_then(|d| musaic_core::config::resolve_path(&d).ok());
        let here = musaic_core::config::resolve_path(config_dir).ok();
        let non_default = (default.is_none() || default != here).then(|| config_dir.to_path_buf());
        Self {
            platform: Platform::current(),
            application_dirs: std::iter::once(PathBuf::from("/Applications"))
                .chain(home.as_ref().map(|h| h.join("Applications")))
                .collect(),
            home,
            appdata: std::env::var_os("APPDATA").map(PathBuf::from),
            local_appdata: std::env::var_os("LOCALAPPDATA").map(PathBuf::from),
            executable: std::env::current_exe().ok(),
            temp_root: std::env::temp_dir(),
            allow_temporary_program: false,
            non_default_config_dir: non_default,
            cleanup: false,
            mount: probe_mount,
        }
    }

    /// Where Claude Desktop's configuration file is.
    pub fn config_file(&self) -> Option<PathBuf> {
        let home = self.home.as_deref()?;
        Some(config_file(self.platform, home, self.appdata.as_deref()))
    }

    fn program_name(&self) -> &'static str {
        match self.platform {
            Platform::Win32 => "musaic-cli.exe",
            _ => "musaic-cli",
        }
    }

    /// The `--config-dir` arguments, empty for the default directory.
    fn dir_args(&self) -> Vec<String> {
        match &self.non_default_config_dir {
            Some(d) => vec!["--config-dir".into(), d.to_string_lossy().into_owned()],
            None => Vec::new(),
        }
    }

    /// The program that ships beside the running executable, verified: a
    /// regular file, canonical, and somewhere that will still be there
    /// tomorrow.
    pub fn cli_location(&self) -> CliLocation {
        let Some(path) = self
            .executable
            .as_ref()
            .and_then(|e| e.parent())
            .map(|d| d.join(self.program_name()))
        else {
            return CliLocation::Missing;
        };
        if !path.is_file() {
            return CliLocation::Missing;
        }
        let canonical = musaic_core::config::resolve_path(&path).unwrap_or(path);
        match self.unstable_reason(&canonical) {
            Some(reason) => CliLocation::Unstable(reason),
            None => CliLocation::Found(canonical),
        }
    }

    /// Why a program at this canonical location would not last, if it would
    /// not. Under `/Volumes/` the mount decides: a downloaded disk image is
    /// read-only, an external drive is writable, and a mount that cannot be
    /// read is refused as a disk image.
    fn unstable_reason(&self, canonical: &Path) -> Option<Unstable> {
        let text = canonical.to_string_lossy();
        if text.starts_with("/Volumes/") && (self.mount)(canonical) != MountKind::Writable {
            return Some(Unstable::DiskImage);
        }
        if text.contains("/.mount_") {
            return Some(Unstable::AppImage);
        }
        if text.contains("/AppTranslocation/") {
            return Some(Unstable::Translocated);
        }
        if !self.allow_temporary_program {
            let temp = musaic_core::config::resolve_path(&self.temp_root)
                .unwrap_or_else(|_| self.temp_root.clone());
            if canonical.starts_with(&temp) {
                return Some(Unstable::Temporary);
            }
        }
        None
    }

    /// Whether Claude Desktop appears to be installed (research D11).
    pub fn claude_found(&self) -> bool {
        match self.platform {
            Platform::Darwin => {
                self.application_dirs
                    .iter()
                    .any(|d| d.join("Claude.app").is_dir())
                    || self.config_dir_exists()
            }
            Platform::Win32 => {
                self.config_dir_exists()
                    || self
                        .local_appdata
                        .as_ref()
                        .is_some_and(|d| d.join("AnthropicClaude").is_dir())
            }
            Platform::Linux => self.config_dir_exists(),
        }
    }

    fn config_dir_exists(&self) -> bool {
        self.config_file()
            .and_then(|f| f.parent().map(Path::is_dir))
            .unwrap_or(false)
    }

    /// Whether the extension's folder exists. Read-only.
    pub fn extension(&self) -> ExtensionState {
        match self.home.as_deref() {
            Some(home) => detect_extension(self.platform, home, self.appdata.as_deref()),
            None => ExtensionState::NotDetected,
        }
    }

    /// Whose `musaic` entry Claude's configuration file holds. Read-only.
    pub fn config_entry(&self) -> EntryOwner {
        let Some(file) = self.config_file() else {
            return EntryOwner::Absent;
        };
        match std::fs::read_to_string(&file) {
            Ok(text) => entry_owner(Some(&text)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => EntryOwner::Absent,
            Err(_) => EntryOwner::Unreadable,
        }
    }
}

/// The command a bundle of this shape runs for the program at `cli`.
pub fn bundle_command(env: &HostEnv, shape: BundleShape, cli: &Path) -> Command {
    let mut args = vec!["mcp".to_string()];
    args.extend(env.dir_args());
    let program = match shape {
        BundleShape::Installed => cli.to_string_lossy().into_owned(),
        BundleShape::SelfContained => {
            format!("${{__dirname}}/{}", self_contained_entry(env.platform))
        }
    };
    Command { program, args }
}

fn self_contained_entry(platform: Platform) -> String {
    match platform {
        Platform::Win32 => "server/musaic-cli.exe".into(),
        _ => "server/musaic-cli".into(),
    }
}

/// The readiness report, derived from the files as they are now.
pub fn status(env: &HostEnv, app_version: &str) -> ClaudeDesktopStatus {
    let bundle_supported = env.platform != Platform::Linux;
    let claude_found = env.claude_found();
    let config_entry = env.config_entry();
    let shape = shape_for(env.platform);
    let location = env.cli_location();
    let cli = match &location {
        CliLocation::Found(p) => CliState::Found(bundle_command(env, shape, p)),
        CliLocation::Missing => CliState::Missing,
        CliLocation::Unstable(u) => CliState::Unstable(*u),
    };
    let program = match &location {
        CliLocation::Found(p) => Some(p.to_string_lossy().into_owned()),
        _ => None,
    };
    let route = if !bundle_supported {
        Route::ConfigFileOnly
    } else if !claude_found {
        Route::Blocked
    } else {
        match (config_entry, &cli) {
            (EntryOwner::Absent, CliState::Found(_)) => Route::Bundle,
            (EntryOwner::Ours, _) => Route::ConfigFileOnly,
            _ => Route::Blocked,
        }
    };
    let with_program = |args: Vec<String>| {
        program.as_ref().map(|p| Command {
            program: p.clone(),
            args,
        })
    };
    let setup_command = if claude_found {
        let mut args = vec!["setup".into(), "--host".into(), "claude-desktop".into()];
        args.extend(env.dir_args());
        with_program(args)
    } else {
        None
    };
    let uninstall_command = (config_entry == EntryOwner::Ours)
        .then(|| {
            with_program(vec![
                "uninstall".into(),
                "--host".into(),
                "claude-desktop".into(),
            ])
        })
        .flatten();
    ClaudeDesktopStatus {
        platform: env.platform,
        bundle_supported,
        claude_found,
        config_entry,
        extension: env.extension(),
        cli,
        shape,
        route,
        setup_command,
        uninstall_command,
        app_version: app_version.to_string(),
    }
}

/// The bundle's manifest (MCP bundle manifest version 0.3). Fixed key order,
/// no `user_config`, no `env`: nothing is configurable and nothing secret is in
/// it.
pub fn manifest(
    command: &Command,
    shape: BundleShape,
    version: &str,
    platform: Platform,
) -> String {
    let s = |t: &str| serde_json::to_string(t).expect("a string is JSON");
    let entry_point = match shape {
        BundleShape::Installed => command.program.clone(),
        BundleShape::SelfContained => self_contained_entry(platform),
    };
    let args = command
        .args
        .iter()
        .map(|a| s(a))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "{{\n  \"manifest_version\": \"0.3\",\n  \"name\": {},\n  \"display_name\": \"Musaic\",\n  \"version\": {},\n  \"description\": {},\n  \"author\": {{\"name\": {}}},\n  \"icon\": \"icon.png\",\n  \"server\": {{\n    \"type\": \"binary\",\n    \"entry_point\": {},\n    \"mcp_config\": {{\"command\": {}, \"args\": [{}]}}\n  }},\n  \"tools_generated\": true,\n  \"compatibility\": {{\"platforms\": [{}]}}\n}}\n",
        s(BUNDLE_NAME),
        s(version),
        s(DESCRIPTION),
        s(BUNDLE_AUTHOR),
        s(&entry_point),
        s(&command.program),
        args,
        s(platform.as_str()),
    )
}

/// The zip's one timestamp: the earliest the format can say.
fn fixed_time() -> zip::DateTime {
    zip::DateTime::default()
}

fn file_options(mode: u32) -> zip::write::SimpleFileOptions {
    zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .last_modified_time(fixed_time())
        .unix_permissions(mode)
}

/// Zip some `(name, bytes, mode)` entries in the order given: the same input
/// gives the same bytes.
pub fn zip_entries(entries: &[(String, Vec<u8>, u32)]) -> Result<Vec<u8>, String> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, bytes, mode) in entries {
        writer
            .start_file(name.as_str(), file_options(*mode))
            .map_err(|e| e.to_string())?;
        writer.write_all(bytes).map_err(|e| e.to_string())?;
    }
    let cursor = writer.finish().map_err(|e| e.to_string())?;
    Ok(cursor.into_inner())
}

/// The bundle's bytes: `manifest.json`, `icon.png`, and in the self-contained
/// shape the program itself with its executable bit.
pub fn write_bundle(
    command: &Command,
    shape: BundleShape,
    version: &str,
    platform: Platform,
    program: &Path,
) -> Result<Vec<u8>, String> {
    let mut entries = vec![
        (
            "manifest.json".to_string(),
            manifest(command, shape, version, platform).into_bytes(),
            0o644,
        ),
        ("icon.png".to_string(), ICON.to_vec(), 0o644),
    ];
    if shape == BundleShape::SelfContained {
        let bytes =
            std::fs::read(program).map_err(|e| format!("the program cannot be read: {e}"))?;
        entries.push((self_contained_entry(platform), bytes, 0o755));
    }
    zip_entries(&entries)
}

/// Write `bytes` as `Musaic.mcpb` in a fresh private directory under `root`.
pub fn prepare(root: &Path, bytes: &[u8]) -> std::io::Result<PathBuf> {
    let dir = tempfile::Builder::new()
        .prefix(DIR_PREFIX)
        .tempdir_in(root)?
        .keep();
    // Private to the user whatever the umask says.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)) {
            let _ = std::fs::remove_dir_all(&dir);
            return Err(e);
        }
    }
    let path = dir.join(BUNDLE_FILE);
    let write = || -> std::io::Result<()> {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        opts.open(&path)?.write_all(bytes)
    };
    if let Err(e) = write() {
        let _ = std::fs::remove_dir_all(&dir);
        return Err(e);
    }
    Ok(path)
}

/// Remove a generated file and its directory; nothing else.
pub fn remove(path: &Path) {
    let Some(dir) = path.parent() else { return };
    if !is_hand_off_dir(dir) {
        return;
    }
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_dir(dir);
}

/// A directory this module made: the prefix, not a link, holding nothing but
/// the bundle's file name.
fn is_hand_off_dir(dir: &Path) -> bool {
    let named = dir
        .file_name()
        .is_some_and(|n| n.to_string_lossy().starts_with(DIR_PREFIX));
    if !named || dir.is_symlink() || !dir.is_dir() {
        return false;
    }
    match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .flatten()
            .all(|e| e.file_name() == BUNDLE_FILE && e.path().is_file()),
        Err(_) => false,
    }
}

/// Remove the generated directories under `root` that are older than
/// `older_than` (all of them when `None`). Returns how many went.
pub fn sweep(root: &Path, older_than: Option<Duration>, now: SystemTime) -> usize {
    let Ok(entries) = std::fs::read_dir(root) else {
        return 0;
    };
    let mut removed = 0;
    for e in entries.flatten() {
        let dir = e.path();
        if !is_hand_off_dir(&dir) {
            continue;
        }
        if let Some(age) = older_than {
            let modified = std::fs::metadata(&dir).and_then(|m| m.modified());
            let old = modified
                .ok()
                .and_then(|m| now.duration_since(m).ok())
                .is_some_and(|d| d >= age);
            if !old {
                continue;
            }
        }
        let _ = std::fs::remove_file(dir.join(BUNDLE_FILE));
        if std::fs::remove_dir(&dir).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// Remove every leftover bundle (the app has just started).
pub(crate) fn sweep_at_start(app: &crate::App) {
    if app.0.host_env.cleanup {
        sweep(&app.0.host_env.temp_root, None, SystemTime::now());
    }
}

/// Remove bundles older than [`MAX_AGE`] (the app's periodic tick).
pub(crate) fn sweep_old(app: &crate::App) {
    if app.0.host_env.cleanup {
        sweep(&app.0.host_env.temp_root, Some(MAX_AGE), SystemTime::now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_manifest_is_valid_json_with_the_required_fields() {
        let command = Command {
            program: "/Apps/My \"App\"/é/musaic-cli".into(),
            args: vec!["mcp".into(), "--config-dir".into(), "/a b/ü".into()],
        };
        let text = manifest(&command, BundleShape::Installed, "1.2.3", Platform::Darwin);
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["manifest_version"], "0.3");
        assert_eq!(
            v["server"]["mcp_config"]["command"],
            command.program.as_str()
        );
        assert_eq!(v["server"]["entry_point"], command.program.as_str());
        assert_eq!(v["server"]["mcp_config"]["args"][2], "/a b/ü");
        assert!(v.get("user_config").is_none());
        assert!(v["server"]["mcp_config"].get("env").is_none());
    }

    fn env_with(mount: fn(&Path) -> MountKind) -> HostEnv {
        HostEnv {
            platform: Platform::Darwin,
            home: None,
            appdata: None,
            local_appdata: None,
            application_dirs: Vec::new(),
            executable: None,
            temp_root: PathBuf::from("/nonexistent-musaic-temp-root"),
            allow_temporary_program: false,
            non_default_config_dir: None,
            cleanup: false,
            mount,
        }
    }

    const ON_VOLUME: &str = "/Volumes/Drive/Musaic.app/Contents/MacOS/musaic-cli";

    #[test]
    fn a_read_only_volume_is_refused_as_a_disk_image() {
        let env = env_with(|_| MountKind::ReadOnly);
        assert_eq!(
            env.unstable_reason(Path::new(ON_VOLUME)),
            Some(Unstable::DiskImage)
        );
    }

    #[test]
    fn a_writable_external_drive_is_accepted() {
        let env = env_with(|_| MountKind::Writable);
        assert_eq!(env.unstable_reason(Path::new(ON_VOLUME)), None);
    }

    #[test]
    fn a_volume_whose_mount_cannot_be_read_is_refused() {
        let env = env_with(|_| MountKind::Unknown);
        assert_eq!(
            env.unstable_reason(Path::new(ON_VOLUME)),
            Some(Unstable::DiskImage)
        );
    }

    #[test]
    fn the_mount_is_asked_only_under_volumes() {
        let env = env_with(|_| panic!("the mount is not asked outside /Volumes/"));
        assert_eq!(
            env.unstable_reason(Path::new(
                "/Applications/Musaic.app/Contents/MacOS/musaic-cli"
            )),
            None
        );
        // A writable volume does not hide the other reasons.
        let env = env_with(|_| MountKind::Writable);
        assert_eq!(
            env.unstable_reason(Path::new(
                "/Volumes/Drive/AppTranslocation/X/d/Musaic.app/Contents/MacOS/musaic-cli"
            )),
            Some(Unstable::Translocated)
        );
    }

    #[test]
    fn the_real_probe_reads_a_mount_without_failing() {
        // The probe is only read, never relied on for a value here: the
        // temporary directory may be on any kind of volume.
        let got = probe_mount(&std::env::temp_dir());
        if cfg!(target_os = "macos") {
            assert_ne!(got, MountKind::Unknown);
        } else {
            assert_eq!(got, MountKind::Unknown);
        }
    }
}
