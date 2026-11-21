//! `setup` and `uninstall`: put Musaic's MCP entry in an assistant host's
//! configuration, safely (research R11, FR-046 to FR-048).
//!
//! The rules: a file is backed up before it changes; the new text is written
//! to a temp file in the same directory and renamed over it; every unrelated
//! entry keeps its text byte for byte (values are carried as raw JSON, so
//! neither their order nor their formatting changes); a file that is not
//! valid JSON, or a `musaic` entry we did not write, is refused with nothing
//! changed; and if any write fails every file already written is restored.
//! `--dry-run` prints the same plan and writes nothing.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::de::{Deserializer, MapAccess, Visitor};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json, value::RawValue};

use crate::commands::{EXIT_OK, EXIT_PROBLEMS, Outcome};

/// The entry's name in `mcpServers`.
pub const ENTRY_NAME: &str = "musaic";

/// Where each supported host keeps its configuration.
#[derive(Debug, Clone)]
pub struct HostPaths {
    pub claude_desktop: PathBuf,
    pub claude_code: PathBuf,
}

impl HostPaths {
    /// The layout under `home` for this operating system (`appdata` is
    /// `%APPDATA%` on Windows).
    pub fn under(home: &Path, appdata: Option<&Path>) -> Self {
        let desktop = if cfg!(target_os = "macos") {
            home.join("Library/Application Support/Claude/claude_desktop_config.json")
        } else if cfg!(windows) {
            appdata
                .map(Path::to_path_buf)
                .unwrap_or_else(|| home.join("AppData/Roaming"))
                .join("Claude/claude_desktop_config.json")
        } else {
            home.join(".config/Claude/claude_desktop_config.json")
        };
        Self {
            claude_desktop: desktop,
            claude_code: home.join(".claude.json"),
        }
    }

    pub fn from_env() -> Self {
        let home = musaic_core::config::home_dir().unwrap_or_else(|| PathBuf::from("."));
        Self::under(
            &home,
            std::env::var_os("APPDATA").map(PathBuf::from).as_deref(),
        )
    }
}

#[derive(Debug, Clone)]
pub struct Options {
    pub dry_run: bool,
    /// `claude-desktop`, `claude-code` or `all`.
    pub host: String,
    pub uninstall: bool,
    /// Added to the entry as `--config-dir` when the config dir is not the default.
    pub config_dir: Option<PathBuf>,
    pub paths: HostPaths,
    /// The CLI's own absolute path (what the entry's `command` will be).
    pub executable: Option<PathBuf>,
    /// Test seam: make the n-th write (counting from 0) fail.
    pub fail_write_number: Option<usize>,
}

impl Options {
    pub fn new(paths: HostPaths) -> Self {
        Self {
            dry_run: false,
            host: "all".into(),
            uninstall: false,
            config_dir: None,
            paths,
            executable: None,
            fail_write_number: None,
        }
    }
}

// ------------------------------------------------- ordered raw JSON object

/// A JSON object whose values are kept as the text they had.
struct Ordered(Vec<(String, Box<RawValue>)>);

impl<'de> Deserialize<'de> for Ordered {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Ordered;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a JSON object")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Ordered, A::Error> {
                let mut out = Vec::new();
                while let Some((k, v)) = map.next_entry::<String, Box<RawValue>>()? {
                    out.push((k, v));
                }
                Ok(Ordered(out))
            }
        }
        d.deserialize_map(V)
    }
}

impl Ordered {
    fn parse(text: &str) -> Result<Self, String> {
        serde_json::from_str(text)
            .map_err(|e| format!("it is not valid JSON ({})", e.classify_text()))
    }

    fn get(&self, key: &str) -> Option<&RawValue> {
        self.0
            .iter()
            .rev()
            .find(|(k, _)| k == key)
            .map(|(_, v)| &**v)
    }

    fn set(&mut self, key: &str, value: Box<RawValue>) {
        match self.0.iter_mut().find(|(k, _)| k == key) {
            Some(slot) => slot.1 = value,
            None => self.0.push((key.to_string(), value)),
        }
    }

    fn remove(&mut self, key: &str) {
        self.0.retain(|(k, _)| k != key);
    }

    /// Pretty text: one entry per line at `indent`, values verbatim.
    fn render(&self, indent: usize) -> String {
        if self.0.is_empty() {
            return "{}".into();
        }
        let pad = " ".repeat(indent + 2);
        let mut s = String::from("{\n");
        for (i, (k, v)) in self.0.iter().enumerate() {
            s.push_str(&pad);
            s.push_str(&serde_json::to_string(k).unwrap_or_default());
            s.push_str(": ");
            s.push_str(v.get());
            if i + 1 < self.0.len() {
                s.push(',');
            }
            s.push('\n');
        }
        s.push_str(&" ".repeat(indent));
        s.push('}');
        s
    }
}

trait ErrText {
    fn classify_text(&self) -> &'static str;
}

impl ErrText for serde_json::Error {
    fn classify_text(&self) -> &'static str {
        match self.classify() {
            serde_json::error::Category::Eof => "it ends early",
            serde_json::error::Category::Syntax => "a syntax error",
            serde_json::error::Category::Data => "it is not an object",
            serde_json::error::Category::Io => "it cannot be read",
        }
    }
}

fn raw(text: &str) -> Box<RawValue> {
    RawValue::from_string(text.to_string()).expect("valid JSON text")
}

// ------------------------------------------------------------- the entry

/// The entry setup writes.
pub fn entry_json(command: &Path, config_dir: Option<&Path>) -> Value {
    let mut args = vec![json!("mcp")];
    if let Some(d) = config_dir {
        args.push(json!("--config-dir"));
        args.push(json!(d.display().to_string()));
    }
    json!({"type": "stdio", "command": command.display().to_string(), "args": args, "env": {}})
}

fn entry_text(command: &Path, config_dir: Option<&Path>) -> String {
    let v = entry_json(command, config_dir);
    // Fixed key order: type, command, args, env.
    format!(
        "{{\n      \"type\": \"stdio\",\n      \"command\": {},\n      \"args\": {},\n      \"env\": {{}}\n    }}",
        v["command"], v["args"]
    )
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    Add,
    Update,
    Remove,
    None,
    Refuse,
}

#[derive(Debug, Clone, Serialize)]
pub struct HostPlan {
    pub id: &'static str,
    pub path: String,
    pub detected: bool,
    pub action: Action,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip)]
    new_text: Option<String>,
    #[serde(skip)]
    existed: bool,
}

fn detected(host: &str, path: &Path) -> bool {
    if path.is_file() {
        return true;
    }
    match host {
        // The app's own directory exists once it has been run.
        "claude-desktop" => path.parent().is_some_and(Path::is_dir),
        _ => false,
    }
}

fn plan_host(id: &'static str, path: &Path, opts: &Options, command: &Path) -> HostPlan {
    let mut plan = HostPlan {
        id,
        path: path.display().to_string(),
        detected: detected(id, path),
        action: Action::None,
        reason: None,
        new_text: None,
        existed: path.is_file(),
    };
    if !plan.detected {
        plan.reason = Some("not detected".into());
        return plan;
    }
    let text = if plan.existed {
        match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(_) => {
                plan.action = Action::Refuse;
                plan.reason = Some("the file cannot be read".into());
                return plan;
            }
        }
    } else {
        "{}".to_string()
    };
    let mut root = match Ordered::parse(&text) {
        Ok(o) => o,
        Err(why) => {
            plan.action = Action::Refuse;
            plan.reason = Some(format!("{why}; fix the file by hand"));
            return plan;
        }
    };
    let mut servers = match root.get("mcpServers") {
        Some(v) => match Ordered::parse(v.get()) {
            Ok(o) => o,
            Err(_) => {
                plan.action = Action::Refuse;
                plan.reason = Some("`mcpServers` is not an object".into());
                return plan;
            }
        },
        None => Ordered(Vec::new()),
    };
    let existing = servers.get(ENTRY_NAME).map(|v| v.get().to_string());
    let existing_value: Option<Value> = existing
        .as_deref()
        .and_then(|t| serde_json::from_str(t).ok());
    if opts.uninstall {
        match existing_value {
            None => plan.reason = Some("no entry".into()),
            Some(v) if is_ours(&v) => {
                servers.remove(ENTRY_NAME);
                root.set("mcpServers", raw(&servers.render(2)));
                plan.action = Action::Remove;
                plan.new_text = Some(root.render(0) + "\n");
            }
            Some(_) => plan.reason = Some("the entry was not written by Musaic".into()),
        }
        return plan;
    }
    let want = entry_text(command, opts.config_dir.as_deref());
    let want_value: Value = serde_json::from_str(&want).unwrap_or(Value::Null);
    match existing_value {
        Some(v) if !is_ours(&v) => {
            plan.action = Action::Refuse;
            plan.reason = Some(
                "an entry named `musaic` exists that Musaic did not write; rename or remove it, then run setup again"
                    .into(),
            );
            return plan;
        }
        Some(v) if v == want_value => {
            plan.reason = Some("already up to date".into());
            return plan;
        }
        Some(_) => plan.action = Action::Update,
        None => plan.action = Action::Add,
    }
    servers.set(ENTRY_NAME, raw(&want));
    root.set("mcpServers", raw(&servers.render(2)));
    plan.new_text = Some(root.render(0) + "\n");
    plan
}

fn civil_from_secs(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}{m:02}{d:02}T{:02}{:02}{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

fn stamp() -> String {
    let secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    civil_from_secs(secs)
}

/// One file as it was before setup touched it.
struct Written {
    path: PathBuf,
    backup: Option<PathBuf>,
}

fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    // A host config kept as a symlink (dotfiles) is updated where it points,
    // not replaced by a plain file.
    let resolved;
    let path = if path.is_symlink() {
        resolved = std::fs::canonicalize(path)?;
        resolved.as_path()
    } else {
        path
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // The replacement keeps the original's permissions: these files can hold
    // other servers' credentials (`env`), and a fresh file would get the
    // umask's (often world-readable) mode instead.
    // The temp file starts owner-only, so the text is never readable by
    // others while it is written.
    let before = std::fs::metadata(path).ok().map(|m| m.permissions());
    let tmp = path.with_extension(format!("musaic-tmp-{}", std::process::id()));
    let write = || -> std::io::Result<()> {
        use std::io::Write;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        opts.open(&tmp)?.write_all(text.as_bytes())?;
        if let Some(perms) = before {
            std::fs::set_permissions(&tmp, perms)?;
        }
        std::fs::rename(&tmp, path)
    };
    let result = write();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

fn rollback(done: &[Written]) {
    for w in done.iter().rev() {
        match &w.backup {
            Some(b) => {
                let _ = std::fs::copy(b, &w.path);
            }
            None => {
                let _ = std::fs::remove_file(&w.path);
            }
        }
    }
}

fn in_appimage(exe: &Path) -> bool {
    exe.to_string_lossy().contains("/.mount_")
}

/// Plan, and unless `--dry-run`, carry out.
pub fn run(opts: &Options) -> Outcome {
    let wanted: Vec<&'static str> = match opts.host.as_str() {
        "all" => vec!["claude-desktop", "claude-code"],
        "claude-desktop" => vec!["claude-desktop"],
        "claude-code" => vec!["claude-code"],
        other => {
            return crate::commands::usage(&format!(
                "unknown host {other}; use claude-desktop, claude-code or all"
            ));
        }
    };
    let exe = opts
        .executable
        .clone()
        .and_then(|p| p.canonicalize().ok().or(Some(p)))
        .unwrap_or_else(|| PathBuf::from("musaic-cli"));
    let manual = json!({"mcpServers": {ENTRY_NAME: entry_json(&exe, opts.config_dir.as_deref())}});
    let appimage = !opts.uninstall && in_appimage(&exe);
    let mut plans: Vec<HostPlan> = Vec::new();
    for id in wanted {
        let path = if id == "claude-desktop" {
            &opts.paths.claude_desktop
        } else {
            &opts.paths.claude_code
        };
        let mut p = plan_host(id, path, opts, &exe);
        if appimage && matches!(p.action, Action::Add | Action::Update) {
            p.action = Action::Refuse;
            p.reason = Some(
                "this musaic-cli is inside the AppImage, which is not at a stable path; copy it out (for example to ~/.local/bin) and run setup from there"
                    .into(),
            );
            p.new_text = None;
        }
        plans.push(p);
    }
    let refused = plans.iter().any(|p| p.action == Action::Refuse);
    let mut files = Vec::new();
    // Nothing is written if any host is refused: a half-done setup is worse
    // than none.
    if !opts.dry_run && !refused {
        let mut done: Vec<Written> = Vec::new();
        for (n, p) in plans.iter().filter(|p| p.new_text.is_some()).enumerate() {
            let path = PathBuf::from(&p.path);
            let backup = if p.existed {
                let b = PathBuf::from(format!("{}.musaic-backup-{}", p.path, stamp()));
                if std::fs::copy(&path, &b).is_err() {
                    rollback(&done);
                    return failure_outcome(&plans, &format!("{} could not be backed up", p.path));
                }
                Some(b)
            } else {
                None
            };
            let text = p.new_text.as_deref().unwrap_or_default();
            let result = if opts.fail_write_number == Some(n) {
                Err(std::io::Error::other("injected write failure"))
            } else {
                write_atomic(&path, text)
            };
            if result.is_err() {
                // The file in hand was not replaced; restore the ones before it.
                rollback(&done);
                if let Some(b) = &backup {
                    let _ = std::fs::remove_file(b);
                }
                return failure_outcome(
                    &plans,
                    &format!("{} could not be written; every file was restored", p.path),
                );
            }
            files.push(
                json!({"path": p.path, "backup": backup.as_ref().map(|b| b.display().to_string())}),
            );
            done.push(Written { path, backup });
        }
    } else if opts.dry_run {
        for p in plans.iter().filter(|p| p.new_text.is_some()) {
            files.push(json!({"path": p.path}));
        }
    }
    Outcome {
        json: json!({
            "ok": !refused,
            "dryRun": opts.dry_run,
            "hosts": plans.iter().map(|p| serde_json::to_value(p).unwrap_or(Value::Null)).collect::<Vec<_>>(),
            "files": files,
            "skills": [],
            "manualEntry": manual,
        }),
        code: if refused { EXIT_PROBLEMS } else { EXIT_OK },
    }
}

fn failure_outcome(plans: &[HostPlan], why: &str) -> Outcome {
    Outcome {
        json: json!({
            "ok": false,
            "error": {"code": "INTERNAL", "message": why},
            "hosts": plans.iter().map(|p| serde_json::to_value(p).unwrap_or(Value::Null)).collect::<Vec<_>>(),
            "files": [],
            "skills": [],
        }),
        code: EXIT_PROBLEMS,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn timestamps_are_utc_stamps() {
        assert_eq!(civil_from_secs(0), "19700101T000000Z");
        assert_eq!(civil_from_secs(1_800_000_000), "20270115T080000Z");
    }

    #[test]
    fn rendering_keeps_value_text_verbatim() {
        let o = Ordered::parse(r#"{ "b": [1,   2],"a": {"x":  1} }"#).unwrap();
        let text = o.render(0);
        assert!(text.contains(r#""b": [1,   2],"#));
        assert!(text.contains(r#""a": {"x":  1}"#));
        assert!(
            text.find("\"b\"").unwrap() < text.find("\"a\"").unwrap(),
            "order kept"
        );
    }
}
