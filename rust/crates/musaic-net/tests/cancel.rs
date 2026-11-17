//! Cancelling a paced request: it stops while it waits for its pacing slot,
//! for a lock another caller is sleeping on, and for a retry delay; it takes
//! no slot, sends nothing more, and surfaces as cancelled rather than as a
//! network failure. The limit itself is never loosened.
//!
//! Nothing here touches the network: transports are in-process stubs.

mod common;

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::ThreadClock;
use musaic_net::{
    ClientError, FailureClass, Limiter, MusicBrainzClient, Paced, PacingPolicy, ReplayTransport,
    Request, Response, RetryPolicy, SystemClock, Transport, TransportError,
};

fn mb() -> Request {
    MusicBrainzClient::<ReplayTransport>::search_request("A", "B")
}

const SECS: fn(u64) -> Duration = Duration::from_secs;

/// Counts requests; sets `flag` after answering `trip_after` of them.
struct Stub {
    status: u16,
    headers: Vec<(String, String)>,
    sent: AtomicU32,
    flag: Arc<AtomicBool>,
    trip_after: u32,
}

impl Stub {
    fn new(
        status: u16,
        retry_after: Option<&str>,
        flag: &Arc<AtomicBool>,
        trip_after: u32,
    ) -> Self {
        Self {
            status,
            headers: retry_after
                .map(|v| vec![("Retry-After".into(), v.into())])
                .unwrap_or_default(),
            sent: AtomicU32::new(0),
            flag: Arc::clone(flag),
            trip_after,
        }
    }
}

impl Transport for Stub {
    fn send(&self, _request: &Request) -> Result<Response, TransportError> {
        let n = self.sent.fetch_add(1, Ordering::SeqCst) + 1;
        if n >= self.trip_after {
            self.flag.store(true, Ordering::SeqCst);
        }
        Ok(Response {
            status: self.status,
            headers: self.headers.clone(),
            body: b"{\"release-groups\": []}".to_vec(),
        })
    }
}

/// Cancelled during a retry delay: the busy answer is the last thing sent,
/// the 30 s wait is not slept, and the error is "cancelled", not "network".
#[test]
fn cancel_during_a_retry_delay_sends_nothing_more() {
    ThreadClock::reset();
    let flag = Arc::new(AtomicBool::new(false));
    let t = Stub::new(503, Some("30"), &flag, 1);
    let paced = Paced::new(&t, ThreadClock).with_cancel(Arc::clone(&flag));
    let err = MusicBrainzClient::new(paced)
        .search_release_groups("A", "B")
        .unwrap_err();
    assert!(err.is_cancelled(), "{err}");
    assert_eq!(err.class(), FailureClass::Cancelled);
    assert!(
        matches!(err, ClientError::Transport(TransportError::Stopped)),
        "{err}"
    );
    assert!(!matches!(err.class(), FailureClass::Network));
    assert_eq!(t.sent.load(Ordering::SeqCst), 1);
    assert!(
        ThreadClock::sleeps().is_empty(),
        "no wait was slept: {:?}",
        ThreadClock::sleeps()
    );
}

/// A cancelled caller takes no slot and moves nothing: the next caller's
/// slot is exactly where it would have been, one gap after the last real
/// request, and a cancelled caller never sends.
#[test]
fn a_cancelled_caller_leaves_the_pacer_as_it_was() {
    ThreadClock::reset();
    let limiter = Arc::new(Limiter::new(ThreadClock, PacingPolicy::default()));
    assert_eq!(limiter.acquire("musicbrainz.org"), Duration::ZERO);
    for _ in 0..3 {
        let err = limiter
            .acquire_cancellable("musicbrainz.org", &|| true)
            .unwrap_err();
        assert!(err.is_cancelled());
    }
    assert!(ThreadClock::sleeps().is_empty());
    // Not cancelled: still waits the one-second gap from the last request.
    let got = limiter
        .acquire_cancellable("musicbrainz.org", &|| false)
        .unwrap();
    assert_eq!(got, SECS(1));

    // The same through a paced transport: the flag is set before the call.
    let flag = Arc::new(AtomicBool::new(true));
    let t = Stub::new(200, None, &flag, u32::MAX);
    let cancelled = Paced::shared(&t, Arc::clone(&limiter), RetryPolicy::default())
        .with_cancel(Arc::clone(&flag));
    assert!(matches!(
        cancelled.send(&mb()),
        Err(TransportError::Stopped)
    ));
    assert_eq!(t.sent.load(Ordering::SeqCst), 0);
    let live = Paced::shared(&t, limiter, RetryPolicy::default());
    live.send(&mb()).unwrap();
    assert_eq!(
        {
            use musaic_net::Clock as _;
            ThreadClock.now()
        },
        SECS(2),
        "the next real request took the slot after the first"
    );
}

/// On the real clock, with a 60 s gap so waiting is the only way out: a
/// caller asleep for its slot and a second one queued behind it each stop
/// within moments of their own flag being set, independently; the other
/// keeps waiting; nothing but the first request is ever sent.
#[test]
fn waiting_callers_stop_promptly_on_their_own_flag() {
    let limiter = Arc::new(Limiter::new(
        SystemClock,
        PacingPolicy {
            min_interval: vec![("musicbrainz.org".into(), SECS(60))],
        },
    ));
    let never = Arc::new(AtomicBool::new(false));
    let t = Arc::new(Stub::new(200, None, &never, u32::MAX));
    // The first request takes the slot at once.
    Paced::shared(Arc::clone(&t), Arc::clone(&limiter), RetryPolicy::default())
        .send(&mb())
        .unwrap();

    let waiter = |flag: &Arc<AtomicBool>| {
        let p = Paced::shared(Arc::clone(&t), Arc::clone(&limiter), RetryPolicy::default())
            .with_cancel(Arc::clone(flag));
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(p.send(&mb()));
        });
        rx
    };
    let (flag_b, flag_c) = (
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
    );
    let b = waiter(&flag_b);
    // Let B reach its sleep and C queue behind it (generous; a late start
    // only makes the stops below easier, never wrong).
    std::thread::sleep(Duration::from_millis(300));
    let c = waiter(&flag_c);
    std::thread::sleep(Duration::from_millis(300));

    let begun = Instant::now();
    flag_c.store(true, Ordering::SeqCst);
    let got = c.recv_timeout(SECS(30)).expect("C stops");
    assert!(matches!(got, Err(TransportError::Stopped)), "{got:?}");
    assert!(
        b.try_recv().is_err(),
        "B is still waiting for its slot, not stopped by C's flag"
    );

    flag_b.store(true, Ordering::SeqCst);
    let got = b.recv_timeout(SECS(30)).expect("B stops");
    assert!(matches!(got, Err(TransportError::Stopped)), "{got:?}");
    assert!(
        begun.elapsed() < SECS(30),
        "both stopped long before the 60 s slot"
    );
    assert_eq!(t.sent.load(Ordering::SeqCst), 1, "only the first was sent");
}

/// The retry delay itself (a real 30 s `Retry-After`) is interrupted
/// by the flag from another thread; the request that got the busy answer is
/// the only one sent.
#[test]
fn a_retry_delay_is_interrupted_from_another_thread() {
    let flag = Arc::new(AtomicBool::new(false));
    let never = Arc::new(AtomicBool::new(false));
    let t = Arc::new(Stub::new(503, Some("30"), &never, u32::MAX));
    let p = Paced::shared(
        Arc::clone(&t),
        Arc::new(Limiter::new(SystemClock, PacingPolicy::default())),
        RetryPolicy::default(),
    )
    .with_cancel(Arc::clone(&flag));
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(MusicBrainzClient::new(p).search_release_groups("A", "B"));
    });
    // Wait for the busy answer, then for the retry to be asleep.
    let deadline = Instant::now() + SECS(30);
    while t.sent.load(Ordering::SeqCst) == 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    std::thread::sleep(Duration::from_millis(300));
    assert!(rx.try_recv().is_err(), "still waiting out the delay");
    flag.store(true, Ordering::SeqCst);
    let err = rx
        .recv_timeout(SECS(20))
        .expect("the delay was interrupted")
        .unwrap_err();
    assert!(err.is_cancelled(), "{err}");
    assert_eq!(t.sent.load(Ordering::SeqCst), 1);
}

/// A request that is not cancelled is untouched by the flag machinery: a
/// transport with a flag that never trips paces exactly as before.
#[test]
fn an_unset_flag_changes_nothing() {
    ThreadClock::reset();
    let flag = Arc::new(AtomicBool::new(false));
    let log = Mutex::new(Vec::new());
    struct Rec<'a>(&'a Mutex<Vec<Duration>>);
    impl Transport for Rec<'_> {
        fn send(&self, _r: &Request) -> Result<Response, TransportError> {
            use musaic_net::Clock as _;
            self.0.lock().unwrap().push(ThreadClock.now());
            Ok(Response {
                status: 200,
                headers: vec![],
                body: b"{\"release-groups\": []}".to_vec(),
            })
        }
    }
    let p = Paced::new(Rec(&log), ThreadClock).with_cancel(flag);
    for _ in 0..3 {
        p.send(&mb()).unwrap();
    }
    assert_eq!(*log.lock().unwrap(), [0, 1, 2].map(SECS));
}

/// A cover download cancelled at shutdown is no failure: it logs nothing
/// above DEBUG (a refused or unreachable download is a WARNING), and says
/// it was cancelled.
#[test]
fn a_cancelled_cover_download_is_not_logged_as_a_failure() {
    struct Stopped;
    impl Transport for Stopped {
        fn send(&self, _r: &Request) -> Result<Response, TransportError> {
            Err(TransportError::Stopped)
        }
    }
    let client = musaic_net::CoverArtClient::new(Stopped);
    let (got, lines) = musaic_log::capture(|| client.download("https://example.com/cover.jpg"));
    let e = got.unwrap_err();
    assert!(e.is_cancelled(), "{e}");
    let loud: Vec<_> = lines
        .iter()
        .filter(|l| l.level != musaic_log::Level::Debug)
        .collect();
    assert!(loud.is_empty(), "{loud:?}");
}
