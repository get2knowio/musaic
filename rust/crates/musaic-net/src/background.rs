//! Background requests that yield to the ones a user is waiting on.
//!
//! A background sweep (a health check walking the whole library) and the
//! user's own lookups share one host limit, MusicBrainz's one request per
//! second. [`BackgroundPaced`] sends the sweep's requests through the same
//! [`Limiter`] as the foreground, but takes a slot only when the host has
//! been quiet, so the sweep fills the gaps instead of taking turns.
//!
//! Before each try (the first and every retry after a busy answer) a
//! background request:
//!
//! 1. stops with [`TransportError::Stopped`] if its stop flag is set;
//! 2. waits, re-checking every poll interval, while the caller's `busy`
//!    predicate is true (the app is doing something the sweep should not
//!    compete with);
//! 3. takes a slot with [`Limiter::try_acquire_quiet`]: only once no request
//!    has taken a slot for that host for `quiet` (default
//!    [`BACKGROUND_QUIET`], 1.5 s), nobody is waiting in
//!    [`Limiter::acquire`] for it, and the pacing gap and any busy-answer
//!    push-back allow a request now. Otherwise it sleeps (at most one poll
//!    interval at a time) and starts again from step 1.
//!
//! What this guarantees:
//!
//! - **The shared limit holds.** Background slots are slots of the same
//!   limiter, so foreground and background requests together never exceed
//!   the host's pacing.
//! - **Foreground waits at most one pacing gap behind background.** A
//!   background caller never sleeps holding the host's slot lock and never
//!   queues behind a foreground caller; the only way it delays a foreground
//!   request is by having taken the slot just before it, which costs that
//!   request at most one gap (1 s for MusicBrainz). Without background
//!   traffic such a request would have gone at once, since with `quiet` at
//!   least the gap the host was idle anyway. One exception is the server's
//!   doing: a busy answer (503 / 429) to a background request pushes back
//!   the whole host, foreground included, as any busy answer does.
//! - **Background makes progress only in gaps.** There is no fairness
//!   guarantee for background work: a foreground stream with gaps shorter
//!   than `quiet` holds it off for as long as the stream lasts. Once
//!   foreground traffic stops (and `busy` is false), a background request
//!   goes `quiet` after the last foreground slot, give or take one poll
//!   interval on a real clock. Its own requests count as traffic too, so a
//!   sweep sends at most one request per `quiet`.
//! - **Stopping is prompt.** Every sleep is at most one poll interval
//!   (default [`BACKGROUND_POLL`]), so a set stop flag ends the wait within
//!   about that long, with nothing sent. A request already on the wire is
//!   not interrupted.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::pacing::send_with_retry;
use crate::{Clock, Limiter, Paced, Request, Response, RetryPolicy, Transport, TransportError};

/// How long a host must have had no request before a background request
/// goes: one and a half MusicBrainz slots, so a user clicking through
/// albums at about a request a second keeps the sweep out.
pub const BACKGROUND_QUIET: Duration = Duration::from_millis(1500);

/// The longest a background caller sleeps before looking again at its stop
/// flag, its `busy` predicate and the host.
pub const BACKGROUND_POLL: Duration = Duration::from_millis(250);

type Busy = Arc<dyn Fn() -> bool + Send + Sync>;

/// A transport for background work: paced on a shared [`Limiter`], and
/// only when the host is quiet and the caller is not busy. See the
/// [module docs](self) for the guarantee.
///
/// Build it from the foreground [`Paced`] with [`Paced::background`] so the
/// two share one limiter, or with [`BackgroundPaced::new`] and the same
/// limiter (e.g. [`crate::process_limiter`]).
pub struct BackgroundPaced<T, C> {
    inner: T,
    limiter: Arc<Limiter<C>>,
    retry: RetryPolicy,
    busy: Busy,
    stop: Arc<AtomicBool>,
    quiet: Duration,
    poll: Duration,
}

impl<T: Clone, C> Clone for BackgroundPaced<T, C> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            limiter: Arc::clone(&self.limiter),
            retry: self.retry.clone(),
            busy: Arc::clone(&self.busy),
            stop: Arc::clone(&self.stop),
            quiet: self.quiet,
            poll: self.poll,
        }
    }
}

impl<T: Transport, C: Clock> BackgroundPaced<T, C> {
    /// `inner` is the bare transport (not a [`Paced`]: the slot is taken
    /// here). `busy` is polled before each try; setting `stop` ends a
    /// waiting request with [`TransportError::Stopped`].
    pub fn new(
        inner: T,
        limiter: Arc<Limiter<C>>,
        retry: RetryPolicy,
        busy: impl Fn() -> bool + Send + Sync + 'static,
        stop: Arc<AtomicBool>,
    ) -> Self {
        Self {
            inner,
            limiter,
            retry,
            busy: Arc::new(busy),
            stop,
            quiet: BACKGROUND_QUIET,
            poll: BACKGROUND_POLL,
        }
    }

    /// How long the host must have been idle (default [`BACKGROUND_QUIET`]).
    pub fn with_quiet(mut self, quiet: Duration) -> Self {
        self.quiet = quiet;
        self
    }

    /// The longest single sleep (default [`BACKGROUND_POLL`]). Clamped to
    /// at least 1 ms so a wait never spins.
    pub fn with_poll(mut self, poll: Duration) -> Self {
        self.poll = poll.max(Duration::from_millis(1));
        self
    }

    pub fn limiter(&self) -> &Arc<Limiter<C>> {
        &self.limiter
    }

    /// Steps 1-3 of the module docs: returns once a slot is taken.
    fn take_quiet_slot(&self, host: &str) -> Result<(), TransportError> {
        let clock = self.limiter.clock();
        loop {
            if self.stop.load(Ordering::SeqCst) {
                return Err(TransportError::Stopped);
            }
            if (self.busy)() {
                clock.sleep(self.poll);
                continue;
            }
            match self.limiter.try_acquire_quiet(host, self.quiet) {
                Ok(_) => return Ok(()),
                Err(not_before) => {
                    let wait = not_before.saturating_sub(clock.now());
                    clock.sleep(if wait.is_zero() {
                        self.poll
                    } else {
                        wait.min(self.poll)
                    });
                }
            }
        }
    }
}

impl<T: Transport, C: Clock> Transport for BackgroundPaced<T, C> {
    fn send(&self, request: &Request) -> Result<Response, TransportError> {
        send_with_retry(&self.inner, &self.limiter, &self.retry, request, |host| {
            self.take_quiet_slot(host)
        })
    }
}

impl<T: Transport + Clone, C: Clock> Paced<T, C> {
    /// A background twin of this transport: same inner transport, same
    /// limiter, same retry policy.
    pub fn background(
        &self,
        busy: impl Fn() -> bool + Send + Sync + 'static,
        stop: Arc<AtomicBool>,
    ) -> BackgroundPaced<T, C> {
        BackgroundPaced::new(
            self.inner.clone(),
            Arc::clone(self.limiter()),
            self.retry.clone(),
            busy,
            stop,
        )
    }
}
