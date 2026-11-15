//! The two guard flags Python keeps in `contextvars`:
//! `library_events.in_post_scan_rerun` (recursion guard) and
//! `checks._apply.in_bulk_run` (bulk-loop suppression).
//!
//! # Why this shape
//!
//! A Python `ContextVar` is per *thread* for plain threads and per *task*
//! for asyncio tasks; `set()`/`reset(token)` scope a value to a dynamic
//! extent, and `copy_context().run(f)` carries a snapshot into another
//! thread. The bus relies on exactly three properties
//! (`tests/test_library_events.py`):
//!
//! 1. a value set in a frame is visible to everything that frame calls,
//!    and is restored when the frame exits, even by exception;
//! 2. a background subscriber sees the publisher's values *as of publish*
//!    (the snapshot), on another thread;
//! 3. a value set on one thread / task is invisible to unrelated
//!    concurrent work.
//!
//! Rust equivalent chosen here, which is the mechanism `tokio::task_local!`
//! uses, written against `std` only so the crate builds without tokio:
//!
//! * a `thread_local!` cell holds the *current* [`GuardContext`];
//! * [`run_in`] (sync) installs a value for the duration of a closure and
//!   restores the previous one from a drop guard, so it also restores on
//!   panic (property 1) — the analogue of `set()` + `finally: reset()`;
//! * [`scope`] (async) wraps a future so the value is installed only while
//!   that future is being polled and removed before `poll` returns. Other
//!   tasks multiplexed on the same worker thread therefore never observe it
//!   (property 3 for async), which a bare thread-local set across an
//!   `.await` would violate;
//! * the bus captures [`current`] at publish time and re-installs it with
//!   [`run_in`] on the background thread (property 2) — the
//!   `copy_context().run` equivalent;
//! * a new `std::thread` starts from the default context (property 3 for
//!   threads), like Python threads (3.13 has no `thread_inherit_context`).
//!
//! Differences from Python a host must know about:
//!
//! * `asyncio.create_task` / `asyncio.to_thread(ctx.run, ...)` copy the
//!   context; `tokio::spawn` / `spawn_blocking` do not. Wrap explicitly:
//!   `tokio::spawn(context::inherit(fut))` or
//!   `let c = context::current(); spawn_blocking(move || context::run_in(c, f))`.
//! * Setting a flag must go through [`run_in`]/[`scope`] (or the
//!   `with_*`/`scope_*` helpers); there is no free-standing `set`, so a
//!   value cannot leak past its scope.

use std::cell::Cell;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

/// Snapshot of both guard flags. `Default` is "neither set", the
/// `ContextVar` defaults.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct GuardContext {
    /// `library_events.in_post_scan_rerun`.
    pub in_post_scan_rerun: bool,
    /// `checks._apply.in_bulk_run`.
    pub in_bulk_run: bool,
}

thread_local! {
    static CURRENT: Cell<GuardContext> = const {
        Cell::new(GuardContext { in_post_scan_rerun: false, in_bulk_run: false })
    };
}

/// The flags visible to the calling code right now.
pub fn current() -> GuardContext {
    CURRENT.with(Cell::get)
}

pub fn in_post_scan_rerun() -> bool {
    current().in_post_scan_rerun
}

pub fn in_bulk_run() -> bool {
    current().in_bulk_run
}

struct Restore(GuardContext);

impl Drop for Restore {
    fn drop(&mut self) {
        CURRENT.with(|c| c.set(self.0));
    }
}

fn swap_in(ctx: GuardContext) -> Restore {
    Restore(CURRENT.with(|c| c.replace(ctx)))
}

/// Run `f` with `ctx` installed; the previous value comes back afterwards,
/// including when `f` panics.
pub fn run_in<R>(ctx: GuardContext, f: impl FnOnce() -> R) -> R {
    let _restore = swap_in(ctx);
    f()
}

/// `token = in_post_scan_rerun.set(True); try: f() finally: reset(token)`.
pub fn with_post_scan_rerun<R>(f: impl FnOnce() -> R) -> R {
    run_in(
        GuardContext {
            in_post_scan_rerun: true,
            ..current()
        },
        f,
    )
}

/// `token = in_bulk_run.set(True); try: f() finally: reset(token)`.
pub fn with_bulk_run<R>(f: impl FnOnce() -> R) -> R {
    run_in(
        GuardContext {
            in_bulk_run: true,
            ..current()
        },
        f,
    )
}

/// A future that runs with a fixed [`GuardContext`] installed while it is
/// polled. Executor-agnostic (works on tokio, or any other executor).
pub struct Scoped<F> {
    ctx: GuardContext,
    fut: Pin<Box<F>>,
}

impl<F: Future> Future for Scoped<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<F::Output> {
        let this = self.get_mut();
        let _restore = swap_in(this.ctx);
        this.fut.as_mut().poll(cx)
    }
}

/// Run `fut` with `ctx` installed (the async counterpart of [`run_in`]).
pub fn scope<F: Future>(ctx: GuardContext, fut: F) -> Scoped<F> {
    Scoped {
        ctx,
        fut: Box::pin(fut),
    }
}

/// Carry the caller's current flags into `fut`, for handing it to a
/// spawner that does not (`tokio::spawn`). Equivalent of asyncio's
/// context copy on task creation.
pub fn inherit<F: Future>(fut: F) -> Scoped<F> {
    scope(current(), fut)
}

/// `fut` with `in_post_scan_rerun` set for its whole run. The other flag is
/// taken from the context current when this is called.
pub fn scope_post_scan_rerun<F: Future>(fut: F) -> Scoped<F> {
    scope(
        GuardContext {
            in_post_scan_rerun: true,
            ..current()
        },
        fut,
    )
}

/// `fut` with `in_bulk_run` set for its whole run.
pub fn scope_bulk_run<F: Future>(fut: F) -> Scoped<F> {
    scope(
        GuardContext {
            in_bulk_run: true,
            ..current()
        },
        fut,
    )
}

/// The guard `services/post_scan.py::run_post_scan_checks` wraps around its
/// body: if a rerun is already in flight on this dispatch chain, return
/// without running (`None`); otherwise run `f` with `in_post_scan_rerun`
/// set, resetting it afterwards even if `f` panics.
pub fn run_post_scan_guarded<R>(f: impl FnOnce() -> R) -> Option<R> {
    if in_post_scan_rerun() {
        return None;
    }
    Some(with_post_scan_rerun(f))
}

/// Async counterpart (`run_post_scan_checks_async`). The flag is checked
/// when the returned future first runs, like a coroutine body.
pub async fn run_post_scan_guarded_async<F: Future>(fut: F) -> Option<F::Output> {
    if in_post_scan_rerun() {
        return None;
    }
    Some(scope_post_scan_rerun(fut).await)
}
