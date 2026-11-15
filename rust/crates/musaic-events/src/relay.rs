//! SSE relay core (`musaic/services/library_events_relay.py`).
//!
//! One bus subscriber (topic `Event`, priority 400, name `_sse_event_relay`)
//! serialises each event once and fans it out to per-connection bounded
//! queues (default capacity [`QUEUE_MAXSIZE`] = 256). A full queue drops its
//! oldest entry and takes the new one.
//!
//! Python needs `loop.call_soon_threadsafe` because `asyncio.Queue` is not
//! thread-safe, and a `try/except RuntimeError` so a closed loop does not
//! starve later connections. Here each queue is a `Mutex<VecDeque>` that any
//! thread may push to directly, and the relay holds only `Weak` references:
//! a connection whose consumer is gone fails to upgrade and is skipped (and
//! dropping a [`Connection`] unregisters it), so a dead consumer can never
//! block or starve healthy ones.
//!
//! What an axum handler needs: `let mut conn = relay.connect();` then loop
//! on `conn.recv().await` (or adapt [`Connection::poll_recv`] into a
//! `Stream` with `futures::stream::poll_fn`), writing
//! [`sse_frame`](crate::sse_frame)`(&msg.name, &msg.data)`; drop `conn` on
//! disconnect. Ready handshake and 30 s pings stay in the handler.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use crate::bus::{Bus, Subscriber};
use crate::event::{Event, Topic};

/// `QUEUE_MAXSIZE` in the Python app.
pub const QUEUE_MAXSIZE: usize = 256;
/// The relay's bus priority (runs after every other subscriber).
pub const RELAY_PRIORITY: i64 = 400;
/// The relay's registration name.
pub const RELAY_NAME: &str = "_sse_event_relay";

/// One relayed event: SSE event name and JSON data line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelayMessage {
    pub name: Arc<str>,
    pub data: Arc<str>,
}

impl RelayMessage {
    pub fn new(name: &str, data: &str) -> Self {
        RelayMessage {
            name: name.into(),
            data: data.into(),
        }
    }

    /// `_event_to_payload` + `json.dumps`.
    pub fn from_event(ev: &Event) -> Self {
        RelayMessage {
            name: ev.name().into(),
            data: ev.payload_json().into(),
        }
    }

    /// The full SSE frame.
    pub fn frame(&self) -> String {
        crate::event::sse_frame(&self.name, &self.data)
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

struct QueueState {
    items: VecDeque<RelayMessage>,
    waker: Option<Waker>,
    dropped: u64,
}

struct Queue {
    capacity: usize,
    state: Mutex<QueueState>,
    ready: Condvar,
}

impl Queue {
    /// `_safe_put`: enqueue, dropping the oldest entry when full.
    fn put(&self, msg: RelayMessage) {
        let waker = {
            let mut st = lock(&self.state);
            // capacity 0 = unbounded, like asyncio.Queue(maxsize=0).
            if self.capacity > 0 && st.items.len() >= self.capacity {
                st.items.pop_front();
                st.dropped += 1;
            }
            st.items.push_back(msg);
            st.waker.take()
        };
        self.ready.notify_all();
        if let Some(w) = waker {
            w.wake();
        }
    }
}

struct RelayInner {
    conns: Mutex<Vec<Weak<Queue>>>,
}

impl RelayInner {
    fn enqueue(&self, ev: &Event) {
        let msg = RelayMessage::from_event(ev);
        let snapshot: Vec<Weak<Queue>> = lock(&self.conns).clone();
        for q in snapshot {
            // A consumer that is gone simply fails to upgrade.
            if let Some(q) = q.upgrade() {
                q.put(msg.clone());
            }
        }
    }
}

/// The relay: owns the list of connections and the bus callback.
#[derive(Clone)]
pub struct Relay {
    inner: Arc<RelayInner>,
    subscriber: Subscriber,
}

impl Default for Relay {
    fn default() -> Self {
        Relay::new()
    }
}

impl Relay {
    pub fn new() -> Self {
        let inner = Arc::new(RelayInner {
            conns: Mutex::new(Vec::new()),
        });
        let weak = Arc::downgrade(&inner);
        let subscriber = Subscriber::from_fn(move |ev| {
            if let Some(r) = weak.upgrade() {
                r.enqueue(ev);
            }
            Ok(())
        })
        .priority(RELAY_PRIORITY)
        .name(RELAY_NAME);
        Relay { inner, subscriber }
    }

    /// `register_bus_subscriber()`: subscribe at `Event`, priority 400.
    /// Idempotent per relay (same callback identity).
    pub fn register_on(&self, bus: &Bus) {
        bus.register(Topic::Event, self.subscriber.clone());
    }

    /// `_enqueue`: relay one event to every connection (also what the bus
    /// subscriber calls).
    pub fn enqueue(&self, ev: &Event) {
        self.inner.enqueue(ev);
    }

    /// `register_connection` with the Python app's per-connection capacity.
    pub fn connect(&self) -> Connection {
        self.connect_with_capacity(QUEUE_MAXSIZE)
    }

    pub fn connect_with_capacity(&self, capacity: usize) -> Connection {
        let q = Arc::new(Queue {
            capacity,
            state: Mutex::new(QueueState {
                items: VecDeque::new(),
                waker: None,
                dropped: 0,
            }),
            ready: Condvar::new(),
        });
        lock(&self.inner.conns).push(Arc::downgrade(&q));
        Connection {
            queue: q,
            relay: Arc::downgrade(&self.inner),
        }
    }

    /// `subscriber_count()`: registered connections (including any whose
    /// consumer vanished without unregistering).
    pub fn subscriber_count(&self) -> usize {
        lock(&self.inner.conns).len()
    }

    /// Test hook: register an entry whose consumer is already gone, the
    /// analogue of the Python app's closed-loop entry.
    #[doc(hidden)]
    pub fn push_dead_entry_for_tests(&self) {
        lock(&self.inner.conns).push(Weak::new());
    }
}

/// One SSE client's queue. Dropping it unregisters it
/// (`unregister_connection`). Single consumer.
pub struct Connection {
    queue: Arc<Queue>,
    relay: Weak<RelayInner>,
}

impl Connection {
    /// Push with drop-oldest (`_safe_put`), as the relay does.
    pub fn push(&self, msg: RelayMessage) {
        self.queue.put(msg);
    }

    pub fn try_recv(&self) -> Option<RelayMessage> {
        lock(&self.queue.state).items.pop_front()
    }

    pub fn poll_recv(&mut self, cx: &mut Context<'_>) -> Poll<RelayMessage> {
        let mut st = lock(&self.queue.state);
        match st.items.pop_front() {
            Some(m) => Poll::Ready(m),
            None => {
                st.waker = Some(cx.waker().clone());
                Poll::Pending
            }
        }
    }

    /// Wait for the next message (executor-agnostic).
    pub async fn recv(&mut self) -> RelayMessage {
        std::future::poll_fn(|cx| self.poll_recv(cx)).await
    }

    /// Blocking wait with a timeout, for sync consumers and tests.
    pub fn recv_timeout(&self, timeout: Duration) -> Option<RelayMessage> {
        let deadline = Instant::now() + timeout;
        let mut st = lock(&self.queue.state);
        loop {
            if let Some(m) = st.items.pop_front() {
                return Some(m);
            }
            let now = Instant::now();
            if now >= deadline {
                return None;
            }
            st = self
                .queue
                .ready
                .wait_timeout(st, deadline - now)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }

    pub fn len(&self) -> usize {
        lock(&self.queue.state).items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// How many entries overflow has evicted (Python logs a warning each time).
    pub fn dropped(&self) -> u64 {
        lock(&self.queue.state).dropped
    }

    pub fn capacity(&self) -> usize {
        self.queue.capacity
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        if let Some(r) = self.relay.upgrade() {
            let me = Arc::as_ptr(&self.queue);
            let mut conns = lock(&r.conns);
            if let Some(i) = conns.iter().position(|w| std::ptr::eq(w.as_ptr(), me)) {
                conns.remove(i);
            }
        }
    }
}
