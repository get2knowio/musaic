//! Shared helpers for the BPM tests: repo paths, golden loading, fixture
//! presence checks and a small parallel map across files.
#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A test-data JSON file: public for the synthetic library, private (Tier 2)
/// for the real-audio set.
pub fn read_json(rel: &str, private: bool) -> serde_json::Value {
    if private {
        musaic_testdata::read_json(musaic_testdata::private_required(rel))
    } else {
        musaic_testdata::public_json(rel)
    }
}

/// Where the private half of the stage golden lives (real-library files).
pub const PRIVATE_STAGES: &str = "crates/musaic-bpm/bpm-stages.json";

/// The stage golden: the in-crate public half (synthetic files), merged with
/// the private half (real-library files) when that is available. The second
/// value says whether it was.
pub fn stage_golden() -> (serde_json::Value, bool) {
    let public: serde_json::Value =
        serde_json::from_str(include_str!("../data/bpm-stages.json")).expect("bpm-stages.json");
    // The private files are measured from the real-audio set itself.
    match musaic_testdata::private_json(PRIVATE_STAGES)
        .filter(|_| musaic_testdata::real_library().is_some())
    {
        Some(private) => (musaic_testdata::merge(public, private), true),
        None => (public, false),
    }
}

/// Library root for a golden's `library` kind: the generated synthetic
/// library (panics with the generator command when absent) or the private
/// real-audio set (the calling test is gated on it).
pub fn library(kind: &str) -> PathBuf {
    match kind {
        "synthetic" => musaic_testdata::synthetic_library(),
        "real" => musaic_testdata::private_required("real-library/library"),
        other => panic!("unknown library kind {other}"),
    }
}

/// Map `f` over `items` on all cores, preserving order.
pub fn par_map<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let next = AtomicUsize::new(0);
    let out: Mutex<Vec<Option<R>>> = Mutex::new((0..items.len()).map(|_| None).collect());
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= items.len() {
                        break;
                    }
                    let r = f(&items[i]);
                    out.lock().unwrap()[i] = Some(r);
                }
            });
        }
    });
    out.into_inner()
        .unwrap()
        .into_iter()
        .map(Option::unwrap)
        .collect()
}

pub fn f64s(v: &serde_json::Value) -> Vec<f64> {
    v.as_array()
        .expect("array")
        .iter()
        .map(|x| x.as_f64().unwrap_or(f64::NEG_INFINITY))
        .collect()
}
