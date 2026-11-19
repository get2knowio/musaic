//! `FingerprintPool::pause`: while a pause is held no pass starts (a
//! multi-disc conversion folds album rows a pass could still write under);
//! a pass asked for meanwhile starts once the last pause ends.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use musaic_services::fingerprint_pool::{
    AlbumJob, FingerprintPool, PoolDeps, PoolStatus, RealSleeper,
};

fn deps(opened: Arc<AtomicUsize>) -> PoolDeps {
    PoolDeps {
        // Counts the passes that started; no database, so each ends at once.
        open_db: Arc::new(move || {
            opened.fetch_add(1, Ordering::SeqCst);
            Err("no database in this test".into())
        }),
        availability: Arc::new(|| Ok(())),
        job: AlbumJob::new(|_, _, _, _| Ok(())),
        clock: Arc::new(|| "2026-01-01T00:00:00Z".to_string()),
        sleeper: Arc::new(RealSleeper),
        fingerprint_impl: "test".into(),
    }
}

#[test]
fn no_pass_starts_while_paused_and_the_one_asked_for_starts_after() {
    let pool = FingerprintPool::new();
    let opened = Arc::new(AtomicUsize::new(0));
    let first = pool.pause();
    let second = pool.pause();
    assert!(pool.is_paused());
    let s = pool.warm_library("/lib", deps(Arc::clone(&opened)));
    assert_eq!(s.status, PoolStatus::Idle);
    assert!(!pool.is_running());
    drop(first);
    // Still paused by the second hold.
    assert_eq!(pool.state().status, PoolStatus::Idle);
    drop(second);
    assert!(!pool.is_paused());
    pool.join();
    assert_eq!(opened.load(Ordering::SeqCst), 1);
    assert_eq!(pool.state().status, PoolStatus::Done);
}

#[test]
fn a_pause_with_nothing_asked_for_starts_nothing() {
    let pool = FingerprintPool::new();
    let opened = Arc::new(AtomicUsize::new(0));
    drop(pool.pause());
    pool.join();
    assert_eq!(opened.load(Ordering::SeqCst), 0);
    assert_eq!(pool.state().status, PoolStatus::Idle);
    // And the pool works as before once no pause is held.
    pool.warm_library("/lib", deps(Arc::clone(&opened)));
    pool.join();
    assert_eq!(opened.load(Ordering::SeqCst), 1);
}
