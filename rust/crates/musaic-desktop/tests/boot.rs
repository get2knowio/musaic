//! The desktop app boots (binds 127.0.0.1:<ephemeral>, answers
//! `/api/health`, the window loads the SPA), a second launch focuses the
//! first, and quitting stops the backend, releases the port, leaves no
//! child process and no lock on library.db.
//!
//! Opens a real window (hidden with `--hidden`), so it is `#[ignore]`d:
//!
//! ```text
//! (cd frontend && npm ci && npm run build)
//! cd rust && cargo test -p musaic-desktop --test boot -- --ignored --nocapture
//! ```
//!
//! Every run uses a fresh `work/desktop-boot-<pid>-<n>/` with a copy of
//! `testdata/synthetic-library/library`, a config dir there, in-memory secrets and offline
//! network.
//!
//! **Packaged mode** (the same checks against the bundle,
//! docs/desktop.md "Packaging (macOS)"): with
//! `MUSAIC_DESKTOP_BIN=<…>/Musaic.app/Contents/MacOS/musaic-desktop` the
//! test drives that binary instead of the cargo-built one, passes no
//! `--frontend-dir` (the SPA must come from the bundle's `frontend/`
//! resource), runs it with `HOME` inside the run dir and a system-only
//! `PATH` (`/usr/bin:/bin:/usr/sbin:/sbin`, no Homebrew, no `DYLD_*`, no
//! fpcalc / libchromaprint variables), and additionally waits for the
//! fingerprint pool to finish and checks that fingerprints were stored and
//! the fixture's duplicate track was found.
//!
//! **Quit routes** (ask before quitting only while work runs):
//! `quit_routes_ask_only_while_work_runs` drives the hidden window through
//! the selftest controls (`POST /__musaic/selftest/*`, gate.rs): it holds a
//! work guard and sends a quit down the app menu Quit item's handler (what
//! Cmd-Q runs) and down the window's `CloseRequested` path (what the close
//! button raises), answering "Keep running" or "Quit", and quits idle both
//! ways. The keystroke / click themselves, and the native prompt, need a
//! visible window (docs/desktop.md "Quitting").
//!
//! The run dir is removed when the test passes and kept (its path printed)
//! when it fails, or always with `MUSAIC_KEEP_WORK=1`. Removal only ever
//! touches a real directory named `desktop-boot-*` directly inside the
//! repository's `work/` (never a symlink, never `work/desktop-demo/`). The
//! library copy is cheap: `std::fs::copy` clones on APFS.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .unwrap()
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let t = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &t);
        } else {
            std::fs::copy(e.path(), &t).unwrap();
        }
    }
}

struct Run {
    dir: PathBuf,
    config: PathBuf,
    library: PathBuf,
    report: PathBuf,
}

impl Drop for Run {
    /// Passed: remove the run dir. Failed (dropped while panicking) or
    /// `MUSAIC_KEEP_WORK=1`: keep it and say where it is.
    fn drop(&mut self) {
        let keep = std::env::var_os("MUSAIC_KEEP_WORK").is_some_and(|v| v == "1");
        if std::thread::panicking() || keep {
            eprintln!("boot test: kept run dir {}", self.dir.display());
            return;
        }
        if let Err(why) = remove_run_dir(&self.dir) {
            eprintln!("boot test: not removing {}: {why}", self.dir.display());
        }
    }
}

/// Remove `dir` only if it is a real directory (not a symlink) named
/// `desktop-boot-*` whose canonical parent is the repository's `work/`.
fn remove_run_dir(dir: &Path) -> Result<(), String> {
    let meta = std::fs::symlink_metadata(dir).map_err(|e| e.to_string())?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return Err("not a real directory".into());
    }
    let work = repo_root()
        .join("work")
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let canon = dir.canonicalize().map_err(|e| e.to_string())?;
    let name = canon.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if canon.parent() != Some(work.as_path()) || !name.starts_with("desktop-boot-") {
        return Err(format!("outside {}/desktop-boot-*", work.display()));
    }
    // remove_dir_all does not follow symlinks inside the tree.
    std::fs::remove_dir_all(&canon).map_err(|e| e.to_string())
}

fn fresh_run(tag: &str) -> Run {
    let root = repo_root();
    let src = musaic_testdata::synthetic_library();
    assert!(
        root.join("frontend/dist/index.html").is_file(),
        "frontend/dist missing: (cd frontend && npm ci && npm run build)"
    );
    let dir = root.join("work").join(format!(
        "desktop-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis()
    ));
    let library = dir.join("library");
    copy_dir(&src, &library);
    let config = dir.join("config");
    std::fs::create_dir_all(&config).unwrap();
    Run {
        report: dir.join("report.jsonl"),
        dir,
        config,
        library,
    }
}

/// `MUSAIC_DESKTOP_BIN`: the packaged binary to drive instead of the
/// cargo-built one (see the module docs).
fn packaged_bin() -> Option<PathBuf> {
    std::env::var_os("MUSAIC_DESKTOP_BIN")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn launch(run: &Run, report: Option<&Path>) -> Child {
    let root = repo_root();
    let packaged = packaged_bin();
    let mut c = Command::new(
        packaged
            .clone()
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_musaic-desktop"))),
    );
    c.arg("--config-dir").arg(&run.config);
    if packaged.is_some() {
        // The SPA comes from the bundle's resources; no
        // Homebrew on PATH, no library search paths, no fpcalc overrides.
        let home = run.dir.join("home");
        std::fs::create_dir_all(&home).unwrap();
        c.env("HOME", &home)
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env_remove("DYLD_LIBRARY_PATH")
            .env_remove("DYLD_FALLBACK_LIBRARY_PATH")
            .env_remove("DYLD_INSERT_LIBRARIES")
            .env_remove("MUSAIC_FPCALC")
            .env_remove("FPCALC")
            .env_remove("MUSAIC_LIBCHROMAPRINT");
    } else {
        c.arg("--frontend-dir").arg(root.join("frontend/dist"));
    }
    c.arg("--dev-memory-secrets")
        .arg("--network")
        .arg("offline")
        .arg("--hidden")
        // Never inherit a real config dir or network mode from the caller.
        .env_remove("MUSAIC_CONFIG_DIR")
        .env_remove("MUSAIC_NETWORK")
        .env_remove("MUSAIC_REPLAY_DIR")
        .env_remove("MUSAIC_PORT")
        .env_remove("MUSAIC_FRONTEND_DIR")
        .stdin(Stdio::null());
    if let Some(r) = report {
        c.arg("--selftest-report").arg(r);
    }
    c.spawn().unwrap()
}

fn events(report: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(report)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

fn wait_for<T>(what: &str, secs: u64, mut f: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

struct Resp {
    status: u16,
    headers: String,
    body: String,
}

/// Minimal HTTP/1.1 client (no extra dependencies).
fn http(
    addr: SocketAddr,
    method: &str,
    path: &str,
    cookie: Option<&str>,
    body: Option<&str>,
) -> Resp {
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(5)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(60))).unwrap();
    let mut req = format!(
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\nAccept-Encoding: identity\r\n"
    );
    if let Some(c) = cookie {
        req.push_str(&format!("Cookie: musaic_launch={c}\r\n"));
    }
    if let Some(b) = body {
        req.push_str(&format!(
            "Content-Type: application/json\r\nOrigin: http://{addr}\r\nContent-Length: {}\r\n",
            b.len()
        ));
    }
    req.push_str("\r\n");
    if let Some(b) = body {
        req.push_str(b);
    }
    s.write_all(req.as_bytes()).unwrap();
    let mut raw = Vec::new();
    let _ = s.read_to_end(&mut raw);
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    Resp {
        status,
        headers: head.to_ascii_lowercase(),
        body: body.to_string(),
    }
}

fn children_of(pid: u32) -> Vec<u32> {
    let out = Command::new("pgrep")
        .args(["-P", &pid.to_string()])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.trim().parse().ok())
        .collect()
}

fn alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success()
}

fn wait_exit(child: &mut Child, secs: u64) -> std::process::ExitStatus {
    wait_for("process exit", secs, || child.try_wait().unwrap())
}

#[test]
#[ignore = "opens a (hidden) window; run with --ignored after building frontend/dist"]
fn boots_serves_focuses_and_tears_down() {
    let run = fresh_run("boot");
    let t0 = Instant::now();
    let mut first = launch(&run, Some(&run.report));
    let pid = first.id();

    // Backend bound on loopback, ephemeral port.
    let backend = wait_for("backend event", 60, || {
        events(&run.report)
            .into_iter()
            .find(|e| e["event"] == "backend" || e["event"] == "boot-failed")
    });
    assert_eq!(backend["event"], "backend", "boot failed: {backend}");
    let boot_ms = t0.elapsed().as_millis();
    let port = u16::try_from(backend["port"].as_u64().unwrap()).unwrap();
    assert_ne!(port, 8765, "ephemeral port expected");
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    if let Some(bin) = packaged_bin() {
        // The SPA is the bundle's own `Contents/Resources/frontend`.
        let resources = bin
            .parent()
            .and_then(Path::parent)
            .unwrap()
            .join("Resources/frontend")
            .canonicalize()
            .unwrap();
        let served = PathBuf::from(backend["frontend_dir"].as_str().unwrap())
            .canonicalize()
            .unwrap();
        assert_eq!(served, resources, "packaged run must serve the bundled SPA");
    }
    let launch_url = backend["launch_url"].as_str().unwrap().to_string();
    let token = launch_url.split("t=").nth(1).unwrap().to_string();
    assert!(
        events(&run.report)
            .iter()
            .any(|e| e["event"] == "window-created")
    );

    // Window loaded the SPA through the launch URL: the page finished on
    // `/` and the React app's own requests reached the backend with the
    // launch cookie (it ran, and its fetch/EventSource carry the cookie).
    wait_for("SPA page load + app requests", 60, || {
        let ev = events(&run.report);
        let loaded = ev.iter().any(|e| {
            e["event"] == "page-load"
                && e["phase"] == "finished"
                && e["url"] == format!("http://127.0.0.1:{port}/")
        });
        let api = ev
            .iter()
            .any(|e| e["event"] == "request" && e["path"] == "/api/library-events/stream");
        let asset = ev.iter().any(|e| {
            e["event"] == "request"
                && e["path"]
                    .as_str()
                    .is_some_and(|p| p.starts_with("/assets/"))
        });
        (loaded && api && asset).then_some(())
    });
    let load_ms = t0.elapsed().as_millis();

    // Per-launch token: the launch link is one-shot and nothing answers
    // without the cookie; with it, health answers.
    let r = http(addr, "GET", "/api/health", None, None);
    assert_eq!(r.status, 403, "no cookie must be refused: {}", r.body);
    assert!(r.headers.contains("content-security-policy"));
    assert_eq!(
        http(
            addr,
            "GET",
            &format!("/__musaic/launch?t={token}"),
            None,
            None
        )
        .status,
        410
    );
    let health = http(addr, "GET", "/api/health", Some(&token), None);
    assert_eq!(health.status, 200, "{}", health.body);
    let index = http(addr, "GET", "/", Some(&token), None);
    assert_eq!(index.status, 200);
    assert!(index.body.contains("<div id=\"root\">"));
    assert!(
        index
            .headers
            .contains("content-security-policy: default-src 'self'")
    );
    // A foreign Host is still refused by musaic-api's Host check.
    let mut s = TcpStream::connect(addr).unwrap();
    s.write_all(
        format!(
            "GET /api/health HTTP/1.1\r\nHost: evil.example\r\nCookie: musaic_launch={token}\r\nConnection: close\r\n\r\n"
        )
        .as_bytes(),
    )
    .unwrap();
    let mut out = String::new();
    let _ = s.read_to_string(&mut out);
    assert!(out.starts_with("HTTP/1.1 403"), "{out}");

    // window.confirm shim endpoint (selftest answers Cancel).
    let mut s = TcpStream::connect(addr).unwrap();
    s.write_all(format!(
        "POST /__musaic/dialog/confirm HTTP/1.1\r\nHost: {addr}\r\nOrigin: http://{addr}\r\nCookie: musaic_launch={token}\r\nContent-Type: text/plain\r\nContent-Length: 5\r\nConnection: close\r\n\r\nsure?"
    ).as_bytes()).unwrap();
    let mut out = String::new();
    let _ = s.read_to_string(&mut out);
    assert!(
        out.starts_with("HTTP/1.1 200") && out.ends_with("false"),
        "{out}"
    );

    // The native folder picker endpoint (selftest
    // answers "cancelled" without showing the panel, so the plugin's
    // panel itself is not exercised here; the app did start with the
    // plugin registered).
    let mut s = TcpStream::connect(addr).unwrap();
    s.write_all(format!(
        "POST /__musaic/dialog/folder HTTP/1.1\r\nHost: {addr}\r\nOrigin: http://{addr}\r\nCookie: musaic_launch={token}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    ).as_bytes()).unwrap();
    let mut out = String::new();
    let _ = s.read_to_string(&mut out);
    assert!(
        out.starts_with("HTTP/1.1 200") && out.ends_with(r#"{"path":null}"#),
        "{out}"
    );

    // Configure the library and run a scan to completion, so teardown has
    // a started fingerprint pool to stop.
    let lib = serde_json::json!({"library_dir": run.library}).to_string();
    let r = http(addr, "POST", "/api/config", Some(&token), Some(&lib));
    assert_eq!(r.status, 200, "{}", r.body);
    let scan = http(addr, "GET", "/api/scan/stream", Some(&token), None);
    assert!(
        scan.body.contains("event: done"),
        "scan: {}",
        &scan.body[..scan.body.len().min(400)]
    );
    let albums = http(addr, "GET", "/api/albums", Some(&token), None);
    assert_eq!(albums.status, 200);

    // The pool fingerprints in process with no
    // fpcalc / libchromaprint reachable, and duplicate-tracks finds the
    // fixture's duplicate ("Mono Harbor - Dupes") from those fingerprints.
    let mut pool_summary = String::new();
    if packaged_bin().is_some() {
        let pool = wait_for("fingerprint pool done", 300, || {
            let r = http(
                addr,
                "GET",
                "/api/fingerprint-pool/status",
                Some(&token),
                None,
            );
            let v: serde_json::Value = serde_json::from_str(&r.body).ok()?;
            (v["status"] != "running").then_some(v)
        });
        assert_eq!(pool["status"], "done", "pool: {pool}");
        assert!(pool["completed"].as_u64().unwrap() > 0, "pool: {pool}");
        assert_eq!(pool["errors"], 0, "pool: {pool}");
        let all: serde_json::Value = serde_json::from_str(&albums.body).unwrap();
        let dupes_id = all["albums"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["dir_name"] == "Mono Harbor - Dupes [FLAC]")
            .map(|a| a["id"].clone())
            .expect("fixture album Mono Harbor - Dupes");
        let dup = wait_for("duplicate-tracks finding", 120, || {
            let r = http(addr, "GET", "/api/checks/findings", Some(&token), None);
            let v: serde_json::Value = serde_json::from_str(&r.body).ok()?;
            let list = v
                .as_array()
                .cloned()
                .or_else(|| v["findings"].as_array().cloned())?;
            list.into_iter()
                .find(|f| f["check_id"] == "duplicate-tracks" && f["album_id"] == dupes_id)
        });
        pool_summary = format!(
            "pool {} completed {} errors {}; duplicate finding: {}",
            pool["status"], pool["completed"], pool["errors"], dup["summary"]
        );
    }

    // A second launch on the same config dir focuses the first and
    // exits 0 without starting a server.
    let mut second = launch(&run, None);
    let st = wait_exit(&mut second, 30);
    assert!(st.success(), "second instance exit: {st:?}");
    wait_for("focus-requested", 10, || {
        events(&run.report)
            .iter()
            .any(|e| e["event"] == "focus-requested")
            .then_some(())
    });
    assert!(alive(pid), "first instance must survive the second launch");

    // Quit (SIGTERM, as an OS logout or `kill` would) → clean teardown.
    let kids = children_of(pid);
    let t_quit = Instant::now();
    assert!(
        Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .status()
            .unwrap()
            .success()
    );
    let st = wait_exit(&mut first, 30);
    let quit_ms = t_quit.elapsed().as_millis();
    assert_eq!(st.code(), Some(0), "exit status {st:?}");
    let ev = events(&run.report);
    let stopped = ev
        .iter()
        .find(|e| e["event"] == "stopped")
        .expect("teardown ran");
    assert_eq!(stopped["state_shutdown"], true);
    assert!(ev.iter().any(|e| e["event"] == "exit"));
    // Port released.
    TcpListener::bind(addr).expect("port must be free after exit");
    assert!(TcpStream::connect_timeout(&addr, Duration::from_millis(500)).is_err());
    // No child process left behind.
    for k in &kids {
        assert!(!alive(*k), "child {k} survived");
    }
    assert!(!alive(pid));
    // No lock on library.db and the instance info file is gone.
    let db = rusqlite::Connection::open(run.config.join("library.db")).unwrap();
    db.execute_batch("BEGIN EXCLUSIVE; COMMIT;")
        .expect("library.db must not be locked");
    assert!(!run.config.join("desktop-instance.json").exists());
    // A hidden (and selftest) run neither reads nor writes window
    // state: the plugin was not registered and no state file was left.
    let created = ev
        .iter()
        .find(|e| e["event"] == "window-created")
        .expect("window-created event");
    assert_eq!(created["window_state"], false, "{created}");
    assert!(
        !run.config.join("window-state.json").exists(),
        "hidden run must not write window-state.json"
    );
    if packaged_bin().is_some() {
        // Fingerprints were stored by the Rust implementation.
        let (n, impls): (i64, String) = db
            .query_row(
                "SELECT COUNT(*), COALESCE(GROUP_CONCAT(DISTINCT fingerprint_impl), '') \
                 FROM track_fingerprints WHERE fingerprint <> ''",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert!(n > 0, "no fingerprints stored");
        assert_eq!(impls, "rust-chromaprint");
        println!("packaged: {n} fingerprints stored (impl {impls:?}); {pool_summary}");
    }
    let wal = run.config.join("library.db-wal");
    println!(
        "boot: port {port}; backend ready {boot_ms} ms, SPA loaded {load_ms} ms after launch; \
         quit took {quit_ms} ms (drained: {}); children before quit: {kids:?}; wal left: {} ({} bytes); run dir {}",
        stopped["drained"],
        wal.exists(),
        std::fs::metadata(&wal).map(|m| m.len()).unwrap_or(0),
        run.dir.display()
    );
}

/// One hidden selftest launch for the quit test: its process, address and
/// launch token, once the window has loaded the SPA.
struct Live {
    child: Child,
    addr: SocketAddr,
    token: String,
}

fn boot_hidden(run: &Run) -> Live {
    let child = launch(run, Some(&run.report));
    let backend = wait_for("backend event", 60, || {
        events(&run.report)
            .into_iter()
            .find(|e| e["event"] == "backend" || e["event"] == "boot-failed")
    });
    assert_eq!(backend["event"], "backend", "boot failed: {backend}");
    let port = u16::try_from(backend["port"].as_u64().unwrap()).unwrap();
    let token = backend["launch_url"]
        .as_str()
        .unwrap()
        .split("t=")
        .nth(1)
        .unwrap()
        .to_string();
    wait_for("SPA page load", 60, || {
        events(&run.report)
            .iter()
            .any(|e| {
                e["event"] == "page-load"
                    && e["phase"] == "finished"
                    && e["url"] == format!("http://127.0.0.1:{port}/")
            })
            .then_some(())
    });
    Live {
        child,
        addr: SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
        token,
    }
}

/// `POST /__musaic/selftest/{cmd}` (a body so the helper sends the app's
/// own Origin).
fn selftest(live: &Live, cmd: &str) -> Resp {
    http(
        live.addr,
        "POST",
        &format!("/__musaic/selftest/{cmd}"),
        Some(&live.token),
        Some("{}"),
    )
}

fn named<'a>(ev: &'a [serde_json::Value], name: &str) -> Vec<&'a serde_json::Value> {
    ev.iter().filter(|e| e["event"] == name).collect()
}

/// Wait for the `n`th `quit-request` event (1-based).
fn nth_quit_request(run: &Run, n: usize) -> serde_json::Value {
    wait_for("quit-request event", 30, || {
        named(&events(&run.report), "quit-request")
            .get(n - 1)
            .map(|e| (*e).clone())
    })
}

/// Teardown after a quit: exit 0, server stopped with the app's
/// shutdown, port free, instance info gone, library.db unlocked.
fn assert_clean_exit(run: &Run, live: &mut Live) -> Vec<serde_json::Value> {
    let st = wait_exit(&mut live.child, 30);
    assert_eq!(st.code(), Some(0), "exit status {st:?}");
    let ev = events(&run.report);
    let stopped = named(&ev, "stopped");
    assert_eq!(stopped.len(), 1, "teardown must run once: {ev:?}");
    assert_eq!(stopped[0]["state_shutdown"], true);
    assert_eq!(named(&ev, "exit").len(), 1);
    TcpListener::bind(live.addr).expect("port must be free after exit");
    assert!(!run.config.join("desktop-instance.json").exists());
    let db = rusqlite::Connection::open(run.config.join("library.db")).unwrap();
    db.execute_batch("BEGIN EXCLUSIVE; COMMIT;")
        .expect("library.db must not be locked");
    ev
}

fn still_running(live: &mut Live) {
    std::thread::sleep(Duration::from_millis(800));
    assert!(
        live.child.try_wait().unwrap().is_none(),
        "a cancelled quit must leave the app running"
    );
    let r = http(live.addr, "GET", "/api/health", Some(&live.token), None);
    assert_eq!(r.status, 200, "backend must still answer: {}", r.body);
}

//
// the app menu's Quit (Cmd-Q) and the window's close button ask while work
// runs and quit at once when idle; "Keep running" keeps the app; a
// confirmed quit is asked once and tears down cleanly. Four hidden launches.
#[test]
#[ignore = "opens a (hidden) window; run with --ignored after building frontend/dist"]
fn quit_routes_ask_only_while_work_runs() {
    // 1. Busy: Quit menu → "Keep running"; close button → "Keep running";
    //    then idle: Quit menu quits without asking.
    let run = fresh_run("boot-quit1");
    let mut live = boot_hidden(&run);
    if cfg!(target_os = "macos") {
        // The app menu carries our Quit item instead of the predefined one
        // (which would send `terminate:` unseen), and the Edit menu keeps
        // the predefined clipboard items the webview's text fields need.
        let ev = events(&run.report);
        let menu = &named(&ev, "menu")[0]["menu"];
        let subs = menu.as_array().expect("menu bar");
        let app_items = subs[0]["items"].as_array().unwrap();
        assert!(
            app_items
                .iter()
                .any(|i| i["kind"] == "item" && i["id"] == "musaic-quit"),
            "{menu}"
        );
        assert!(
            !app_items.iter().any(|i| i["kind"] == "predefined"
                && i["text"].as_str().is_some_and(|t| t.starts_with("Quit"))),
            "predefined Quit must be gone: {menu}"
        );
        for want in ["About", "Hide"] {
            assert!(
                app_items.iter().any(|i| i["kind"] == "predefined"
                    && i["text"].as_str().is_some_and(|t| t.starts_with(want))),
                "{want} missing: {menu}"
            );
        }
        let edit = subs
            .iter()
            .find(|s| s["title"] == "Edit")
            .expect("Edit menu");
        for want in ["Undo", "Redo", "Cut", "Copy", "Paste", "Select All"] {
            assert!(
                edit["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|i| i["kind"] == "predefined" && i["text"] == want),
                "Edit → {want} missing: {menu}"
            );
        }
        let titles: Vec<_> = subs.iter().map(|s| s["title"].clone()).collect();
        assert_eq!(
            titles,
            ["Musaic", "File", "Edit", "View", "Album", "Window", "Help"],
            "{menu}"
        );
        let has = |title: &str, id: &str| {
            subs.iter()
                .find(|s| s["title"] == title)
                .and_then(|s| s["items"].as_array())
                .is_some_and(|items| items.iter().any(|i| i["id"] == id))
        };
        for (title, id) in [
            ("Musaic", "app.settings"),
            ("File", "library.rescan"),
            ("Edit", "library.find"),
            ("View", "view.library"),
            ("View", "view.review"),
            ("Album", "album.reveal"),
            ("Help", "help.shortcuts"),
        ] {
            assert!(has(title, id), "{title} → {id} missing: {menu}");
        }

        // The page reports which commands are available (no library
        // configured, nothing selected) ...
        let state = wait_for("menu state from the page", 60, || {
            named(&events(&run.report), "menu-state")
                .first()
                .map(|e| (*e).clone())
        });
        let enabled = state["enabled"].as_array().unwrap();
        assert!(enabled.iter().any(|i| i == "view.library"), "{state}");
        assert!(enabled.iter().any(|i| i == "app.settings"), "{state}");
        assert!(!enabled.iter().any(|i| i == "album.reveal"), "{state}");
        assert!(!enabled.iter().any(|i| i == "library.rescan"), "{state}");
        // ... and a menu command reaches it: Keyboard Shortcuts opens a
        // modal, after which the page dims every command.
        let r = selftest(&live, "menu?id=help.shortcuts");
        assert_eq!(r.status, 200, "{}", r.body);
        let sent = wait_for("menu command", 30, || {
            named(&events(&run.report), "menu-command")
                .first()
                .map(|e| (*e).clone())
        });
        assert_eq!(sent["delivered"], true, "{sent}");
        wait_for("menu dimmed under the modal", 30, || {
            named(&events(&run.report), "menu-state")
                .iter()
                .any(|e| e["enabled"].as_array().is_some_and(Vec::is_empty))
                .then_some(())
        });
        // Not a command: refused.
        let r = selftest(&live, "menu?id=musaic-quit");
        assert_eq!(r.status, 400, "{}", r.body);
    }
    let r = selftest(&live, "hold-work");
    assert_eq!(r.status, 200, "{}", r.body);
    let r = selftest(&live, "quit?via=menu&answer=keep");
    assert_eq!(r.status, 200, "{}", r.body);
    let q = nth_quit_request(&run, 1);
    assert_eq!(q["route"], "quit-menu");
    assert_eq!(q["outcome"], "cancelled");
    still_running(&mut live);
    let r = selftest(&live, "quit?via=close&answer=keep");
    assert_eq!(r.status, 200, "{}", r.body);
    let q = nth_quit_request(&run, 2);
    assert_eq!(q["route"], "close-button");
    assert_eq!(q["outcome"], "cancelled");
    still_running(&mut live);
    let r = selftest(&live, "release-work");
    assert_eq!(r.status, 200, "{}", r.body);
    let _ = selftest(&live, "quit?via=menu");
    let ev = assert_clean_exit(&run, &mut live);
    let prompts = named(&ev, "quit-prompt");
    assert_eq!(prompts.len(), 2, "{prompts:?}");
    for (p, route) in prompts.iter().zip(["quit-menu", "close-button"]) {
        assert_eq!(p["route"], route);
        assert_eq!(p["answer"], false);
        assert!(
            p["message"].as_str().unwrap().contains("applying changes"),
            "{p}"
        );
    }
    let requests = named(&ev, "quit-request");
    assert_eq!(requests[2]["route"], "quit-menu");
    assert_eq!(requests[2]["outcome"], "proceed");
    drop(run);

    // 2. Busy: close button → "Quit": one prompt (the last-window exit
    //    request that follows is not asked again), clean teardown.
    let run = fresh_run("boot-quit2");
    let mut live = boot_hidden(&run);
    assert_eq!(selftest(&live, "hold-work").status, 200);
    let _ = selftest(&live, "quit?via=close&answer=quit");
    let ev = assert_clean_exit(&run, &mut live);
    let prompts = named(&ev, "quit-prompt");
    assert_eq!(prompts.len(), 1, "{prompts:?}");
    assert_eq!(prompts[0]["route"], "close-button");
    assert_eq!(prompts[0]["answer"], true);
    let outcomes: Vec<_> = named(&ev, "quit-request")
        .iter()
        .map(|e| {
            format!(
                "{}:{}",
                e["route"].as_str().unwrap(),
                e["outcome"].as_str().unwrap()
            )
        })
        .collect();
    assert_eq!(
        outcomes,
        ["close-button:confirmed", "last-window:already-confirmed"]
    );
    drop(run);

    // 3. Busy: Quit menu → "Quit": one prompt, clean teardown.
    let run = fresh_run("boot-quit3");
    let mut live = boot_hidden(&run);
    assert_eq!(selftest(&live, "hold-work").status, 200);
    let _ = selftest(&live, "quit?via=menu&answer=quit");
    let ev = assert_clean_exit(&run, &mut live);
    let prompts = named(&ev, "quit-prompt");
    assert_eq!(prompts.len(), 1, "{prompts:?}");
    assert_eq!(prompts[0]["route"], "quit-menu");
    assert_eq!(prompts[0]["answer"], true);
    drop(run);

    // 4. Idle: close button quits without asking.
    let run = fresh_run("boot-quit4");
    let mut live = boot_hidden(&run);
    let _ = selftest(&live, "quit?via=close");
    let ev = assert_clean_exit(&run, &mut live);
    assert!(named(&ev, "quit-prompt").is_empty());
    let outcomes: Vec<_> = named(&ev, "quit-request")
        .iter()
        .map(|e| {
            format!(
                "{}:{}",
                e["route"].as_str().unwrap(),
                e["outcome"].as_str().unwrap()
            )
        })
        .collect();
    assert_eq!(outcomes, ["close-button:proceed", "last-window:proceed"]);
}
