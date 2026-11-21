//! A running app for the CLI's tests: the API's loopback server in process
//! (which writes the descriptor), on a scratch config dir with a copy of the
//! synthetic library and in-memory agent secrets.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use musaic_api::loopback::{LoopbackOptions, LoopbackServer, serve_loopback};
use musaic_api::{AppState, Network};
use musaic_cli::client::{Client, config_id_of};
use musaic_cli::credentials::Memory;
use musaic_commands::PermissionKind;
use musaic_secrets::MemoryStore;

pub struct World {
    pub tmp: tempfile::TempDir,
    pub config: PathBuf,
    pub rt: tokio::runtime::Runtime,
    pub server: Option<LoopbackServer>,
    pub state: AppState,
    pub store: Arc<MemoryStore>,
    pub log: Arc<Mutex<Vec<String>>>,
}

pub fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for e in std::fs::read_dir(src).unwrap().flatten() {
        let to = dst.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &to);
        } else {
            std::fs::copy(e.path(), &to).unwrap();
        }
    }
}

fn build_state(
    config: &Path,
    store: Arc<MemoryStore>,
    log: Arc<Mutex<Vec<String>>>,
    dev_credentials: bool,
) -> AppState {
    AppState::builder(config)
        .secrets(Arc::new(MemoryStore::new()))
        .agent_secrets(store)
        .agent_dev_credentials(dev_credentials)
        .network(Network::Offline)
        .subscribers(false)
        .auto_snapshot(false)
        .log(Arc::new(move |l: &str| {
            log.lock().unwrap().push(l.to_string())
        }))
        .build()
        .unwrap()
}

impl World {
    pub fn new() -> World {
        Self::with(false, true)
    }

    pub fn with(dev_credentials: bool, scanned: bool) -> World {
        let tmp = tempfile::tempdir().unwrap();
        let config = tmp.path().join("config");
        let store = Arc::new(MemoryStore::new());
        let log = Arc::new(Mutex::new(Vec::new()));
        let state = build_state(&config, store.clone(), log.clone(), dev_credentials);
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let mut w = World {
            tmp,
            config,
            rt,
            server: None,
            state,
            store,
            log,
        };
        if scanned {
            let lib = w.tmp.path().join("library");
            copy_dir(&musaic_testdata::synthetic_library(), &lib);
            let ops = w.state.ops();
            ops.library()
                .set_library_dir(lib.to_str().unwrap())
                .unwrap();
            assert!(ops.scan().subscribe(false).unwrap().count() > 2);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            while w.state.gate().snapshot() != (0, false) {
                assert!(std::time::Instant::now() < deadline);
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
        w.serve();
        w
    }

    pub fn serve(&mut self) {
        let state = self.state.clone();
        self.server = Some(
            self.rt
                .block_on(serve_loopback(state, LoopbackOptions::default(), |r, _| r))
                .unwrap(),
        );
    }

    /// Stop the server (the descriptor is removed).
    pub fn stop(&mut self) {
        if let Some(s) = self.server.take() {
            self.rt.block_on(s.stop());
        }
    }

    /// Stop and start a new app on the same config dir: a new instance and port.
    pub fn restart(&mut self) {
        self.stop();
        self.state = build_state(&self.config, self.store.clone(), self.log.clone(), false);
        self.serve();
    }

    pub fn pair(&self, scopes: &[PermissionKind]) -> String {
        self.state
            .ops()
            .agent()
            .create_pairing("test", scopes)
            .unwrap()
            .pairing
            .id
    }

    pub fn secret(&self, pairing: &str) -> Vec<u8> {
        let hex = musaic_secrets::agent::get_secret(
            self.store.as_ref(),
            &config_id_of(&self.config),
            pairing,
        )
        .unwrap()
        .unwrap();
        musaic_commands::auth::hex_decode(&hex).unwrap()
    }

    pub fn secret_hex(&self, pairing: &str) -> String {
        musaic_secrets::agent::get_secret(self.store.as_ref(), &config_id_of(&self.config), pairing)
            .unwrap()
            .unwrap()
    }

    /// A client holding this pairing's secret in memory.
    pub fn client(&self, pairing: &str) -> Client {
        let m = Memory::new();
        m.put(&config_id_of(&self.config), pairing, self.secret(pairing));
        Client::new(&self.config, Box::new(m), Some(pairing.to_string()))
    }
}

impl Drop for World {
    fn drop(&mut self) {
        self.stop();
    }
}
