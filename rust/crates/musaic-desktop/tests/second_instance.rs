//! What a second launch does, without a window.
//!
//! The boot test (`tests/boot.rs`, ignored: it opens a hidden window)
//! drives the whole binary. This one runs on every `cargo test`: the first
//! instance's lock and info file plus the real server and gate
//! (`backend::start`, the same stack the window uses), and the second
//! launch's half, `instance::ask_running_to_focus`, over TCP. It is why
//! `tauri-plugin-single-instance` is not used: a second launch on the same
//! config dir already finds the first, has it focus its window, and exits;
//! a second launch on another config dir is independent.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use musaic_desktop::backend;
use musaic_desktop::gate::{self, DialogKind, Hooks};
use musaic_desktop::instance::{self, Acquired};
use tokio::sync::oneshot;

#[derive(Default)]
struct Focus(AtomicUsize);

impl Hooks for Focus {
    fn focus(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
    fn dialog(&self, _kind: DialogKind, _message: String) -> oneshot::Receiver<bool> {
        let (tx, rx) = oneshot::channel();
        let _ = tx.send(false);
        rx
    }
}

fn state(dir: &std::path::Path) -> musaic_api::AppState {
    musaic_api::AppState::builder(dir.to_path_buf())
        .secrets(Arc::new(musaic_secrets::MemoryStore::new()))
        .network(musaic_api::Network::Offline)
        .build()
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn second_launch_focuses_the_first_and_fails_cleanly_when_it_cannot() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().to_path_buf();
    let Acquired::First(lock) = instance::acquire(&config).unwrap() else {
        panic!("first acquire must win");
    };
    let hooks = Arc::new(Focus::default());
    let secret = gate::random_token().unwrap();
    let b = backend::start(state(&config), 0, secret.clone(), hooks.clone())
        .await
        .unwrap();
    lock.publish(b.addr.port(), &secret).unwrap();

    // The second launch: lock refused, focus asked, first window focused.
    assert!(matches!(
        instance::acquire(&config).unwrap(),
        Acquired::Running
    ));
    let c = config.clone();
    tokio::task::spawn_blocking(move || instance::ask_running_to_focus(&c, Duration::from_secs(5)))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(hooks.0.load(Ordering::SeqCst), 1);

    // An info file with the wrong secret (another process wrote it): the
    // first refuses, the second reports it instead of claiming success.
    lock.publish(b.addr.port(), "not-the-secret").unwrap();
    let c = config.clone();
    let err = tokio::task::spawn_blocking(move || {
        instance::ask_running_to_focus(&c, Duration::from_millis(500))
    })
    .await
    .unwrap()
    .unwrap_err();
    assert!(err.contains("403"), "{err}");
    assert_eq!(hooks.0.load(Ordering::SeqCst), 1);

    // The first stops (port closed) but the info file lingers: the second
    // gives up after its timeout with an error, not a hang.
    lock.publish(b.addr.port(), &secret).unwrap();
    let _ = b.stop().await;
    let c = config.clone();
    let t = std::time::Instant::now();
    let r = tokio::task::spawn_blocking(move || {
        instance::ask_running_to_focus(&c, Duration::from_millis(600))
    })
    .await
    .unwrap();
    assert!(r.is_err());
    assert!(t.elapsed() < Duration::from_secs(10));

    // Another config dir is another instance.
    let other = tempfile::tempdir().unwrap();
    assert!(matches!(
        instance::acquire(other.path()).unwrap(),
        Acquired::First(_)
    ));
    lock.retire();
}
