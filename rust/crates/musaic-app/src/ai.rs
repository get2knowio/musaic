//! The research requests' host side: the change signal and the one watch
//! channel the jobs stream connections subscribe to ([`AiQueue`]), the ids
//! of new job cards, and [`AppEnv`], the [`ResolverEnv`] that gives the
//! synthesis this app's resolver and track reader.
//!
//! Musaic has no built-in model: research the resolver cannot settle goes to
//! paired assistants (`crate::desktop_research`).
//!
//! The jobs stream: one watch receiver per connection (as for the pool
//! stream), so no connection can consume another's wake-up; the Python app
//! shares one `asyncio.Event` that each of them `clear()`s.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};

use musaic_ai::{Py, ResolverEnv, Signal};
use musaic_resolve::AlbumResolution;
use rusqlite::Connection;
use tokio::sync::watch;

use crate::app::{App, AppInner};

/// Makes new job ids (`secrets.token_hex(8)`: 16 hex characters).
pub type JobIds = Arc<dyn Fn() -> String + Send + Sync>;

/// 16 random hex characters (std's randomly seeded hasher over a counter,
/// the clock and the thread: unique, not secret).
pub fn random_job_id() -> String {
    use std::hash::{BuildHasher, Hasher};
    static N: AtomicU64 = AtomicU64::new(0);
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u64(N.fetch_add(1, Ordering::Relaxed));
    h.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    );
    h.write(format!("{:?}", std::thread::current().id()).as_bytes());
    format!("{:016x}", h.finish())
}

/// The research requests' shared state.
pub struct AiQueue {
    pub signal: Arc<Signal>,
    /// Bumped on every [`Signal::notify`]; one receiver per stream
    /// connection.
    pub changes: watch::Sender<u64>,
    pub ids: JobIds,
}

impl AiQueue {
    pub fn new(ids: JobIds) -> AiQueue {
        let signal = Signal::new();
        let (changes, _) = watch::channel(0u64);
        let tx = changes.clone();
        signal.add_listener(Arc::new(move || {
            tx.send_modify(|n| *n = n.wrapping_add(1));
        }));
        AiQueue {
            signal,
            changes,
            ids,
        }
    }

    /// `notify_jobs_changed()`.
    pub fn notify(&self) {
        self.signal.notify();
    }
}

/// The synthesis' view of this app.
pub struct AppEnv {
    pub(crate) inner: Weak<AppInner>,
}

impl AppEnv {
    pub fn new(state: &App) -> AppEnv {
        AppEnv {
            inner: Arc::downgrade(&state.0),
        }
    }

    fn st(&self) -> Option<App> {
        self.inner.upgrade().map(App)
    }
}

impl ResolverEnv for AppEnv {
    fn resolve(
        &self,
        conn: &Connection,
        album_id: &str,
        hint: &str,
    ) -> Result<Arc<AlbumResolution>, String> {
        let st = self.st().ok_or("the app has stopped")?;
        let lib = st.library_dir();
        st.parts()
            .resolve(conn, lib.as_deref(), album_id, hint, false)
    }

    fn read_tracks(&self, album_path: &str) -> Py {
        Py::from(&musaic_services::library_ops::read_tracks(album_path))
    }
}

impl App {
    /// `notify_jobs_changed()`: wakes every jobs stream.
    pub fn ai_jobs_changed(&self) {
        self.0.ai.notify();
    }
}
