//! Shared helpers for the operations surface's tests: an app on a scratch config
//! dir (in-memory secrets, no network), a copy of the synthetic library to point
//! it at, and a held restore guard.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use musaic_app::{App, AppBuilder};
use musaic_services::gate::RestoreGuard;
use musaic_services::production::Network;

/// A scratch directory holding the config dir and library copies; removed on drop.
pub struct Scratch {
    pub tmp: tempfile::TempDir,
    pub config: PathBuf,
}

/// An app on a fresh config dir under the system temp dir: no lifespan work
/// (no subscribers, no auto-snapshot), in-memory secrets, no network.
pub fn app() -> (App, Scratch) {
    app_with(|b| b)
}

/// [`app`] with more of the builder set by `more` (a scan hook, a fingerprinter).
pub fn app_with(more: impl FnOnce(AppBuilder) -> AppBuilder) -> (App, Scratch) {
    let tmp = tempfile::tempdir().expect("scratch dir");
    let config = tmp.path().join("config");
    let builder = App::builder(&config)
        .secrets(Arc::new(musaic_secrets::MemoryStore::new()))
        .network(Network::Offline)
        .subscribers(false)
        .auto_snapshot(false)
        .check_sweep(false)
        .ai_worker(false);
    let app = more(builder).build().expect("app builds");
    (app, Scratch { tmp, config })
}

/// A fingerprinter that answers at once, so the pool pass a finished scan starts
/// does no real work.
pub struct Fixed;

impl musaic_services::fingerprint::Fingerprinter for Fixed {
    fn fingerprint(&self, _path: &Path) -> Result<(String, u64), String> {
        Ok(("AQAAstub".into(), 3))
    }
    fn implementation(&self) -> &str {
        musaic_db::FINGERPRINT_IMPL_RUST
    }
}

/// A pool that never sleeps.
pub struct NoSleep;

impl musaic_services::fingerprint_pool::PoolSleeper for NoSleep {
    fn sleep(
        &self,
        _d: std::time::Duration,
        cancel: &musaic_services::fingerprint::CancelToken,
    ) -> bool {
        !cancel.is_cancelled()
    }
}

/// An app whose fingerprinting and pool pacing are stubbed.
pub fn app_quiet_pool(more: impl FnOnce(AppBuilder) -> AppBuilder) -> (App, Scratch) {
    app_with(|b| {
        more(
            b.fingerprinting(musaic_services::production::Fingerprinting::Local(
                Arc::new(Fixed),
            ))
            .pool_sleeper(Arc::new(NoSleep)),
        )
    })
}

/// Points the app at a copy of the synthetic library, through the operation.
pub fn configure(app: &App, scratch: &Scratch) -> PathBuf {
    let lib = library_copy(scratch);
    app.ops()
        .library()
        .set_library_dir(lib.to_str().unwrap())
        .expect("library folder accepted");
    lib
}

/// Copies the synthetic library into the scratch dir and returns the copy (the
/// original is never written to).
pub fn library_copy(scratch: &Scratch) -> PathBuf {
    let dst = scratch.tmp.path().join("library");
    copy_dir(&musaic_testdata::synthetic_library(), &dst);
    dst
}

fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).expect("create dir");
    for entry in std::fs::read_dir(src).expect("read dir") {
        let entry = entry.expect("entry");
        let to = dst.join(entry.file_name());
        if entry.file_type().expect("file type").is_dir() {
            copy_dir(&entry.path(), &to);
        } else {
            std::fs::copy(entry.path(), &to).expect("copy file");
        }
    }
}

/// Holds the restore gate: while it lives, work that writes is refused as busy.
/// Background work a scan started (the fingerprint pass, a check rerun) is
/// waited out first.
pub fn hold_restore(app: &App) -> RestoreGuard {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        match app.gate().begin_restore() {
            Ok(g) => return g,
            Err(_) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            Err(e) => panic!("no work is running: {e}"),
        }
    }
}

/// Waits until the fingerprint pass a finished scan starts has finished, so a
/// check that reads fingerprints (duplicate tracks) sees every album's. The
/// pass runs on its own thread; on a slow machine a check run straight after
/// the scan can otherwise find no fingerprints at all.
#[allow(dead_code)] // not every test binary that includes this module calls it
pub fn wait_for_fingerprints(app: &App) {
    use musaic_services::fingerprint_pool::PoolStatus;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        let s = app.ops().system().pool_status();
        if s.status == PoolStatus::Done && s.completed + s.errors >= s.total {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the fingerprint pass did not finish: {s:?}"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

// ------------------------------------------------------------ agent bridge

use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use musaic_app::ops::agent::{AgentError, CreatedPairing, Principal};
use musaic_commands::auth::{self, ExchangeRequest, client_proof, hex_decode, random_hex};
use musaic_commands::{PermissionKind, ToolCall, ToolError, ToolOutput, parse_call};
use musaic_secrets::MemoryStore;

/// A scripted wall clock the agent tests move by hand.
#[derive(Clone)]
pub struct FakeClock(pub Arc<Mutex<SystemTime>>);

impl FakeClock {
    pub fn new() -> Self {
        FakeClock(Arc::new(Mutex::new(
            SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000),
        )))
    }

    pub fn advance(&self, secs: u64) {
        *self.0.lock().unwrap() += Duration::from_secs(secs);
    }

    pub fn wall(&self) -> musaic_app::app::WallClock {
        let t = self.0.clone();
        Arc::new(move || *t.lock().unwrap())
    }
}

/// An app with a fake clock and an agent secrets store the test can read.
pub struct AgentWorld {
    pub app: App,
    pub scratch: Scratch,
    pub clock: FakeClock,
    pub store: Arc<MemoryStore>,
}

pub fn agent_world() -> AgentWorld {
    let clock = FakeClock::new();
    let store = Arc::new(MemoryStore::new());
    let (c2, s2) = (clock.clone(), store.clone());
    let (app, scratch) = app_quiet_pool(move |b| b.clock(c2.wall()).agent_secrets(s2));
    AgentWorld {
        app,
        scratch,
        clock,
        store,
    }
}

/// An agent world whose library was configured and scanned.
pub fn agent_world_scanned() -> AgentWorld {
    let w = agent_world();
    configure(&w.app, &w.scratch);
    let n = w.app.ops().scan().subscribe(false).unwrap().count();
    assert!(n > 2);
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while w.app.gate().snapshot() != (0, false) {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    // The pass a scan starts writes fingerprints on its own thread; a test
    // that proves a read wrote nothing must not see those writes.
    wait_for_fingerprints(&w.app);
    w
}

pub fn config_id(app: &App) -> String {
    let resolved = musaic_core::config::resolve_path(app.config_dir()).unwrap();
    auth::config_dir_id(&resolved.to_string_lossy())
}

impl AgentWorld {
    pub fn pair(&self, name: &str, scopes: &[PermissionKind]) -> CreatedPairing {
        self.app
            .ops()
            .agent()
            .create_pairing(name, scopes)
            .expect("pairing created")
    }

    /// The handshake an honest client makes, then the principal of its token.
    pub fn login(&self, pairing_id: &str) -> Principal {
        let ops_agent = self.app.ops();
        let agent = ops_agent.agent();
        let hello = agent.hello();
        let secret = hex_decode(
            &musaic_secrets::agent::get_secret(
                self.store.as_ref(),
                &config_id(&self.app),
                pairing_id,
            )
            .unwrap()
            .expect("the secret is in the store"),
        )
        .unwrap();
        let client_nonce = random_hex(16);
        let proof = client_proof(
            &secret,
            pairing_id,
            &hello.instance_id,
            &hello.server_nonce,
            &client_nonce,
        );
        let resp = agent
            .exchange(&ExchangeRequest {
                pairing_id: pairing_id.to_string(),
                instance_id: hello.instance_id.clone(),
                server_nonce: hello.server_nonce.clone(),
                client_nonce: client_nonce.clone(),
                client_proof: proof,
            })
            .expect("exchange succeeds");
        assert!(auth::verify_server_proof(
            &secret,
            &resp.server_proof,
            pairing_id,
            &hello.instance_id,
            &hello.server_nonce,
            &client_nonce
        ));
        agent
            .authenticate(&resp.token)
            .expect("the token authenticates")
    }

    /// One tool call from a JSON body, as the HTTP route would parse it.
    pub fn call(
        &self,
        p: &Principal,
        tool: &str,
        body: serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        let call: ToolCall = parse_call(tool, body.to_string().as_bytes())?;
        self.app.ops().agent().call_tool(p, call)
    }
}

pub fn tool_code(e: &Result<ToolOutput, ToolError>) -> String {
    match e {
        Ok(_) => "ok".into(),
        Err(e) => e.code.as_str().to_string(),
    }
}

pub fn agent_err_code(e: &AgentError) -> String {
    match e {
        AgentError::Tool(t) => t.code.as_str().to_string(),
        AgentError::Conflict { .. } => "CONFLICT".into(),
        AgentError::ChangedSince { .. } => "CHANGED_SINCE".into(),
        AgentError::Op(_) => "OP".into(),
    }
}
