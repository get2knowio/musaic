//! Shared helpers: golden loading, a tiny executor, the script interpreter
//! that replays the recorded scenarios.
#![allow(dead_code)]

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::{Pin, pin};
use std::sync::{Arc, Mutex, mpsc};
use std::task::{Context, Poll, Wake, Waker};
use std::thread::{self, Thread};
use std::time::Duration;

use musaic_events::{
    ArgValue, Bus, Callback, Event, SideEffects, Subscriber, SubscriberError, Topic, context,
    run_bulk, run_post_scan_guarded, run_post_scan_guarded_async,
};
use serde_json::{Map, Value, json};

pub fn golden(name: &str) -> Value {
    let p: PathBuf = musaic_testdata::public("golden/events").join(name);
    serde_json::from_str(&std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{p:?}: {e}")))
        .unwrap()
}

struct ThreadWaker(Thread);

impl Wake for ThreadWaker {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }
}

/// Minimal single-future executor (the crate is executor-agnostic).
pub fn block_on<F: Future>(f: F) -> F::Output {
    let mut f = pin!(f);
    let waker = Waker::from(Arc::new(ThreadWaker(thread::current())));
    let mut cx = Context::from_waker(&waker);
    loop {
        if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
        thread::park_timeout(Duration::from_millis(50));
    }
}

/// A future that is pending once, then ready (a yield point).
pub struct YieldOnce(bool);

pub fn yield_once() -> YieldOnce {
    YieldOnce(false)
}

impl Future for YieldOnce {
    type Output = ();
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.0 {
            Poll::Ready(())
        } else {
            self.0 = true;
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    }
}

pub fn event_from(v: &Value) -> Result<Event, musaic_events::FromFieldsError> {
    let class = v["class"].as_str().unwrap();
    let empty = Map::new();
    let args = v.get("args").and_then(Value::as_object).unwrap_or(&empty);
    Event::from_fields(
        class,
        args.iter().map(|(k, a)| {
            let av = match a {
                Value::Null => ArgValue::Null,
                Value::String(s) => ArgValue::Str(s.clone()),
                Value::Number(n) if n.is_i64() => ArgValue::Int(n.as_i64().unwrap()),
                Value::Array(xs) if xs.iter().all(Value::is_string) => {
                    ArgValue::StrList(xs.iter().map(|x| x.as_str().unwrap().to_owned()).collect())
                }
                _ => ArgValue::Other,
            };
            (k.as_str(), av)
        }),
    )
}

pub fn ev(v: &Value) -> Event {
    event_from(v).unwrap_or_else(|e| panic!("{v}: {e}"))
}

pub fn flag_state() -> Value {
    let c = context::current();
    json!({"rerun": c.in_post_scan_rerun, "bulk": c.in_bulk_run})
}

/// `entry` plus the current flags.
pub fn flagged(mut entry: Value) -> Value {
    let f = flag_state();
    for (k, v) in f.as_object().unwrap() {
        entry[k] = v.clone();
    }
    entry
}

#[derive(Default)]
pub struct Trace {
    sync: Mutex<Vec<Value>>,
    bg: Mutex<Vec<Value>>,
}

impl Trace {
    pub fn rec(&self, mut entry: Value) {
        let name = thread::current().name().map(str::to_owned);
        match name {
            Some(n) if n.starts_with("library_events:") => {
                entry["thread"] = Value::String(n);
                self.bg.lock().unwrap().push(entry);
            }
            _ => self.sync.lock().unwrap().push(entry),
        }
    }

    /// `{"sync": [...], "background": [...]}`, background canonically sorted
    /// (it races; the generator sorts it too).
    pub fn to_value(&self) -> Value {
        let mut bg = self.bg.lock().unwrap().clone();
        bg.sort_by_key(|v| v.to_string());
        json!({"sync": self.sync.lock().unwrap().clone(), "background": bg})
    }
}

/// Sort the golden's background list the same way before comparing.
pub fn canon(trace: &Value) -> Value {
    let mut t = trace.clone();
    let bg = t["background"].as_array_mut().unwrap();
    bg.sort_by_key(|v| v.to_string());
    t
}

pub struct Interp {
    pub bus: Bus,
    pub trace: Arc<Trace>,
    callbacks: Mutex<HashMap<String, Callback>>,
}

impl Interp {
    pub fn new(bus: Bus, trace: Arc<Trace>) -> Self {
        Interp {
            bus,
            trace,
            callbacks: Mutex::new(HashMap::new()),
        }
    }

    fn callback(&self, sub: &str, behavior: &str, nested: Option<&Value>) -> Callback {
        if let Some(cb) = self.callbacks.lock().unwrap().get(sub) {
            return cb.clone();
        }
        let trace = self.trace.clone();
        let bus = self.bus.clone();
        let sub_s = sub.to_owned();
        let behavior = behavior.to_owned();
        let nested = nested.map(ev);
        let cb: Callback = Arc::new(move |e: &Event| -> Result<(), SubscriberError> {
            let base = flagged(json!({"sub": sub_s, "event": e.name()}));
            let with = |k: &str, v: Value| {
                let mut b = base.clone();
                b[k] = v;
                b
            };
            match behavior.as_str() {
                "record" => trace.rec(base),
                "raise" => {
                    trace.rec(with("action", json!("raise")));
                    return Err("intentional".into());
                }
                "republish" => {
                    trace.rec(with("action", json!("republish")));
                    bus.publish(nested.as_ref().unwrap());
                }
                "skip_if_rerun" => {
                    let a = if context::in_post_scan_rerun() {
                        "skip"
                    } else {
                        "run"
                    };
                    trace.rec(with("action", json!(a)));
                }
                "guarded_republish" => {
                    if context::in_post_scan_rerun() {
                        trace.rec(with("action", json!("skip")));
                        return Ok(());
                    }
                    trace.rec(with("action", json!("run")));
                    let n = nested.clone().unwrap();
                    context::with_post_scan_rerun(|| bus.publish(&n));
                }
                "set_flag_then_raise" => {
                    return context::with_post_scan_rerun(|| {
                        let mut b = with("action", json!("set_then_raise"));
                        b["inside"] = flag_state();
                        trace.rec(b);
                        Err("oops".into())
                    });
                }
                other => panic!("behavior {other}"),
            }
            Ok(())
        });
        self.callbacks
            .lock()
            .unwrap()
            .insert(sub.to_owned(), cb.clone());
        cb
    }

    pub fn steps(&self, steps: &[Value]) {
        for s in steps {
            self.step(s);
        }
    }

    pub fn wait(&self) {
        assert!(
            self.bus.wait_background_idle(Duration::from_secs(5)),
            "background subscribers did not finish"
        );
    }

    pub fn step(&self, s: &Value) {
        let topic = || Topic::from_name(s["topic"].as_str().unwrap()).unwrap();
        match s["op"].as_str().unwrap() {
            "register" => {
                let sub = s["sub"].as_str().unwrap();
                let cb = self.callback(
                    sub,
                    s.get("behavior")
                        .and_then(Value::as_str)
                        .unwrap_or("record"),
                    s.get("event"),
                );
                self.bus.register(
                    topic(),
                    Subscriber::new(cb)
                        .priority(s.get("priority").and_then(Value::as_i64).unwrap_or(200))
                        .background(
                            s.get("background")
                                .and_then(Value::as_bool)
                                .unwrap_or(false),
                        )
                        .name(sub),
                );
            }
            "unregister" => {
                let sub = s["sub"].as_str().unwrap();
                let cb = self
                    .callbacks
                    .lock()
                    .unwrap()
                    .get(sub)
                    .cloned()
                    .unwrap_or_else(|| Arc::new(|_: &Event| Ok(())));
                self.bus.unregister(topic(), &Subscriber::new(cb).id());
            }
            "publish" => self.bus.publish(&ev(&s["event"])),
            "with_flag" => {
                let inner = s["steps"].as_array().unwrap();
                match s["flag"].as_str().unwrap() {
                    "in_post_scan_rerun" => context::with_post_scan_rerun(|| self.steps(inner)),
                    "in_bulk_run" => context::with_bulk_run(|| self.steps(inner)),
                    f => panic!("flag {f}"),
                }
            }
            "read_flags" => self.trace.rec(json!({"read_flags": flag_state()})),
            "in_thread" => thread::scope(|sc| {
                thread::Builder::new()
                    .name("other".into())
                    .spawn_scoped(sc, || self.steps(s["steps"].as_array().unwrap()))
                    .unwrap()
                    .join()
                    .unwrap();
            }),
            "concurrent_unrelated" => {
                let (held_tx, held_rx) = mpsc::channel();
                let (rel_tx, rel_rx) = mpsc::channel::<()>();
                let e = ev(&s["event"]);
                thread::scope(|sc| {
                    let a = thread::Builder::new()
                        .name("thread-a".into())
                        .spawn_scoped(sc, move || {
                            context::with_post_scan_rerun(|| {
                                held_tx.send(()).unwrap();
                                let _ = rel_rx.recv_timeout(Duration::from_secs(2));
                            })
                        })
                        .unwrap();
                    held_rx.recv().unwrap();
                    thread::Builder::new()
                        .name("thread-b".into())
                        .spawn_scoped(sc, || self.bus.publish(&e))
                        .unwrap()
                        .join()
                        .unwrap();
                    rel_tx.send(()).unwrap();
                    a.join().unwrap();
                });
            }
            "post_scan" => self.post_scan(s),
            "bulk" => {
                let publish = s
                    .get("publish_after")
                    .and_then(Value::as_bool)
                    .unwrap_or(true);
                run_bulk(&self.bus, publish, || {
                    self.steps(s["steps"].as_array().unwrap())
                });
            }
            "wait" => self.wait(),
            op => panic!("op {op}"),
        }
    }

    fn post_scan(&self, s: &Value) {
        let empty = vec![];
        let inner = s.get("inner").and_then(Value::as_array).unwrap_or(&empty);
        let variant = s["variant"].as_str().unwrap();
        if variant == "sync" {
            run_post_scan_guarded(|| {
                // One runs_during_scan check ("a"); its run raising is
                // swallowed per check, like run_post_scan_checks.
                self.trace.rec(flagged(json!({"runner": "a"})));
                for st in inner {
                    if st["op"] == "raise" {
                        return;
                    }
                    self.step(st);
                }
            });
        } else {
            block_on(run_post_scan_guarded_async(self.post_scan_async(inner)));
        }
        self.trace
            .rec(flagged(json!({"post_scan_returned": variant})));
    }

    fn post_scan_async<'a>(&'a self, inner: &'a [Value]) -> Pin<Box<dyn Future<Output = ()> + 'a>> {
        Box::pin(async move {
            self.trace.rec(flagged(json!({"runner": "a"})));
            for st in inner {
                match st["op"].as_str().unwrap() {
                    "raise" => return,
                    "post_scan" => {
                        run_post_scan_guarded_async(self.post_scan_async(&[])).await;
                    }
                    _ => self.step(st),
                }
            }
        })
    }
}

/// Recording stand-ins for the catalog's side effects. Ids starting with
/// "boom" make the target fail, as in the generator.
pub struct Fx {
    pub trace: Arc<Trace>,
    pub library_dir: Option<String>,
    /// Stub one level lower, as the generator's `real_post_scan`: the
    /// target is the runner body and records one `run_check("a")`.
    pub real_post_scan: bool,
    /// Check "a" publishes `AlbumMbAnchorSet` from inside the sweep.
    pub check_publishes: Option<Bus>,
}

fn boom(id: &str) -> Result<(), SubscriberError> {
    if id.starts_with("boom") {
        Err("intentional".into())
    } else {
        Ok(())
    }
}

impl SideEffects for Fx {
    fn library_dir(&self) -> Option<String> {
        self.library_dir.clone()
    }
    fn resolver_invalidate(&self, album_id: &str) -> Result<(), SubscriberError> {
        self.trace.rec(flagged(
            json!({"target": "resolver_invalidate", "album_id": album_id}),
        ));
        boom(album_id)
    }
    fn delete_thumb(&self, album_id: &str) -> Result<(), SubscriberError> {
        self.trace.rec(flagged(
            json!({"target": "delete_thumb", "album_id": album_id}),
        ));
        boom(album_id)
    }
    fn run_post_scan_checks(
        &self,
        library_dir: &str,
        album_ids: Option<&[String]>,
    ) -> Result<(), SubscriberError> {
        if self.real_post_scan {
            // The runner body (the catalog applies the guard): one check "a".
            self.trace.rec(flagged(json!({
                "target": "run_check", "check_id": "a", "library_path": library_dir,
                "scope_kind": if album_ids.is_some() { "albums" } else { "all" },
                "album_ids": album_ids,
            })));
            if let Some(bus) = &self.check_publishes {
                let id = album_ids
                    .and_then(|a| a.first())
                    .map_or("lib", String::as_str);
                bus.publish(&Event::AlbumMbAnchorSet {
                    album_id: id.to_owned(),
                    source: "fingerprint+mb".to_owned(),
                });
            }
        } else {
            self.trace.rec(flagged(json!({
                "target": "run_post_scan_checks", "library_dir": library_dir, "album_ids": album_ids,
            })));
        }
        Ok(())
    }
    fn run_check_scoped(
        &self,
        check_id: &str,
        library_path: &str,
        album_ids: &[String],
    ) -> Result<(), SubscriberError> {
        self.trace.rec(flagged(json!({
            "target": "run_check", "check_id": check_id, "library_path": library_path,
            "scope_kind": "albums", "album_ids": album_ids,
        })));
        Ok(())
    }
    fn notify_findings_changed(&self) -> Result<(), SubscriberError> {
        self.trace
            .rec(flagged(json!({"target": "notify_findings_changed"})));
        Ok(())
    }
}

/// Replay one catalog scenario (`register_all` + its script) and return the
/// trace. `real` = stub at the runner body (one check "a").
pub fn replay_catalog(sc: &Value, real: bool, check_publishes: bool) -> Value {
    let trace = Arc::new(Trace::default());
    let bus = Bus::new();
    let relay = musaic_events::Relay::new();
    let lib = match sc.get("library_dir") {
        Some(Value::Null) => None,
        Some(v) => Some(v.as_str().unwrap().to_owned()),
        None => Some("/lib".to_owned()),
    };
    let make = || -> Arc<dyn SideEffects> {
        Arc::new(Fx {
            trace: trace.clone(),
            library_dir: lib.clone(),
            real_post_scan: real,
            check_publishes: check_publishes.then(|| bus.clone()),
        })
    };
    musaic_events::register_all(&bus, make(), Some(&relay));
    if sc
        .get("register_twice")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        musaic_events::register_all(&bus, make(), Some(&relay));
    }
    let interp = Interp::new(bus.clone(), trace.clone());
    interp.steps(sc["script"].as_array().unwrap());
    interp.wait();
    bus.reset();
    trace.to_value()
}

fn run_check_a(album_ids: Value, thread: &str) -> Value {
    let scope = if album_ids.is_null() { "all" } else { "albums" };
    json!({"target": "run_check", "check_id": "a", "library_path": "/lib", "scope_kind": scope,
           "album_ids": album_ids, "rerun": true, "bulk": false,
           "thread": format!("library_events:{thread}")})
}

/// The rerun-guard change, enumerated: the only catalog.json scenario whose trace changes
/// is `composed_with_real_post_scan_guard` (the one stubbed below the
/// runner guard), and it gains exactly these four background calls.
pub fn rerun_guard_delta_for_catalog_json(scenario: &str) -> Option<Vec<Value>> {
    (scenario == "composed_with_real_post_scan_guard").then(|| {
        vec![
            run_check_a(json!(["a1"]), "_post_scan_rerun"),
            run_check_a(Value::Null, "_post_scan_rerun_library_wide"),
            run_check_a(Value::Null, "_post_scan_rerun_library_wide:bulk"),
            run_check_a(json!(["new"]), "_post_scan_rerun_for_rename"),
        ]
    })
}
