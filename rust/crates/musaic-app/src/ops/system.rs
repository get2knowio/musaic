//! The `system` group: health, the fingerprint pool's state and feed, and the
//! library change events feed.

use std::sync::Arc;
use std::time::Duration;

use musaic_events::{Connection, RelayMessage};
pub use musaic_services::fingerprint_pool::PoolState;

use super::events::Feed;
use crate::App;

/// The answer of [`System::health`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    Ok,
}

/// One library change event as the relay carries it: the event's name and its
/// payload, already serialised (the relay renders each event once for every
/// subscriber).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryEvent {
    name: Arc<str>,
    payload: Arc<str>,
}

impl LibraryEvent {
    /// The event's type name (`AlbumTagsChanged`, ...).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The payload as the relay renders it.
    pub fn payload_json(&self) -> &str {
        &self.payload
    }
}

impl From<RelayMessage> for LibraryEvent {
    fn from(m: RelayMessage) -> Self {
        LibraryEvent {
            name: m.name,
            payload: m.data,
        }
    }
}

/// A subscription to every event the bus publishes, with its own bounded queue
/// that drops its oldest entry when full, so a slow reader never holds another
/// back. Dropping it unsubscribes.
pub struct LibraryEvents {
    conn: Connection,
    shutdown: tokio::sync::watch::Receiver<bool>,
}

impl LibraryEvents {
    /// Blocking: the next event, or `None` after `timeout` or once the
    /// application is shutting down.
    pub fn next(&self, timeout: Duration) -> Option<LibraryEvent> {
        if *self.shutdown.borrow() {
            return None;
        }
        self.conn.recv_timeout(timeout).map(Into::into)
    }

    /// Async: the next event, or `None` once the application is shutting down.
    pub async fn recv(&mut self) -> Option<LibraryEvent> {
        if *self.shutdown.borrow() {
            return None;
        }
        tokio::select! {
            m = self.conn.recv() => Some(m.into()),
            _ = self.shutdown.wait_for(|down| *down) => None,
        }
    }

    /// How many events this subscription has dropped to overflow.
    pub fn dropped(&self) -> u64 {
        self.conn.dropped()
    }
}

/// The `system` operations.
pub struct System<'a> {
    pub(super) app: &'a App,
}

impl System<'_> {
    /// Touches nothing: the process is up.
    pub fn health(&self) -> Health {
        Health::Ok
    }

    /// The fingerprint pool's state now.
    pub fn pool_status(&self) -> PoolState {
        self.app.pool().state()
    }

    /// The pool's state now, then again after each change; changes that arrive
    /// between two reads are one value carrying the newest state.
    pub fn pool_feed(&self) -> Feed<PoolState> {
        // Subscribed before the state is read: a change in between is
        // delivered as one more value, never lost.
        let changes = self.app.0.pool_changes.subscribe();
        let app = self.app.clone();
        Feed::counted(changes, self.app.shutdown_signal(), move || {
            app.pool().state()
        })
    }

    /// Every bus event from now on. The subscription is registered before this
    /// returns.
    pub fn library_events(&self) -> LibraryEvents {
        LibraryEvents {
            conn: self.app.relay().connect(),
            shutdown: self.app.shutdown_signal(),
        }
    }
}
