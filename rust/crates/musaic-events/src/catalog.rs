//! The subscriber catalog (`musaic/services/event_subscribers.py`),
//! and the guard checks, with every side effect
//! injected through [`SideEffects`] (the real check runner, resolver and
//! thumb store do not exist in Rust yet).
//!
//! [`CATALOG`] is the data: one [`SubscriberSlot`] per `events.register(...)`
//! call in `register_all()`, in that order. [`register_all`] wires each
//! slot to the matching handler; the relay slot (priority 400) is
//! registered by [`Relay::register_on`](crate::Relay::register_on).

use std::sync::Arc;

use crate::bus::{Bus, Subscriber, SubscriberError};
use crate::context::{self, run_post_scan_guarded, with_post_scan_rerun};
use crate::event::{Event, EventKind, Topic};
use crate::relay::Relay;

/// The side-effect targets the subscribers call. Each mirrors one the Python app
/// call site; an `Err` is logged-and-swallowed by the subscriber, as
/// the Python app's per-subscriber `try/except` does.
pub trait SideEffects: Send + Sync + 'static {
    /// `config.load_config().library_dir`; `None` = no library configured,
    /// in which case the rerun subscribers do nothing.
    fn library_dir(&self) -> Option<String>;
    /// `services.resolver.invalidate(album_id)`.
    fn resolver_invalidate(&self, album_id: &str) -> Result<(), SubscriberError>;
    /// `thumbs.delete_thumb(album_id)`.
    fn delete_thumb(&self, album_id: &str) -> Result<(), SubscriberError>;
    /// The body of `services.post_scan.run_post_scan_checks(library_dir,
    /// album_ids=...)`: run every `runs_during_scan` check, scoped to
    /// `album_ids` (`None` = library-wide). The catalog calls it inside
    /// [`run_post_scan_guarded`](crate::run_post_scan_guarded), which sets
    /// `in_post_scan_rerun` for its duration; do NOT guard again
    /// here, or every bus-driven rerun short-circuits (the Python app's bug).
    fn run_post_scan_checks(
        &self,
        library_dir: &str,
        album_ids: Option<&[String]>,
    ) -> Result<(), SubscriberError>;
    /// `checks.runner.run_check(check_id, library_path=..., scope_kind="albums",
    /// album_ids=...)`; used only for `filename-mismatch` on rename.
    fn run_check_scoped(
        &self,
        check_id: &str,
        library_path: &str,
        album_ids: &[String],
    ) -> Result<(), SubscriberError>;
    /// `checks.runner.notify_changed()` (wakes `/api/checks/stream`).
    fn notify_findings_changed(&self) -> Result<(), SubscriberError>;
}

/// The Python app subscriber function a slot runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Handler {
    ResolverInvalidate,
    ResolverInvalidateForRename,
    ThumbDrop,
    ThumbDropForRename,
    PostScanRerun,
    PostScanRerunForRename,
    PostScanRerunLibraryWide,
    FilenameMismatchRerun,
    FindingsRunnerNotify,
    SseEventRelay,
}

impl Handler {
    /// The Python function's `__qualname__`: registration identity, so the
    /// same handler on two topics is two registrations (as in Python) and
    /// a second `register_all` is a no-op.
    pub fn function_name(self) -> &'static str {
        match self {
            Handler::ResolverInvalidate => "_resolver_invalidate",
            Handler::ResolverInvalidateForRename => "_resolver_invalidate_for_rename",
            Handler::ThumbDrop => "_thumb_drop",
            Handler::ThumbDropForRename => "_thumb_drop_for_rename",
            Handler::PostScanRerun => "_post_scan_rerun",
            Handler::PostScanRerunForRename => "_post_scan_rerun_for_rename",
            Handler::PostScanRerunLibraryWide => "_post_scan_rerun_library_wide",
            Handler::FilenameMismatchRerun => "_filename_mismatch_rerun",
            Handler::FindingsRunnerNotify => "_findings_runner_notify",
            Handler::SseEventRelay => "_enqueue",
        }
    }
}

/// One `events.register(...)` call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SubscriberSlot {
    pub name: &'static str,
    pub topic: Topic,
    pub priority: i64,
    pub background: bool,
    pub handler: Handler,
}

const fn slot(
    name: &'static str,
    topic: Topic,
    priority: i64,
    background: bool,
    handler: Handler,
) -> SubscriberSlot {
    SubscriberSlot {
        name,
        topic,
        priority,
        background,
        handler,
    }
}

/// `register_all()` as data, in registration order.
pub const CATALOG: &[SubscriberSlot] = &[
    // Invalidation tier (100), inline.
    slot(
        "_resolver_invalidate",
        Topic::AlbumScoped,
        100,
        false,
        Handler::ResolverInvalidate,
    ),
    slot(
        "_resolver_invalidate_for_rename",
        Topic::Kind(EventKind::AlbumRenamed),
        100,
        false,
        Handler::ResolverInvalidateForRename,
    ),
    slot(
        "_thumb_drop",
        Topic::Kind(EventKind::AlbumCoverChanged),
        100,
        false,
        Handler::ThumbDrop,
    ),
    slot(
        "_thumb_drop:on-delete",
        Topic::Kind(EventKind::AlbumDeleted),
        100,
        false,
        Handler::ThumbDrop,
    ),
    slot(
        "_thumb_drop_for_rename",
        Topic::Kind(EventKind::AlbumRenamed),
        100,
        false,
        Handler::ThumbDropForRename,
    ),
    // Mutation tier (200), background.
    slot(
        "_post_scan_rerun",
        Topic::AlbumScoped,
        200,
        true,
        Handler::PostScanRerun,
    ),
    slot(
        "_post_scan_rerun_for_rename",
        Topic::Kind(EventKind::AlbumRenamed),
        200,
        true,
        Handler::PostScanRerunForRename,
    ),
    slot(
        "_post_scan_rerun_library_wide",
        Topic::Kind(EventKind::LibraryScanCompleted),
        200,
        true,
        Handler::PostScanRerunLibraryWide,
    ),
    slot(
        "_post_scan_rerun_library_wide:bulk",
        Topic::Kind(EventKind::BulkBatchCompleted),
        200,
        true,
        Handler::PostScanRerunLibraryWide,
    ),
    slot(
        "_filename_mismatch_rerun",
        Topic::Kind(EventKind::AlbumRenamed),
        200,
        true,
        Handler::FilenameMismatchRerun,
    ),
    // Notification tier (300).
    slot(
        "_findings_runner_notify",
        Topic::Event,
        300,
        false,
        Handler::FindingsRunnerNotify,
    ),
    // Relay tier (400).
    slot(
        crate::relay::RELAY_NAME,
        Topic::Event,
        crate::relay::RELAY_PRIORITY,
        false,
        Handler::SseEventRelay,
    ),
];

/// The Python app's logger for `services/event_subscribers.py`.
pub const SUBSCRIBERS_LOG: &str = "musaic.services.event_subscribers";

/// The `_log.exception(...)` message of the Python app subscriber `handler`
/// when it fails on `ev` (musaic/services/event_subscribers.py:64-261).
/// Logged at ERROR with `: <error text>` standing in for the traceback.
pub fn failure_message(handler: Handler, ev: &Event) -> String {
    let id = ev.album_id().unwrap_or("");
    let (old, new) = match ev {
        Event::AlbumRenamed {
            old_album_id,
            new_album_id,
        } => (old_album_id.as_str(), new_album_id.as_str()),
        _ => ("", ""),
    };
    match handler {
        Handler::ResolverInvalidate => format!("_resolver_invalidate failed for {id}"),
        Handler::ResolverInvalidateForRename => {
            format!("_resolver_invalidate_for_rename failed ({old} \u{2192} {new})")
        }
        Handler::ThumbDrop => format!("_thumb_drop failed for {id}"),
        Handler::ThumbDropForRename => format!("_thumb_drop_for_rename failed for {old}"),
        Handler::PostScanRerun => format!("_post_scan_rerun failed for {id} (continuing)"),
        Handler::PostScanRerunForRename => {
            format!("_post_scan_rerun_for_rename failed ({old} \u{2192} {new})")
        }
        Handler::PostScanRerunLibraryWide => {
            "_post_scan_rerun_library_wide failed (continuing)".to_string()
        }
        Handler::FilenameMismatchRerun => {
            format!("_filename_mismatch_rerun failed for {new} (continuing)")
        }
        Handler::FindingsRunnerNotify => "_findings_runner_notify failed (continuing)".to_string(),
        Handler::SseEventRelay => "_sse_event_relay failed".to_string(),
    }
}

/// The guards every rerun subscriber checks first: skip inside a post-scan
/// rerun, skip inside a bulk loop, skip with no library configured.
fn rerun_library_dir(fx: &dyn SideEffects) -> Option<String> {
    if context::in_post_scan_rerun() || context::in_bulk_run() {
        return None;
    }
    fx.library_dir()
}

/// The rerun subscribers do not set `in_post_scan_rerun`
/// themselves (the Python app did, so its guarded runner always
/// short-circuited). The runner guard sets it for the sweep, so publishes
/// from inside the sweep, and background subscribers they spawn (which
/// inherit the flag), still skip their reruns.
fn guarded_rerun(
    fx: &dyn SideEffects,
    lib: &str,
    album_ids: Option<&[String]>,
) -> Result<(), SubscriberError> {
    run_post_scan_guarded(|| fx.run_post_scan_checks(lib, album_ids)).unwrap_or(Ok(()))
}

fn run_handler(handler: Handler, fx: &dyn SideEffects, ev: &Event) -> Result<(), SubscriberError> {
    match handler {
        Handler::ResolverInvalidate => match ev.album_id() {
            Some(id) => fx.resolver_invalidate(id),
            None => Ok(()),
        },
        Handler::ResolverInvalidateForRename => {
            if let Event::AlbumRenamed {
                old_album_id,
                new_album_id,
            } = ev
            {
                // One try block in Python: a failure on old skips new.
                fx.resolver_invalidate(old_album_id)?;
                fx.resolver_invalidate(new_album_id)?;
            }
            Ok(())
        }
        Handler::ThumbDrop => match ev.album_id() {
            Some(id) => fx.delete_thumb(id),
            None => Ok(()),
        },
        Handler::ThumbDropForRename => match ev {
            Event::AlbumRenamed { old_album_id, .. } => fx.delete_thumb(old_album_id),
            _ => Ok(()),
        },
        Handler::PostScanRerun => {
            let (Some(id), Some(lib)) = (ev.album_id(), rerun_library_dir(fx)) else {
                return Ok(());
            };
            let ids = [id.to_owned()];
            guarded_rerun(fx, &lib, Some(&ids))
        }
        Handler::PostScanRerunForRename => {
            let (Event::AlbumRenamed { new_album_id, .. }, Some(lib)) = (ev, rerun_library_dir(fx))
            else {
                return Ok(());
            };
            let ids = [new_album_id.clone()];
            guarded_rerun(fx, &lib, Some(&ids))
        }
        Handler::PostScanRerunLibraryWide => {
            let Some(lib) = rerun_library_dir(fx) else {
                return Ok(());
            };
            guarded_rerun(fx, &lib, None)
        }
        Handler::FilenameMismatchRerun => {
            let (Event::AlbumRenamed { new_album_id, .. }, Some(lib)) = (ev, rerun_library_dir(fx))
            else {
                return Ok(());
            };
            let ids = [new_album_id.clone()];
            with_post_scan_rerun(|| fx.run_check_scoped("filename-mismatch", &lib, &ids))
        }
        Handler::FindingsRunnerNotify => fx.notify_findings_changed(),
        Handler::SseEventRelay => Ok(()), // wired by Relay::register_on
    }
}

/// Wire every [`CATALOG`] slot onto `bus`, and the relay if given.
///
/// Deliberate change from the Python app: the Python app's rerun
/// subscribers set `in_post_scan_rerun` before calling its guarded
/// `run_post_scan_checks`, so bus-driven reruns never ran a check
/// (`testdata/golden/events/catalog.json`, scenario
/// `composed_with_real_post_scan_guard`). Here the post-scan rerun
/// handlers call [`SideEffects::run_post_scan_checks`] (the unguarded
/// body) inside [`run_post_scan_guarded`](crate::run_post_scan_guarded);
/// `_filename_mismatch_rerun` still sets the flag itself around its direct
/// `run_check`, as in the Python app. Expected traces:
/// `testdata/golden/events/catalog-rerun-guard.json` (`fixed`).
pub fn register_all(bus: &Bus, effects: Arc<dyn SideEffects>, relay: Option<&Relay>) {
    for s in CATALOG {
        if s.handler == Handler::SseEventRelay {
            if let Some(r) = relay {
                r.register_on(bus);
            }
            continue;
        }
        let fx = effects.clone();
        let handler = s.handler;
        let sub = Subscriber::from_fn(move |ev| {
            // Each the Python app subscriber catches its own exception, logs it and
            // returns (the bus never sees it).
            if let Err(e) = run_handler(handler, fx.as_ref(), ev) {
                musaic_log::error(
                    SUBSCRIBERS_LOG,
                    &format!("{}: {e}", failure_message(handler, ev)),
                );
            }
            Ok(())
        })
        .key(handler.function_name())
        .priority(s.priority)
        .background(s.background)
        .name(s.name);
        bus.register(s.topic, sub);
    }
}

/// The bulk-dispatcher shape (`api/health.py::quick_fix_all` `event_gen`):
/// run `f` with `in_bulk_run` set, reset it, then, if `publish_after`,
/// publish one `BulkBatchCompleted` — also when `f` panics (Python's
/// `finally`). Async callers use [`run_bulk_async`].
pub fn run_bulk<R>(bus: &Bus, publish_after: bool, f: impl FnOnce() -> R) -> R {
    let _publish = PublishOnDrop {
        bus,
        publish: publish_after,
    };
    context::with_bulk_run(f)
}

/// Async [`run_bulk`]: the flag is set while `fut` is polled; the
/// `BulkBatchCompleted` is published when the batch finishes or the future
/// is dropped mid-way (client disconnect, like `CancelledError` → `finally`).
pub async fn run_bulk_async<F: std::future::Future>(
    bus: &Bus,
    publish_after: bool,
    fut: F,
) -> F::Output {
    let _publish = PublishOnDrop {
        bus,
        publish: publish_after,
    };
    context::scope_bulk_run(fut).await
}

struct PublishOnDrop<'a> {
    bus: &'a Bus,
    publish: bool,
}

impl Drop for PublishOnDrop<'_> {
    fn drop(&mut self) {
        if self.publish {
            self.bus.publish(&Event::BulkBatchCompleted);
        }
    }
}
