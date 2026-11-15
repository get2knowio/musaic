//! The `musaic-api` binary's command line and startup failures (the
//! server.log file; the Python app's `musaic/main.py:run` for `--host` / `--port`
//! / `MUSAIC_HOST` / `MUSAIC_PORT` and its `_configure_logging` line
//! `[musaic] Logging to … (level=…)`). Every run is the real binary on
//! 127.0.0.1 with a temp config dir, `--verification-memory-secrets` and
//! `--network offline` (no keychain, no network).
//!
//! Pinned: `--help` names every flag and variable and every flag it names
//! is accepted; usage errors exit 2, startup failures exit 1, each with one
//! `[musaic] …` line on stderr (the log's own lines, timestamped, may come
//! before it); a clean start writes the Python app's log line on stdout and a
//! user-only server.log, and Ctrl-C (SIGINT) exits 0.
//! Deliberate (no Python counterpart): the Python app's argparse exits 2 on an
//! unknown flag as here, but its MUSAIC_PORT parse error is a traceback
//! (exit 1); here it is a usage error (2) with one line.

use std::path::Path;
use std::process::{Command, Output, Stdio};
#[cfg(unix)]
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_musaic-api");

fn cmd(cfg: &Path) -> Command {
    let mut c = cmd_no_host(cfg);
    c.args(["--host", "127.0.0.1"]);
    c
}

fn cmd_no_host(cfg: &Path) -> Command {
    let mut c = Command::new(BIN);
    for (k, _) in std::env::vars() {
        if k.starts_with("MUSAIC_") || k.starts_with("MEDIA_MANAGER_") {
            c.env_remove(k);
        }
    }
    c.arg("--config-dir")
        .arg(cfg)
        .arg("--verification-memory-secrets")
        .stdin(Stdio::null());
    c
}

fn run(c: &mut Command) -> Output {
    c.output().expect("run musaic-api")
}

/// The one `[musaic]` line of stderr, which must be its last line.
fn failure_line(o: &Output) -> String {
    let err = String::from_utf8_lossy(&o.stderr);
    let ours: Vec<&str> = err.lines().filter(|l| l.starts_with("[musaic] ")).collect();
    assert_eq!(ours.len(), 1, "one failure line: {err}");
    assert_eq!(err.trim_end().lines().last(), Some(ours[0]), "{err}");
    ours[0].to_string()
}

#[test]
fn help_names_every_flag_and_variable_and_every_flag_is_accepted() {
    let td = tempfile::tempdir().unwrap();
    let o = run(Command::new(BIN).arg("--help"));
    assert_eq!(o.status.code(), Some(0));
    let help = String::from_utf8(o.stdout).unwrap();
    for needle in [
        "--host",
        "--port",
        "--config-dir",
        "--network",
        "--replay-dir",
        "--net-log",
        "--verification-memory-secrets",
        "--log-level",
        "--help",
        "--version",
        "MUSAIC_HOST",
        "MUSAIC_PORT",
        "MUSAIC_CONFIG_DIR",
        "MUSAIC_NETWORK",
        "MUSAIC_REPLAY_DIR",
        "MUSAIC_NET_LOG_DIR",
        "MUSAIC_LOG_LEVEL",
        "MUSAIC_ALLOWED_HOSTS",
        "MUSAIC_FRONTEND_DIR",
        "server.log",
        "live | offline | replay",
        "DEBUG | INFO | WARNING | ERROR | CRITICAL",
    ] {
        assert!(help.contains(needle), "--help lacks {needle}:\n{help}");
    }
    assert!(
        !help.contains("MEDIA_MANAGER"),
        "no MEDIA_MANAGER_* variables"
    );
    // Every flag the help names parses: given a bad value where it takes
    // one, the refusal is about the value, never "unknown argument".
    let flags: Vec<String> = help
        .split_whitespace()
        .filter(|w| w.starts_with("--"))
        .map(|w| w.trim_end_matches(',').to_string())
        .collect();
    assert!(flags.len() >= 10, "{flags:?}");
    for f in flags
        .into_iter()
        .filter(|f| f != "--help" && f != "--version")
    {
        // `--flag=probe`: a bad or repeated value, never a server start.
        let o = run(cmd(&td.path().join("cfg")).arg(format!("{f}=probe")));
        assert_ne!(o.status.code(), Some(1), "{f}: {o:?}");
        let err = String::from_utf8_lossy(&o.stderr);
        assert!(!err.contains("unknown argument"), "{f}: {err}");
    }
    let o = run(Command::new(BIN).arg("-V"));
    assert_eq!(o.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&o.stdout).starts_with("musaic-api "));
}

#[test]
fn usage_errors_exit_2_with_one_line() {
    let td = tempfile::tempdir().unwrap();
    let cfg = td.path().join("cfg");
    let replay = td.path().join("replay");
    std::fs::create_dir(&replay).unwrap();
    let r = replay.to_string_lossy().into_owned();
    type Case<'a> = (Vec<&'a str>, Vec<(&'a str, &'a str)>, &'a str);
    let cases: Vec<Case> = vec![
        (
            vec!["--network", "offline", "--bogus"],
            vec![],
            "unknown argument \"--bogus\"",
        ),
        (
            vec!["--network", "offline", "stray"],
            vec![],
            "unknown argument \"stray\"",
        ),
        (
            vec!["--network", "offline", "--port"],
            vec![],
            "--port needs a value",
        ),
        (
            vec!["--network", "offline", "--port", "http"],
            vec![],
            "--port \"http\" is not a port number",
        ),
        (
            vec!["--network", "offline", "--port", "70000"],
            vec![],
            "is not a port number",
        ),
        (
            vec!["--network", "offline"],
            vec![("MUSAIC_PORT", "eighty")],
            "MUSAIC_PORT=\"eighty\" is not a port number",
        ),
        (
            vec!["--network", "offline", "--port", "1", "--port", "2"],
            vec![],
            "--port given more than once",
        ),
        (
            vec!["--network", "offline", "--log-level", "loud"],
            vec![],
            "--log-level \"loud\" is not a level",
        ),
        (
            vec!["--network", "offline", "--replay-dir", &r],
            vec![],
            "--replay-dir is only valid with --network replay",
        ),
        (
            vec!["--network", "live", "--net-log", &r],
            vec![],
            "--net-log is only valid with --network offline|replay",
        ),
        (
            vec!["--network", "replay"],
            vec![],
            "--network replay needs --replay-dir",
        ),
        (
            vec!["--network", "carrier-pigeon"],
            vec![],
            "unknown --network \"carrier-pigeon\"",
        ),
        (
            vec!["--network", "offline", "--verification-memory-secrets"],
            vec![],
            "--verification-memory-secrets given more than once",
        ),
        (
            vec!["--network", "offline", "--host", ""],
            vec![],
            "--host must not be empty",
        ),
    ];
    for (args, envs, want) in cases {
        let mut c = if args.contains(&"--host") {
            cmd_no_host(&cfg)
        } else {
            cmd(&cfg)
        };
        c.args(&args);
        for (k, v) in envs {
            c.env(k, v);
        }
        let o = run(&mut c);
        assert_eq!(o.status.code(), Some(2), "{args:?}: {o:?}");
        let line = failure_line(&o);
        assert!(line.contains(want), "{args:?}: {line}");
        // Refused before anything is created.
        assert!(!cfg.exists(), "{args:?} created the config dir");
    }
    // The memory store refuses a config dir outside temp / `work`.
    let o = run(Command::new(BIN).args([
        "--config-dir",
        "/nonexistent-musaic-cfg",
        "--verification-memory-secrets",
        "--network",
        "offline",
    ]));
    assert_eq!(o.status.code(), Some(2));
    assert!(failure_line(&o).contains("--verification-memory-secrets refused"));
}

#[test]
fn port_in_use_exits_1() {
    let td = tempfile::tempdir().unwrap();
    let held = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = held.local_addr().unwrap().port().to_string();
    let o = run(cmd(&td.path().join("cfg")).args(["--network", "offline", "--port", &port]));
    assert_eq!(o.status.code(), Some(1), "{o:?}");
    let line = failure_line(&o);
    assert!(
        line.starts_with(&format!("[musaic] cannot bind 127.0.0.1:{port}: ")),
        "{line}"
    );
    // Bound first: no database was created.
    assert!(!td.path().join("cfg/library.db").exists());
}

#[cfg(unix)]
#[test]
fn unwritable_config_dir_exits_1() {
    use std::os::unix::fs::PermissionsExt;
    let td = tempfile::tempdir().unwrap();
    let cfg = td.path().join("cfg");
    std::fs::create_dir(&cfg).unwrap();
    std::fs::set_permissions(&cfg, std::fs::Permissions::from_mode(0o500)).unwrap();
    let o = run(cmd(&cfg).args(["--network", "offline", "--port", "0"]));
    std::fs::set_permissions(&cfg, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(o.status.code(), Some(1), "{o:?}");
    let line = failure_line(&o);
    assert!(
        line.starts_with("[musaic] startup failed: database: "),
        "{line}"
    );
    // The log could not be opened: the Python app's warning, on stdout.
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(
        out.contains("[musaic] Warning: couldn't set up file log ("),
        "{out}"
    );
}

#[test]
fn database_newer_than_this_build_exits_1() {
    let td = tempfile::tempdir().unwrap();
    let cfg = td.path().join("cfg");
    std::fs::create_dir(&cfg).unwrap();
    let c = rusqlite::Connection::open(cfg.join("library.db")).unwrap();
    c.execute_batch(&format!(
        "CREATE TABLE schema_version (version INTEGER NOT NULL); INSERT INTO schema_version VALUES ({});",
        musaic_db::SCHEMA_VERSION + 1
    ))
    .unwrap();
    drop(c);
    let o = run(cmd(&cfg).args(["--network", "offline", "--port", "0"]));
    assert_eq!(o.status.code(), Some(1), "{o:?}");
    let line = failure_line(&o);
    assert!(
        line.contains(&format!(
            "database schema v{} is newer than this build understands (v{})",
            musaic_db::SCHEMA_VERSION + 1,
            musaic_db::SCHEMA_VERSION
        )),
        "{line}"
    );
    // The refusal is in the log file too.
    let log = std::fs::read_to_string(cfg.join("server.log")).unwrap();
    assert!(
        log.contains("[CRITICAL] musaic.main: startup failed:"),
        "{log}"
    );
}

#[cfg(unix)]
#[test]
fn a_clean_start_logs_and_sigint_exits_0() {
    use std::io::{BufRead, BufReader};
    use std::os::unix::fs::PermissionsExt;
    let td = tempfile::tempdir().unwrap();
    let cfg = td.path().join("cfg");
    let mut child = cmd(&cfg)
        .args(["--network", "offline", "--port", "0"])
        .env("MUSAIC_LOG_LEVEL", "debug")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut out = BufReader::new(child.stdout.take().unwrap());
    let mut lines = Vec::new();
    let t0 = Instant::now();
    loop {
        let mut l = String::new();
        if out.read_line(&mut l).unwrap() == 0 || t0.elapsed() > Duration::from_secs(60) {
            break;
        }
        let done = l.starts_with("[musaic] listening on http://127.0.0.1:");
        lines.push(l);
        if done {
            break;
        }
    }
    assert_eq!(
        lines.first().map(String::as_str),
        Some(&*format!(
            "[musaic] Logging to {} (level=DEBUG)\n",
            cfg.join("server.log").display()
        )),
        "{lines:?}"
    );
    assert!(
        lines.last().unwrap().starts_with("[musaic] listening on"),
        "{lines:?}"
    );
    let pid = child.id().to_string();
    assert!(
        Command::new("kill")
            .args(["-INT", &pid])
            .status()
            .unwrap()
            .success()
    );
    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(0));
    let log = std::fs::read_to_string(cfg.join("server.log")).unwrap();
    let mode = std::fs::metadata(cfg.join("server.log"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);
    for want in [
        format!(
            "[INFO] musaic.main: musaic-api {} starting: schema v{}, network offline, secrets in memory (verification)",
            env!("CARGO_PKG_VERSION"),
            musaic_db::SCHEMA_VERSION
        ),
        format!(
            "[INFO] musaic.db: created database {}",
            cfg.join("library.db").display()
        ),
        // Accurate whether keys were moved by an earlier start or there
        // never were any.
        "[INFO] musaic.secrets: settings secret migration (startup): no API key stored in the \
         database file, its backup or older snapshots (keys are kept in the system keyring)"
            .into(),
        "[INFO] musaic.main: listening on http://127.0.0.1:".into(),
        "[INFO] musaic.main: shutting down".into(),
        "[INFO] musaic.main: stopped".into(),
    ] {
        assert!(log.contains(&want), "{want}\n{log}");
    }
}

/// One plain HTTP/1.1 request to the running binary; the whole response
/// (the connection is closed after it).
#[cfg(unix)]
fn http(port: u16, method: &str, path: &str, body: Option<&str>) -> String {
    use std::io::{Read, Write};
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(60))).unwrap();
    let body = body.unwrap_or("");
    write!(
        s,
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out);
    String::from_utf8_lossy(&out).into_owned()
}

/// SIGTERM while the fingerprint pool is in the middle of a pass: the
/// process shuts down cleanly (exit 0, the pass cancelled), and the
/// database's write-ahead log is checkpointed and removed, so the main
/// file alone holds everything. Before the fix the binary only listened for
/// Ctrl-C, so SIGTERM killed it with the pool's connection open and the WAL
/// left behind.
#[cfg(unix)]
#[test]
fn sigterm_during_a_pool_pass_checkpoints_the_database() {
    use std::io::{BufRead, BufReader};
    let td = tempfile::tempdir().unwrap();
    let root = musaic_testdata::canonical(td.path());
    let cfg = root.join("cfg");
    let lib = root.join("library");
    copy_tree(&musaic_testdata::synthetic_library(), &lib);
    let mut child = cmd(&cfg)
        .args(["--network", "offline", "--port", "0"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut out = BufReader::new(child.stdout.take().unwrap());
    let mut port = None;
    let t0 = Instant::now();
    while port.is_none() && t0.elapsed() < Duration::from_secs(60) {
        let mut l = String::new();
        if out.read_line(&mut l).unwrap() == 0 {
            break;
        }
        port = l
            .trim()
            .strip_prefix("[musaic] listening on http://127.0.0.1:")
            .map(|p| p.parse::<u16>().unwrap());
    }
    let port = port.expect("the server never listened");
    let body = serde_json::json!({ "library_dir": lib.to_string_lossy() }).to_string();
    let r = http(port, "POST", "/api/config", Some(&body));
    assert!(r.starts_with("HTTP/1.1 200"), "{r}");
    let r = http(port, "GET", "/api/scan/stream", None);
    assert!(r.contains("event: done"), "{r}");
    // The pass pauses 0.5 s between albums, so with every synthetic album
    // to fingerprint it runs for several seconds.
    let t0 = Instant::now();
    loop {
        let r = http(port, "GET", "/api/fingerprint-pool/status", None);
        let json = &r[r.find("\r\n\r\n").unwrap() + 4..];
        let v: serde_json::Value = serde_json::from_str(json).unwrap();
        if v["status"] == "running" && v["total"].as_u64().unwrap_or(0) > 0 {
            assert!(v["completed"].as_u64() < v["total"].as_u64(), "{v}");
            break;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(30),
            "pool never ran: {v}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        cfg.join("library.db-wal").exists(),
        "WAL mode while running"
    );
    let pid = child.id().to_string();
    assert!(
        Command::new("kill")
            .args(["-TERM", &pid])
            .status()
            .unwrap()
            .success()
    );
    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(0), "a clean exit, not death by signal");
    let wal = cfg.join("library.db-wal");
    assert!(
        !wal.exists() || std::fs::metadata(&wal).unwrap().len() == 0,
        "WAL left behind: {} bytes",
        std::fs::metadata(&wal).unwrap().len()
    );
    let log = std::fs::read_to_string(cfg.join("server.log")).unwrap();
    for want in [
        "[INFO] musaic.main: shutting down",
        "Eager fingerprinting cancelled",
        "[INFO] musaic.main: stopped",
    ] {
        assert!(log.contains(want), "{want}\n{log}");
    }
    assert!(!log.contains("still busy"), "{log}");
    assert!(!log.contains("checkpoint failed"), "{log}");
    let conn = rusqlite::Connection::open(cfg.join("library.db")).unwrap();
    let ok: String = conn
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .unwrap();
    assert_eq!(ok, "ok");
}

#[cfg(unix)]
fn copy_tree(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for e in std::fs::read_dir(src).unwrap().flatten() {
        let to = dst.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_tree(&e.path(), &to);
        } else {
            std::fs::copy(e.path(), &to).unwrap();
        }
    }
}
