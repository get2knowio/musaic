//! In-process library event bus, guard flags and SSE relay core: the Rust
//! port of `musaic/library_events.py`, `musaic/services/library_events_relay.py`,
//! the `in_bulk_run` flag of `musaic/checks/_apply.py` and the subscriber
//! catalog of `musaic/services/event_subscribers.py`.
//!
//! * [`event`] — the event enum, MRO topics, and the relay's JSON payload
//!   byte-for-byte as Python's `json.dumps` writes it.
//! * [`bus`] — register / unregister / publish with priorities, MRO
//!   fan-out, isolation and background subscribers.
//! * [`context`] — the `in_post_scan_rerun` / `in_bulk_run` flags and why
//!   they are a scoped thread-local plus a poll-scoped future wrapper
//!   rather than a plain thread-local.
//! * [`catalog`] — `register_all()` as data with injected side effects,
//!   and the bulk-dispatcher helper.
//! * [`relay`] — per-connection bounded drop-oldest queues.
//!
//! No HTTP here; the `/api/library-events/stream` handler comes later.
//!
//! Deliberate change from the Python app: bus-driven post-scan
//! reruns really run the checks; the runner guard, not the subscriber,
//! sets `in_post_scan_rerun` (see [`catalog::register_all`]).
//!
//! The Python app behaviour kept as-is (decision pending): Python accepts `None`
//! in any event field and relays `null`, which these types cannot express
//! (no the Python app producer does it).

pub mod bus;
pub mod catalog;
pub mod context;
pub mod event;
pub mod relay;

pub use bus::{
    Bus, Callback, CallbackId, FailureHook, FailureKind, RegistrationInfo, Spawner, Subscriber,
    SubscriberError,
};
pub use catalog::{
    CATALOG, Handler, SideEffects, SubscriberSlot, register_all, run_bulk, run_bulk_async,
};
pub use context::{
    GuardContext, in_bulk_run, in_post_scan_rerun, run_post_scan_guarded,
    run_post_scan_guarded_async,
};
pub use event::{
    AgentWhat, ArgValue, Event, EventKind, FieldValue, FromFieldsError, Topic, ready_frame,
    sse_frame,
};
pub use relay::{Connection, QUEUE_MAXSIZE, Relay, RelayMessage};
