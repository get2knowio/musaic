//! The scan service against testdata/golden/services/scan.json, recorded by
//! a generator that drove the Python app's own `services.scan` (start_scan /
//! subscribe / get_state) in one asyncio loop. The Python app could not
//! cancel a scan, so no recording has one; cancelling is pinned by
//! `tests/scan_cancel.rs`.
//!
//! Each story replays the same library changes between scans. Per scan
//! compared: the state `start_scan` returned, the state before and after, the
//! whole event buffer (seq, name, payload), what a subscriber from seq 0 got,
//! the bus events in order, whether the post-scan hook ran (the Python app's
//! `fingerprint_pool.stop()` + `warm_library(library_dir)`, recorded as those
//! two calls; here one `after_scan` hook), and the database rows.
//!
//! Deliberate change, as enumerated deltas on
//! the Python app's record ([`deltas`]): the sanity check counts only albums that
//! were Present before the scan, and `marked_missing` / its counts announce
//! only Present -> Missing transitions. Every other difference fails, and so
//! does a delta whose old value is not in the record.
//!
//! Deliberate change, as enumerated deltas too ([`stopped_deltas`], issue
//! #193): a scan that saved nothing (aborted by the sanity check, or
//! failed) ends with status `aborted` / `error` and publishes
//! `LibraryScanStopped(reason, album_count)` instead of
//! `LibraryScanCompleted`, and an aborted scan no longer kicks off the
//! fingerprint pool. The Python app reported an aborted scan as `done`,
//! announced completion on every path and warmed the pool after an abort.
//!
//! Deliberate change, applied to the whole record ([`in_scan_order`]):
//! albums are discovered in name order, not in the filesystem's listing
//! order (APFS's, when the golden was recorded). The record's album events,
//! and everything ordered by album row (rows, `marked_missing`, the
//! Missing / returned bus events), are put in the order this build produces
//! on every filesystem; their contents are compared unchanged.
//!
//! Injected failure (not a delta): the Python app's error story patched
//! `cache.save_snapshot` to raise "injected save failure"; here the worker's
//! database is opened read-only so the save fails. Only the failure's text
//! (`error.detail`, `state.error`) is masked on both sides.

mod common;

use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use common::*;
use musaic_events::{Bus, Event};
use musaic_scan::TrackOrder;
use musaic_services::scan::{Poll, ScanDeps, ScanService, StartScan};
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};

fn golden() -> &'static Value {
    static G: OnceLock<Value> = OnceLock::new();
    G.get_or_init(|| load_golden("scan.json"))
}

fn album_columns() -> Vec<String> {
    golden()["album_columns"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c.as_str().unwrap().to_string())
        .collect()
}

struct Story {
    _td: tempfile::TempDir,
    lib: PathBuf,
    db: PathBuf,
    norm: Norm,
    bus: Arc<Bus>,
    log: Arc<Mutex<Vec<Event>>>,
    hook_calls: Arc<Mutex<Vec<String>>>,
}

impl Story {
    fn deps(
        &self,
        read_only_db: bool,
        on_album: Option<musaic_services::scan::AlbumHook>,
    ) -> ScanDeps {
        let db = self.db.clone();
        let calls = Arc::clone(&self.hook_calls);
        ScanDeps {
            open_db: Arc::new(move || {
                let c = if read_only_db {
                    Connection::open_with_flags(&db, OpenFlags::SQLITE_OPEN_READ_ONLY)
                } else {
                    Connection::open(&db)
                }
                .map_err(|e| e.to_string())?;
                c.pragma_update(None, "foreign_keys", "ON")
                    .map_err(|e| e.to_string())?;
                Ok(c)
            }),
            bus: Arc::clone(&self.bus),
            clock: Arc::new(|| "2030-01-01T00:00:00Z".to_string()),
            track_order: TrackOrder::DiscTrack,
            after_scan: Some(Arc::new(move |lib: &str| {
                calls.lock().unwrap().push(lib.to_string())
            })),
            on_album,
            on_worker_done: None,
            log: None,
        }
    }

    fn take_bus(&self) -> Vec<Value> {
        let mut l = self.log.lock().unwrap();
        let out = l.iter().map(|e| self.norm.value(&event_json(e))).collect();
        l.clear();
        out
    }

    fn take_hook_calls(&self) -> Value {
        let mut c = self.hook_calls.lock().unwrap();
        let out: Vec<Value> = c
            .iter()
            .flat_map(|lib| vec![json!(["stop"]), json!(["warm_library", self.norm.str(lib)])])
            .collect();
        c.clear();
        Value::Array(out)
    }
}

fn buffer_json(svc: &ScanService, norm: &Norm) -> Value {
    Value::Array(
        svc.buffer()
            .iter()
            .map(|e| norm.value(&json!([e.seq, e.name, e.data.to_value()])))
            .collect(),
    )
}

/// The generator masks nothing in the error story; the injected failure's
/// text is the only thing masked here (both sides).
fn mask_injected(mut v: Value, injected: bool) -> Value {
    if !injected {
        return v;
    }
    if let Some(arr) = v.as_array_mut() {
        for e in arr.iter_mut() {
            if e[1] == "error" {
                e[2]["detail"] = json!("<INJECTED>");
            }
        }
    }
    if v.get("error").is_some_and(|e| !e.is_null()) {
        v["error"] = json!("<INJECTED>");
    }
    v
}

fn drain(svc: &ScanService, from: u64, norm: &Norm) -> Value {
    Value::Array(
        svc.subscribe(from)
            .map(|e| norm.value(&json!([e.seq, e.name, e.data.to_value()])))
            .collect(),
    )
}

/// One edit of a recorded scan step (JSON pointers into the step record).
enum Change {
    /// Precondition: the record holds this value at the path.
    Expect(String, Value),
    /// Replace the old value (asserted) by the new one.
    Set(String, Value, Value),
    /// Remove the array element at the path (asserted to be the value).
    Remove(String, Value),
    /// Insert into the array at the path, before the index.
    Insert(String, usize, Value),
    /// Append to the array at the path.
    Push(String, Value),
}

fn set(path: &str, old: Value, new: Value) -> Change {
    Change::Set(path.to_string(), old, new)
}

struct Delta {
    story: &'static str,
    step: usize,
    why: &'static str,
    changes: Vec<Change>,
}

impl Delta {
    fn new(story: &'static str, step: usize, changes: Vec<Change>) -> Delta {
        Delta {
            story,
            step,
            why: "present-only-sanity",
            changes,
        }
    }

    /// The same edit of the event buffer and of what a subscriber from 0
    /// got (the two are identical in every recorded scan).
    fn on_events(&mut self, edit: impl Fn(&str) -> Vec<Change>) {
        for key in ["/buffer", "/subscriber"] {
            self.changes.extend(edit(key));
        }
    }

    /// `album` goes Missing at this step (row `idx` of the id-ordered dump).
    fn now_missing(&mut self, idx: usize, n: u32) {
        self.changes.extend([
            Change::Expect(format!("/db/albums/{idx}/id"), album(n)["id"].clone()),
            Change::Set(
                format!("/db/albums/{idx}/missing_since"),
                Value::Null,
                json!("<NOW>"),
            ),
        ]);
    }
}

fn album(n: u32) -> Value {
    json!({"id": format!("<ID:Artist {n:02} - Album {n:02} [FLAC]>"),
        "path": format!("<LIB>/Artist {n:02} - Album {n:02} [FLAC]")})
}

fn done(seq: u64, count: u64, marked: u64, newly: u64) -> Value {
    json!([seq, "done", {"count": count, "marked_missing_count": marked,
        "newly_missing_count": newly, "returned_count": 0, "scanned_at": "<NOW>"}])
}

fn marked(seq: u64, n: u32) -> Value {
    json!([seq, "marked_missing", album(n)])
}

fn marked_missing_event(n: u32) -> Value {
    json!({"type": "AlbumMarkedMissing", "album_id": album(n)["id"], "missing_since": "<NOW>"})
}

/// The Python app's `aborted` entry (format of services/scan.py, as recorded).
fn aborted(seq: u64, vanish: u64, cached: u64) -> Value {
    let ratio = vanish as f64 / cached as f64;
    json!([seq, "aborted", {"cached_count": cached,
        "detail": format!("Scan aborted: {vanish} of {cached} cached album(s) ({:.0}%) appear to \
be missing from disk. This looks like a drive outage or a mount problem rather than real \
deletions. No changes were written. Check that <LIB> is mounted and readable, then re-run the \
scan.", ratio * 100.0),
        "reason": "vanish_ratio_exceeded", "threshold": 0.3, "vanish_count": vanish,
        "vanish_ratio": ratio}])
}

/// The deltas of the Present-only sanity check. Story step indices count every op
/// (add / remove too). Database rows are listed in id order.
///
/// small (albums 01-04; 02 removed at step 4 and Missing from step 5):
/// - step 6 rescan-while-missing: the Python app announced 02 again
///   (marked_missing, marked_missing_count 1, removed_count 1, no
///   transition); now nothing is announced.
/// - step 8 album-returns starts from that state (removed_count 0).
///
/// ten (01, 05, 09 Missing from step 14; first-scan order 05 10 03 08 01
/// 07 02 04 06 09; 10 is row 1):
/// - step 16 one-more-vanishes (10 gone): the Python app aborted on 4 of 10;
///   now 1 of the 7 Present albums commits (7 is also below the 10-album
///   minimum): 10 goes Missing, announced once.
/// - step 17 rescan-after-abort: the Python app aborted again; now nothing
///   vanishes among the 6 Present albums and nothing is announced; 10 stays
///   Missing.
///
/// fifteen (story added to the generator for this decision; 01-04 Missing
/// from step 5; first-scan order 05 10 03 08 01 12 07 14 02 11 04 15 06 13
/// 09; rows 0, 6, 12 are 05, 07, 06):
/// - step 10 four-more-vanish (05-08 gone): the Python app aborted on 8 of 15;
///   now 4 of the 11 Present albums (36%) still abort, with those numbers.
/// - step 12 three-more-vanish (08 back): the Python app aborted on 7 of 15; now
///   3 of 11 Present (27%) commit: 05, 07, 06 go Missing (rowid order),
///   announced once each.
/// - step 13 rescan: the Python app aborted again; now nothing vanishes among the
///   8 Present albums and nothing is announced; and 08, whose return step 12
///   now committed, is read from the cache instead of afresh.
fn deltas() -> Vec<Delta> {
    let mut small6 = Delta::new(
        "small",
        6,
        vec![set("/state_after/removed_count", json!(1), json!(0))],
    );
    small6.on_events(|k| {
        vec![
            Change::Remove(format!("{k}/7"), marked(7, 2)),
            Change::Set(format!("{k}/7"), done(8, 3, 1, 0), done(7, 3, 0, 0)),
        ]
    });
    let small8 = Delta::new(
        "small",
        8,
        vec![set("/state_before/removed_count", json!(1), json!(0))],
    );

    let mut ten16 = Delta::new(
        "ten",
        16,
        vec![
            set("/state_after/removed_count", json!(0), json!(1)),
            Change::Insert("/bus_events".into(), 0, marked_missing_event(10)),
        ],
    );
    ten16.now_missing(1, 10);
    ten16.on_events(|k| {
        vec![
            Change::Set(format!("{k}/13"), aborted(13, 4, 10), marked(13, 10)),
            Change::Push(k.to_string(), done(14, 6, 1, 1)),
        ]
    });
    let mut ten17 = Delta::new(
        "ten",
        17,
        vec![set("/state_before/removed_count", json!(0), json!(1))],
    );
    ten17.now_missing(1, 10);
    ten17.on_events(|k| {
        vec![Change::Set(
            format!("{k}/13"),
            aborted(13, 4, 10),
            done(13, 6, 0, 0),
        )]
    });

    let mut f10 = Delta::new("fifteen", 10, Vec::new());
    f10.on_events(|k| {
        vec![Change::Set(
            format!("{k}/15"),
            aborted(15, 8, 15),
            aborted(15, 4, 11),
        )]
    });
    let mut f12 = Delta::new(
        "fifteen",
        12,
        vec![set("/state_after/removed_count", json!(0), json!(3))],
    );
    let mut f13 = Delta::new(
        "fifteen",
        13,
        vec![set("/state_before/removed_count", json!(0), json!(3))],
    );
    for (i, (row, n)) in [(0, 5), (6, 7), (12, 6)].into_iter().enumerate() {
        f12.changes.push(Change::Insert(
            "/bus_events".into(),
            i,
            marked_missing_event(n),
        ));
        f12.now_missing(row, n);
        f13.now_missing(row, n);
    }
    f12.on_events(|k| {
        vec![
            Change::Set(format!("{k}/17"), aborted(17, 7, 15), marked(17, 5)),
            Change::Push(k.to_string(), marked(18, 7)),
            Change::Push(k.to_string(), marked(19, 6)),
            Change::Push(k.to_string(), done(20, 8, 3, 3)),
        ]
    });
    // Album 08 came back at step 12. The Python app never committed that scan,
    // so its row kept the old mtime and the rescan read 08 again ("fresh",
    // scanner dict); now step 12 committed, so 08 is "cached" (the row's
    // dict). New value: the Python app's own cached entry for 08 at step 5.
    let fresh_08 = story("fifteen")["steps"][13]["buffer"][3].clone();
    let mut cached_08 = story("fifteen")["steps"][5]["buffer"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e[1] == "album" && e[2]["id"] == album(8)["id"])
        .unwrap()
        .clone();
    cached_08[0] = json!(3);
    f13.on_events(|k| {
        vec![
            Change::Expect(format!("{k}/3/2/id"), album(8)["id"].clone()),
            Change::Expect(format!("{k}/3/2/_status"), json!("fresh")),
            Change::Set(format!("{k}/3"), fresh_08.clone(), cached_08.clone()),
            Change::Set(format!("{k}/17"), aborted(17, 7, 15), done(17, 8, 0, 0)),
        ]
    });
    // Story sidecar-return: album 01 returns at step 6 after its tag
    // rows were deleted while it was Missing (step 3). The Python app imported its
    // sidecar again; now only album 03, whose row this scan inserts, imports.
    // Rows sorted by (album, filename, tag): 01's two come first.
    let side = |n: u32, f: &str, t: &str| json!({"album_id": album(n)["id"], "filename": f, "tag": t, "created_at": "<NOW>"});
    let sidecar6 = Delta {
        story: "sidecar-return",
        step: 6,
        why: "sidecar-import-new-only",
        changes: vec![
            Change::Remove(
                "/db/track_tags/0".into(),
                side(1, "01 - One.flac", "Morning"),
            ),
            Change::Remove(
                "/db/track_tags/0".into(),
                side(1, "02 - Two.flac", "Evening"),
            ),
            Change::Expect(
                "/db/track_tags/0".into(),
                side(3, "01 - One.flac", "Morning"),
            ),
        ],
    };
    let mut all = vec![small6, small8, ten16, ten17, f10, f12, f13, sidecar6];
    all.extend(stopped_deltas());
    all
}

/// The recorded `LibraryScanCompleted` of a scan that saved nothing.
fn completed(album_count: u64) -> Value {
    json!({"type": "LibraryScanCompleted", "album_count": album_count})
}

/// What replaces it (deliberate change, issue #193).
fn stopped(reason: &str, album_count: u64) -> Value {
    json!({"type": "LibraryScanStopped", "reason": reason, "album_count": album_count})
}

/// The recorded post-scan hook calls (`fingerprint_pool.stop()` +
/// `warm_library`).
fn pool_kickoff() -> Value {
    json!([["stop"], ["warm_library", "<LIB>"]])
}

/// The scans that save nothing, and the state the next scan of the story
/// starts from (issue #193):
///
/// - ten step 5 four-of-ten-vanish (aborted, 6 albums read): status
///   `aborted`, `LibraryScanStopped(vanish_ratio_exceeded, 6)`, no pool
///   kick-off; step 10 they-return starts from `aborted`. (Steps 16 and 17
///   aborted in the Python app too, but they commit now, see [`deltas`].)
/// - fifteen step 10 four-more-vanish (still aborted, 7 albums read): the
///   same; step 12 three-more-vanish starts from `aborted`.
/// - error step 0 save-fails (1 album read): `LibraryScanStopped(error, 1)`
///   (status `error` and no pool kick-off were already the Python app's).
fn stopped_deltas() -> Vec<Delta> {
    let aborted_scan = |story: &'static str, step: usize, read: u64| Delta {
        story,
        step,
        why: "stopped-scan-no-completion",
        changes: vec![
            set("/state_after/status", json!("done"), json!("aborted")),
            set(
                "/bus_events/0",
                completed(read),
                stopped("vanish_ratio_exceeded", read),
            ),
            set("/fingerprint_pool_calls", pool_kickoff(), json!([])),
        ],
    };
    let after_abort = |story: &'static str, step: usize| Delta {
        story,
        step,
        why: "stopped-scan-no-completion",
        changes: vec![set("/state_before/status", json!("done"), json!("aborted"))],
    };
    vec![
        aborted_scan("ten", 5, 6),
        after_abort("ten", 10),
        aborted_scan("fifteen", 10, 7),
        after_abort("fifteen", 12),
        Delta {
            story: "error",
            step: 0,
            why: "stopped-scan-no-completion",
            changes: vec![set("/bus_events/0", completed(1), stopped("error", 1))],
        },
    ]
}

/// The record of one scan step in this build's scan order (see the module
/// docs). `rows` is the album rows' insertion order so far (normalised
/// paths): albums new in this step are stored in name order after the ones
/// already stored, and rowid order is what `marked_missing`, the bus events
/// of Missing / returning albums and the album rows follow.
fn in_scan_order(step: &mut Value, rows: &mut Vec<String>) {
    let path_of = |v: &Value| v["path"].as_str().unwrap().to_string();
    if let Some(albums) = step.pointer("/db/albums").and_then(Value::as_array) {
        let mut new: Vec<String> = albums
            .iter()
            .map(path_of)
            .filter(|p| !rows.contains(p))
            .collect();
        new.sort();
        rows.extend(new);
    }
    let rank = |p: &str| {
        rows.iter()
            .position(|r| r == p)
            .unwrap_or_else(|| panic!("no album row for {p}"))
    };
    if let Some(albums) = step.pointer_mut("/db/albums").and_then(Value::as_array_mut) {
        albums.sort_by_key(|a| rank(a["path"].as_str().unwrap()));
    }
    // seq -> payload in this build's order, from the full buffer.
    let mut by_seq: std::collections::HashMap<u64, Value> = std::collections::HashMap::new();
    if let Some(buffer) = step["buffer"].as_array() {
        for (name, key) in [("album", None), ("marked_missing", Some(()))] {
            let slots: Vec<&Value> = buffer.iter().filter(|e| e[1] == name).collect();
            let mut payloads: Vec<Value> = slots.iter().map(|e| e[2].clone()).collect();
            match key {
                None => payloads.sort_by_key(path_of),
                Some(()) => payloads.sort_by_key(|d| rank(d["path"].as_str().unwrap())),
            }
            for (e, d) in slots.iter().zip(payloads) {
                by_seq.insert(e[0].as_u64().unwrap(), d);
            }
        }
        let full = buffer.clone();
        for list in [
            "buffer",
            "subscriber",
            "subscriber_after_completion",
            "late_subscriber_from_0",
            "late_subscriber_from_3",
            "buffer_mid",
        ] {
            let Some(events) = step.get_mut(list).and_then(Value::as_array_mut) else {
                continue;
            };
            for e in events.iter_mut() {
                let seq = e[0].as_u64().unwrap();
                // Every list is a slice of the same buffer.
                assert!(full.contains(e), "{list}: event {seq} not in the buffer");
                if let Some(d) = by_seq.get(&seq) {
                    e[2] = d.clone();
                }
            }
        }
    }
    if let Some(bus) = step.get_mut("bus_events").and_then(Value::as_array_mut) {
        let album_rank = |e: &Value| {
            let id = e["album_id"].as_str().unwrap();
            let dir = id.strip_prefix("<ID:").unwrap().strip_suffix('>').unwrap();
            rank(&format!("<LIB>/{dir}"))
        };
        for ty in ["AlbumMarkedMissing", "AlbumTagsChanged"] {
            let slots: Vec<usize> = (0..bus.len()).filter(|&i| bus[i]["type"] == ty).collect();
            let mut evs: Vec<Value> = slots.iter().map(|&i| bus[i].clone()).collect();
            evs.sort_by_key(album_rank);
            for (&i, e) in slots.iter().zip(evs) {
                bus[i] = e;
            }
        }
    }
}

/// The Python app's record of one step with its deltas applied; counts uses.
fn expected_step(story: &str, step: usize, rec: &Value, used: &mut usize) -> Value {
    let mut out = rec.clone();
    for d in deltas()
        .iter()
        .filter(|d| d.story == story && d.step == step)
    {
        let ctx = format!("{story} step {step} ({})", d.why);
        let at = |out: &Value, p: &str| -> Value {
            out.pointer(p)
                .unwrap_or_else(|| panic!("{ctx}: no {p} in the record"))
                .clone()
        };
        let array = |out: &mut Value, p: &str| -> Vec<Value> {
            std::mem::take(out.pointer_mut(p).unwrap().as_array_mut().unwrap())
        };
        for c in &d.changes {
            match c {
                Change::Expect(p, v) => assert_eq!(&at(&out, p), v, "{ctx}: {p}"),
                Change::Set(p, old, new) => {
                    assert_eq!(&at(&out, p), old, "{ctx}: {p} old value");
                    assert_ne!(old, new, "{ctx}: {p}: delta changes nothing");
                    *out.pointer_mut(p).unwrap() = new.clone();
                }
                Change::Remove(p, old) => {
                    assert_eq!(&at(&out, p), old, "{ctx}: {p} old value");
                    let (arr, i) = p.rsplit_once('/').unwrap();
                    let mut v = array(&mut out, arr);
                    v.remove(i.parse().unwrap());
                    *out.pointer_mut(arr).unwrap() = Value::Array(v);
                }
                Change::Insert(p, i, new) => {
                    let mut v = array(&mut out, p);
                    v.insert(*i, new.clone());
                    *out.pointer_mut(p).unwrap() = Value::Array(v);
                }
                Change::Push(p, new) => {
                    let mut v = array(&mut out, p);
                    v.push(new.clone());
                    *out.pointer_mut(p).unwrap() = Value::Array(v);
                }
            }
        }
        *used += 1;
    }
    out
}

fn run_story(svc: &ScanService, st: &Value, first_story: bool) {
    let id = st["id"].as_str().unwrap();
    let mut used = 0usize;
    let (td, lib) = temp_lib(golden());
    for a in st["albums"].as_array().unwrap() {
        build_album(&lib, a);
    }
    let mut norm = Norm::new(&lib);
    for a in st["albums"].as_array().unwrap() {
        norm.learn_dir(a["dir"].as_str().unwrap());
    }
    let db = fresh_db(td.path());
    let (bus, log) = recording_bus();
    let mut story = Story {
        _td: td,
        lib,
        db,
        norm,
        bus,
        log,
        hook_calls: Arc::default(),
    };
    let lib_s = story.lib.to_string_lossy().into_owned();
    let mut first_scan = true;
    let mut rows: Vec<String> = Vec::new();
    for (idx, step) in st["steps"].as_array().unwrap().iter().enumerate() {
        let mut expected = expected_step(id, idx, step, &mut used);
        if expected.get("buffer").is_some() {
            in_scan_order(&mut expected, &mut rows);
        }
        let step = &expected;
        let op = step["step"]["op"].as_str().unwrap();
        match op {
            "add" => {
                build_album(&story.lib, &step["step"]["album"]);
                story
                    .norm
                    .learn_dir(step["step"]["album"]["dir"].as_str().unwrap());
                continue;
            }
            "remove" => {
                std::fs::remove_dir_all(story.lib.join(step["step"]["dir"].as_str().unwrap()))
                    .unwrap();
                continue;
            }
            "clear_tags" => {
                // What a vocabulary strip does to a Missing album's rows.
                let dir = story.lib.join(step["step"]["dir"].as_str().unwrap());
                let id = musaic_core::album_id(&musaic_scan::py_resolve(&dir).to_string_lossy());
                let n = connect(&story.db)
                    .execute("DELETE FROM track_tags WHERE album_id = ?1", [&id])
                    .unwrap();
                assert!(n > 0, "the album had tag rows to clear");
                continue;
            }
            _ => {}
        }
        let label = format!("{id}/{}", step["step"]["label"].as_str().unwrap());
        let injected = step["step"]["inject_save_error"].as_bool() == Some(true);
        // The Python app's module state carries over between stories: only the
        // very first state, and states inside a story, are comparable.
        if first_story || !first_scan {
            assert_eq!(
                story.norm.value(&svc.state().to_json().to_value()),
                step["state_before"],
                "{label}: state before"
            );
        }
        first_scan = false;
        let force = step["step"]["force"].as_bool().unwrap_or(false);
        if op == "scan" {
            let StartScan::Started(state, job) =
                svc.start_scan(&lib_s, force, story.deps(injected, None))
            else {
                panic!("{label}: a scan was already running");
            };
            assert_eq!(
                mask_injected(story.norm.value(&state.to_json().to_value()), injected),
                mask_injected(step["start_return"].clone(), injected),
                "{label}: start_scan return"
            );
            let worker = std::thread::spawn(move || job.run());
            let live = drain(svc, 0, &story.norm);
            worker.join().unwrap();
            assert_eq!(
                mask_injected(live, injected),
                mask_injected(step["subscriber"].clone(), injected),
                "{label}: subscriber from 0"
            );
        } else {
            // scan_concurrent: pause the worker after the gate_after-th album.
            let gate_after = step["step"]["gate_after"].as_u64().unwrap() as usize;
            let (reached_tx, reached_rx) = mpsc::channel::<()>();
            let (release_tx, release_rx) = mpsc::channel::<()>();
            let release_rx = Mutex::new(release_rx);
            let count = Mutex::new(0usize);
            let reached_tx = Mutex::new(reached_tx);
            let hook: musaic_services::scan::AlbumHook = Arc::new(move |_album, _fresh| {
                let mut n = count.lock().unwrap();
                *n += 1;
                if *n == gate_after {
                    reached_tx.lock().unwrap().send(()).unwrap();
                    let _ = release_rx
                        .lock()
                        .unwrap()
                        .recv_timeout(Duration::from_secs(30));
                }
            });
            let StartScan::Started(state, job) =
                svc.start_scan(&lib_s, false, story.deps(false, Some(hook)))
            else {
                panic!("{label}: already running");
            };
            assert_eq!(
                story.norm.value(&state.to_json().to_value()),
                step["start_return"],
                "{label}: start_scan return"
            );
            let worker = std::thread::spawn(move || job.run());
            reached_rx.recv_timeout(Duration::from_secs(30)).unwrap();
            assert_eq!(
                story.norm.value(&svc.state().to_json().to_value()),
                step["state_mid"],
                "{label}: state mid-scan"
            );
            assert_eq!(
                buffer_json(svc, &story.norm),
                step["buffer_mid"],
                "{label}: buffer mid-scan"
            );
            // A second start while running returns the running state, unchanged.
            match svc.start_scan(&lib_s, true, story.deps(false, None)) {
                StartScan::AlreadyRunning(s) => assert_eq!(
                    story.norm.value(&s.to_json().to_value()),
                    step["second_start_return"],
                    "{label}: second start"
                ),
                StartScan::Started(..) => panic!("{label}: second scan started"),
            }
            // Late subscribers: replay first, then live.
            let mut sub0 = svc.subscribe(0);
            let mut sub3 = svc.subscribe(3);
            let mut got0 = Vec::new();
            let mut got3 = Vec::new();
            for (sub, got) in [(&mut sub0, &mut got0), (&mut sub3, &mut got3)] {
                while let Poll::Event(e) = sub.next_timeout(Duration::from_millis(50)) {
                    got.push(story.norm.value(&json!([e.seq, e.name, e.data.to_value()])));
                }
            }
            assert_eq!(
                json!(got0.len()),
                step["late0_before_release"],
                "{label}: replay from 0"
            );
            assert_eq!(
                json!(got3.len()),
                step["late3_before_release"],
                "{label}: replay from 3"
            );
            release_tx.send(()).unwrap();
            for (sub, got) in [(&mut sub0, &mut got0), (&mut sub3, &mut got3)] {
                loop {
                    match sub.next_timeout(Duration::from_secs(30)) {
                        Poll::Event(e) => {
                            got.push(story.norm.value(&json!([e.seq, e.name, e.data.to_value()])))
                        }
                        Poll::Finished => break,
                        Poll::Pending => panic!("{label}: subscriber stalled"),
                    }
                }
            }
            worker.join().unwrap();
            assert_eq!(
                Value::Array(got0),
                step["late_subscriber_from_0"],
                "{label}: late from 0"
            );
            assert_eq!(
                Value::Array(got3),
                step["late_subscriber_from_3"],
                "{label}: late from 3"
            );
            assert_eq!(
                drain(svc, 0, &story.norm),
                step["subscriber_after_completion"],
                "{label}: subscriber after completion"
            );
        }
        assert_eq!(
            mask_injected(
                story.norm.value(&svc.state().to_json().to_value()),
                injected
            ),
            mask_injected(step["state_after"].clone(), injected),
            "{label}: state after"
        );
        assert_eq!(
            mask_injected(buffer_json(svc, &story.norm), injected),
            mask_injected(step["buffer"].clone(), injected),
            "{label}: buffer"
        );
        assert_eq!(
            Value::Array(story.take_bus()),
            step["bus_events"],
            "{label}: bus events"
        );
        assert_eq!(
            story.take_hook_calls(),
            step["fingerprint_pool_calls"],
            "{label}: post-scan hook"
        );
        let conn = connect(&story.db);
        assert_eq!(
            dump_db(&conn, &story.norm, &album_columns()),
            step["db"],
            "{label}: db"
        );
    }
    let listed = deltas().iter().filter(|d| d.story == id).count();
    assert_eq!(used, listed, "{id}: every listed delta applies to a step");
}

fn story(id: &str) -> &'static Value {
    golden()["stories"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == id)
        .unwrap()
}

/// First scan, unchanged rescan (cached),
/// album added, removed (-> Missing, AlbumMarkedMissing), rescan while
/// Missing (the Python app announced marked_missing again with no new
/// transition; delta: nothing announced), return (AlbumTagsChanged
/// missing_since), force rescan.
#[test]
fn small_library_lifecycle() {
    run_story(&ScanService::new(), story("small"), true);
}

/// 4 of 10 vanish -> aborted, nothing written; exactly 30% commits. Delta:
/// the abort ends `aborted` with LibraryScanStopped and no pool kick-off
/// (the Python app: `done`, LibraryScanCompleted, kick-off). In the Python app
/// Missing albums kept counting toward the ratio, so later scans kept
/// aborting; deltas: 1 more of 7 Present commits, then a rescan
/// commits with nothing to announce.
#[test]
fn sanity_abort_and_threshold() {
    run_story(&ScanService::new(), story("ten"), false);
}

/// Concurrency: status mid-scan, second start while running is a
/// no-op returning the running state, subscribers joining mid-scan get the
/// buffer replayed (from 0 and from 3) then live events, one joining after
/// completion gets the whole buffer and ends.
#[test]
fn concurrent_start_and_late_subscribers() {
    run_story(&ScanService::new(), story("concurrency"), false);
}

/// Edge: an empty library still emits start + done.
#[test]
fn empty_library() {
    run_story(&ScanService::new(), story("empty"), false);
}

/// Negative: a failing save -> error event, status
/// error, no post-scan hook; delta: LibraryScanStopped(error) instead of the
/// Python app's LibraryScanCompleted.
#[test]
fn save_failure_is_an_error() {
    run_story(&ScanService::new(), story("error"), false);
}

/// The sanity check with 10 or more albums still Present (story `fifteen`,
/// added to the generator): 4 of 15 vanish and commit as in the Python app; the
/// three scans the Python app then aborted are deltas (see [`deltas`]).
#[test]
fn sanity_check_counts_present_albums() {
    run_story(&ScanService::new(), story("fifteen"), false);
}

/// A returning album whose tag rows were
/// removed while it was Missing does not re-import its stale sidecar; an
/// album first seen in the same scan does (sidecars, the Missing ->
/// Present return). Story `sidecar-return`, added to the generator.
#[test]
fn returning_album_does_not_reimport_its_sidecar() {
    run_story(&ScanService::new(), story("sidecar-return"), false);
}

/// Every story in the golden is exercised above.
#[test]
fn every_story_is_covered() {
    let ids: Vec<&str> = golden()["stories"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        [
            "small",
            "ten",
            "concurrency",
            "empty",
            "error",
            "fifteen",
            "sidecar-return"
        ]
    );
}

/// The sequence-number contract on its own (no Python app run needed
/// beyond the stories above): a subscription from a seq past the end
/// of a finished scan ends at once.
#[test]
fn subscription_past_the_end_finishes() {
    let svc = ScanService::new();
    let (td, lib) = temp_lib(golden());
    let db = fresh_db(td.path());
    let (bus, log) = recording_bus();
    let story = Story {
        _td: td,
        lib: lib.clone(),
        db,
        norm: Norm::new(&lib),
        bus,
        log,
        hook_calls: Arc::default(),
    };
    let StartScan::Started(_, job) =
        svc.start_scan(&lib.to_string_lossy(), false, story.deps(false, None))
    else {
        panic!()
    };
    job.run();
    let mut sub = svc.subscribe(100);
    assert_eq!(sub.next_timeout(Duration::from_secs(10)), Poll::Finished);
}

/// The thumbnail drain point (task: `ScanDeps::on_worker_done`). The Python app
/// order, services/scan.py: `_scan_worker` emits `done` (or `error` from its
/// `except`) and then, in its `finally`, `thumb_pool.shutdown(wait=True)`
/// (lines 395-404); only after the worker returns does `_run_scan` set the
/// terminal status and publish `LibraryScanCompleted` (lines 183-222). So
/// on a clean scan, an aborted scan and a failing one alike, the hook runs
/// after the worker's last scan event, while the status is still
/// `running`, and before the bus event. No capture records that instant
/// (state comparisons are taken after the scenario); this pins the code
/// order of the Python app's source. Deliberate change (issue #193): the
/// failed scan's bus event is `LibraryScanStopped`, not the Python app's
/// `LibraryScanCompleted`; the order is the same.
#[test]
fn worker_done_hook_runs_before_library_scan_completed_on_every_path() {
    use musaic_events::{Subscriber, Topic};
    for (label, read_only_db, last_event, bus_event) in [
        ("clean", false, "done", "LibraryScanCompleted"),
        ("save fails", true, "error", "LibraryScanStopped"),
    ] {
        let svc = ScanService::new();
        let (td, lib) = temp_lib(golden());
        let db = fresh_db(td.path());
        let (bus, log) = recording_bus();
        let order: Arc<Mutex<Vec<String>>> = Arc::default();
        let o2 = Arc::clone(&order);
        bus.register(
            Topic::Event,
            Subscriber::from_fn(move |ev| {
                if matches!(
                    ev,
                    Event::LibraryScanCompleted { .. } | Event::LibraryScanStopped { .. }
                ) {
                    o2.lock().unwrap().push(ev.name().into());
                }
                Ok(())
            })
            .name("order_recorder"),
        );
        let story = Story {
            _td: td,
            lib: lib.clone(),
            db,
            norm: Norm::new(&lib),
            bus,
            log,
            hook_calls: Arc::default(),
        };
        let mut deps = story.deps(read_only_db, None);
        let o3 = Arc::clone(&order);
        let svc2 = svc.clone();
        deps.on_worker_done = Some(Arc::new(move || {
            let st = svc2.state();
            let last = svc2.buffer().last().map(|e| e.name).unwrap_or("");
            o3.lock().unwrap().push(format!(
                "worker_done status={} last_event={last}",
                st.status.as_str()
            ));
        }));
        let StartScan::Started(_, job) = svc.start_scan(&lib.to_string_lossy(), false, deps) else {
            panic!("{label}: already running")
        };
        job.run();
        assert_eq!(
            *order.lock().unwrap(),
            vec![
                format!("worker_done status=running last_event={last_event}"),
                bus_event.to_string(),
            ],
            "{label}"
        );
        assert_eq!(
            svc.state().status.as_str(),
            if last_event == "done" {
                "done"
            } else {
                "error"
            },
            "{label}"
        );
    }
}

/// A panicking worker-done hook is logged and the scan still ends (`done`)
/// and publishes, as the Python app's `finally` cannot stop `_run_scan`.
#[test]
fn panicking_worker_done_hook_is_logged_and_the_scan_finishes() {
    let svc = ScanService::new();
    let (td, lib) = temp_lib(golden());
    let db = fresh_db(td.path());
    let (bus, log) = recording_bus();
    let story = Story {
        _td: td,
        lib: lib.clone(),
        db,
        norm: Norm::new(&lib),
        bus,
        log: Arc::clone(&log),
        hook_calls: Arc::default(),
    };
    let mut deps = story.deps(false, None);
    deps.on_worker_done = Some(Arc::new(|| panic!("drain failed")));
    let lines: Arc<Mutex<Vec<String>>> = Arc::default();
    let l2 = Arc::clone(&lines);
    deps.log = Some(Arc::new(move |l: &str| {
        l2.lock().unwrap().push(l.to_string())
    }));
    let StartScan::Started(_, job) = svc.start_scan(&lib.to_string_lossy(), false, deps) else {
        panic!()
    };
    job.run();
    assert_eq!(svc.state().status.as_str(), "done");
    assert!(
        log.lock()
            .unwrap()
            .iter()
            .any(|e| matches!(e, Event::LibraryScanCompleted { .. }))
    );
    let lines = lines.lock().unwrap();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("drain failed"), "{lines:?}");
}
