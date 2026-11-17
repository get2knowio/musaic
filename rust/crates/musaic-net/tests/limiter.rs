//! The shared limiter: one limit across clones,
//! threads and callers, on virtual time (no sleeping) except one short
//! real-time check.

mod common;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::ThreadClock;
use musaic_net::{
    AcoustIdClient, Limiter, LiveTransport, MusicBrainzClient, Paced, PacingPolicy,
    ReplayTransport, Request, Response, RetryPolicy, SystemClock, Transport, TransportError,
    process_limiter,
};

/// Notes the sending thread's virtual time and the host of every request.
#[derive(Default)]
struct Recorder {
    sent: Mutex<Vec<(Duration, String)>>,
}

impl Transport for Recorder {
    fn send(&self, request: &Request) -> Result<Response, TransportError> {
        use musaic_net::Clock as _;
        self.sent
            .lock()
            .unwrap()
            .push((ThreadClock.now(), request.host().to_string()));
        Ok(Response {
            status: 200,
            headers: vec![],
            body: b"{\"release-groups\": []}".to_vec(),
        })
    }
}

impl Recorder {
    fn times(&self, host: &str) -> Vec<Duration> {
        let mut t: Vec<Duration> = self
            .sent
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.1 == host)
            .map(|s| s.0)
            .collect();
        t.sort();
        t
    }
}

fn mb() -> Request {
    MusicBrainzClient::<ReplayTransport>::search_request("A", "B")
}

fn acoustid() -> Request {
    AcoustIdClient::<ReplayTransport>::lookup_request("test-key-not-real", "AQAA", 10)
}

/// Eight threads, each with its own clone of one MusicBrainz client,
/// three searches each. Every request gets its own one-second slot: the 24
/// send times are exactly 0, 1, ..., 23 s, whatever the interleaving.
#[test]
fn musicbrainz_limit_is_shared_by_clones_across_threads() {
    let rec = Recorder::default();
    let client = MusicBrainzClient::new(Paced::new(&rec, ThreadClock));
    std::thread::scope(|s| {
        for _ in 0..8 {
            let c = client.clone();
            s.spawn(move || {
                for _ in 0..3 {
                    c.search_release_groups("A", "B").unwrap();
                }
            });
        }
    });
    let want: Vec<Duration> = (0..24).map(Duration::from_secs).collect();
    assert_eq!(rec.times("musicbrainz.org"), want);
}

/// AcoustID across threads stays at three per second, on its own
/// schedule, while MusicBrainz traffic runs alongside.
#[test]
fn acoustid_limit_is_shared_across_threads_and_independent_of_musicbrainz() {
    let rec = Recorder::default();
    let paced = Paced::new(&rec, ThreadClock);
    std::thread::scope(|s| {
        for i in 0..6 {
            let p = paced.clone();
            s.spawn(move || {
                for _ in 0..2 {
                    p.send(&acoustid()).unwrap();
                }
                if i == 0 {
                    p.send(&mb()).unwrap();
                }
            });
        }
    });
    let a = rec.times("api.acoustid.org");
    assert_eq!(a.len(), 12);
    let want: Vec<Duration> = (0..12).map(|i| Duration::from_millis(334 * i)).collect();
    assert_eq!(a, want);
    for w in a.windows(4) {
        assert!(w[3] - w[0] >= Duration::from_secs(1), "{a:?}");
    }
    assert_eq!(rec.times("musicbrainz.org").len(), 1);
}

/// Two separately built pacers on the same limiter share the limit
/// (what `process_limiter` gives every live client); two separate limiters
/// do not, which is why the host must use the process-wide one.
#[test]
fn separately_built_pacers_share_one_limiter() {
    let rec = Recorder::default();
    let limiter = Arc::new(Limiter::new(ThreadClock, PacingPolicy::default()));
    let a = Paced::shared(&rec, limiter.clone(), RetryPolicy::default());
    let b = Paced::shared(&rec, limiter, RetryPolicy::default());
    std::thread::scope(|s| {
        s.spawn(|| a.send(&mb()).unwrap());
        s.spawn(|| b.send(&mb()).unwrap());
    });
    assert_eq!(
        rec.times("musicbrainz.org"),
        [Duration::ZERO, Duration::from_secs(1)]
    );

    let rec2 = Recorder::default();
    let c = Paced::new(&rec2, ThreadClock);
    let d = Paced::new(&rec2, ThreadClock);
    std::thread::scope(|s| {
        s.spawn(|| c.send(&mb()).unwrap());
        s.spawn(|| d.send(&mb()).unwrap());
    });
    assert_eq!(rec2.times("musicbrainz.org"), [Duration::ZERO; 2]);
}

/// The host app's clients all land on one process-wide limiter.
#[test]
fn live_clients_use_the_process_wide_limiter() {
    let a = musaic_net::live::live_paced(LiveTransport::new());
    let b = musaic_net::live::live_paced(LiveTransport::new());
    assert!(Arc::ptr_eq(a.limiter(), b.limiter()));
    assert!(Arc::ptr_eq(a.limiter(), &process_limiter()));
}

/// Panics on the first request it sees, as a caller that is torn down
/// mid-request would.
struct PanicsOnce {
    inner: Recorder,
    tripped: Mutex<bool>,
}

impl Transport for PanicsOnce {
    fn send(&self, request: &Request) -> Result<Response, TransportError> {
        {
            let mut t = self.tripped.lock().unwrap();
            if !*t {
                *t = true;
                drop(t);
                panic!("caller went away mid-request");
            }
        }
        self.inner.send(request)
    }
}

/// A caller that dies after taking a slot leaves the
/// limiter usable and the spacing intact; its slot is simply unused.
#[test]
fn an_abandoned_request_does_not_break_the_limiter() {
    let t = PanicsOnce {
        inner: Recorder::default(),
        tripped: Mutex::new(false),
    };
    let paced = Paced::new(&t, ThreadClock);
    std::thread::scope(|s| {
        let p = paced.clone();
        let died = s.spawn(move || p.send(&mb())).join();
        assert!(died.is_err());
        for _ in 0..3 {
            let p = paced.clone();
            s.spawn(move || p.send(&mb()).unwrap());
        }
    });
    let times = t.inner.times("musicbrainz.org");
    assert_eq!(
        times,
        [1, 2, 3].map(Duration::from_secs),
        "slot 0 went to the dead caller"
    );
}

/// MusicBrainz pacing on real time: four threads sharing a 200 ms limit never send two
/// requests closer than that apart.
///
/// A stamp is taken after the slot is granted, so a thread preempted
/// between the two (common on a loaded CI runner) shrinks the measured gap
/// to the next send; the 50 ms of slack absorbs that while still proving the
/// limit (unpaced, the gaps would be microseconds).
#[test]
fn real_time_pacing_across_threads() {
    #[derive(Default)]
    struct Stamps(Mutex<Vec<Instant>>);
    impl Transport for Stamps {
        fn send(&self, _: &Request) -> Result<Response, TransportError> {
            self.0.lock().unwrap().push(Instant::now());
            Ok(Response {
                status: 200,
                headers: vec![],
                body: vec![],
            })
        }
    }
    let stamps = Stamps::default();
    let policy = PacingPolicy {
        min_interval: vec![("musicbrainz.org".into(), Duration::from_millis(200))],
    };
    let paced = Paced::with_policies(&stamps, SystemClock, policy, RetryPolicy::default());
    std::thread::scope(|s| {
        for _ in 0..4 {
            let p = paced.clone();
            s.spawn(move || {
                for _ in 0..2 {
                    p.send(&mb()).unwrap();
                }
            });
        }
    });
    let mut t = stamps.0.lock().unwrap().clone();
    t.sort();
    assert_eq!(t.len(), 8);
    for w in t.windows(2) {
        // Slack for timer granularity and preemption (see above).
        assert!(
            w[1] - w[0] >= Duration::from_millis(150),
            "{:?}",
            w[1] - w[0]
        );
    }
}

// ---------------------------------------------------------------------------
// Retry-After capped at 30 s; a busy answer
// pushes back the host's shared limiter so every caller on it waits.
// ---------------------------------------------------------------------------

/// Answers busy to the first `busy` requests (with the given headers), then
/// 200; notes the sending thread's virtual time.
struct BusyFirst {
    busy: Mutex<u32>,
    status: u16,
    headers: Vec<(String, String)>,
    rec: Recorder,
}

impl BusyFirst {
    fn new(n: u32, status: u16, retry_after: Option<&str>) -> Self {
        Self {
            busy: Mutex::new(n),
            status,
            headers: retry_after
                .map(|v| vec![("Retry-After".to_string(), v.to_string())])
                .unwrap_or_default(),
            rec: Recorder::default(),
        }
    }
}

impl Transport for BusyFirst {
    fn send(&self, request: &Request) -> Result<Response, TransportError> {
        let ok = self.rec.send(request)?;
        let mut left = self.busy.lock().unwrap();
        if *left > 0 {
            *left -= 1;
            return Ok(Response {
                status: self.status,
                headers: self.headers.clone(),
                body: b"busy".to_vec(),
            });
        }
        Ok(ok)
    }
}

const SECS: fn(u64) -> Duration = Duration::from_secs;

/// A numeric Retry-After above 30 s waits 30 s and the request
/// goes on (it is not aborted); at or under the cap it is honoured as given;
/// 503 and 429 alike. The tries stay bounded.
#[test]
fn retry_after_is_capped_at_thirty_seconds() {
    for (status, ra, want) in [
        (503, "120", 30),
        (429, "31", 30),
        (503, "30", 30),
        (429, "29", 29),
        (503, "86400", 30),
    ] {
        ThreadClock::reset();
        let t = BusyFirst::new(1, status, Some(ra));
        let mb = MusicBrainzClient::new(Paced::new(&t, ThreadClock));
        mb.search_release_groups("A", "B")
            .unwrap_or_else(|e| panic!("{status} Retry-After {ra}: {e}"));
        assert_eq!(
            t.rec.times("musicbrainz.org"),
            [Duration::ZERO, SECS(want)],
            "{status} Retry-After {ra}"
        );
        assert_eq!(ThreadClock::sleeps(), [SECS(want)], "{status} {ra}");
    }
    // Still bounded: four busy answers of "Retry-After: 600" end as the
    // busy status after 3 waits of 30 s.
    ThreadClock::reset();
    let t = BusyFirst::new(10, 503, Some("600"));
    let err = MusicBrainzClient::new(Paced::new(&t, ThreadClock))
        .search_release_groups("A", "B")
        .unwrap_err();
    assert!(
        matches!(err, musaic_net::ClientError::Status { status: 503, .. }),
        "{err}"
    );
    assert_eq!(
        t.rec.times("musicbrainz.org"),
        [0, 30, 60, 90].map(SECS),
        "four tries, 30 s apart"
    );
}

/// An HTTP date as a server writes it (IMF-fixdate), `secs` after the epoch.
fn http_date(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Civil date from days since the epoch.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    const WEEKDAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    format!(
        "{}, {day:02} {} {year} {:02}:{:02}:{:02} GMT",
        WEEKDAYS[(days % 7) as usize],
        MONTHS[month as usize - 1],
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// An HTTP-date Retry-After is honoured like the numeric form: a date a
/// few seconds ahead is the wait (to the second the wall clock moved while
/// the test ran); a date far ahead waits the 30 s cap and the request goes
/// on; a date already past, or one that is not a date, falls back to our own
/// backoff.
#[test]
fn http_date_retry_after_is_honoured() {
    // 12 s ahead: the second the wall clock ticks over between building and
    // reading the header can shave a second or two, never add one.
    ThreadClock::reset();
    let t = BusyFirst::new(1, 503, Some(&http_date(unix_now() + 12)));
    MusicBrainzClient::new(Paced::new(&t, ThreadClock))
        .search_release_groups("A", "B")
        .unwrap();
    let sent = t.rec.times("musicbrainz.org");
    assert_eq!(sent.len(), 2, "{sent:?}");
    assert!(
        (SECS(9)..=SECS(12)).contains(&sent[1]),
        "waited {:?} for a date 12 s ahead",
        sent[1]
    );

    // Far ahead: capped, not stalled for years.
    ThreadClock::reset();
    let t = BusyFirst::new(1, 429, Some("Wed, 21 Oct 2099 07:28:00 GMT"));
    MusicBrainzClient::new(Paced::new(&t, ThreadClock))
        .search_release_groups("A", "B")
        .unwrap();
    assert_eq!(t.rec.times("musicbrainz.org"), [0, 30].map(SECS));

    // Past, or not a date at all: our backoff (1 s, then 2 s).
    for ra in ["Sun, 06 Nov 1994 08:49:37 GMT", "next Tuesday", "-5", ""] {
        ThreadClock::reset();
        let t = BusyFirst::new(2, 503, Some(ra));
        MusicBrainzClient::new(Paced::new(&t, ThreadClock))
            .search_release_groups("A", "B")
            .unwrap();
        assert_eq!(
            t.rec.times("musicbrainz.org"),
            [0, 1, 3].map(SECS),
            "{ra:?}"
        );
    }
}

/// A numeric figure too large for any integer waits the cap rather than
/// being ignored.
#[test]
fn an_enormous_numeric_retry_after_waits_the_cap() {
    ThreadClock::reset();
    let t = BusyFirst::new(1, 503, Some("99999999999999999999999999"));
    MusicBrainzClient::new(Paced::new(&t, ThreadClock))
        .search_release_groups("A", "B")
        .unwrap();
    assert_eq!(t.rec.times("musicbrainz.org"), [0, 30].map(SECS));
}

/// A busy answer to one caller holds every caller on that host.
/// Caller A (one try only, so it does not retry) gets 503 with
/// Retry-After 10 at t=0. Seven callers then start on other threads: none
/// sends before t=10, and they still leave one second apart (no thundering
/// herd when the wait ends): exactly 10..16 s. Before the decision they
/// went at 1..7 s. Another host (AcoustID) is not held.
#[test]
fn a_busy_answer_holds_every_caller_on_that_host() {
    let t = BusyFirst::new(1, 503, Some("10"));
    let limiter = Arc::new(Limiter::new(ThreadClock, PacingPolicy::default()));
    let one_try = RetryPolicy {
        max_attempts: 1,
        ..RetryPolicy::default()
    };
    let a = Paced::shared(&t, limiter.clone(), one_try);
    std::thread::scope(|s| {
        let r = s.spawn(|| a.send(&mb()).unwrap().status).join().unwrap();
        assert_eq!(r, 503, "A's answer is returned after its one try");
    });
    let others = Paced::shared(&t, limiter, RetryPolicy::default());
    std::thread::scope(|s| {
        for _ in 0..7 {
            let p = others.clone();
            s.spawn(move || p.send(&mb()).unwrap());
        }
        let p = others.clone();
        s.spawn(move || p.send(&acoustid()).unwrap());
    });
    let want: Vec<Duration> = std::iter::once(0).chain(10..17).map(SECS).collect();
    assert_eq!(t.rec.times("musicbrainz.org"), want);
    assert_eq!(t.rec.times("api.acoustid.org"), [Duration::ZERO]);
}

/// A busy answer for an unpaced host (the CAA) holds it too.
#[test]
fn a_busy_answer_holds_an_unpaced_host() {
    let t = BusyFirst::new(1, 503, Some("5"));
    let limiter = Arc::new(Limiter::new(ThreadClock, PacingPolicy::default()));
    let one_try = RetryPolicy {
        max_attempts: 1,
        ..RetryPolicy::default()
    };
    let head = musaic_net::CoverArtClient::<ReplayTransport>::front_request("x");
    let a = Paced::shared(&t, limiter.clone(), one_try);
    std::thread::scope(|s| {
        s.spawn(|| a.send(&head).unwrap());
    });
    let b = Paced::shared(&t, limiter, RetryPolicy::default());
    std::thread::scope(|s| {
        s.spawn(|| b.send(&head).unwrap());
    });
    assert_eq!(
        t.rec.times("coverartarchive.org"),
        [Duration::ZERO, SECS(5)]
    );
}

/// busy answers on real time: caller B is already asleep waiting for its slot
/// when caller A's busy answer arrives. B must see the push-back when it
/// wakes and keep waiting, not send at its old slot. A's own retry waits
/// too, and the two still leave one gap apart.
///
/// Timing is chosen for loaded machines: the busy answer comes ~30 ms after
/// A's send, B's slot is 300 ms after it and the push-back 1 s after the busy
/// answer, so scheduling delays of a few hundred ms cannot reorder them. The
/// transport stamps the moment it returns the busy answer instead of
/// assuming the 30 ms sleep took 30 ms.
#[test]
fn a_caller_already_waiting_sees_a_later_push_back() {
    use std::sync::mpsc;
    struct Slow {
        first: Mutex<Option<mpsc::Sender<()>>>,
        stamps: Mutex<Vec<Instant>>,
        busy_at: Mutex<Option<Instant>>,
    }
    impl Transport for Slow {
        fn send(&self, _: &Request) -> Result<Response, TransportError> {
            self.stamps.lock().unwrap().push(Instant::now());
            if let Some(tx) = self.first.lock().unwrap().take() {
                tx.send(()).unwrap();
                // B is now inside acquire, asleep until the 300 ms slot.
                std::thread::sleep(Duration::from_millis(30));
                *self.busy_at.lock().unwrap() = Some(Instant::now());
                return Ok(Response {
                    status: 503,
                    headers: vec![],
                    body: vec![],
                });
            }
            Ok(Response {
                status: 200,
                headers: vec![],
                body: vec![],
            })
        }
    }
    let (tx, rx) = mpsc::channel();
    let t = Slow {
        first: Mutex::new(Some(tx)),
        stamps: Mutex::new(Vec::new()),
        busy_at: Mutex::new(None),
    };
    let policy = PacingPolicy {
        min_interval: vec![("musicbrainz.org".into(), Duration::from_millis(300))],
    };
    let retry = RetryPolicy {
        max_attempts: 2,
        base_backoff: Duration::from_millis(1000),
        max_backoff: Duration::from_millis(1000),
    };
    let paced = Paced::with_policies(&t, SystemClock, policy, retry);
    std::thread::scope(|s| {
        let a = paced.clone();
        s.spawn(move || assert_eq!(a.send(&mb()).unwrap().status, 200));
        rx.recv().unwrap();
        let b = paced.clone();
        s.spawn(move || assert_eq!(b.send(&mb()).unwrap().status, 200));
    });
    let st = t.stamps.lock().unwrap().clone();
    assert_eq!(st.len(), 3);
    let busy_at = t.busy_at.lock().unwrap().expect("a busy answer");
    // Without the push-back B would have sent at its 300 ms slot, ~270 ms
    // after the busy answer; with it, both later sends wait the full 1 s.
    for later in &st[1..] {
        assert!(
            *later >= busy_at + Duration::from_millis(990),
            "sent {:?} after the busy answer",
            later.saturating_duration_since(busy_at)
        );
    }
    let mut after = st[1..].to_vec();
    after.sort();
    // One 300 ms gap apart; slack for preemption between slot and stamp.
    assert!(
        after[1] - after[0] >= Duration::from_millis(250),
        "{:?}",
        after[1] - after[0]
    );
}
