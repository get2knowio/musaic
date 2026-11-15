//! Replaying a musaic-services golden (testdata/golden/services/*.json,
//! recorded by driving the Python app's real routes through TestClient) through
//! this router: a library built from the golden's FLAC recipes, an app
//! state like the generator's (no lifespan: no bus subscribers, no
//! auto-snapshot; network off), a recorder on the bus, and the
//! generator's normalisation. The recipes and the normalisation are the
//! ones musaic-services' own golden tests use (included, not copied).
#![allow(dead_code)]

#[path = "../../../musaic-services/tests/common/mod.rs"]
pub mod svc;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};

use axum::Router;
use musaic_api::{AppState, AppStateBuilder, Fingerprinting, Network, router};
use musaic_events::{Event, Subscriber, Topic};
use musaic_scan::py_resolve;
use serde_json::Value;

use crate::common::{self, Got};

/// The generators' fixed clock value for rows they stamp.
pub const NOW_EPOCH: u64 = 1_893_456_000; // 2030-01-01T00:00:00Z

pub struct World {
    _td: tempfile::TempDir,
    pub root: PathBuf,
    pub lib: PathBuf,
    pub cfg: PathBuf,
    pub norm: svc::Norm,
    /// dir name -> album id.
    pub ids: HashMap<String, String>,
    pub state: AppState,
    pub app: Router,
    pub log: Arc<Mutex<Vec<Event>>>,
}

impl World {
    /// The golden's albums built under `<tmp>/lib`; `extra_dirs` are album
    /// directory names that appear later (renames). `customise` adjusts the
    /// builder (stubs, pauses).
    pub fn new(
        g: &Value,
        extra_dirs: &[String],
        customise: impl FnOnce(AppStateBuilder) -> AppStateBuilder,
    ) -> World {
        let td = tempfile::tempdir().unwrap();
        let root = py_resolve(td.path());
        let lib = root.join("lib");
        let cfg = root.join("cfg");
        std::fs::create_dir_all(&lib).unwrap();
        std::fs::create_dir_all(&cfg).unwrap();
        for a in g["albums"].as_array().unwrap() {
            svc::build_album(&lib, a);
        }
        let mut norm = svc::Norm::new(&lib);
        let mut ids = HashMap::new();
        let dirs = g["albums"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["dir"].as_str().unwrap().to_string())
            .chain(extra_dirs.iter().cloned());
        for d in dirs {
            norm.learn_dir(&d);
            ids.insert(
                d.clone(),
                musaic_core::album_id(&py_resolve(&lib.join(&d)).to_string_lossy()),
            );
        }
        let b = AppState::builder(&cfg)
            .secrets(Arc::new(musaic_secrets::MemoryStore::new()))
            .network(Network::Offline)
            .fingerprinting(Fingerprinting::Unavailable(common::PYTHON_NO_KEY.into()))
            .subscribers(false)
            .auto_snapshot(false)
            .clock(Arc::new(|| UNIX_EPOCH + Duration::from_secs(NOW_EPOCH)));
        let state = customise(b).build().unwrap();
        let log: Arc<Mutex<Vec<Event>>> = Arc::default();
        let l = log.clone();
        state.bus().register(
            Topic::Event,
            Subscriber::from_fn(move |ev| {
                l.lock().unwrap().push(ev.clone());
                Ok(())
            })
            .priority(400)
            .name("golden_recorder"),
        );
        let app = router(state.clone());
        World {
            _td: td,
            root,
            lib,
            cfg,
            norm,
            ids,
            state,
            app,
            log,
        }
    }

    pub fn lib_str(&self) -> String {
        self.lib.to_string_lossy().into_owned()
    }

    pub fn id(&self, dir: &str) -> String {
        self.ids[dir].clone()
    }

    /// `<ID:dir>` and `<LIB>` tokens back to real values.
    pub fn unnorm(&self, s: &str) -> String {
        let mut out = s.replace("<LIB>", &self.lib_str());
        for (d, id) in &self.ids {
            out = out.replace(&format!("<ID:{d}>"), id);
        }
        out
    }

    pub fn unnorm_value(&self, v: &Value) -> Value {
        match v {
            Value::String(s) => Value::String(self.unnorm(s)),
            Value::Array(a) => Value::Array(a.iter().map(|x| self.unnorm_value(x)).collect()),
            Value::Object(o) => Value::Object(
                o.iter()
                    .map(|(k, x)| (k.clone(), self.unnorm_value(x)))
                    .collect(),
            ),
            other => other.clone(),
        }
    }

    /// Send a recorded request (`{method, path, json?, params?}`).
    pub async fn send(&self, req: &Value) -> Got {
        let method = req["method"].as_str().unwrap();
        let mut path = self.unnorm(req["path"].as_str().unwrap());
        if let Some(p) = req.get("params").and_then(Value::as_object) {
            let q: Vec<String> = p
                .iter()
                .map(|(k, v)| format!("{k}={}", v.as_str().unwrap()))
                .collect();
            path = format!("{path}?{}", q.join("&"));
        }
        let body = req.get("json").map(|j| self.unnorm_value(j));
        common::call(&self.app, method, &path, body.as_ref()).await
    }

    pub async fn get(&self, path: &str) -> Got {
        common::call(&self.app, "GET", path, None).await
    }

    pub async fn post(&self, path: &str, body: Option<&Value>) -> Got {
        common::call(&self.app, "POST", path, body).await
    }

    /// Configure the library and run a full scan to its end.
    pub async fn configure_and_scan(&self) {
        let r = self
            .post(
                "/api/config",
                Some(&serde_json::json!({"library_dir": self.lib_str()})),
            )
            .await;
        assert_eq!(r.status, 200, "{}", r.text());
        let r = self.get("/api/scan/stream").await;
        assert_eq!(r.status, 200);
        assert!(r.text().contains("event: done"), "{}", r.text());
        // The stream's `done` precedes the scan thread's last steps (pool
        // kick-off, end-of-scan log) and the release of its work guard, so a
        // test that takes the restore gate next would race them.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while self.state.gate().snapshot().0 != 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "work still running 30 s after the scan's done event"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    pub fn take_events(&self) -> Vec<Value> {
        let mut l = self.log.lock().unwrap();
        let out = l
            .iter()
            .map(|e| self.norm.value(&svc::event_json(e)))
            .collect();
        l.clear();
        out
    }

    /// A response body as the generators record it: parsed JSON, else text.
    pub fn body(&self, got: &Got) -> Value {
        if got.ctype().starts_with("application/json") {
            self.norm.value(&got.json())
        } else {
            Value::String(self.norm.str(&got.text()))
        }
    }

    pub fn conn(&self) -> rusqlite::Connection {
        svc::connect(self.state.db_path())
    }
}
