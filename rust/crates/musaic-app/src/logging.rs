//! The app's log: written to a file, never with a key value in it.
//!
//! Two layers:
//!
//! * [`AppLog`], held by the [`crate::App`]: every line the app logs
//!   goes through it with a [`Level`] and a target (the Python app's logger
//!   name, `musaic.services.scan`, ...). It first replaces every secret
//!   value the app has seen ([`Masks`]) and every credential-shaped token
//!   ([`scrub_patterns`]) by `<redacted>`, then hands the line to the sink
//!   the host chose: the builder's `.log(...)` line seam (tests), or a
//!   [`LogSink`] (`.log_sink(...)`, the binary's [`ServerLog`]). Library
//!   users and tests choose their own sink; only the binary writes a file.
//! * [`ServerLog`], the binary's sink: the Python app's `_configure_logging`
//!   (musaic/main.py:131-191). One line per record,
//!   `%Y-%m-%d %H:%M:%S [LEVEL] logger: message`, to stderr (the Python app's
//!   `logging.StreamHandler()`, whose default stream is stderr) and to
//!   `<config>/server.log`, a rotating file: before a line that would bring
//!   a non-empty file to `max_bytes` (5 MiB) the file is renamed to
//!   `server.log.1` (`.1` -> `.2` ... up to `.5`, the oldest dropped), as
//!   `RotatingFileHandler(maxBytes=5*1024*1024, backupCount=5)` does. Level
//!   from `MUSAIC_LOG_LEVEL` (INFO by default; Python's names, an unknown one
//!   means INFO, as `getattr(logging, name, logging.INFO)`); the Python app's
//!   `MEDIA_MANAGER_LOG_LEVEL` fallback is not read (a deliberate change: the
//!   MEDIA_MANAGER names are dropped).
//!
//! Beyond the Python app:
//!
//! * Writing never blocks or fails the app: records go through a bounded
//!   queue to one writer thread (a full queue drops the record and counts
//!   it; the count is written when the queue drains). A file that cannot be
//!   opened (read-only config dir) leaves the console only, with one
//!   warning; a failing write (disk full) is reported once on stderr and
//!   the next records are tried again.
//! * On Unix the file (and every rotated generation, renames keep the mode)
//!   is user-only (0600); an existing file with wider permissions is
//!   narrowed when opened.
//! * Timestamps are UTC (the Python app's `asctime` is local time; no time zone
//!   database is linked into this build). Candidate unknown.

use std::borrow::Cow;
use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, OnceLock, PoisonError, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use musaic_secrets::{SecretStore, StoreError};

/// The file name in the config dir (`server.log`).
pub const FILE_NAME: &str = "server.log";
/// `maxBytes=5 * 1024 * 1024`.
pub const MAX_BYTES: u64 = 5 * 1024 * 1024;
/// `backupCount=5`.
pub const BACKUPS: usize = 5;
/// Records waiting for the writer thread before new ones are dropped.
pub const QUEUE: usize = 4096;
/// The level variable (`MEDIA_MANAGER_LOG_LEVEL` is not read).
pub const LEVEL_ENV: &str = "MUSAIC_LOG_LEVEL";
/// What replaces a secret in a log line.
pub const REDACTED: &str = "<redacted>";

/// Python's logging levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Debug = 10,
    Info = 20,
    Warning = 30,
    Error = 40,
    Critical = 50,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Debug => "DEBUG",
            Level::Info => "INFO",
            Level::Warning => "WARNING",
            Level::Error => "ERROR",
            Level::Critical => "CRITICAL",
        }
    }
}

/// `getattr(logging, name.upper(), logging.INFO)` for the names a level
/// variable can hold: the threshold (a record at or above it is written).
/// `NOTSET` (0) lets everything through. `None` for a name Python would
/// not resolve (the caller decides: the variable falls back to INFO as in
/// the Python app; the `--log-level` flag refuses it).
pub fn parse_level(name: &str) -> Option<u8> {
    match name.to_ascii_uppercase().as_str() {
        "NOTSET" => Some(0),
        "DEBUG" => Some(10),
        "INFO" => Some(20),
        "WARNING" | "WARN" => Some(30),
        "ERROR" => Some(40),
        "CRITICAL" | "FATAL" => Some(50),
        _ => None,
    }
}

/// The threshold a `MUSAIC_LOG_LEVEL` value means: its level, else INFO
/// (`getattr(logging, name.upper(), logging.INFO)`; testdata/golden/logging/
/// rotation.json `level_of_name`).
pub fn level_from_env_value(value: &str) -> u8 {
    parse_level(value).unwrap_or(Level::Info as u8)
}

/// Where structured records go: (level, logger name, message).
pub type LogSink = Arc<dyn Fn(Level, &str, &str) + Send + Sync>;

/// The secret values the app has seen (read from or written to its secret
/// store, or found in the environment). Shared by the app's log and the
/// binary's file sink.
#[derive(Clone, Default)]
pub struct Masks(Arc<RwLock<Vec<String>>>);

/// Values shorter than this are not masked as substrings (they would
/// shred ordinary words); no real key is this short.
const MIN_MASK_LEN: usize = 6;

impl Masks {
    pub fn new() -> Masks {
        Masks::default()
    }

    /// Remember `value` (and its trimmed form) as a secret.
    pub fn add(&self, value: &str) {
        let mut g = self.0.write().unwrap_or_else(PoisonError::into_inner);
        for v in [value, value.trim()] {
            if v.chars().count() >= MIN_MASK_LEN && !g.iter().any(|x| x == v) {
                g.push(v.to_string());
            }
        }
        // Longest first, so a value that contains another is masked whole.
        g.sort_by_key(|s| std::cmp::Reverse(s.len()));
    }

    /// `line` with every remembered value and every credential-shaped token
    /// replaced by [`REDACTED`].
    pub fn apply<'a>(&self, line: &'a str) -> Cow<'a, str> {
        let g = self.0.read().unwrap_or_else(PoisonError::into_inner);
        let mut out: Cow<'a, str> = Cow::Borrowed(line);
        for v in g.iter() {
            if out.contains(v.as_str()) {
                out = Cow::Owned(out.replace(v.as_str(), REDACTED));
            }
        }
        drop(g);
        match scrub_patterns(&out) {
            Some(s) => Cow::Owned(s),
            None => out,
        }
    }
}

/// Query parameters whose value is a credential (`client` is AcoustID's).
const SECRET_PARAMS: &[&str] = &[
    "client",
    "key",
    "api_key",
    "apikey",
    "token",
    "access_token",
    "password",
];
/// Header names whose value is a credential.
const SECRET_HEADERS: &[&str] = &[
    "authorization",
    "proxy-authorization",
    "x-api-key",
    "api-key",
    "anthropic-api-key",
];
/// Token prefixes of well-known API keys.
const KEY_PREFIXES: &[&str] = &["sk-ant-", "sk-proj-", "sk-", "fc-"];

fn is_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~' | '+' | '/' | '=')
}

/// Credential-shaped tokens a line can carry without the app having seen
/// the value: `?client=…` / `&key=…` query values, `Authorization: …` and
/// friends (header form or JSON `"authorization": "…"`), `Bearer …`, and
/// tokens starting with a well-known key prefix. `None` when nothing
/// matched.
pub fn scrub_patterns(line: &str) -> Option<String> {
    let lower = line.to_ascii_lowercase();
    // Byte ranges to replace.
    let mut cuts: Vec<(usize, usize)> = Vec::new();
    let value_end = |from: usize, stop: &dyn Fn(char) -> bool| -> usize {
        line[from..]
            .char_indices()
            .find(|&(_, c)| stop(c))
            .map_or(line.len(), |(i, _)| from + i)
    };
    for p in SECRET_PARAMS {
        let pat = format!("{p}=");
        let mut at = 0;
        while let Some(i) = lower[at..].find(&pat) {
            let start = at + i;
            at = start + pat.len();
            let prev = line[..start].chars().next_back();
            if !matches!(prev, Some('?' | '&' | ';' | ' ' | '"' | '\'')) {
                continue;
            }
            let end = value_end(at, &|c| {
                matches!(c, '&' | ' ' | '"' | '\'' | '#' | ',' | ')' | '\n' | '\r')
            });
            if end > at && line[at..end] != *REDACTED {
                cuts.push((at, end));
            }
        }
    }
    for h in SECRET_HEADERS {
        let mut at = 0;
        while let Some(i) = lower[at..].find(h) {
            let start = at + i;
            at = start + h.len();
            let prev = line[..start].chars().next_back();
            if prev.is_some_and(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
                continue;
            }
            // `name: value`, `name=value`, `"name": "value"`.
            let rest = &line[at..];
            let skip = rest
                .char_indices()
                .find(|&(_, c)| !matches!(c, '"' | '\'' | ':' | '=' | ' '))
                .map_or(rest.len(), |(i, _)| i);
            if !rest[..skip].contains([':', '=']) {
                continue;
            }
            let from = at + skip;
            let end = value_end(from, &|c| matches!(c, '"' | '\'' | ',' | '\n' | '\r' | '}'));
            if end > from {
                cuts.push((from, end));
            }
        }
    }
    let mut at = 0;
    while let Some(i) = lower[at..].find("bearer ") {
        let from = at + i + "bearer ".len();
        at = from;
        let end = value_end(from, &|c| !is_token_char(c));
        if end > from {
            cuts.push((from, end));
        }
    }
    for p in KEY_PREFIXES {
        let mut at = 0;
        while let Some(i) = line[at..].find(p) {
            let start = at + i;
            at = start + p.len();
            let prev = line[..start].chars().next_back();
            if prev.is_some_and(is_token_char) {
                continue;
            }
            let end = value_end(start, &|c| !is_token_char(c));
            // A key, not a word: at least 16 characters after the prefix.
            if end - start >= p.len() + 16 {
                cuts.push((start, end));
            }
        }
    }
    if cuts.is_empty() {
        return None;
    }
    cuts.sort();
    let mut out = String::with_capacity(line.len());
    let mut pos = 0;
    for (s, e) in cuts {
        if e <= pos {
            continue;
        }
        let s = s.max(pos);
        out.push_str(&line[pos..s]);
        out.push_str(REDACTED);
        pos = e;
    }
    out.push_str(&line[pos..]);
    Some(out)
}

/// A secret store that remembers every value it reads or writes in
/// [`Masks`], so no later log line can carry it.
pub struct MaskingStore {
    inner: crate::SharedSecrets,
    masks: Masks,
}

impl MaskingStore {
    pub fn new(inner: crate::SharedSecrets, masks: Masks) -> MaskingStore {
        MaskingStore { inner, masks }
    }
}

impl SecretStore for MaskingStore {
    fn get(&self, key: &str) -> Result<Option<String>, StoreError> {
        let v = self.inner.get(key)?;
        if let Some(v) = &v {
            self.masks.add(v);
        }
        Ok(v)
    }
    fn set(&self, key: &str, value: &str) -> Result<(), StoreError> {
        self.masks.add(value);
        self.inner.set(key, value)
    }
    fn delete(&self, key: &str) -> Result<(), StoreError> {
        self.inner.delete(key)
    }
}

/// The app's log: masks, then the host's sink.
#[derive(Clone)]
pub struct AppLog {
    sink: LogSink,
    masks: Masks,
}

impl AppLog {
    pub fn new(sink: LogSink, masks: Masks) -> AppLog {
        AppLog { sink, masks }
    }

    /// A line sink (the builder's `.log(...)` seam): `[LEVEL] logger: message`.
    pub fn line_sink(l: musaic_services::keyscrub::Logger) -> LogSink {
        Arc::new(move |level: Level, target: &str, msg: &str| {
            l(&format!("[{}] {target}: {msg}", level.as_str()));
        })
    }

    /// The library default: stderr, no timestamp.
    pub fn stderr_sink() -> LogSink {
        Arc::new(|level: Level, target: &str, msg: &str| {
            eprintln!("[{}] {target}: {msg}", level.as_str());
        })
    }

    pub fn masks(&self) -> &Masks {
        &self.masks
    }

    pub fn log(&self, level: Level, target: &str, msg: &str) {
        let m = self.masks.apply(msg);
        (self.sink)(level, target, &m);
    }
    pub fn info(&self, target: &str, msg: &str) {
        self.log(Level::Info, target, msg);
    }
    pub fn warn(&self, target: &str, msg: &str) {
        self.log(Level::Warning, target, msg);
    }
    pub fn error(&self, target: &str, msg: &str) {
        self.log(Level::Error, target, msg);
    }
}

/// The services log through the app's log (masked, then the host's sink).
impl musaic_services::host_log::HostLog for AppLog {
    fn info(&self, target: &str, msg: &str) {
        AppLog::info(self, target, msg);
    }
    fn warn(&self, target: &str, msg: &str) {
        AppLog::warn(self, target, msg);
    }
    fn error(&self, target: &str, msg: &str) {
        AppLog::error(self, target, msg);
    }
}

// ------------------------------------------------------------- the file sink

enum Msg {
    /// A formatted line; `true`: also to the console.
    Line(String, bool),
    Flush(SyncSender<()>),
}

/// Options of [`ServerLog::start`] (tests shrink the rotation size and fix
/// the clock).
#[derive(Clone)]
pub struct ServerLogOptions {
    pub max_bytes: u64,
    pub backups: usize,
    /// Also write every line to stderr.
    pub console: bool,
    pub now: Arc<dyn Fn() -> SystemTime + Send + Sync>,
}

impl Default for ServerLogOptions {
    fn default() -> Self {
        ServerLogOptions {
            max_bytes: MAX_BYTES,
            backups: BACKUPS,
            console: true,
            now: Arc::new(SystemTime::now),
        }
    }
}

/// The binary's log sink: stderr + rotating `<config>/server.log`.
pub struct ServerLog {
    tx: SyncSender<Msg>,
    now: Arc<dyn Fn() -> SystemTime + Send + Sync>,
    threshold: u8,
    masks: Masks,
    dropped: AtomicU64,
    path: Option<PathBuf>,
}

impl ServerLog {
    /// Open `<config_dir>/server.log` (creating the dir, as the Python app's
    /// `CONFIG_DIR.mkdir(parents=True, exist_ok=True)`) and start the
    /// writer thread. Never fails: the second value is the file's path, or
    /// why there is no file (the console still works).
    pub fn start(
        config_dir: &Path,
        threshold: u8,
        masks: Masks,
        opts: ServerLogOptions,
    ) -> (Arc<ServerLog>, Result<PathBuf, String>) {
        let path = config_dir.join(FILE_NAME);
        let file = std::fs::create_dir_all(config_dir)
            .and_then(|()| open_log(&path))
            .map_err(|e| e.to_string());
        let opened = match &file {
            Ok(_) => Ok(path.clone()),
            Err(e) => Err(e.clone()),
        };
        let (tx, rx) = sync_channel::<Msg>(QUEUE);
        let writer = Writer {
            file: file.ok().map(|f| Rotating::new(path.clone(), f, &opts)),
            console: opts.console,
            reported: AtomicBool::new(false),
        };
        let spawned = std::thread::Builder::new()
            .name("server-log".into())
            .spawn(move || writer.run(rx));
        let opened = match spawned {
            Ok(_) => opened,
            Err(e) => Err(format!("cannot start the log writer: {e}")),
        };
        let log = Arc::new(ServerLog {
            tx,
            now: opts.now.clone(),
            threshold,
            masks,
            dropped: AtomicU64::new(0),
            path: opened.as_ref().ok().cloned(),
        });
        (log, opened)
    }

    /// The file being written, if any.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn enabled(&self, level: Level) -> bool {
        level as u8 >= self.threshold
    }

    /// Queue one record (masked, formatted); never blocks.
    pub fn log(&self, level: Level, target: &str, msg: &str) {
        self.record(level, target, msg, true);
    }

    /// As [`Self::log`], the file only (the binary's last line before a
    /// failed start, which it prints itself).
    pub fn log_file_only(&self, level: Level, target: &str, msg: &str) {
        self.record(level, target, msg, false);
    }

    fn record(&self, level: Level, target: &str, msg: &str, console: bool) {
        if !self.enabled(level) {
            return;
        }
        let msg = self.masks.apply(msg);
        let line = format!(
            "{} [{}] {target}: {msg}\n",
            timestamp((self.now)()),
            level.as_str()
        );
        self.send(line, console);
    }

    fn send(&self, line: String, console: bool) {
        let dropped = self.dropped.swap(0, Ordering::SeqCst);
        if dropped > 0 {
            let note = format!(
                "{} [WARNING] musaic.log: {dropped} log record(s) dropped (writer behind)\n",
                timestamp((self.now)())
            );
            if self.tx.try_send(Msg::Line(note, true)).is_err() {
                self.dropped.fetch_add(dropped, Ordering::SeqCst);
            }
        }
        match self.tx.try_send(Msg::Line(line, console)) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                self.dropped.fetch_add(1, Ordering::SeqCst);
            }
            Err(TrySendError::Disconnected(_)) => {}
        }
    }

    /// Wait (up to `timeout`) until every queued record is written.
    pub fn flush(&self, timeout: Duration) -> bool {
        let (tx, rx) = sync_channel(1);
        if self.tx.send(Msg::Flush(tx)).is_err() {
            return false;
        }
        rx.recv_timeout(timeout).is_ok()
    }

    /// This log as a [`LogSink`] for the app builder.
    pub fn sink(self: &Arc<Self>) -> LogSink {
        let me = Arc::clone(self);
        Arc::new(move |level: Level, target: &str, msg: &str| me.log(level, target, msg))
    }
}

/// `open(path, "a")`, user-only on Unix.
fn open_log(path: &Path) -> std::io::Result<File> {
    let mut o = OpenOptions::new();
    o.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        o.mode(0o600);
        let f = o.open(path)?;
        let m = f.metadata()?;
        if m.permissions().mode() & 0o077 != 0 {
            f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(f)
    }
    #[cfg(not(unix))]
    {
        o.open(path)
    }
}

struct Rotating {
    path: PathBuf,
    file: Option<File>,
    size: u64,
    max_bytes: u64,
    backups: usize,
}

impl Rotating {
    fn new(path: PathBuf, file: File, opts: &ServerLogOptions) -> Rotating {
        let size = file.metadata().map(|m| m.len()).unwrap_or(0);
        Rotating {
            path,
            file: Some(file),
            size,
            max_bytes: opts.max_bytes,
            backups: opts.backups,
        }
    }

    fn generation(&self, n: usize) -> PathBuf {
        let mut s = self.path.clone().into_os_string();
        s.push(format!(".{n}"));
        PathBuf::from(s)
    }

    /// `RotatingFileHandler.doRollover`.
    fn rollover(&mut self) -> std::io::Result<()> {
        self.file = None;
        if self.backups > 0 {
            for i in (1..self.backups).rev() {
                let src = self.generation(i);
                if src.exists() {
                    let dst = self.generation(i + 1);
                    if dst.exists() {
                        std::fs::remove_file(&dst)?;
                    }
                    std::fs::rename(&src, &dst)?;
                }
            }
            let dst = self.generation(1);
            if dst.exists() {
                std::fs::remove_file(&dst)?;
            }
            if self.path.exists() {
                std::fs::rename(&self.path, &dst)?;
            }
        } else {
            let _ = std::fs::remove_file(&self.path);
        }
        Ok(())
    }

    fn write(&mut self, line: &str) -> std::io::Result<()> {
        let n = line.len() as u64;
        // `shouldRollover` (Python 3.13): never an empty file; else when
        // `stream.tell() + len(msg) >= maxBytes`, `tell()` in bytes and
        // `len(msg)` in characters, as Python counts them.
        let chars = line.chars().count() as u64;
        let mut rolled = Ok(());
        if self.max_bytes > 0 && self.size > 0 && self.size + chars >= self.max_bytes {
            // Whatever happens, the line is written to the base name; a
            // failed rollover is reported after it.
            rolled = self.rollover();
        }
        if self.file.is_none() {
            let f = open_log(&self.path)?;
            self.size = f.metadata().map(|m| m.len()).unwrap_or(0);
            self.file = Some(f);
        }
        let f = self.file.as_mut().expect("opened above");
        match f.write_all(line.as_bytes()) {
            Ok(()) => {
                self.size += n;
                rolled
            }
            Err(e) => {
                // Partial writes leave the size unknown: re-read it, and
                // reopen next time.
                self.size = std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0);
                self.file = None;
                Err(e)
            }
        }
    }
}

struct Writer {
    file: Option<Rotating>,
    console: bool,
    reported: AtomicBool,
}

impl Writer {
    fn run(mut self, rx: Receiver<Msg>) {
        for m in rx {
            match m {
                Msg::Line(line, console) => {
                    if self.console && console {
                        let _ = std::io::stderr().write_all(line.as_bytes());
                    }
                    if let Some(f) = &mut self.file {
                        match f.write(&line) {
                            Ok(()) => {
                                self.reported.store(false, Ordering::SeqCst);
                            }
                            Err(e) => {
                                if !self.reported.swap(true, Ordering::SeqCst) {
                                    let _ = writeln!(
                                        std::io::stderr(),
                                        "[musaic] Warning: couldn't write or rotate {} ({e}); continuing",
                                        f.path.display()
                                    );
                                }
                            }
                        }
                    }
                }
                Msg::Flush(done) => {
                    let _ = done.try_send(());
                }
            }
        }
    }
}

/// `%Y-%m-%d %H:%M:%S` in UTC.
pub fn timestamp(t: SystemTime) -> String {
    let secs = t
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}",
        rem / 3600,
        (rem / 60) % 60,
        rem % 60
    )
}

/// Days since 1970-01-01 -> (year, month, day), proleptic Gregorian
/// (Howard Hinnant's `civil_from_days`).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

// ------------------------------------------------------------- process-wide

static GLOBAL: OnceLock<LogSink> = OnceLock::new();

/// The process-wide sink for lines logged where no app state is at hand
/// (an internal server error, a caught panic). Set once by the binary;
/// without it those lines go to stderr.
pub fn set_global(sink: LogSink) {
    let _ = GLOBAL.set(sink);
}

/// Values of the environment's credential-looking variables (the provider
/// SDKs read some of them): kept out of the log.
pub fn mask_env(masks: &Masks) {
    for (k, v) in std::env::vars() {
        let k = k.to_ascii_uppercase();
        if ["KEY", "TOKEN", "SECRET", "PASSWORD"]
            .iter()
            .any(|w| k.contains(w))
        {
            masks.add(&v);
        }
    }
}

/// Panics reach the log (with a backtrace when `RUST_BACKTRACE` asks for
/// one) instead of only the console.
pub fn log_panics(log: Arc<ServerLog>) {
    std::panic::set_hook(Box::new(move |info| {
        let p = info.payload();
        let msg = p
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| p.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "panic".to_string());
        let thread = std::thread::current();
        let name = thread.name().unwrap_or("unnamed");
        let at = info
            .location()
            .map_or_else(String::new, |l| format!(" at {}:{}", l.file(), l.line()));
        let bt = std::backtrace::Backtrace::capture();
        let bt = match bt.status() {
            std::backtrace::BacktraceStatus::Captured => format!("\n{bt}"),
            _ => String::new(),
        };
        log.log(
            Level::Error,
            "musaic.panic",
            &format!("thread '{name}' panicked{at}: {msg}{bt}"),
        );
    }));
}

/// The process log a host (the server binary, the desktop app) starts once.
pub struct ProcessLog {
    pub log: Arc<ServerLog>,
    /// Shared with the app state (`AppBuilder::log_masks`): every
    /// secret the app reads or writes is masked in the file.
    pub masks: Masks,
    /// The file's path, or why there is none (the console still works).
    pub opened: Result<PathBuf, String>,
}

/// Start `<config_dir>/server.log` the way the server binary does
/// (`_configure_logging`, musaic/main.py:131-191): masks seeded from the
/// environment's credential-looking variables, the rotating file plus
/// stderr, the process-wide sink ([`set_global`]) and the lower crates'
/// sink (`musaic_log`) pointed at it, and panics logged. Call once per
/// process; later calls still return a working log, but the process-wide
/// sinks and the panic hook stay with the first.
pub fn start_process_log(config_dir: &Path, threshold: u8, opts: ServerLogOptions) -> ProcessLog {
    let masks = Masks::new();
    mask_env(&masks);
    let (log, opened) = ServerLog::start(config_dir, threshold, masks.clone(), opts);
    set_global(log.sink());
    let lower = log.clone();
    musaic_log::set_sink(Arc::new(move |level, target, msg| {
        let level = match level {
            musaic_log::Level::Debug => Level::Debug,
            musaic_log::Level::Warning => Level::Warning,
            musaic_log::Level::Error => Level::Error,
        };
        lower.log(level, target, msg);
    }));
    log_panics(log.clone());
    ProcessLog { log, masks, opened }
}

/// Log through the process-wide sink (stderr when the binary set none).
pub fn global(level: Level, target: &str, msg: &str) {
    match GLOBAL.get() {
        Some(s) => s(level, target, msg),
        None => eprintln!("[{}] {target}: {msg}", level.as_str()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_dates() {
        // Pinned against Python: datetime.utcfromtimestamp(0) and
        // datetime(2026, 11, 11, 19, 17, 9, tzinfo=utc).timestamp().
        assert_eq!(timestamp(UNIX_EPOCH), "1970-01-01 00:00:00");
        assert_eq!(
            timestamp(UNIX_EPOCH + Duration::from_secs(1_794_424_629)),
            "2026-11-11 19:17:09"
        );
        assert_eq!(
            timestamp(UNIX_EPOCH + Duration::from_secs(951_782_400)),
            "2000-02-29 00:00:00"
        );
    }

    #[test]
    fn levels_follow_python_names() {
        assert_eq!(parse_level("debug"), Some(10));
        assert_eq!(parse_level("WARN"), Some(30));
        assert_eq!(parse_level("fatal"), Some(50));
        assert_eq!(parse_level("NOTSET"), Some(0));
        assert_eq!(parse_level("verbose"), None);
        assert_eq!(parse_level(" info "), None);
    }

    #[test]
    fn patterns_redact_credentials() {
        let cases = [
            (
                "GET https://api.acoustid.org/v2/lookup?client=AbCdEf123&meta=x",
                "GET https://api.acoustid.org/v2/lookup?client=<redacted>&meta=x",
            ),
            (
                "Authorization: Bearer abc.def-ghi",
                "Authorization: <redacted>",
            ),
            (
                r#"{"x-api-key": "k-123456789", "a": 1}"#,
                r#"{"x-api-key": "<redacted>", "a": 1}"#,
            ),
            (
                "invalid key sk-ant-api03-ABCDEFGHIJKLMNOPQRSTUV (401)",
                "invalid key <redacted> (401)",
            ),
            ("token bearer XYZ123", "token bearer <redacted>"),
        ];
        for (inp, want) in cases {
            assert_eq!(scrub_patterns(inp).as_deref(), Some(want), "{inp}");
        }
        for clean in [
            "check tags-out-of-sync completed in 12ms (3 findings, scope=all)",
            "the key acoustid_api_key was saved",
            "monkey=1 risk-free",
        ] {
            assert_eq!(scrub_patterns(clean), None, "{clean}");
        }
    }

    #[test]
    fn masks_replace_seen_values_longest_first() {
        let m = Masks::new();
        m.add("abc");
        m.add("secret-value-1");
        m.add("secret-value-12");
        assert_eq!(
            m.apply("a secret-value-12 and secret-value-1 and abc"),
            "a <redacted> and <redacted> and abc"
        );
    }
}
