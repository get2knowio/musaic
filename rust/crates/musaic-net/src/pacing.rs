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
//! slot, which only widens a gap. A [`Paced`] given a flag
//! ([`Paced::with_cancel`]) also stops on purpose: while it waits for its
//! slot or for a retry delay it looks at the flag every [`CANCEL_POLL`] and
//! ends with [`crate::TransportError::Stopped`], sending nothing and taking
//! no slot.
//!
//! Busy answers: a 503 or 429 pushes back the
//! answering host's shared slot, so every caller on that host waits, not
//! only the one that got the answer (MusicBrainz marks its 503s as a global
//! rate-limit zone). The wait is the server's `Retry-After` when it is
//! longer than our own backoff, capped at [`MAX_RETRY_WAIT`] (a larger
//! figure waits the cap; it does not abort). `Retry-After` is read in both
//! forms: whole seconds, or an HTTP date, which is turned into a delay
//! against the wall clock at the moment the answer arrives (the limiter
//! itself runs on a monotonic clock). The push-back is kept apart
//! from the slot lock, so a caller already asleep waiting for its slot sees
//! it when it wakes and sleeps on; callers still leave one gap apart when
//! the wait ends.
//!
//! Background work (a sweep nobody is waiting on) takes its slots through
//! [`Limiter::acquire_quiet`] instead, usually via
//! [`crate::BackgroundPaced`]; see [`crate::background`] for what that
//! promises foreground callers.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, TryLockError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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

/// The longest a cancellable wait sleeps before looking at its cancel flag
/// again.
pub const CANCEL_POLL: Duration = Duration::from_millis(100);

/// How often a cancellable caller looks for the slot lock another caller is
/// sleeping on.
const LOCK_POLL: Duration = Duration::from_millis(10);

/// When requests to one host went and may go.
#[derive(Debug, Default)]
struct SlotTimes {
    /// Earliest time the next request may go out under the pacing gap.
    next: Duration,
    /// When the last request to this host took its slot (foreground or
    /// background); `None` before the first.
    last: Option<Duration>,
}

/// One host's state.
#[derive(Debug, Default)]
struct HostSlot {
    /// Held while a foreground caller waits for its slot; a background
    /// caller never sleeps holding it.
    times: Mutex<SlotTimes>,
    /// No request before this time: the host said it is busy.
    /// Never held across a sleep, so a busy answer lands at once.
    hold_until: Mutex<Duration>,
}

impl HostSlot {
    fn hold(&self) -> Duration {
        *self
            .hold_until
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
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
        self.acquire_inner(host, None)
            .expect("an uncancellable wait always takes its slot")
    }

    /// [`Limiter::acquire`] that gives up with
    /// [`TransportError::Stopped`] once `cancelled` returns true, whether
    /// it is waiting for another caller's turn, for the pacing gap or for a
    /// push-back. It looks at `cancelled` at least every [`CANCEL_POLL`].
    /// A caller that gives up takes no slot and moves nothing: the next
    /// caller's slot is where it would have been.
    pub fn acquire_cancellable(
        &self,
        host: &str,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Duration, TransportError> {
        self.acquire_inner(host, Some(cancelled))
            .ok_or(TransportError::Stopped)
    }

    fn acquire_inner(&self, host: &str, cancelled: Option<&dyn Fn() -> bool>) -> Option<Duration> {
        let is_cancelled = || cancelled.is_some_and(|c| c());
        let gap = self.pacing.interval_for(host).unwrap_or(Duration::ZERO);
        let slot = self.slot(host);
        let mut times = match cancelled {
            None => slot
                .times
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
            // Another caller sleeping for its slot holds the lock; wait for
            // it in short steps so a cancel is seen meanwhile.
            Some(_) => loop {
                match slot.times.try_lock() {
                    Ok(times) => break times,
                    Err(TryLockError::Poisoned(poisoned)) => break poisoned.into_inner(),
                    Err(TryLockError::WouldBlock) => {
                        if is_cancelled() {
                            return None;
                        }
                        self.clock.sleep(LOCK_POLL);
                    }
                }
            },
        };
        // A push-back may arrive while this caller sleeps: look again on
        // waking, until nothing is in the way.
        loop {
            if is_cancelled() {
                return None;
            }
            let target = times.next.max(slot.hold());
            let now = self.clock.now();
            if target <= now {
                break;
            }
            let wait = target - now;
            self.clock.sleep(if cancelled.is_some() {
                wait.min(CANCEL_POLL)
            } else {
                wait
            });
        }
        let taken = self.clock.now();
        times.next = taken + gap;
        times.last = Some(taken);
        Some(taken)
    }

    /// Take a send slot for `host` only if no request has gone to it for
    /// `quiet` (and the pacing gap and any push-back also allow one now).
    /// Never blocks: returns `Ok(taken)` with the slot taken, or
    /// `Err(not_before)`, the earliest time a retry could succeed if no
    /// other request arrives in the meantime.
    ///
    /// A caller already waiting in [`Limiter::acquire`] for this host counts
    /// as a request: it holds the slot lock, and this reports busy instead
    /// of queueing behind it.
    pub fn try_acquire_quiet(&self, host: &str, quiet: Duration) -> Result<Duration, Duration> {
        let gap = self.pacing.interval_for(host).unwrap_or(Duration::ZERO);
        let slot = self.slot(host);
        let mut times = match slot.times.try_lock() {
            Ok(times) => times,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => {
                // Someone is taking a slot at or after now; quiet cannot
                // start before it (the floor keeps a zero gap from spinning).
                let now = self.clock.now();
                return Err(now + quiet.max(gap).max(Duration::from_millis(1)));
            }
        };
        let quiet_from = times.last.map_or(Duration::ZERO, |last| last + quiet);
        let target = times.next.max(slot.hold()).max(quiet_from);
        let now = self.clock.now();
        if target > now {
            return Err(target);
        }
        times.next = now + gap;
        times.last = Some(now);
        Ok(now)
    }

    /// Wait until no request has gone to `host` for `quiet`, then take a
    /// send slot. The wait happens outside the slot lock and is re-checked
    /// on waking, so any foreground request that arrives meanwhile goes
    /// first and restarts the quiet period. See [`crate::background`].
    pub fn acquire_quiet(&self, host: &str, quiet: Duration) -> Duration {
        loop {
            match self.try_acquire_quiet(host, quiet) {
                Ok(taken) => return taken,
                Err(not_before) => {
                    let now = self.clock.now();
                    self.clock.sleep(not_before.saturating_sub(now));
                }
            }
        }
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
    pub(crate) inner: T,
    limiter: Arc<Limiter<C>>,
    pub(crate) retry: RetryPolicy,
    cancel: Option<Arc<AtomicBool>>,
}

impl<T: Clone, C> Clone for Paced<T, C> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            limiter: Arc::clone(&self.limiter),
            retry: self.retry.clone(),
            cancel: self.cancel.clone(),
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
            cancel: None,
        }
    }

    /// A transport that stops with [`TransportError::Stopped`] once `flag`
    /// is set: a request waiting for its pacing slot or for a retry delay
    /// ends within [`CANCEL_POLL`] without sending anything, and no retry is
    /// started. A request already on the wire is not interrupted (its
    /// timeout bounds it). The limit still holds for every caller sharing
    /// the limiter; a cancelled caller takes no slot.
    pub fn with_cancel(mut self, flag: Arc<AtomicBool>) -> Self {
        self.cancel = Some(flag);
        self
    }

    pub fn limiter(&self) -> &Arc<Limiter<C>> {
        &self.limiter
    }
}

/// `Retry-After` in either form: whole seconds, or an HTTP date (RFC 9110:
/// the IMF-fixdate, and the two obsolete forms a server may still send)
/// measured against the wall clock `now`. A date already past is zero. A
/// figure too large to hold is the largest duration; callers cap it
/// ([`MAX_RETRY_WAIT`]), so a hostile value never waits longer than the cap.
/// Anything else is absent.
pub(crate) fn parse_retry_after(value: &str, now: SystemTime) -> Option<Duration> {
    let value = value.trim();
    if !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()) {
        return Some(Duration::from_secs(
            value.parse::<u64>().unwrap_or(u64::MAX),
        ));
    }
    let when = parse_http_date(value)?;
    let now = now
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    Some(Duration::from_secs(when.saturating_sub(now).max(0) as u64))
}

/// Seconds since the Unix epoch of an HTTP date (GMT).
fn parse_http_date(s: &str) -> Option<i64> {
    let tokens: Vec<&str> = s
        .split(|c: char| c.is_ascii_whitespace() || matches!(c, ',' | '-' | ':'))
        .filter(|t| !t.is_empty())
        .collect();
    // Drop the weekday; then "dd Mon yyyy hh mm ss" (IMF-fixdate; RFC 850
    // has a two-digit year) or "Mon dd hh mm ss yyyy" (asctime).
    let t = tokens.get(1..)?;
    let num = |i: usize| t.get(i)?.parse::<i64>().ok();
    let day_first = t.first()?.bytes().all(|b| b.is_ascii_digit());
    let (day, month, mut year, h, m, sec) = if day_first {
        (
            num(0)?,
            month_number(t.get(1)?)?,
            num(2)?,
            num(3)?,
            num(4)?,
            num(5)?,
        )
    } else {
        (
            num(1)?,
            month_number(t.first()?)?,
            num(5)?,
            num(2)?,
            num(3)?,
            num(4)?,
        )
    };
    if day_first && t.get(2)?.len() <= 2 {
        year += if year >= 70 { 1900 } else { 2000 };
    }
    let valid = (1..=31).contains(&day)
        && (0..24).contains(&h)
        && (0..60).contains(&m)
        && (0..=60).contains(&sec)
        && (1970..=9999).contains(&year);
    if !valid {
        return None;
    }
    // Days from the civil date.
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + h * 3600 + m * 60 + sec)
}

fn month_number(name: &str) -> Option<i64> {
    const MONTHS: [&str; 12] = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    let name = name.get(..3)?.to_ascii_lowercase();
    MONTHS.iter().position(|m| *m == name).map(|i| i as i64 + 1)
}

fn retry_after(resp: &Response) -> Option<Duration> {
    parse_retry_after(resp.header("retry-after")?, SystemTime::now())
}

impl<T: Transport, C: Clock> Transport for Paced<T, C> {
    fn send(&self, request: &Request) -> Result<Response, TransportError> {
        send_with_retry(&self.inner, &self.limiter, &self.retry, request, |host| {
            match &self.cancel {
                None => {
                    self.limiter.acquire(host);
                }
                Some(flag) => {
                    self.limiter
                        .acquire_cancellable(host, &|| flag.load(Ordering::SeqCst))?;
                }
            }
            Ok(())
        })
    }
}

/// The send-and-retry loop shared by [`Paced`] and
/// [`crate::BackgroundPaced`]; they differ only in how a slot is taken
/// before each try. `take_slot` may refuse (a stopped background caller),
/// which ends the request with that error.
pub(crate) fn send_with_retry<T: Transport, C: Clock>(
    inner: &T,
    limiter: &Limiter<C>,
    retry: &RetryPolicy,
    request: &Request,
    mut take_slot: impl FnMut(&str) -> Result<(), TransportError>,
) -> Result<Response, TransportError> {
    let host = request.host().to_lowercase();
    let mut backoff = retry.base_backoff;
    let mut attempt = 1;
    loop {
        take_slot(&host)?;
        let resp = inner.send(request)?;
        let busy = resp.status == 503 || resp.status == 429;
        if !busy {
            return Ok(resp);
        }
        // The server's own figure when it is longer than ours; MusicBrainz
        // sends "Retry-After: 0" while busy, which must not mean "hammer".
        // Capped. The whole host backs off, this caller
        // included: its retry waits for a slot like everyone else.
        let wait = retry_after(&resp)
            .map_or(backoff, |ra| ra.max(backoff))
            .min(MAX_RETRY_WAIT);
        let now = limiter.clock().now();
        limiter.push_back(&host, now + wait);
        if attempt >= retry.max_attempts {
            return Ok(resp);
        }
        backoff = (backoff * 2).min(retry.max_backoff);
        attempt += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 06 Nov 1994 08:49:37 GMT, the example date in the HTTP standard.
    const EXAMPLE: u64 = 784_111_777;

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    #[test]
    fn all_three_http_date_forms_are_read() {
        for form in [
            "Sun, 06 Nov 1994 08:49:37 GMT",
            "Sunday, 06-Nov-94 08:49:37 GMT",
            "Sun Nov  6 08:49:37 1994",
        ] {
            assert_eq!(
                parse_retry_after(form, at(EXAMPLE - 10)),
                Some(Duration::from_secs(10)),
                "{form}"
            );
        }
    }

    #[test]
    fn seconds_dates_past_and_junk() {
        let now = at(EXAMPLE);
        assert_eq!(parse_retry_after(" 7 ", now), Some(Duration::from_secs(7)));
        assert_eq!(parse_retry_after("0", now), Some(Duration::ZERO));
        assert_eq!(
            parse_retry_after("Sun, 06 Nov 1994 08:49:00 GMT", now),
            Some(Duration::ZERO),
            "a date already past"
        );
        assert_eq!(
            parse_retry_after("99999999999999999999999999", now),
            Some(Duration::from_secs(u64::MAX))
        );
        for junk in [
            "",
            "soon",
            "-3",
            "1.5",
            "Sun, 32 Nov 1994 08:49:37 GMT",
            "Sun, 06 Foo 1994 08:49:37 GMT",
            "Sun, 06 Nov 1994 25:49:37 GMT",
            "Sun, 06 Nov",
        ] {
            assert_eq!(parse_retry_after(junk, now), None, "{junk:?}");
        }
    }
}
