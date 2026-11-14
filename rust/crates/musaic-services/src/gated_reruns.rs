//! The bus-driven check reruns under the restore gate:
//! [`GatedEffects`] wraps the checks' side effects so every post-scan /
//! scoped rerun takes the gate for its duration, logs its outcome
//! (`musaic.checks.runner`), and a rerun refused during a restore is
//! replayed, coalesced, once the restore ends ([`install`]). This lives
//! here rather than in musaic-api so a non-HTTP face registers the same
//! subscribers.

use std::sync::Arc;

use musaic_checks::ChecksSideEffects;

use crate::gate::{DeferredRerun, WorkGate};
use crate::host_log::HostLog;

/// The logger name of check runs (`musaic.checks.runner`).
pub const CHECKS_TARGET: &str = "musaic.checks.runner";

/// The bus-driven check reruns (post-scan, post-apply, rename)
/// are check runs too: each takes a [`crate::gate::WorkGuard`] for its
/// duration and is skipped (logged by the bus as a subscriber error) while
/// a restore runs. A skipped rerun is recorded by the gate and
/// replayed, coalesced into one library-wide rerun, when the restore ends
/// ([`Self::replay`]).
pub struct GatedEffects {
    inner: ChecksSideEffects,
    gate: Arc<WorkGate>,
    log: Arc<dyn HostLog>,
}

/// Wrap `inner` in the gate and set the gate's after-restore hook to
/// replay the reruns refused during a restore, each replay on its own
/// thread (`rerun-after-restore`). The hook holds the effects weakly.
pub fn install(
    inner: ChecksSideEffects,
    gate: Arc<WorkGate>,
    log: Arc<dyn HostLog>,
) -> Arc<GatedEffects> {
    let gated = Arc::new(GatedEffects {
        inner,
        gate: gate.clone(),
        log,
    });
    // Reruns refused during a restore run once it ends.
    let weak = Arc::downgrade(&gated);
    gate.set_after_restore(Arc::new(move |d: DeferredRerun| {
        let Some(fx) = weak.upgrade() else {
            return;
        };
        let _ = std::thread::Builder::new()
            .name("rerun-after-restore".into())
            .spawn(move || fx.replay(d));
    }));
    gated
}

impl GatedEffects {
    /// The reruns refused during a restore, after it: one library-wide
    /// post-scan sweep (it covers every album-scoped post-scan rerun that
    /// was skipped), then filename-mismatch over the albums whose rename
    /// rerun was skipped. Each takes the gate again, so a restore that has
    /// started meanwhile defers them once more.
    pub fn replay(&self, d: DeferredRerun) {
        use musaic_events::SideEffects;
        let Some(lib) = self.inner.library_dir().filter(|l| !l.is_empty()) else {
            return;
        };
        let swept = musaic_events::run_post_scan_guarded(|| self.run_post_scan_checks(&lib, None));
        if let Some(Err(e)) = swept {
            self.log.error(
                CHECKS_TARGET,
                &format!("check rerun after restore failed: {e}"),
            );
        }
        if !d.filename_mismatch.is_empty() {
            let ids: Vec<String> = d.filename_mismatch.into_iter().collect();
            let r = musaic_events::context::with_post_scan_rerun(|| {
                self.run_check_scoped("filename-mismatch", &lib, &ids)
            });
            if let Err(e) = r {
                self.log.error(
                    CHECKS_TARGET,
                    &format!("filename-mismatch rerun after restore failed: {e}"),
                );
            }
        }
    }
}

impl musaic_events::SideEffects for GatedEffects {
    fn library_dir(&self) -> Option<String> {
        self.inner.library_dir()
    }
    fn resolver_invalidate(&self, album_id: &str) -> Result<(), musaic_events::SubscriberError> {
        self.inner.resolver_invalidate(album_id)
    }
    fn delete_thumb(&self, album_id: &str) -> Result<(), musaic_events::SubscriberError> {
        self.inner.delete_thumb(album_id)
    }
    fn run_post_scan_checks(
        &self,
        library_dir: &str,
        album_ids: Option<&[String]>,
    ) -> Result<(), musaic_events::SubscriberError> {
        let _g = self.gate.begin_rerun(&[])?;
        let t0 = std::time::Instant::now();
        let r = self.inner.run_post_scan_checks(library_dir, album_ids);
        let scope = album_ids.map_or_else(
            || "library".to_string(),
            |ids| format!("{} album(s)", ids.len()),
        );
        match &r {
            Ok(()) => self.log.info(
                CHECKS_TARGET,
                &format!(
                    "post-scan check rerun ({scope}) completed in {}ms",
                    t0.elapsed().as_millis()
                ),
            ),
            Err(e) => self.log.error(
                CHECKS_TARGET,
                &format!("post-scan check rerun ({scope}) failed: {e}"),
            ),
        }
        r
    }
    fn run_check_scoped(
        &self,
        check_id: &str,
        library_path: &str,
        album_ids: &[String],
    ) -> Result<(), musaic_events::SubscriberError> {
        let skipped: &[String] = if check_id == "filename-mismatch" {
            album_ids
        } else {
            &[]
        };
        let _g = self.gate.begin_rerun(skipped)?;
        let t0 = std::time::Instant::now();
        let r = self
            .inner
            .run_check_scoped(check_id, library_path, album_ids);
        match &r {
            Ok(()) => self.log.info(
                CHECKS_TARGET,
                &format!(
                    "check {check_id} rerun ({} album(s)) completed in {}ms",
                    album_ids.len(),
                    t0.elapsed().as_millis()
                ),
            ),
            Err(e) => self.log.error(
                CHECKS_TARGET,
                &format!("check {check_id} rerun failed: {e}"),
            ),
        }
        r
    }
    fn notify_findings_changed(&self) -> Result<(), musaic_events::SubscriberError> {
        self.inner.notify_findings_changed()
    }
}
