//! Selftest report: JSON lines a test harness reads to follow the boot
//! (`--selftest-report FILE`, dev runs only). Disabled in normal runs.

use std::collections::HashSet;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Default)]
pub struct Report(Option<Arc<Inner>>);

struct Inner {
    path: PathBuf,
    lock: Mutex<HashSet<String>>,
}

impl Report {
    pub fn new(path: Option<PathBuf>) -> Self {
        Report(path.map(|path| {
            Arc::new(Inner {
                path,
                lock: Mutex::new(HashSet::new()),
            })
        }))
    }

    pub fn enabled(&self) -> bool {
        self.0.is_some()
    }

    pub fn event(&self, name: &str, mut data: serde_json::Value) {
        let Some(inner) = &self.0 else { return };
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        if let Some(o) = data.as_object_mut() {
            o.insert("event".into(), name.into());
            o.insert("ts".into(), ts.into());
        }
        let _g = inner.lock.lock().unwrap_or_else(PoisonError::into_inner);
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&inner.path)
        {
            let _ = writeln!(f, "{data}");
        }
    }

    /// First authenticated request per (method, path).
    pub fn seen(&self, method: &str, path: &str) {
        let Some(inner) = &self.0 else { return };
        let key = format!("{method} {path}");
        let first = inner
            .lock
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key);
        if first {
            self.event(
                "request",
                serde_json::json!({"method": method, "path": path}),
            );
        }
    }
}
