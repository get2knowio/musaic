//! `musaic-api`: the server binary (the Python app's `musaic` console script,
//! `musaic/main.py:run`). `musaic-api --help` lists every flag and
//! environment variable ([`HELP`]); a flag wins over its variable.
//!
//! Network (`--network MODE` or `MUSAIC_NETWORK`):
//! * `live` — real HTTPS through the paced live transport. The default, but
//!   only when no mode is given at all (no `--network`, no `MUSAIC_NETWORK`,
//!   no `--replay-dir` / `MUSAIC_REPLAY_DIR`).
//! * `offline` — every outbound request fails as if the host did not
//!   resolve (no network at all).
//! * `replay` — answered from recorded traffic in `--replay-dir DIR` /
//!   `MUSAIC_REPLAY_DIR` (the layout the replay transport records); a miss fails as
//!   unobserved. Giving a replay dir without a mode selects replay. Replay
//!   and offline never fall through to live: neither builds a live
//!   transport, so no socket can leave the machine.
//!
//! `--net-log DIR` / `MUSAIC_NET_LOG_DIR` (offline / replay only): append one
//! JSON line per outbound request to `DIR/blocked.jsonl` (offline),
//! `DIR/replay-hits.jsonl` or `DIR/unobserved.jsonl` (replay), with the same
//! fields per line (`ts`, `method`, `url`, plus the replay `key` when there is one).
//!
//! Secrets live in the OS keyring. `--verification-memory-secrets` (for
//! end-to-end test runs only) selects an in-memory store instead; it refuses
//! to start unless the config dir is under the system temp dir or under a
//! directory named `work`, so it cannot be pointed at a real `~/.musaic`.
//! `--verification-memory-secrets=unavailable|fail-set:<key>`
//! makes that store fail like a missing keyring / a refused write
//! (`musaic_api::verification`, which also says why it cannot be enabled by
//! accident).
//!
//! Ctrl-C or SIGTERM: open SSE streams end cleanly, the server
//! drains, the fingerprint pool is stopped (bounded wait) and the database's
//! WAL is checkpointed before the process exits.
//!
//! Host / Origin check: loopback names are always accepted. A
//! deliberate non-loopback bind adds that address; `MUSAIC_ALLOWED_HOSTS`
//! (comma-separated names) adds more.
//!
//! Log: `<config>/server.log` and stderr, rotating at 5 MiB with 5
//! generations, level from `--log-level` / `MUSAIC_LOG_LEVEL`
//! (`musaic_api::logging`). Secret values never reach it.
//!
//! Exit status: 0 after a clean shutdown (Ctrl-C, SIGTERM) or `--help`; 2 for a
//! usage error (unknown or repeated flag, bad value, contradictory
//! combination); 1 when startup fails (config dir not writable, database
//! newer than this build, port in use). Each failure is one stderr line.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use musaic_api::logging::{Level, ServerLog, ServerLogOptions};
use musaic_api::verification::{MEMORY_SECRETS_FLAG, MemorySecrets, scratch_dir_ok};
use musaic_api::{AllowedHosts, AppState, Network, SharedSecrets, router};
use musaic_net::{FollowRedirects, ReplayTransport, Request, Response, Transport, TransportError};

/// `musaic-api --help`.
pub const HELP: &str = "\
musaic-api: the Musaic server (HTTP API + web UI)

Usage: musaic-api [OPTIONS]

Options (each overrides its environment variable):
  --host HOST            Address to bind [env MUSAIC_HOST; default 127.0.0.1]
  --port PORT            Port to bind, 0 for any free one [env MUSAIC_PORT; default 8765]
  --config-dir DIR       Config dir: library.db, config.json, server.log, thumbs/
                         [env MUSAIC_CONFIG_DIR; default ~/.musaic]
  --network MODE         live | offline | replay [env MUSAIC_NETWORK; default live,
                         or replay when a replay dir is given]
  --replay-dir DIR       Recorded traffic to answer from (replay only) [env MUSAIC_REPLAY_DIR]
  --net-log DIR          Log every outbound request as JSON lines (offline / replay only)
                         [env MUSAIC_NET_LOG_DIR]
  --verification-memory-secrets
                         Keep secrets in memory, not the OS keyring (verification runs;
                         the config dir must be under the temp dir or a `work` dir).
                         Written with =unavailable or =fail-set:<key>, the store fails
                         as a missing keyring / a refused write of <key>
  --log-level LEVEL      DEBUG | INFO | WARNING | ERROR | CRITICAL
                         [env MUSAIC_LOG_LEVEL; default INFO; an unknown value in the
                         variable means INFO]
  -h, --help             Print this help and exit
  -V, --version          Print the version and exit

Environment only:
  MUSAIC_ALLOWED_HOSTS   Comma-separated extra host names the server answers to
                         (needed when bound to 0.0.0.0 or ::)
  MUSAIC_FRONTEND_DIR    Built web UI to serve (frontend/dist)

Log: <config dir>/server.log and stderr (5 MiB x 5 generations; no secret values).
Exit status: 0 clean shutdown, 1 startup failure, 2 usage error.
";

/// The flags that take a value, and the switches.
const VALUED: &[&str] = &[
    "--host",
    "--port",
    "--config-dir",
    "--network",
    "--replay-dir",
    "--net-log",
    "--log-level",
];
const SWITCHES: &[&str] = &[MEMORY_SECRETS_FLAG];

/// The parsed command line.
#[derive(Debug, Default)]
struct Cli {
    values: Vec<(&'static str, String)>,
    switches: Vec<&'static str>,
}

impl Cli {
    fn get(&self, name: &str) -> Option<String> {
        self.values
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, v)| v.clone())
    }
    fn on(&self, name: &str) -> bool {
        self.switches.contains(&name)
    }
}

enum Parsed {
    Run(Cli),
    Help,
    Version,
}

fn parse(args: impl IntoIterator<Item = String>) -> Result<Parsed, String> {
    let mut cli = Cli::default();
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-h" | "--help" => return Ok(Parsed::Help),
            "-V" | "--version" => return Ok(Parsed::Version),
            _ => {}
        }
        let (name, inline) = match a.split_once('=') {
            Some((n, v)) if n.starts_with("--") => (n.to_string(), Some(v.to_string())),
            _ => (a.clone(), None),
        };
        if let Some(&flag) = VALUED.iter().find(|f| **f == name) {
            let v = match inline {
                Some(v) => v,
                None => it
                    .next()
                    .ok_or_else(|| format!("{flag} needs a value (see --help)"))?,
            };
            if cli.values.iter().any(|(n, _)| *n == flag) {
                return Err(format!("{flag} given more than once"));
            }
            cli.values.push((flag, v));
        } else if let Some(&sw) = SWITCHES.iter().find(|f| **f == name) {
            if cli.switches.contains(&sw) {
                return Err(format!("{sw} given more than once"));
            }
            match inline {
                // Only the verification switch takes a value,
                // and only in the one-argument `--flag=MODE` form.
                Some(v) if sw == MEMORY_SECRETS_FLAG => cli.values.push((sw, v)),
                Some(_) => return Err(format!("{sw} takes no value")),
                None => {}
            }
            cli.switches.push(sw);
        } else {
            return Err(format!("unknown argument {a:?} (see --help)"));
        }
    }
    Ok(Parsed::Run(cli))
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// The log, once started (so a failing startup still flushes it).
static SERVER_LOG: OnceLock<Arc<ServerLog>> = OnceLock::new();

fn flush_log() {
    if let Some(l) = SERVER_LOG.get() {
        l.flush(Duration::from_secs(2));
    }
}

/// Ctrl-C (SIGINT) or, on Unix, SIGTERM (what service managers, `kill` and
/// a system shutdown send): either starts the clean shutdown. Without the
/// SIGTERM handler the process would die at once, with the fingerprint pool
/// mid-pass and the database's write-ahead log not checkpointed.
///
/// The handlers are installed when this is called, not when the returned
/// future is first polled: it is called before the "listening" line, so a
/// signal sent as soon as that line appears already means a clean shutdown
/// (otherwise the default action would end the process at once).
fn stop_signal() -> impl std::future::Future<Output = ()> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let int = signal(SignalKind::interrupt());
        let term = signal(SignalKind::terminate());
        async move {
            match (int, term) {
                (Ok(mut int), Ok(mut term)) => {
                    tokio::select! {
                        _ = int.recv() => {}
                        _ = term.recv() => {}
                    }
                }
                (Ok(mut int), Err(_)) => {
                    int.recv().await;
                }
                (Err(_), Ok(mut term)) => {
                    term.recv().await;
                }
                (Err(_), Err(_)) => {
                    let _ = tokio::signal::ctrl_c().await;
                }
            }
        }
    }
    #[cfg(not(unix))]
    {
        let ctrl_c = tokio::signal::windows::ctrl_c();
        async move {
            match ctrl_c {
                Ok(mut c) => {
                    c.recv().await;
                }
                Err(_) => {
                    let _ = tokio::signal::ctrl_c().await;
                }
            }
        }
    }
}

/// One stderr line and the exit status; the line is also logged.
fn die(code: i32, msg: &str) -> ! {
    let one_line = msg.replace(['\n', '\r'], " ");
    if let Some(l) = SERVER_LOG.get() {
        // The log writes to the file only here: the line below is the
        // single stderr line.
        l.log_file_only(Level::Critical, "musaic.main", &one_line);
        l.flush(Duration::from_secs(2));
    }
    eprintln!("[musaic] {one_line}");
    std::process::exit(code);
}

/// Appends request records as JSON lines (one object per request).
struct NetLog {
    dir: PathBuf,
    lock: Mutex<()>,
}

impl NetLog {
    fn write(&self, file: &str, method: &str, url: &str, key: Option<&str>) {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        let mut rec = serde_json::json!({"ts": ts, "method": method, "url": url});
        if let Some(k) = key {
            rec["key"] = serde_json::Value::String(k.to_string());
        }
        let _g = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
        let res = std::fs::create_dir_all(&self.dir).and_then(|()| {
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.dir.join(file))?;
            writeln!(f, "{rec}")
        });
        if let Err(e) = res {
            musaic_api::logging::global(
                Level::Warning,
                "musaic.main",
                &format!("net log {}: {e}", self.dir.join(file).display()),
            );
        }
    }
}

/// Offline: every request fails as an unresolvable host (as
/// `Network::Offline` does), and is logged to `blocked.jsonl`.
struct LoggedOffline(Arc<NetLog>);

impl Transport for LoggedOffline {
    fn send(&self, request: &Request) -> Result<Response, TransportError> {
        self.0.write(
            "blocked.jsonl",
            request.method.as_str(),
            &request.redacted_url(),
            None,
        );
        Err(TransportError::Dns {
            host: request.host().to_string(),
        })
    }
}

/// Replay with every hop logged as a hit or an unobserved miss.
struct LoggedReplay {
    inner: ReplayTransport,
    log: Arc<NetLog>,
}

impl Transport for LoggedReplay {
    fn send(&self, request: &Request) -> Result<Response, TransportError> {
        let out = self.inner.send(request);
        let key = request.replay_key();
        let file = match &out {
            Ok(_) => "replay-hits.jsonl",
            Err(TransportError::Unobserved { .. }) => "unobserved.jsonl",
            Err(_) => "replay-errors.jsonl",
        };
        self.log.write(
            file,
            request.method.as_str(),
            &request.redacted_url(),
            Some(&key),
        );
        out
    }
}

fn absolute(p: &Path) -> PathBuf {
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir().map_or_else(|_| p.to_path_buf(), |c| c.join(p))
    }
}

fn network_from(cli: &Cli) -> (Network, &'static str) {
    let mode = cli.get("--network").or_else(|| env("MUSAIC_NETWORK"));
    let replay_dir = cli.get("--replay-dir").or_else(|| env("MUSAIC_REPLAY_DIR"));
    let net_log = cli.get("--net-log").or_else(|| env("MUSAIC_NET_LOG_DIR"));
    let mode = match (mode.as_deref(), replay_dir.is_some()) {
        (None, true) => "replay".to_string(),
        (None, false) => "live".to_string(),
        (Some(m), _) => m.to_ascii_lowercase(),
    };
    let log = net_log.map(|d| {
        Arc::new(NetLog {
            dir: absolute(Path::new(&d)),
            lock: Mutex::new(()),
        })
    });
    let network = match mode.as_str() {
        "offline" => {
            if replay_dir.is_some() {
                die(2, "--replay-dir is only valid with --network replay");
            }
            match log {
                Some(l) => Network::Live(Arc::new(LoggedOffline(l))),
                None => Network::Offline,
            }
        }
        "replay" => {
            let Some(dir) = replay_dir else {
                die(
                    2,
                    "--network replay needs --replay-dir DIR (or MUSAIC_REPLAY_DIR)",
                );
            };
            let dir = absolute(Path::new(&dir));
            if !dir.is_dir() {
                die(
                    2,
                    &format!("replay dir {} is not a directory", dir.display()),
                );
            }
            match log {
                // `Network::Live` only carries the transport; this one is the
                // replay transport (redirects followed hop by hop, as
                // `Network::Replay` does) with each hop logged. Nothing here
                // can open a socket.
                Some(l) => Network::Live(Arc::new(FollowRedirects(LoggedReplay {
                    inner: ReplayTransport::new(&dir),
                    log: l,
                }))),
                None => Network::Replay(dir),
            }
        }
        "live" => {
            if replay_dir.is_some() {
                die(2, "--replay-dir is only valid with --network replay");
            }
            if log.is_some() {
                die(2, "--net-log is only valid with --network offline|replay");
            }
            Network::live()
        }
        other => die(
            2,
            &format!("unknown --network {other:?} (offline|replay|live)"),
        ),
    };
    let name = match mode.as_str() {
        "offline" => "offline",
        "replay" => "replay",
        _ => "live",
    };
    (network, name)
}

#[tokio::main]
async fn main() {
    let cli = match parse(std::env::args().skip(1)) {
        Ok(Parsed::Run(c)) => c,
        Ok(Parsed::Help) => {
            print!("{HELP}");
            return;
        }
        Ok(Parsed::Version) => {
            println!("musaic-api {}", env!("CARGO_PKG_VERSION"));
            return;
        }
        Err(e) => die(2, &e),
    };
    let host = cli
        .get("--host")
        .or_else(|| env("MUSAIC_HOST"))
        .unwrap_or_else(|| "127.0.0.1".into());
    if host.trim().is_empty() {
        die(2, "--host must not be empty");
    }
    let port: u16 = match cli.get("--port") {
        Some(p) => p
            .trim()
            .parse()
            .unwrap_or_else(|_| die(2, &format!("--port {p:?} is not a port number (0-65535)"))),
        None => match env("MUSAIC_PORT") {
            Some(p) => p.trim().parse().unwrap_or_else(|_| {
                die(
                    2,
                    &format!("MUSAIC_PORT={p:?} is not a port number (0-65535)"),
                )
            }),
            None => 8765,
        },
    };
    let config_dir = match cli.get("--config-dir") {
        Some(d) if d.is_empty() => die(2, "--config-dir must not be empty"),
        Some(d) => absolute(Path::new(&d)),
        None => match musaic_core::config::config_dir() {
            Some(d) => d,
            None => die(
                2,
                "no home directory and no MUSAIC_CONFIG_DIR; give --config-dir",
            ),
        },
    };
    // `--log-level` must name a level; the variable falls back to INFO
    // (`getattr(logging, name, logging.INFO)`; no MEDIA_MANAGER_*).
    let (level_name, threshold) = match cli.get("--log-level") {
        Some(l) => match musaic_api::logging::parse_level(&l) {
            Some(t) => (l.to_ascii_uppercase(), t),
            None => die(
                2,
                &format!("--log-level {l:?} is not a level (DEBUG|INFO|WARNING|ERROR|CRITICAL)"),
            ),
        },
        None => {
            let raw = env(musaic_api::logging::LEVEL_ENV).unwrap_or_else(|| "INFO".into());
            let t = musaic_api::logging::level_from_env_value(&raw);
            (raw.to_ascii_uppercase(), t)
        }
    };
    let memory_secrets = if cli.on(MEMORY_SECRETS_FLAG) {
        match MemorySecrets::parse(cli.get(MEMORY_SECRETS_FLAG).as_deref()) {
            Ok(m) => Some(m),
            Err(e) => die(2, &e),
        }
    } else {
        None
    };
    // Every refusal before anything is created in the config dir.
    if memory_secrets.is_some() && !scratch_dir_ok(&config_dir) {
        die(
            2,
            &format!(
                "--verification-memory-secrets refused: config dir {} is not under the temp dir or a `work` directory",
                config_dir.display()
            ),
        );
    }
    let (network, network_name) = network_from(&cli);

    // `_configure_logging()` (musaic/main.py:131-191).
    let musaic_api::logging::ProcessLog { log, masks, opened } =
        musaic_api::logging::start_process_log(&config_dir, threshold, ServerLogOptions::default());
    match &opened {
        Ok(p) => println!("[musaic] Logging to {} (level={level_name})", p.display()),
        Err(e) => println!("[musaic] Warning: couldn't set up file log ({e})"),
    }
    let _ = SERVER_LOG.set(log.clone());
    log.log(
        Level::Info,
        "musaic.main",
        &format!(
            "musaic-api {} starting: schema v{}, network {network_name}, secrets {}, config dir {}, pid {}",
            env!("CARGO_PKG_VERSION"),
            musaic_db::SCHEMA_VERSION,
            memory_secrets
                .as_ref()
                .map_or_else(|| "OS keyring".to_string(), MemorySecrets::describe),
            config_dir.display(),
            std::process::id(),
        ),
    );

    let secrets: SharedSecrets = if let Some(m) = &memory_secrets {
        log.log(
            Level::Warning,
            "musaic.main",
            &format!("verification mode: secrets {}", m.describe()),
        );
        Arc::new(m.store())
    } else {
        Arc::new(musaic_secrets::OsKeyring::app())
    };
    let frontend = std::env::var_os("MUSAIC_FRONTEND_DIR")
        .filter(|v| !v.is_empty())
        .map(Into::into);
    let mut allowed = AllowedHosts::for_bind(&host);
    if let Some(extra) = env("MUSAIC_ALLOWED_HOSTS") {
        allowed.extend(AllowedHosts::from_names(extra.split(',')));
    }
    let unspecified = matches!(host.trim(), "0.0.0.0" | "::" | "[::]");
    if unspecified && allowed.names().is_empty() {
        log.log(
            Level::Warning,
            "musaic.main",
            &format!(
                "bound to {host} but MUSAIC_ALLOWED_HOSTS is empty: only requests addressed to localhost / 127.0.0.1 / [::1] are answered"
            ),
        );
    }
    let addrs = match musaic_api::bind::bind_addrs(&host, port) {
        Ok(a) => a,
        Err(e) => die(2, &format!("bad address {host}:{port}: {e}")),
    };
    // Bound before the database is opened: a port in use leaves nothing
    // started (no migration, no snapshot, no worker).
    let listener = match tokio::net::TcpListener::bind(addrs.as_slice()).await {
        Ok(l) => l,
        Err(e) => die(1, &format!("cannot bind {host}:{port}: {e}")),
    };
    // Agent pairing secrets: the OS keyring under service `musaic-agent`, or
    // memory (and a scratch-dir credential file for a separate CLI process) in
    // a verification run, which is already limited to scratch directories.
    let agent_memory = memory_secrets.is_some();
    let agent_secrets: SharedSecrets = if agent_memory {
        Arc::new(musaic_secrets::MemoryStore::new())
    } else {
        Arc::new(musaic_secrets::agent::agent_keyring())
    };
    let state = match AppState::builder(config_dir)
        .allowed_hosts(allowed)
        .secrets(secrets)
        .agent_secrets(agent_secrets)
        .agent_dev_credentials(agent_memory)
        .network(network)
        // Every health check runs on its own (issue #187); checks that
        // need the network only when there is one.
        .check_sweep(true)
        // No `host_cleanup`: this server never writes a Claude Desktop
        // bundle, and its startup sweep would remove the one a running
        // desktop app has just handed to Claude Desktop.
        .sweep_network(network_name != "offline")
        .frontend_dir(frontend)
        .log_sink(log.sink())
        .log_masks(masks)
        .build()
    {
        Ok(s) => s,
        Err(e) => die(1, &format!("startup failed: {e}")),
    };
    let addr = listener
        .local_addr()
        .map_or_else(|_| format!("{host}:{port}"), |a| a.to_string());
    if let Ok(bound) = listener.local_addr() {
        musaic_api::agent_guard::publish_descriptor(&state, bound);
    }
    let stop = stop_signal();
    println!("[musaic] listening on http://{addr}");
    log.log(
        Level::Info,
        "musaic.main",
        &format!("listening on http://{addr}"),
    );
    let stopping = state.clone();
    let served = axum::serve(
        listener,
        router(state.clone())
            .into_make_service_with_connect_info::<musaic_api::agent_guard::ConnInfo>(),
    )
    .with_graceful_shutdown(async move {
        stop.await;
        // Open SSE streams end cleanly so the drain finishes.
        stopping.begin_shutdown();
    })
    .await;
    musaic_api::agent_guard::remove_descriptor(&state);
    log.log(Level::Info, "musaic.main", "shutting down");
    // The lifespan's shutdown: stop the fingerprint pool (its thread exits
    // between two files, waited for with a bound), then checkpoint and close
    // the database, before the process ends.
    let _ = tokio::task::spawn_blocking(move || state.shutdown()).await;
    if let Err(e) = served {
        die(1, &format!("server error: {e}"));
    }
    log.log(Level::Info, "musaic.main", "stopped");
    flush_log();
}
