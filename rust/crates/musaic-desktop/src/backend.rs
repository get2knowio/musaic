//! The in-process musaic-api server on an ephemeral loopback port, through
//! musaic-api's [`serve_loopback`] (bind, serve, graceful teardown live
//! there; this file only builds the state the desktop way and adds the
//! window's [`gate`](crate::gate) layer outside the router).
//!
//! The launch token is musaic-api's (`launch_token: true`).
//! The state logs into the process's `server.log` ([`start_log`]).
//! [`Backend::work`] says what is running for the quit prompt;
//! [`Backend::stop`] ends every SSE stream before draining.

use std::sync::Arc;

use musaic_api::logging::{Level, ProcessLog, ServerLogOptions};
use musaic_api::{
    AllowedHosts, AppState, LoopbackOptions, LoopbackServer, Network, SharedSecrets, StopReport,
    WorkSnapshot, serve_loopback,
};

use crate::gate::{self, Gate, Hooks};
use crate::options::{NetworkMode, Options};

pub use musaic_api::loopback::DRAIN;

/// The logger name of the shell's own lines.
pub const TARGET: &str = "musaic.desktop";

pub struct Backend {
    pub addr: std::net::SocketAddr,
    pub gate: Arc<Gate>,
    server: LoopbackServer,
}

fn network(mode: &NetworkMode) -> Network {
    match mode {
        NetworkMode::Live => Network::live(),
        NetworkMode::Offline => Network::Offline,
        NetworkMode::Replay(d) => Network::Replay(d.clone()),
    }
}

fn secrets(opts: &Options) -> SharedSecrets {
    if opts.dev_memory_secrets {
        Arc::new(musaic_secrets::MemoryStore::new())
    } else {
        // Production: the OS keyring (macOS Keychain, Windows Credential
        // Manager, Secret Service). Untested from this crate: no test or
        // dev run may touch the real keychain.
        Arc::new(musaic_secrets::OsKeyring::app())
    }
}

fn agent_secrets(opts: &Options) -> SharedSecrets {
    if opts.dev_memory_secrets {
        Arc::new(musaic_secrets::MemoryStore::new())
    } else {
        Arc::new(musaic_secrets::agent::agent_keyring())
    }
}

/// The same rotating `<config>/server.log` (5 MiB x 5, secret
/// values masked) the server binary writes, level from `MUSAIC_LOG_LEVEL`
/// (INFO when unset or unknown, as the binary's variable). Once per
/// process.
pub fn start_log(opts: &Options) -> ProcessLog {
    let raw = std::env::var(musaic_api::logging::LEVEL_ENV).unwrap_or_default();
    let threshold = if raw.is_empty() {
        Level::Info as u8
    } else {
        musaic_api::logging::level_from_env_value(&raw)
    };
    let p = musaic_api::logging::start_process_log(
        &opts.config_dir,
        threshold,
        ServerLogOptions::default(),
    );
    p.log.log(
        Level::Info,
        TARGET,
        &format!(
            "musaic-desktop {} starting: schema v{}, secrets {}, config dir {}, pid {}",
            env!("CARGO_PKG_VERSION"),
            musaic_db::SCHEMA_VERSION,
            if opts.dev_memory_secrets {
                "in memory (dev)"
            } else {
                "OS keyring"
            },
            opts.config_dir.display(),
            std::process::id(),
        ),
    );
    p
}

/// Build the state (opens / migrates `library.db`; blocking). With a log,
/// the state writes into it and masks every secret it sees there.
pub fn build_state(opts: &Options, log: Option<&ProcessLog>) -> Result<AppState, String> {
    if let Some(d) = &opts.frontend_dir
        && !d.join("index.html").is_file()
    {
        return Err(format!(
            "frontend dir {} has no index.html (build it with `npm ci && npm run build` in frontend/)",
            d.display()
        ));
    }
    let mut b = AppState::builder(opts.config_dir.clone())
        // Loopback only: no extra host names.
        .allowed_hosts(AllowedHosts::none())
        .secrets(secrets(opts))
        // Agent pairing secrets: the OS keyring under service `musaic-agent`;
        // in memory (with a scratch-dir credential file for a separate CLI
        // process) for a development run, which is limited to scratch dirs.
        .agent_secrets(agent_secrets(opts))
        .agent_dev_credentials(opts.dev_memory_secrets)
        .network(network(&opts.network))
        .frontend_dir(opts.frontend_dir.clone())
        // Every health check runs on its own (issue #187).
        .check_sweep(true)
        // Leftover Claude Desktop bundles in the temporary directory.
        .host_cleanup(true);
    if let Some(l) = log {
        b = b.log_sink(l.log.sink()).log_masks(l.masks.clone());
    }
    b.build().map_err(|e| format!("startup failed: {e}"))
}

/// Bind and serve. Must run inside a Tokio runtime.
pub async fn start(
    state: AppState,
    port: u16,
    focus_secret: String,
    hooks: Arc<dyn Hooks>,
) -> Result<Backend, String> {
    let mut window_gate: Option<Arc<Gate>> = None;
    let gate_state = state.clone();
    let server = serve_loopback(
        state,
        LoopbackOptions {
            port,
            launch_token: true,
            drain: DRAIN,
        },
        |router, bound| {
            let launch = bound
                .launch
                .clone()
                .expect("launch_token: true gives a launch gate");
            let g = Arc::new(Gate::new(launch, focus_secret, hooks, gate_state));
            window_gate = Some(g.clone());
            router.layer(axum::middleware::from_fn_with_state(g, gate::guard))
        },
    )
    .await?;
    let gate = window_gate.ok_or("the window gate was not built")?;
    server
        .state()
        .log()
        .info(TARGET, &format!("listening on http://{}", server.addr));
    Ok(Backend {
        addr: server.addr,
        gate,
        server,
    })
}

impl Backend {
    pub fn origin(&self) -> String {
        self.server.origin()
    }

    pub fn state(&self) -> &AppState {
        self.server.state()
    }

    /// What is running now.
    pub fn work(&self) -> WorkSnapshot {
        self.server.state().work_snapshot()
    }

    /// End SSE streams, stop accepting, drain for up to [`DRAIN`], abort
    /// what is left, then the lifespan teardown (musaic-api's
    /// `LoopbackServer::stop`).
    pub async fn stop(self) -> StopReport {
        let log = self.server.state().log().clone();
        log.info(TARGET, "shutting down");
        let r = self.server.stop().await;
        log.info(
            TARGET,
            &format!(
                "stopped (drained: {}, teardown: {})",
                r.drained, r.state_shutdown
            ),
        );
        r
    }
}
