//! What a face sees of a running operation: the sink's answer, the typed events
//! of a run and the feed.

use std::time::Duration;

use tokio::sync::watch;

pub use musaic_services::analyze::{AnalyzeEvent, AnalyzeSummary, FileResult};

/// A sink's answer: keep calling, or stop. `Gone` ends the calls, never the run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Listening {
    Yes,
    Gone,
}

type Reader<T> = Box<dyn Fn() -> T + Send + Sync>;
/// Reads the value of a two-counter feed, told which counter changed.
type PairReader<T> = Box<dyn Fn(bool, bool) -> T + Send + Sync>;

/// Two change counters and a read that is told which of them changed. A change
/// that `changed()` already saw is kept in `seen` until a value is taken.
struct Pair<T> {
    a: watch::Receiver<u64>,
    b: watch::Receiver<u64>,
    read: PairReader<T>,
    seen: (bool, bool),
}

/// Where a feed's values come from: a watch channel that carries them, or a
/// change counter plus a read of the current value (a state that lives
/// elsewhere and only announces that it changed).
enum Source<T> {
    Value(watch::Receiver<T>),
    Counter(watch::Receiver<u64>, Reader<T>),
    Pair(Pair<T>),
}

impl<T: Clone> Source<T> {
    fn has_changed(&self) -> Result<bool, ()> {
        match self {
            Source::Value(rx) => rx.has_changed().map_err(|_| ()),
            Source::Counter(rx, _) => rx.has_changed().map_err(|_| ()),
            Source::Pair(p) => {
                let a = p.a.has_changed().map_err(|_| ())?;
                let b = p.b.has_changed().map_err(|_| ())?;
                Ok(p.seen.0 || p.seen.1 || a || b)
            }
        }
    }

    /// The first value. A counter's change since it was subscribed stays
    /// pending, so it is delivered as one more value, never lost.
    fn first(&mut self) -> T {
        match self {
            Source::Value(rx) => rx.borrow_and_update().clone(),
            Source::Counter(_, read) => read(),
            Source::Pair(p) => (p.read)(true, true),
        }
    }

    fn take(&mut self) -> T {
        match self {
            Source::Value(rx) => rx.borrow_and_update().clone(),
            Source::Counter(rx, read) => {
                rx.borrow_and_update();
                read()
            }
            Source::Pair(p) => {
                let a = p.seen.0 || p.a.has_changed().unwrap_or(false);
                let b = p.seen.1 || p.b.has_changed().unwrap_or(false);
                p.a.borrow_and_update();
                p.b.borrow_and_update();
                p.seen = (false, false);
                (p.read)(a, b)
            }
        }
    }

    async fn changed(&mut self) -> bool {
        match self {
            Source::Value(rx) => rx.changed().await.is_ok(),
            Source::Counter(rx, _) => rx.changed().await.is_ok(),
            Source::Pair(p) => {
                tokio::select! {
                    r = p.a.changed() => { p.seen.0 = true; r.is_ok() }
                    r = p.b.changed() => { p.seen.1 = true; r.is_ok() }
                }
            }
        }
    }
}

/// A stream of values with the current one first. Several changes between two
/// reads may arrive as one value. Ends when the application begins shutting down.
pub struct Feed<T> {
    source: Source<T>,
    shutdown: watch::Receiver<bool>,
    /// The first value has not been taken yet.
    first: bool,
    /// A change [`wait`](Feed::wait) saw and no value has been taken for yet.
    arrived: bool,
}

impl<T: Clone> Feed<T> {
    /// A feed over `rx`, ending when `shutdown` turns true
    /// (`App::shutdown_signal()`).
    pub fn new(rx: watch::Receiver<T>, shutdown: watch::Receiver<bool>) -> Self {
        Self::over(Source::Value(rx), shutdown)
    }

    /// A feed whose values are `read()`, one per change of `changes`. Subscribe
    /// `changes` before the feed is built so a change in between is delivered.
    pub fn counted(
        changes: watch::Receiver<u64>,
        shutdown: watch::Receiver<bool>,
        read: impl Fn() -> T + Send + Sync + 'static,
    ) -> Self {
        Self::over(Source::Counter(changes, Box::new(read)), shutdown)
    }

    /// A feed over two change counters: `read(first, second)` is told which of
    /// them changed (both, for the first value) and builds the value. A change
    /// of both between two reads is one value that says so. Subscribe both
    /// counters before the feed is built.
    pub fn counted_pair(
        first: watch::Receiver<u64>,
        second: watch::Receiver<u64>,
        shutdown: watch::Receiver<bool>,
        read: impl Fn(bool, bool) -> T + Send + Sync + 'static,
    ) -> Self {
        Self::over(
            Source::Pair(Pair {
                a: first,
                b: second,
                read: Box::new(read),
                seen: (false, false),
            }),
            shutdown,
        )
    }

    fn over(source: Source<T>, shutdown: watch::Receiver<bool>) -> Self {
        Feed {
            source,
            shutdown,
            first: true,
            arrived: false,
        }
    }

    /// Async: resolves with `true` when a value is ready (the first, or a
    /// change), without reading it; the next [`next`](Self::next) with a zero
    /// timeout, or [`changed`](Self::changed), then returns it at once. `false`
    /// once the application is shutting down. For a face whose read must run off
    /// its async threads.
    pub async fn wait(&mut self) -> bool {
        if *self.shutdown.borrow() {
            return false;
        }
        if self.arrived || self.first || self.source.has_changed().unwrap_or(false) {
            return true;
        }
        tokio::select! {
            r = self.source.changed() => {
                self.arrived = r;
                r
            }
            _ = self.shutdown.wait_for(|down| *down) => false,
        }
    }

    fn take_first(&mut self) -> bool {
        std::mem::take(&mut self.first)
    }

    /// Blocking: the next value, or `None` after `timeout` with no change or
    /// once the application is shutting down. The thread sleeps until a change,
    /// the shutdown or the deadline wakes it; no runtime is needed.
    pub fn next(&mut self, timeout: Duration) -> Option<T> {
        let deadline = std::time::Instant::now() + timeout;
        block_until(self.changed(), deadline).flatten()
    }

    /// Async: the next value, or `None` once the application is shutting down.
    pub async fn changed(&mut self) -> Option<T> {
        if *self.shutdown.borrow() {
            return None;
        }
        if self.take_first() {
            return Some(self.source.first());
        }
        if std::mem::take(&mut self.arrived) {
            return Some(self.source.take());
        }
        tokio::select! {
            r = self.source.changed() => r.then(|| self.source.take()),
            _ = self.shutdown.wait_for(|down| *down) => None,
        }
    }
}

/// Polls `fut` on this thread until it is ready or `deadline` passes, parking
/// in between; its waker unparks the thread. Enough for the watch channels a
/// [`Feed`] waits on, which need no runtime. A future dropped at the deadline
/// loses nothing: `watch::Receiver::changed` is cancel-safe.
fn block_until<F: std::future::Future>(fut: F, deadline: std::time::Instant) -> Option<F::Output> {
    use std::sync::Arc;
    use std::task::{Context, Poll, Wake, Waker};

    struct Unpark(std::thread::Thread);
    impl Wake for Unpark {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
        fn wake_by_ref(self: &Arc<Self>) {
            self.0.unpark();
        }
    }

    let waker = Waker::from(Arc::new(Unpark(std::thread::current())));
    let mut cx = Context::from_waker(&waker);
    let mut fut = std::pin::pin!(fut);
    loop {
        if let Poll::Ready(v) = fut.as_mut().poll(&mut cx) {
            return Some(v);
        }
        let now = std::time::Instant::now();
        if now >= deadline {
            return None;
        }
        std::thread::park_timeout(deadline - now);
    }
}
