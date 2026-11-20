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
pub fn hold_restore(app: &App) -> RestoreGuard {
    app.gate().begin_restore().expect("no work is running")
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
