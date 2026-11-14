//! Per-host pacing and retry on "busy" answers.
//!
//! A deliberate change from the Python app, which sleeps only after one kind
//! of MusicBrainz request and never retries. Every live run during development
//! saw MusicBrainz answer 503 with `Retry-After: 0`.
//!
//! The limit is held by a [`Limiter`], which is `Sync` and meant to be shared
//! (`Arc`) by every caller in the process; [`process_limiter`] is the one a
//! host app uses. Cloning a [`Paced`] shares its limiter.
//!
//! Each host has its own slot lock. A caller holds it while it waits for its
//! slot (so the next slot is measured from when this one was really taken,
//! not from when it was planned, and oversleeping can never squeeze two
//! requests together), never across the network call. Hosts do not wait on
//! each other.
//!
//! Cancellation: a caller that gives up (a dropped future around
//! `spawn_blocking`, a panicking transport) at any point leaves the limiter
//! consistent; a poisoned lock is recovered; the worst case is one unused
//! slot, which only widens a gap.
//!
//! Busy answers: a 503 or 429 pushes back the
//! answering host's shared slot, so every caller on that host waits, not
//! only the one that got the answer (MusicBrainz marks its 503s as a global
//! rate-limit zone). The wait is the server's numeric `Retry-After` when it
//! is longer than our own backoff, capped at [`MAX_RETRY_WAIT`] (a larger
//! figure waits the cap; it does not abort). The push-back is kept apart
//! from the slot lock, so a caller already asleep waiting for its slot sees
//! it when it wakes and sleeps on; callers still leave one gap apart when
//! the wait ends. An HTTP-date `Retry-After` is treated as absent (our
//! backoff applies): the limiter runs on a monotonic clock with no wall
//! time to compare a date against.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::{Request, Response, Transport, TransportError};

pub trait Clock {
    /// Monotonic time since an arbitrary origin.
    fn now(&self) -> Duration;
    fn sleep(&self, d: Duration);
}

/// Real time, measured from one origin shared by the whole process, so two
/// `SystemClock`s agree and may share a [`Limiter`].
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

fn process_origin() -> Instant {
    static ORIGIN: OnceLock<Instant> = OnceLock::new();
    *ORIGIN.get_or_init(Instant::now)
}

impl Clock for SystemClock {
    fn now(&self) -> Duration {
        process_origin().elapsed()
    }

    fn sleep(&self, d: Duration) {
        std::thread::sleep(d);
    }
}

impl<C: Clock + ?Sized> Clock for &C {
    fn now(&self) -> Duration {
        (**self).now()
    }

    fn sleep(&self, d: Duration) {
        (**self).sleep(d)
    }
}

impl<C: Clock + ?Sized> Clock for Arc<C> {
    fn now(&self) -> Duration {
        (**self).now()
    }

    fn sleep(&self, d: Duration) {
        (**self).sleep(d)
    }
}

/// Minimum gap between two requests to the same host.
#[derive(Debug, Clone)]
pub struct PacingPolicy {
    pub min_interval: Vec<(String, Duration)>,
}

impl Default for PacingPolicy {
    /// MusicBrainz: one request per second. AcoustID: three per second.
    fn default() -> Self {
        Self {
            min_interval: vec![
                ("musicbrainz.org".into(), Duration::from_millis(1000)),
                ("api.acoustid.org".into(), Duration::from_millis(334)),
            ],
        }
    }
}

impl PacingPolicy {
    fn interval_for(&self, host: &str) -> Option<Duration> {
        self.min_interval
            .iter()
            .find(|(h, _)| host == h || host.ends_with(&format!(".{h}")))
            .map(|(_, d)| *d)
    }
}

/// What to do with 503 and 429.
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    /// Total tries, the first included.
    pub max_attempts: u32,
    /// Wait before the second try; doubles each time, up to `max_backoff`.
    pub base_backoff: Duration,
    pub max_backoff: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 4,
            base_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(30),
        }
    }
}

/// The longest a busy answer makes anyone wait: a larger
/// `Retry-After` waits this long; it does not abort the request.
pub const MAX_RETRY_WAIT: Duration = Duration::from_secs(30);

/// One host's state.
#[derive(Debug, Default)]
struct HostSlot {
    /// Earliest time the next request may go out under the pacing gap.
    /// Held while a caller waits for its slot.
    next: Mutex<Duration>,
    /// No request before this time: the host said it is busy.
    /// Never held across a sleep, so a busy answer lands at once.
    hold_until: Mutex<Duration>,
}

/// Send slots per host, on one clock. Share it (`Arc`) between every caller
/// that must respect the same limit.
#[derive(Debug)]
pub struct Limiter<C> {
    clock: C,
    pacing: PacingPolicy,
    hosts: Mutex<HashMap<String, Arc<HostSlot>>>,
}

impl<C: Clock> Limiter<C> {
    pub fn new(clock: C, pacing: PacingPolicy) -> Self {
        Self {
            clock,
            pacing,
            hosts: Mutex::new(HashMap::new()),
        }
    }

    pub fn clock(&self) -> &C {
        &self.clock
    }

    fn slot(&self, host: &str) -> Arc<HostSlot> {
        let mut map = self
            .hosts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Arc::clone(map.entry(host.to_string()).or_default())
    }

    /// Wait for the next send slot for `host` and take it. Returns the time
    /// it was taken. Hosts without a pacing gap only wait out a push-back.
    pub fn acquire(&self, host: &str) -> Duration {
        let gap = self.pacing.interval_for(host).unwrap_or(Duration::ZERO);
        let slot = self.slot(host);
        let mut next = slot
            .next
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // A push-back may arrive while this caller sleeps: look again on
        // waking, until nothing is in the way.
        loop {
            let hold = *slot
                .hold_until
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let target = (*next).max(hold);
            let now = self.clock.now();
            if target <= now {
                break;
            }
            self.clock.sleep(target - now);
        }
        let taken = self.clock.now();
        *next = taken + gap;
        taken
    }

    /// Nobody sends to `host` before `until` (a busy answer). Only
    /// ever moves the hold later.
    pub fn push_back(&self, host: &str, until: Duration) {
        let slot = self.slot(host);
        let mut hold = slot
            .hold_until
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if until > *hold {
            *hold = until;
        }
    }
}

/// The process-wide limiter on real time, with the default policy
/// (MusicBrainz 1/s, AcoustID 3/s). Every live client a host app builds
/// should use this one.
pub fn process_limiter() -> Arc<Limiter<SystemClock>> {
    static LIMITER: OnceLock<Arc<Limiter<SystemClock>>> = OnceLock::new();
    LIMITER
        .get_or_init(|| Arc::new(Limiter::new(SystemClock, PacingPolicy::default())))
        .clone()
}

/// Wraps a transport with pacing and retry. Clones share the limiter, so
/// the limit holds across every clone, thread and task.
pub struct Paced<T, C> {
    inner: T,
    limiter: Arc<Limiter<C>>,
    retry: RetryPolicy,
}

impl<T: Clone, C> Clone for Paced<T, C> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            limiter: Arc::clone(&self.limiter),
            retry: self.retry.clone(),
        }
    }
}

impl<T: Transport, C: Clock> Paced<T, C> {
    /// A new limiter of its own, default policies.
    pub fn new(inner: T, clock: C) -> Self {
        Self::with_policies(
            inner,
            clock,
            PacingPolicy::default(),
            RetryPolicy::default(),
        )
    }

    /// A new limiter of its own.
    pub fn with_policies(inner: T, clock: C, pacing: PacingPolicy, retry: RetryPolicy) -> Self {
        Self::shared(inner, Arc::new(Limiter::new(clock, pacing)), retry)
    }

    /// Use an existing limiter (e.g. [`process_limiter`]).
    pub fn shared(inner: T, limiter: Arc<Limiter<C>>, retry: RetryPolicy) -> Self {
        Self {
            inner,
            limiter,
            retry,
        }
    }

    pub fn limiter(&self) -> &Arc<Limiter<C>> {
        &self.limiter
    }
}

/// `Retry-After` as whole seconds. The HTTP-date form is treated as absent
/// (there is no wall clock on the limiter's time line).
fn retry_after(resp: &Response) -> Option<Duration> {
    resp.header("retry-after")?
        .trim()
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
}

impl<T: Transport, C: Clock> Transport for Paced<T, C> {
    fn send(&self, request: &Request) -> Result<Response, TransportError> {
        let host = request.host().to_lowercase();
        let mut backoff = self.retry.base_backoff;
        let mut attempt = 1;
        loop {
            self.limiter.acquire(&host);
            let resp = self.inner.send(request)?;
            let busy = resp.status == 503 || resp.status == 429;
            if !busy {
                return Ok(resp);
            }
            // The server's own figure when it is longer than ours; MusicBrainz
            // sends "Retry-After: 0" while busy, which must not mean "hammer".
            // Capped. The whole host backs off, this caller
            // included: its retry waits in `acquire` like everyone else.
            let wait = retry_after(&resp)
                .map_or(backoff, |ra| ra.max(backoff))
                .min(MAX_RETRY_WAIT);
            let now = self.limiter.clock().now();
            self.limiter.push_back(&host, now + wait);
            if attempt >= self.retry.max_attempts {
                return Ok(resp);
            }
            backoff = (backoff * 2).min(self.retry.max_backoff);
            attempt += 1;
        }
    }
}
