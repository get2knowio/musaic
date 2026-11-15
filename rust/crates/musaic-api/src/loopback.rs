//! The in-process server on a loopback port, for hosts that embed the API
//! (the desktop shell; docs/desktop.md asked for `serve_loopback`).
//!
//! [`serve_loopback`] binds `127.0.0.1:<port>` (0 = an ephemeral port),
//! serves [`crate::router_with`] (with the per-launch token
//! when asked), and returns the bound address and a [`LoopbackServer`]
//! whose [`stop`](LoopbackServer::stop) is the whole teardown:
//!
//! 1. [`AppState::begin_shutdown`]: every SSE stream ends cleanly;
//! 2. stop accepting and drain open requests for up to
//!    [`LoopbackOptions::drain`]; whatever is left is aborted (the listener
//!    is dropped either way, so the port is released);
//! 3. [`AppState::shutdown`] (blocking): the lifespan's teardown (pool
//!    stopped and joined, AI worker stopped, pending key scrub retried).
//!
//! The bind host is always the IPv4 loopback: an embedding host never
//! listens on another interface.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::launch::LaunchGate;
use crate::{AppState, RouterOptions, router_with};

/// How long [`LoopbackServer::stop`] waits for open requests by default.
pub const DRAIN: Duration = Duration::from_secs(3);

#[derive(Debug, Clone)]
pub struct LoopbackOptions {
    /// 0 = any free port.
    pub port: u16,
    /// Require the per-launch cookie (the desktop app).
    pub launch_token: bool,
    /// Graceful drain before the remaining connections are aborted.
    pub drain: Duration,
}

impl Default for LoopbackOptions {
    fn default() -> Self {
        LoopbackOptions {
            port: 0,
            launch_token: false,
            drain: DRAIN,
        }
    }
}

/// What the host's own outer layer may need once the port is bound.
#[derive(Debug, Clone)]
pub struct Bound {
    pub addr: SocketAddr,
    pub launch: Option<Arc<LaunchGate>>,
}

/// The running server.
pub struct LoopbackServer {
    pub addr: SocketAddr,
    /// The launch gate, when [`LoopbackOptions::launch_token`] was set.
    pub launch: Option<Arc<LaunchGate>>,
    state: AppState,
    drain: Duration,
    stop: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<std::io::Result<()>>>,
}

/// What [`LoopbackServer::stop`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopReport {
    /// Every connection ended within the drain time.
    pub drained: bool,
    /// The lifespan teardown ran to the end.
    pub state_shutdown: bool,
}

/// Bind and serve; `wrap` adds the host's own outer layers (it gets the
/// router and the bound address). Must run inside a Tokio runtime.
pub async fn serve_loopback(
    state: AppState,
    opts: LoopbackOptions,
    wrap: impl FnOnce(Router, &Bound) -> Router,
) -> Result<LoopbackServer, String> {
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, opts.port))
        .await
        .map_err(|e| format!("cannot bind 127.0.0.1:{}: {e}", opts.port))?;
    let addr = listener
        .local_addr()
        .map_err(|e| format!("no local address: {e}"))?;
    let launch = if opts.launch_token {
        Some(Arc::new(LaunchGate::random(addr.port())?))
    } else {
        None
    };
    let bound = Bound {
        addr,
        launch: launch.clone(),
    };
    let app = wrap(
        router_with(
            state.clone(),
            RouterOptions {
                launch: launch.clone(),
            },
        ),
        &bound,
    );
    let (tx, rx) = oneshot::channel::<()>();
    let task = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = rx.await;
            })
            .await
    });
    Ok(LoopbackServer {
        addr,
        launch,
        state,
        drain: opts.drain,
        stop: Some(tx),
        task: Some(task),
    })
}

impl LoopbackServer {
    /// `http://127.0.0.1:<port>`.
    pub fn origin(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub fn state(&self) -> &AppState {
        &self.state
    }

    /// The whole teardown (module docs). Idempotent work only; consumes the
    /// server.
    pub async fn stop(mut self) -> StopReport {
        self.state.begin_shutdown();
        if let Some(tx) = self.stop.take() {
            let _ = tx.send(());
        }
        let mut drained = false;
        if let Some(mut task) = self.task.take() {
            match tokio::time::timeout(self.drain, &mut task).await {
                Ok(_) => drained = true,
                Err(_) => {
                    task.abort();
                    let _ = task.await;
                }
            }
        }
        let st = self.state.clone();
        let state_shutdown = tokio::task::spawn_blocking(move || st.shutdown())
            .await
            .is_ok();
        StopReport {
            drained,
            state_shutdown,
        }
    }
}
