//! The in-process bus: `register` / `unregister` / `publish` from
//! `musaic/library_events.py`.
//!
//! Contract, pinned by `testdata/golden/events/dispatch.json`:
//!
//! * `publish` collects registrations for every class in the event's MRO
//!   (concrete class, then `AlbumScopedEvent` if applicable, then `Event`)
//!   and stable-sorts them by priority, lower first. Equal priorities keep
//!   MRO-then-registration order: a subclass subscriber beats a base-class
//!   one registered earlier at the same priority.
//! * Registration is deduplicated per topic by callback identity; the same
//!   callback on two topics that both match an event runs twice.
//! * A failing subscriber (`Err` or panic) is reported to the failure hook
//!   and dispatch continues.
//! * `background` subscribers run on a fresh thread named
//!   `library_events:<name>`, with the publisher's [`GuardContext`] as of
//!   publish installed (the `copy_context().run` equivalent).

use std::any::Any;
use std::borrow::Cow;
use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::context::{self, GuardContext};
use crate::event::{Event, EventKind, Topic};

/// Error a subscriber returns; the bus reports it and moves on.
pub type SubscriberError = Box<dyn std::error::Error + Send + Sync>;

/// A subscriber callback.
pub type Callback = Arc<dyn Fn(&Event) -> Result<(), SubscriberError> + Send + Sync>;

/// Runs a background job. Receives the thread name the bus would use.
pub type Spawner =
    Arc<dyn Fn(String, Box<dyn FnOnce() + Send>) -> std::io::Result<()> + Send + Sync>;

/// What went wrong in one subscriber call.
#[derive(Debug)]
pub enum FailureKind {
    Error(String),
    Panic(String),
    SpawnFailed(String),
}

/// Reported for every failed subscriber call: (subscriber name, event name, what).
pub type FailureHook = Arc<dyn Fn(&str, &str, &FailureKind) + Send + Sync>;

/// Registration identity. Python dedupes by `callback is callback`; in Rust
/// closures have no identity, so it is the `Arc` allocation by default, or
/// an explicit key for callbacks that are rebuilt on each registration (the
/// catalog keys by Python function name, so a second `register_all()` stays
/// a no-op as it is in Python).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum CallbackId {
    Ptr(usize),
    Key(Cow<'static, str>),
}

/// A callback plus its registration options (Python's `register` kwargs).
#[derive(Clone)]
pub struct Subscriber {
    callback: Callback,
    key: Option<Cow<'static, str>>,
    priority: i64,
    background: bool,
    name: Option<String>,
}

impl Subscriber {
    /// Default priority 200, foreground, like `register()`'s defaults.
    pub fn new(callback: Callback) -> Self {
        Subscriber {
            callback,
            key: None,
            priority: 200,
            background: false,
            name: None,
        }
    }

    pub fn from_fn(
        f: impl Fn(&Event) -> Result<(), SubscriberError> + Send + Sync + 'static,
    ) -> Self {
        Subscriber::new(Arc::new(f))
    }

    pub fn priority(mut self, p: i64) -> Self {
        self.priority = p;
        self
    }

    pub fn background(mut self, b: bool) -> Self {
        self.background = b;
        self
    }

    pub fn name(mut self, n: impl Into<String>) -> Self {
        self.name = Some(n.into());
        self
    }

    pub fn key(mut self, k: impl Into<Cow<'static, str>>) -> Self {
        self.key = Some(k.into());
        self
    }

    pub fn id(&self) -> CallbackId {
        match &self.key {
            Some(k) => CallbackId::Key(k.clone()),
            None => CallbackId::Ptr(Arc::as_ptr(&self.callback) as *const () as usize),
        }
    }
}

#[derive(Clone)]
struct Registration {
    id: CallbackId,
    callback: Callback,
    background: bool,
    priority: i64,
    name: String,
}

/// Name / priority / background of one registration, for introspection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegistrationInfo {
    pub name: String,
    pub priority: i64,
    pub background: bool,
}

struct Inner {
    subs: Mutex<HashMap<Topic, Vec<Registration>>>,
    in_flight: Mutex<usize>,
    idle: Condvar,
    spawner: Spawner,
    on_failure: Mutex<Option<FailureHook>>,
}

/// The bus. Cheap to clone (shared state). Python has one process-wide
/// bus; the host keeps one `Bus` in its app state.
#[derive(Clone)]
pub struct Bus {
    inner: Arc<Inner>,
}

impl Default for Bus {
    fn default() -> Self {
        Bus::new()
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn panic_message(p: &(dyn Any + Send)) -> String {
    if let Some(s) = p.downcast_ref::<&str>() {
        (*s).to_owned()
    } else if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else {
        "panic".to_owned()
    }
}

fn std_thread_spawner() -> Spawner {
    Arc::new(|name, job| {
        std::thread::Builder::new()
            .name(name)
            .spawn(job)
            .map(|_| ())
    })
}

/// Decrements the in-flight count when a background job ends, however it ends.
struct InFlight(Arc<Inner>);

impl Drop for InFlight {
    fn drop(&mut self) {
        let mut n = lock(&self.0.in_flight);
        *n -= 1;
        if *n == 0 {
            self.0.idle.notify_all();
        }
    }
}

impl Bus {
    /// A bus whose background subscribers each get a `std::thread`, as in
    /// Python (`threading.Thread(daemon=True)`).
    pub fn new() -> Self {
        Bus::with_spawner(std_thread_spawner())
    }

    /// A bus with a custom background runner (e.g. `tokio::task::spawn_blocking`).
    /// The job already re-installs the publisher's guard flags.
    pub fn with_spawner(spawner: Spawner) -> Self {
        Bus {
            inner: Arc::new(Inner {
                subs: Mutex::new(HashMap::new()),
                in_flight: Mutex::new(0),
                idle: Condvar::new(),
                spawner,
                on_failure: Mutex::new(None),
            }),
        }
    }

    /// Where subscriber failures go (Python: `_log.exception`). Default: dropped.
    pub fn set_failure_hook(&self, hook: Option<FailureHook>) {
        *lock(&self.inner.on_failure) = hook;
    }

    /// Subscribe to `topic` (and so to every subclass). A second
    /// registration of the same callback id on the same topic is a no-op.
    pub fn register(&self, topic: Topic, sub: Subscriber) {
        let id = sub.id();
        let reg = Registration {
            name: sub.name.clone().unwrap_or_else(|| "subscriber".to_owned()),
            id,
            callback: sub.callback,
            background: sub.background,
            priority: sub.priority,
        };
        let mut subs = lock(&self.inner.subs);
        let list = subs.entry(topic).or_default();
        if list.iter().any(|r| r.id == reg.id) {
            return;
        }
        list.push(reg);
        list.sort_by_key(|r| r.priority); // stable: registration order on ties
    }

    /// Remove the first registration of `id` from `topic` (no-op if absent).
    pub fn unregister(&self, topic: Topic, id: &CallbackId) {
        let mut subs = lock(&self.inner.subs);
        if let Some(list) = subs.get_mut(&topic)
            && let Some(i) = list.iter().position(|r| &r.id == id)
        {
            list.remove(i);
        }
    }

    /// Drop every registration (`reset_for_tests`).
    pub fn reset(&self) {
        lock(&self.inner.subs).clear();
    }

    /// The registrations on one topic, in their stored order.
    pub fn registrations(&self, topic: Topic) -> Vec<RegistrationInfo> {
        lock(&self.inner.subs)
            .get(&topic)
            .map(|l| l.iter().map(info).collect())
            .unwrap_or_default()
    }

    /// What `publish` would call for an event of `kind`, in order.
    pub fn dispatch_plan(&self, kind: EventKind) -> Vec<RegistrationInfo> {
        self.collect(kind).iter().map(info).collect()
    }

    fn collect(&self, kind: EventKind) -> Vec<Registration> {
        let mut to_call = Vec::new();
        {
            let subs = lock(&self.inner.subs);
            for topic in kind.mro() {
                if let Some(list) = subs.get(topic) {
                    to_call.extend(list.iter().cloned());
                }
            }
        }
        // Stable: equal priorities keep MRO + registration order.
        to_call.sort_by_key(|r| r.priority);
        to_call
    }

    /// Dispatch `event` to every matching subscriber. Foreground ones run
    /// inline in priority order; background ones are started and not
    /// awaited. Never fails.
    pub fn publish(&self, event: &Event) {
        for reg in self.collect(event.kind()) {
            if reg.background {
                self.spawn_background(reg, event);
            } else {
                let outcome = catch_unwind(AssertUnwindSafe(|| (reg.callback)(event)));
                self.report(&reg.name, event, outcome);
            }
        }
    }

    fn report(
        &self,
        name: &str,
        event: &Event,
        outcome: Result<Result<(), SubscriberError>, Box<dyn Any + Send>>,
    ) {
        report(&self.inner, name, event, outcome);
    }

    fn spawn_background(&self, reg: Registration, event: &Event) {
        let ctx: GuardContext = context::current();
        let ev = event.clone();
        *lock(&self.inner.in_flight) += 1;
        let guard = InFlight(self.inner.clone());
        let inner = self.inner.clone();
        let name = reg.name.clone();
        let job: Box<dyn FnOnce() + Send> = Box::new(move || {
            let _guard = guard;
            let outcome = catch_unwind(AssertUnwindSafe(|| {
                context::run_in(ctx, || (reg.callback)(&ev))
            }));
            report(&inner, &reg.name, &ev, outcome);
        });
        if let Err(e) = (self.inner.spawner)(format!("library_events:{name}"), job) {
            // The job (and its in-flight guard) was dropped with the error.
            let kind = FailureKind::SpawnFailed(e.to_string());
            log_failure(&name, event.name(), &kind);
            if let Some(h) = lock(&self.inner.on_failure).clone() {
                h(&name, event.name(), &kind);
            }
        }
    }

    /// Block until no background subscriber is running, or `timeout`
    /// passes. Returns whether the bus went idle. (Python tests poll
    /// `threading.enumerate()` for `library_events:*` threads.)
    pub fn wait_background_idle(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut n = lock(&self.inner.in_flight);
        while *n > 0 {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            n = self
                .inner
                .idle
                .wait_timeout(n, deadline - now)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        true
    }
}

/// The Python app's logger for `library_events.py`.
pub const LIBRARY_EVENTS_LOG: &str = "musaic.library_events";

/// library_events.py:339 `_log.exception("subscriber %s failed for event %s
/// (continuing)", ...)`: ERROR; the error, panic or spawn-failure text
/// stands in for the traceback. Written whether or not a failure hook is
/// set. (The catalog's own subscribers log and swallow their errors as
/// the Python app's do, so they reach this only by panicking.)
fn log_failure(name: &str, event: &str, kind: &FailureKind) {
    let what = match kind {
        FailureKind::Error(s) | FailureKind::Panic(s) | FailureKind::SpawnFailed(s) => s,
    };
    musaic_log::error(
        LIBRARY_EVENTS_LOG,
        &format!("subscriber {name} failed for event {event} (continuing): {what}"),
    );
}

fn info(r: &Registration) -> RegistrationInfo {
    RegistrationInfo {
        name: r.name.clone(),
        priority: r.priority,
        background: r.background,
    }
}

fn report(
    inner: &Inner,
    name: &str,
    event: &Event,
    outcome: Result<Result<(), SubscriberError>, Box<dyn Any + Send>>,
) {
    let kind = match outcome {
        Ok(Ok(())) => return,
        Ok(Err(e)) => FailureKind::Error(e.to_string()),
        Err(p) => FailureKind::Panic(panic_message(p.as_ref())),
    };
    log_failure(name, event.name(), &kind);
    if let Some(h) = lock(&inner.on_failure).clone() {
        h(name, event.name(), &kind);
    }
}
