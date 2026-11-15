//! Background requests on the shared limiter: they wait while the caller is
//! busy, go only when the host is quiet, yield to foreground requests,
//! stop on request, and leave foreground pacing as it was.
//!
//! Most tests run on one thread with a shared virtual clock whose `sleep`
//! can fire scripted foreground requests at given times, so "a user request
//! arrives while the sweep waits" is deterministic. One short real-time
//! check covers the stop flag across threads.

mod common;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use musaic_net::{
    BACKGROUND_POLL, BACKGROUND_QUIET, BackgroundPaced, Clock, Limiter, MusicBrainzClient, Paced,
    PacingPolicy, ReplayTransport, Request, Response, RetryPolicy, SystemClock, Transport,
    TransportError,
};

const MS: fn(u64) -> Duration = Duration::from_millis;
const SECS: fn(u64) -> Duration = Duration::from_secs;
const HOST: &str = "musicbrainz.org";

type Event = Box<dyn FnOnce() + Send>;

/// Virtual time shared by everyone on it. `sleep(d)` advances it by `d`,
/// first firing any scripted event due by then at its own time. Events do
/// not fire inside an event (a foreground request that sleeps for its slot
/// just moves the clock).
#[derive(Default)]
struct ScriptClock {
    now: Mutex<Duration>,
    events: Mutex<Vec<(Duration, Event)>>,
    in_event: AtomicBool,
    sleeps: Mutex<Vec<Duration>>,
}

impl ScriptClock {
    fn set(&self, t: Duration) {
        *self.now.lock().unwrap() = t;
    }

    fn at(&self, t: Duration, f: impl FnOnce() + Send + 'static) {
        self.events.lock().unwrap().push((t, Box::new(f)));
    }

    fn next_due(&self, by: Duration) -> Option<(Duration, Event)> {
        let mut ev = self.events.lock().unwrap();
        let i = (0..ev.len())
            .filter(|&i| ev[i].0 <= by)
            .min_by_key(|&i| ev[i].0)?;
        Some(ev.remove(i))
    }

    fn longest_sleep(&self) -> Duration {
        self.sleeps
            .lock()
            .unwrap()
            .iter()
            .copied()
            .max()
            .unwrap_or_default()
    }
}

impl Clock for ScriptClock {
    fn now(&self) -> Duration {
        *self.now.lock().unwrap()
    }

    fn sleep(&self, d: Duration) {
        self.sleeps.lock().unwrap().push(d);
        let end = self.now() + d;
        if !self.in_event.swap(true, Ordering::SeqCst) {
            while let Some((t, f)) = self.next_due(end) {
                let now = self.now().max(t);
                self.set(now);
                f();
            }
            self.in_event.store(false, Ordering::SeqCst);
        }
        let now = self.now().max(end);
        self.set(now);
    }
}

type Lim = Limiter<Arc<ScriptClock>>;

struct Setup {
    clock: Arc<ScriptClock>,
    limiter: Arc<Lim>,
    /// Slot times of the scripted foreground requests.
    fg: Arc<Mutex<Vec<Duration>>>,
}

impl Setup {
    fn new() -> Self {
        let clock = Arc::new(ScriptClock::default());
        let limiter = Arc::new(Limiter::new(clock.clone(), PacingPolicy::default()));
        Self {
            clock,
            limiter,
            fg: Arc::default(),
        }
    }

    /// A foreground request arriving at `t` (fired from the clock while
    /// the background request sleeps).
    fn foreground_at(&self, t: Duration) {
        let (limiter, fg) = (self.limiter.clone(), self.fg.clone());
        self.clock.at(t, move || {
            let taken = limiter.acquire(HOST);
            fg.lock().unwrap().push(taken);
        });
    }

    /// A foreground request now, from the test itself.
    fn foreground_now(&self) -> Duration {
        let taken = self.limiter.acquire(HOST);
        self.fg.lock().unwrap().push(taken);
        taken
    }

    fn fg(&self) -> Vec<Duration> {
        self.fg.lock().unwrap().clone()
    }

    fn background(
        &self,
        rec: &Arc<Recorder>,
        busy: impl Fn() -> bool + Send + Sync + 'static,
        stop: Arc<AtomicBool>,
    ) -> BackgroundPaced<Arc<Recorder>, Arc<ScriptClock>> {
        BackgroundPaced::new(
            rec.clone(),
            self.limiter.clone(),
            RetryPolicy::default(),
            busy,
            stop,
        )
    }
}

/// Notes the virtual time of every request it sends; answers `statuses`
/// in turn, then 200.
struct Recorder {
    clock: Arc<ScriptClock>,
    sent: Mutex<Vec<Duration>>,
    statuses: Mutex<Vec<(u16, &'static str)>>,
}

impl Recorder {
    fn new(clock: &Arc<ScriptClock>) -> Arc<Self> {
        Arc::new(Self {
            clock: clock.clone(),
            sent: Mutex::default(),
            statuses: Mutex::default(),
        })
    }

    fn sent(&self) -> Vec<Duration> {
        self.sent.lock().unwrap().clone()
    }
}

impl Transport for Recorder {
    fn send(&self, _: &Request) -> Result<Response, TransportError> {
        self.sent.lock().unwrap().push(self.clock.now());
        let mut st = self.statuses.lock().unwrap();
        let (status, retry_after) = if st.is_empty() {
            (200, "")
        } else {
            st.remove(0)
        };
        Ok(Response {
            status,
            headers: if retry_after.is_empty() {
                vec![]
            } else {
                vec![("Retry-After".into(), retry_after.into())]
            },
            body: vec![],
        })
    }
}

fn mb() -> Request {
    MusicBrainzClient::<ReplayTransport>::search_request("A", "B")
}

fn never() -> impl Fn() -> bool + Send + Sync + 'static {
    || false
}

fn no_stop() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(false))
}

/// The defaults the guarantee is written for: quiet is longer than the
/// MusicBrainz gap, and a poll is short.
#[test]
fn defaults() {
    assert_eq!(BACKGROUND_QUIET, MS(1500));
    assert_eq!(BACKGROUND_POLL, MS(250));
}

/// On an idle host, a background request goes at once.
#[test]
fn background_on_an_idle_host_goes_at_once() {
    let s = Setup::new();
    let rec = Recorder::new(&s.clock);
    s.background(&rec, never(), no_stop()).send(&mb()).unwrap();
    assert_eq!(rec.sent(), [Duration::ZERO]);
    assert!(s.clock.sleeps.lock().unwrap().is_empty());
}

/// While `busy` is true nothing is sent; the predicate is re-checked every
/// poll interval, and the request goes at the first check after it clears.
#[test]
fn background_waits_while_busy() {
    let s = Setup::new();
    let rec = Recorder::new(&s.clock);
    let busy = Arc::new(AtomicBool::new(true));
    {
        let busy = busy.clone();
        s.clock
            .at(MS(2900), move || busy.store(false, Ordering::SeqCst));
    }
    let b = busy.clone();
    s.background(&rec, move || b.load(Ordering::SeqCst), no_stop())
        .send(&mb())
        .unwrap();
    assert_eq!(rec.sent(), [SECS(3)], "the first 250 ms check after 2.9 s");
    assert_eq!(s.clock.longest_sleep(), BACKGROUND_POLL);
}

/// A foreground request that arrives while background waits for quiet goes
/// at once (its own slot, not delayed) and restarts the quiet period.
#[test]
fn background_yields_to_foreground_arriving_during_the_quiet_wait() {
    let s = Setup::new();
    let rec = Recorder::new(&s.clock);
    assert_eq!(s.foreground_now(), Duration::ZERO);
    s.clock.set(MS(500));
    s.foreground_at(MS(1200));
    s.background(&rec, never(), no_stop()).send(&mb()).unwrap();
    assert_eq!(s.fg(), [Duration::ZERO, MS(1200)], "foreground not delayed");
    assert_eq!(rec.sent(), [MS(2700)], "1.5 s after the last foreground");
    assert!(s.clock.longest_sleep() <= BACKGROUND_POLL);
}

/// The one cost to foreground: a request that arrives just after a
/// background one took the slot waits out that slot's gap, no more.
#[test]
fn foreground_waits_at_most_one_gap_behind_background() {
    let s = Setup::new();
    let rec = Recorder::new(&s.clock);
    s.background(&rec, never(), no_stop()).send(&mb()).unwrap();
    assert_eq!(rec.sent(), [Duration::ZERO]);
    s.clock.set(MS(100));
    assert_eq!(s.foreground_now(), SECS(1), "one MusicBrainz gap");
    // The next foreground request is paced as usual, not by background.
    assert_eq!(s.foreground_now(), SECS(2));
}

/// A steady foreground stream with gaps shorter than `quiet` keeps
/// background out for as long as it lasts (no fairness is promised); once
/// it stops, background goes `quiet` after the last foreground slot.
#[test]
fn a_steady_foreground_stream_holds_background_off_until_it_stops() {
    let s = Setup::new();
    let rec = Recorder::new(&s.clock);
    s.foreground_now();
    for i in 1..10 {
        s.foreground_at(SECS(i));
    }
    s.clock.set(MS(100));
    s.background(&rec, never(), no_stop()).send(&mb()).unwrap();
    let want: Vec<Duration> = (0..10).map(SECS).collect();
    assert_eq!(s.fg(), want, "foreground unaffected: one per second");
    assert_eq!(rec.sent(), [MS(10_500)]);
}

/// Gaps of at least `quiet` in the foreground stream let background in;
/// a foreground request just behind it waits at most one gap.
#[test]
fn background_fills_gaps_in_foreground_traffic() {
    let s = Setup::new();
    let rec = Recorder::new(&s.clock);
    let bg = s.background(&rec, never(), no_stop());
    s.foreground_now();
    s.foreground_at(SECS(2));
    s.foreground_at(MS(4200));
    s.clock.set(MS(100));
    bg.send(&mb()).unwrap(); // 0 + quiet
    bg.send(&mb()).unwrap(); // the 2 s request went at 2.5: 2.5 + quiet
    s.clock.sleep(MS(300)); // the 4.2 s request arrives
    assert_eq!(rec.sent(), [MS(1500), SECS(4)]);
    assert_eq!(
        s.fg(),
        [Duration::ZERO, MS(2500), SECS(5)],
        "each foreground request waited one gap behind a background one"
    );
}

/// Consecutive background requests count as traffic too: one per `quiet`.
#[test]
fn background_requests_are_spaced_by_quiet() {
    let s = Setup::new();
    let rec = Recorder::new(&s.clock);
    let bg = s.background(&rec, never(), no_stop());
    for _ in 0..4 {
        bg.send(&mb()).unwrap();
    }
    assert_eq!(rec.sent(), [0, 1500, 3000, 4500].map(MS));
}

/// `quiet` and the poll interval are adjustable; a shorter quiet than the
/// gap still obeys the gap.
#[test]
fn quiet_shorter_than_the_gap_still_obeys_pacing() {
    let s = Setup::new();
    let rec = Recorder::new(&s.clock);
    let bg = s
        .background(&rec, never(), no_stop())
        .with_quiet(MS(200))
        .with_poll(MS(100));
    s.foreground_now();
    bg.send(&mb()).unwrap();
    assert_eq!(rec.sent(), [SECS(1)]);
    assert_eq!(s.clock.longest_sleep(), MS(100));
}

/// The stop flag ends a wait (busy or quiet) within one poll, with nothing
/// sent; a flag already set returns without sleeping.
#[test]
fn the_stop_flag_ends_a_wait() {
    // Waiting while busy.
    let s = Setup::new();
    let rec = Recorder::new(&s.clock);
    let stop = no_stop();
    {
        let stop = stop.clone();
        s.clock
            .at(SECS(2), move || stop.store(true, Ordering::SeqCst));
    }
    let err = s.background(&rec, || true, stop).send(&mb()).unwrap_err();
    assert!(matches!(err, TransportError::Stopped), "{err}");
    assert!(rec.sent().is_empty());
    assert_eq!(s.clock.now(), SECS(2));

    // Waiting for quiet.
    let s = Setup::new();
    let rec = Recorder::new(&s.clock);
    s.foreground_now();
    let stop = no_stop();
    {
        let stop = stop.clone();
        s.clock
            .at(MS(600), move || stop.store(true, Ordering::SeqCst));
    }
    let err = s.background(&rec, never(), stop).send(&mb()).unwrap_err();
    assert!(matches!(err, TransportError::Stopped));
    assert!(rec.sent().is_empty());
    assert_eq!(s.clock.now(), MS(750), "the first check after the flag");

    // Already stopped.
    let s = Setup::new();
    let rec = Recorder::new(&s.clock);
    let err = s
        .background(&rec, never(), Arc::new(AtomicBool::new(true)))
        .send(&mb())
        .unwrap_err();
    assert!(matches!(err, TransportError::Stopped));
    assert!(rec.sent().is_empty());
    assert!(s.clock.sleeps.lock().unwrap().is_empty());
}

/// A busy answer to a background request holds the whole host (the server
/// asked), and the background retry still waits for quiet after it.
#[test]
fn a_busy_answer_to_background_holds_the_host_and_retries_quietly() {
    let s = Setup::new();
    let rec = Recorder::new(&s.clock);
    rec.statuses.lock().unwrap().push((503, "5"));
    s.foreground_at(SECS(1));
    let resp = s.background(&rec, never(), no_stop()).send(&mb()).unwrap();
    assert_eq!(resp.status, 200);
    assert_eq!(s.fg(), [SECS(5)], "foreground held by the push-back");
    assert_eq!(rec.sent(), [Duration::ZERO, MS(6500)]);
}

/// With no background user, foreground pacing is what it was: one slot per
/// second, back to back.
#[test]
fn foreground_pacing_is_unchanged_without_background() {
    let s = Setup::new();
    let rec = Recorder::new(&s.clock);
    let paced = Paced::shared(rec.clone(), s.limiter.clone(), RetryPolicy::default());
    for _ in 0..5 {
        paced.send(&mb()).unwrap();
    }
    assert_eq!(rec.sent(), (0..5).map(SECS).collect::<Vec<_>>());
}

/// `Paced::background` shares the foreground's limiter.
#[test]
fn paced_background_shares_the_limiter() {
    let s = Setup::new();
    let rec = Recorder::new(&s.clock);
    let paced = Paced::shared(rec.clone(), s.limiter.clone(), RetryPolicy::default());
    let bg = paced.background(never(), no_stop());
    assert!(Arc::ptr_eq(bg.limiter(), paced.limiter()));
    paced.send(&mb()).unwrap();
    bg.send(&mb()).unwrap();
    paced.send(&mb()).unwrap();
    assert_eq!(rec.sent(), [0, 1500, 2500].map(MS));
}

/// `Limiter::acquire_quiet` on its own: waits for quiet, then takes a slot
/// that paces the next foreground request.
#[test]
fn limiter_acquire_quiet() {
    let s = Setup::new();
    s.foreground_now();
    assert_eq!(s.limiter.try_acquire_quiet(HOST, SECS(2)), Err(SECS(2)));
    assert_eq!(s.limiter.acquire_quiet(HOST, SECS(2)), SECS(2));
    assert_eq!(s.foreground_now(), SECS(3));
    // Hosts are independent.
    assert_eq!(
        s.limiter.try_acquire_quiet("api.acoustid.org", SECS(2)),
        Ok(SECS(3))
    );
}

/// Real time, across threads: a background request waiting on a busy
/// predicate returns promptly once another thread sets its stop flag.
#[test]
fn real_time_stop_from_another_thread() {
    let limiter = Arc::new(Limiter::new(SystemClock, PacingPolicy::default()));
    let rec = Arc::new(Mutex::new(0u32));
    struct Count(Arc<Mutex<u32>>);
    impl Transport for Count {
        fn send(&self, _: &Request) -> Result<Response, TransportError> {
            *self.0.lock().unwrap() += 1;
            Ok(Response {
                status: 200,
                headers: vec![],
                body: vec![],
            })
        }
    }
    let stop = no_stop();
    let bg = BackgroundPaced::new(
        Count(rec.clone()),
        limiter,
        RetryPolicy::default(),
        || true,
        stop.clone(),
    )
    .with_poll(MS(2));
    std::thread::scope(|s| {
        let h = s.spawn(|| {
            let err = bg.send(&mb()).unwrap_err();
            (err, Instant::now())
        });
        std::thread::sleep(MS(5));
        let set_at = Instant::now();
        stop.store(true, Ordering::SeqCst);
        let (err, done) = h.join().unwrap();
        assert!(matches!(err, TransportError::Stopped), "{err}");
        // Generous for a loaded runner; unstopped it would never return.
        assert!(done.saturating_duration_since(set_at) < SECS(2));
    });
    assert_eq!(*rec.lock().unwrap(), 0);
}
