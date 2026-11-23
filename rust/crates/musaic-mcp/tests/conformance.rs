//! The conformance harness: the stdio server driven as a modern (2026-07-28)
//! and as a legacy (2025-11-25) client over in-memory pipes, with a fake
//! backend. These are harness results, not results from a real host.
//!
//! Skills, Resources and the prompt: contracts/skills.md tests 1 to 5.
//! Events: contracts/events.md tests 1, 2, 6, 7, 9 and 10.
//!
//! The wire shapes pinned here were checked against the extension
//! specifications (`docs/mcp.md`, Compatibility notes, names the commits).

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use musaic_commands::events::{
    EVENT_REQUESTED, EVENT_UPDATED, EventOccurrence, ListResult, PollParams, PollResult,
    WorkEventData, descriptors,
};
use musaic_commands::tools::work::{WorkKind, WorkState};
use musaic_commands::{ToolError, ToolErrorCode};
use musaic_mcp::{Backend, serve_io};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream};

// ------------------------------------------------------------ fake backend

#[derive(Default)]
struct Log {
    events: Vec<EventOccurrence>,
    revoked: bool,
    /// Work claims by work id (atomic: the first wins).
    claims: HashMap<String, String>,
    /// What the harness checks stays untouched by a cancelled stream.
    outbox_marks: usize,
    observations: Vec<musaic_commands::events::HostObservation>,
    polls: usize,
}

#[derive(Default)]
struct Shared {
    log: Mutex<Log>,
    changed: Condvar,
}

impl Shared {
    fn create_work(&self, id: &str) {
        self.push(id, EVENT_REQUESTED, WorkState::Queued);
    }

    fn push(&self, id: &str, name: &str, state: WorkState) {
        let mut log = self.log.lock().unwrap();
        let n = log.events.len() + 1;
        log.events.push(EventOccurrence {
            event_id: format!("evt_test_{n}"),
            name: name.to_string(),
            timestamp: "2026-11-23T10:00:00Z".into(),
            data: WorkEventData {
                work_id: id.to_string(),
                kind: WorkKind::MetadataIdentify,
                state,
                revision: n as i64,
                library_epoch: "epoch".into(),
            },
            cursor: Some(format!("c{n}")),
        });
        self.changed.notify_all();
    }

    fn revoke(&self) {
        self.log.lock().unwrap().revoked = true;
        self.changed.notify_all();
    }
}

struct Fake {
    shared: Arc<Shared>,
    principal: String,
}

fn cursor_n(c: &str) -> Option<usize> {
    c.strip_prefix('c')?.parse().ok()
}

impl Backend for Fake {
    fn call_tool(&self, tool_id: &str, input: Value) -> Result<Value, ToolError> {
        match tool_id {
            "musaic_work_claim" => {
                let id = input["workId"].as_str().unwrap().to_string();
                let mut log = self.shared.log.lock().unwrap();
                if log.claims.contains_key(&id) {
                    return Err(ToolError::new(ToolErrorCode::AlreadyClaimed, "held"));
                }
                log.claims.insert(id.clone(), self.principal.clone());
                Ok(json!({"workId": id, "claimed": true}))
            }
            _ => Err(ToolError::new(ToolErrorCode::NotFound, "nothing here")),
        }
    }

    fn events_list(&self) -> Result<ListResult, ToolError> {
        Ok(ListResult {
            events: descriptors(true),
        })
    }

    fn events_poll(&self, params: PollParams) -> Result<PollResult, ToolError> {
        let shared = &self.shared;
        let deadline =
            Instant::now() + Duration::from_millis(u64::from(params.wait_ms.unwrap_or(0)));
        let mut log = shared.log.lock().unwrap();
        log.polls += 1;
        loop {
            if log.revoked {
                return Err(ToolError::new(ToolErrorCode::EventForbidden, "revoked"));
            }
            let head = log.events.len();
            let (after, truncated) = match params.cursor.as_deref() {
                None => {
                    return Ok(PollResult {
                        events: vec![],
                        cursor: format!("c{head}"),
                        truncated: false,
                        has_more: false,
                        next_poll_ms: 30_000,
                    });
                }
                Some(c) => match cursor_n(c) {
                    Some(n) if n <= head => (n, false),
                    _ => (head, true),
                },
            };
            if truncated {
                return Ok(PollResult {
                    events: vec![],
                    cursor: format!("c{head}"),
                    truncated: true,
                    has_more: false,
                    next_poll_ms: 30_000,
                });
            }
            let events: Vec<EventOccurrence> = log.events[after..].to_vec();
            if !events.is_empty() || Instant::now() >= deadline {
                return Ok(PollResult {
                    cursor: format!("c{head}"),
                    events,
                    truncated: false,
                    has_more: false,
                    next_poll_ms: 30_000,
                });
            }
            let left = deadline - Instant::now();
            log = shared.changed.wait_timeout(log, left).unwrap().0;
        }
    }

    fn observe_host(
        &self,
        observation: musaic_commands::events::HostObservation,
    ) -> Result<(), ToolError> {
        self.shared
            .log
            .lock()
            .unwrap()
            .observations
            .push(observation);
        Ok(())
    }
}

// ------------------------------------------------------------------ client

struct Client {
    w: DuplexStream,
    r: BufReader<DuplexStream>,
    next: u64,
    server: tokio::task::JoinHandle<Result<(), musaic_mcp::ServeError>>,
    meta: Option<Value>,
}

fn start(shared: &Arc<Shared>, principal: &str) -> Client {
    let (cw, sr) = tokio::io::duplex(1 << 20);
    let (sw, cr) = tokio::io::duplex(1 << 20);
    let backend: Arc<dyn Backend> = Arc::new(Fake {
        shared: Arc::clone(shared),
        principal: principal.to_string(),
    });
    let server = tokio::spawn(async move { serve_io(backend, "9.9.9", sr, sw).await });
    Client {
        w: cw,
        r: BufReader::new(cr),
        next: 1,
        server,
        meta: None,
    }
}

fn modern_meta(events: bool) -> Value {
    let caps = if events {
        json!({"extensions": {"io.modelcontextprotocol/events": {}}})
    } else {
        json!({})
    };
    json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientCapabilities": caps,
        "io.modelcontextprotocol/clientInfo": {"name": "harness", "version": "1.2.3"}
    })
}

impl Client {
    async fn send(&mut self, v: Value) {
        let mut line = v.to_string();
        line.push('\n');
        self.w.write_all(line.as_bytes()).await.unwrap();
    }

    /// A modern client (stateless: every request carries its `_meta`).
    fn modern(mut self, events: bool) -> Client {
        self.meta = Some(modern_meta(events));
        self
    }

    /// A legacy client: initialize first.
    async fn legacy(mut self) -> Client {
        let init = self
            .request(
                "initialize",
                json!({"protocolVersion":"2025-11-25","capabilities":{"extensions":{"io.modelcontextprotocol/events":{}}},"clientInfo":{"name":"old","version":"0.1"}}),
            )
            .await;
        assert_eq!(init["result"]["protocolVersion"], "2025-11-25", "{init}");
        self.send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
            .await;
        self
    }

    fn params(&self, mut params: Value) -> Value {
        if let Some(meta) = &self.meta {
            params["_meta"] = meta.clone();
        }
        params
    }

    async fn start_request(&mut self, method: &str, params: Value) -> u64 {
        let id = self.next;
        self.next += 1;
        let params = self.params(params);
        self.send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
            .await;
        id
    }

    async fn read(&mut self, within: Duration) -> Option<Value> {
        let mut line = String::new();
        let n = tokio::time::timeout(within, self.r.read_line(&mut line))
            .await
            .ok()?
            .unwrap();
        assert!(n > 0, "the server closed the stream");
        Some(serde_json::from_str(line.trim()).expect("every line is JSON"))
    }

    async fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.start_request(method, params).await;
        self.response(id, Duration::from_secs(30)).await
    }

    /// Messages before the response are dropped.
    async fn response(&mut self, id: u64, within: Duration) -> Value {
        let end = Instant::now() + within;
        loop {
            let left = end.saturating_duration_since(Instant::now());
            let v = self.read(left).await.expect("a reply in time");
            if v["id"] == json!(id) {
                return v;
            }
        }
    }

    /// The next notification (a message with a method), within a time.
    async fn notification(&mut self, within: Duration) -> Option<Value> {
        let end = Instant::now() + within;
        loop {
            let left = end.saturating_duration_since(Instant::now());
            let v = self.read(left).await?;
            if v.get("method").is_some() && v.get("id").is_none() {
                return Some(v);
            }
        }
    }
}

fn digest(bytes: &[u8]) -> String {
    format!("sha256:{}", hex(&Sha256::digest(bytes)))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn fresh() -> Arc<Shared> {
    Arc::new(Shared::default())
}

const SKILL_URI: &str = "skill://musaic-work-delegation/SKILL.md";

// ------------------------------------------------------------------ skills

#[tokio::test]
async fn modern_skill_list_matches_every_served_byte() {
    let mut c = start(&fresh(), "a").modern(false);
    let d = c.request("server/discover", json!({})).await;
    let ext = &d["result"]["capabilities"]["extensions"];
    assert!(ext["io.modelcontextprotocol/skills"].is_object(), "{d}");
    assert!(
        ext["io.modelcontextprotocol/skills"]
            .get("directoryRead")
            .is_none()
    );
    assert!(ext["io.modelcontextprotocol/events"].is_object(), "{d}");
    assert!(d["result"]["capabilities"]["resources"].is_object());
    assert!(d["result"]["capabilities"]["prompts"].is_object());

    let list = c.request("skills/list", json!({})).await;
    assert!(list.get("error").is_none(), "{list}");
    let r = &list["result"];
    assert_eq!(r["resultType"], "complete");
    assert_eq!(r["ttlMs"], 3_600_000);
    assert_eq!(r["cacheScope"], "public");
    assert!(r.get("nextCursor").is_none());
    let skills = r["skills"].as_array().unwrap();
    assert_eq!(skills.len(), 1);
    let skill = &skills[0];
    assert_eq!(skill["uri"], SKILL_URI);
    assert_eq!(skill["frontmatter"]["name"], "musaic-work-delegation");
    assert!(skill["frontmatter"]["metadata"]["version"].is_string());
    let files = skill["resources"].as_array().unwrap();
    assert!(files.len() >= 9);
    let mut seen = std::collections::BTreeSet::new();
    for f in files {
        let uri = f["uri"].as_str().unwrap();
        assert!(seen.insert(uri.to_string()), "listed twice: {uri}");
        let read = c.request("resources/read", json!({"uri": uri})).await;
        let contents = &read["result"]["contents"][0];
        assert_eq!(contents["uri"], uri);
        assert_eq!(contents["mimeType"], "text/markdown");
        let text = contents["text"].as_str().unwrap();
        assert_eq!(text.len() as u64, f["size"].as_u64().unwrap(), "{uri}");
        assert_eq!(
            digest(text.as_bytes()),
            f["digest"].as_str().unwrap(),
            "{uri}"
        );
    }
    assert!(seen.contains(SKILL_URI));
}

#[tokio::test]
async fn skills_get_by_uri_works_without_a_listing() {
    let mut c = start(&fresh(), "a").modern(false);
    let got = c.request("skills/get", json!({"uri": SKILL_URI})).await;
    assert!(got.get("error").is_none(), "{got}");
    assert_eq!(got["result"]["resultType"], "complete");
    assert_eq!(got["result"]["skill"]["uri"], SKILL_URI);
    assert_eq!(got["result"]["ttlMs"], 3_600_000);
    assert_eq!(got["result"]["cacheScope"], "public");
    // The entry equals the listing's.
    let list = c.request("skills/list", json!({})).await;
    assert_eq!(got["result"]["skill"], list["result"]["skills"][0]);
}

fn refusal_uris() -> Vec<String> {
    let p = "skill://musaic-work-delegation/";
    vec![
        format!("{p}../SKILL.md"),
        format!("{p}references/../SKILL.md"),
        format!("{p}%2e%2e/SKILL.md"),
        format!("{p}references%2fwork-lifecycle.md"),
        format!("{p}references\\work-lifecycle.md"),
        format!("{p}SKILL.md?x=1"),
        format!("{p}SKILL.md#frag"),
        format!("{p}references/"),
        format!("{p}unlisted.md"),
        format!("{p}skill.lock.json"),
        "/etc/passwd".into(),
        "file:///etc/passwd".into(),
        "https://example.com/SKILL.md".into(),
        "http://example.com/SKILL.md".into(),
        "skill://other-skill/SKILL.md".into(),
        "skill://musaic-work-delegation".into(),
        "SKILL.md".into(),
        String::new(),
    ]
}

#[tokio::test]
async fn every_refusal_is_the_same_invalid_params_message_and_never_echoes() {
    let mut c = start(&fresh(), "a").modern(false);
    for uri in refusal_uris() {
        for method in ["resources/read", "skills/get"] {
            let r = c.request(method, json!({"uri": uri})).await;
            assert_eq!(r["error"]["code"], -32602, "{method} {uri:?}: {r}");
            assert_eq!(
                r["error"]["message"], "unknown resource",
                "{method} {uri:?}: {r}"
            );
            let text = r.to_string();
            assert!(
                uri.is_empty() || !text.contains(&uri.replace('\\', "\\\\")),
                "echoed {uri:?}: {r}"
            );
        }
    }
}

#[tokio::test]
async fn legacy_sessions_have_no_skill_methods_but_serve_the_same_resources() {
    let shared = fresh();
    let mut modern = start(&shared, "a").modern(false);
    let want = modern
        .request("resources/read", json!({"uri": SKILL_URI}))
        .await["result"]["contents"][0]["text"]
        .clone();
    let mut old = start(&shared, "b").legacy().await;
    for (method, params) in [
        ("skills/list", json!({})),
        ("skills/get", json!({"uri": SKILL_URI})),
    ] {
        let r = old.request(method, params).await;
        assert_eq!(r["error"]["code"], -32601, "{method}: {r}");
    }
    let read = old
        .request("resources/read", json!({"uri": SKILL_URI}))
        .await;
    assert_eq!(read["result"]["contents"][0]["text"], want);
    let list = old.request("resources/list", json!({})).await;
    let resources = list["result"]["resources"].as_array().unwrap();
    // SKILL.md's resource takes `name` and `description` from its frontmatter.
    let entry = resources.iter().find(|r| r["uri"] == SKILL_URI).unwrap();
    assert_eq!(entry["mimeType"], "text/markdown");
    assert_eq!(entry["name"], "musaic-work-delegation", "{entry}");
    assert!(entry["description"].is_string());
    assert!(resources.iter().filter(|r| r["uri"] != SKILL_URI).all(|r| {
        r["uri"]
            .as_str()
            .unwrap()
            .ends_with(r["name"].as_str().unwrap())
    }));
    let prompts = old.request("prompts/list", json!({})).await;
    assert_eq!(
        prompts["result"]["prompts"][0]["name"],
        "musaic_research_album"
    );
}

#[tokio::test]
async fn the_listed_frontmatter_equals_the_frontmatter_of_the_bytes_read() {
    let mut c = start(&fresh(), "a").modern(false);
    let list = c.request("skills/list", json!({})).await;
    let fm = &list["result"]["skills"][0]["frontmatter"];
    let read = c.request("resources/read", json!({"uri": SKILL_URI})).await;
    let text = read["result"]["contents"][0]["text"].as_str().unwrap();
    let parsed = musaic_commands::skill::parse_frontmatter(text).unwrap();
    assert_eq!(*fm, parsed);
}

#[tokio::test]
async fn the_prompt_returns_the_guide_and_validates_the_album_id() {
    let mut c = start(&fresh(), "a").modern(false);
    let g = c
        .request("prompts/get", json!({"name": "musaic_research_album"}))
        .await;
    let text = g["result"]["messages"][0]["content"]["text"]
        .as_str()
        .unwrap();
    assert!(text.starts_with("Process pending Musaic work"));
    assert!(text.contains("# Musaic work delegation"));
    let id = "0123456789abcdef";
    let g = c
        .request(
            "prompts/get",
            json!({"name": "musaic_research_album", "arguments": {"album_id": id}}),
        )
        .await;
    assert!(
        g["result"]["messages"][0]["content"]["text"]
            .as_str()
            .unwrap()
            .contains(id)
    );
    for bad in [
        "short",
        "0123456789ABCDEF",
        "0123456789abcdeg",
        "../../etc/passwd",
        "0123456789abcdef0",
    ] {
        let g = c
            .request(
                "prompts/get",
                json!({"name": "musaic_research_album", "arguments": {"album_id": bad}}),
            )
            .await;
        assert_eq!(g["error"]["code"], -32602, "{bad}: {g}");
    }
}

#[tokio::test]
async fn directory_read_is_not_offered() {
    let mut c = start(&fresh(), "a").modern(false);
    let r = c
        .request(
            "resources/directory/read",
            json!({"uri": "skill://musaic-work-delegation/"}),
        )
        .await;
    assert_eq!(r["error"]["code"], -32601, "{r}");
}

// ------------------------------------------------------------------ events

fn event_names(result: &Value) -> Vec<(String, String)> {
    result["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["name"].as_str().unwrap().to_string(),
                e["data"]["workId"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

#[tokio::test]
async fn events_list_equals_the_catalog() {
    let mut c = start(&fresh(), "a").modern(true);
    let r = c.request("events/list", json!({})).await;
    assert_eq!(r["result"]["resultType"], "complete", "{r}");
    let want = serde_json::to_value(descriptors(true)).unwrap();
    assert_eq!(r["result"]["events"], want);
    // The checked-in catalog says the same.
    let catalog: Value =
        serde_json::from_str(&musaic_commands::events::events_catalog_json()).unwrap();
    assert_eq!(catalog["events"], want);
}

#[tokio::test]
async fn poll_bootstraps_with_a_cursor_then_returns_new_work() {
    let shared = fresh();
    let mut c = start(&shared, "a").modern(true);
    let boot = c
        .request("events/poll", json!({"name": EVENT_REQUESTED}))
        .await;
    assert_eq!(boot["result"]["events"], json!([]), "{boot}");
    assert_eq!(boot["result"]["truncated"], false);
    let cursor = boot["result"]["cursor"].as_str().unwrap().to_string();
    shared.create_work("w1");
    let r = c
        .request(
            "events/poll",
            json!({"name": EVENT_REQUESTED, "cursor": cursor}),
        )
        .await;
    assert_eq!(
        event_names(&r["result"]),
        [(EVENT_REQUESTED.to_string(), "w1".to_string())]
    );
    assert_eq!(r["result"]["resultType"], "complete");
    assert_eq!(r["result"]["data"], Value::Null);
    // Poll carries the cursor at the response level only.
    assert!(r["result"]["events"][0].get("cursor").is_none(), "{r}");
    for key in ["cursor", "truncated", "hasMore", "nextPollMs"] {
        assert!(r["result"].get(key).is_some(), "{key}: {r}");
    }
    // data holds exactly the five keys.
    let data = r["result"]["events"][0]["data"].as_object().unwrap();
    let mut keys: Vec<&str> = data.keys().map(String::as_str).collect();
    keys.sort();
    assert_eq!(
        keys,
        ["kind", "libraryEpoch", "revision", "state", "workId"]
    );
    // An unusable cursor is truncated, never an error.
    let t = c
        .request(
            "events/poll",
            json!({"name": EVENT_REQUESTED, "cursor": "garbage"}),
        )
        .await;
    assert_eq!(t["result"]["truncated"], true, "{t}");
    assert_eq!(t["result"]["events"], json!([]));
    // Bad parameters and unknown names are protocol errors.
    let bad = c
        .request(
            "events/poll",
            json!({"name": EVENT_REQUESTED, "maxEvents": 0}),
        )
        .await;
    assert_eq!(bad["error"]["code"], -32602, "{bad}");
    let bad = c
        .request(
            "events/poll",
            json!({"name": EVENT_REQUESTED, "arguments": {"nope": 1}}),
        )
        .await;
    assert_eq!(bad["error"]["code"], -32602, "{bad}");
    let unknown = c
        .request("events/poll", json!({"name": "musaic.nope.v1"}))
        .await;
    assert_eq!(unknown["error"]["code"], -32023, "{unknown}");
    assert_eq!(unknown["error"]["data"], json!({"kind": "event"}));
}

#[tokio::test]
async fn subscribe_is_unsupported_and_events_need_the_modern_profile() {
    let shared = fresh();
    let mut c = start(&shared, "a").modern(true);
    for m in ["events/subscribe", "events/unsubscribe"] {
        let r = c.request(m, json!({"name": EVENT_REQUESTED})).await;
        assert_eq!(r["error"]["code"], -32026, "{m}: {r}");
        assert_eq!(
            r["error"]["data"],
            json!({"feature": "deliveryMode", "value": "webhook"}),
            "{m}: {r}"
        );
    }
    // A client MAY declare the extension; one that does not is still served.
    let mut undeclared = start(&shared, "b").modern(false);
    let list = undeclared.request("events/list", json!({})).await;
    assert_eq!(list["result"]["resultType"], "complete", "{list}");
    let poll = undeclared
        .request("events/poll", json!({"name": EVENT_REQUESTED}))
        .await;
    assert!(poll["result"]["cursor"].is_string(), "{poll}");
    // Test 10: a legacy session.
    let mut old = start(&shared, "c").legacy().await;
    for (m, p) in [
        ("events/list", json!({})),
        ("events/poll", json!({"name": EVENT_REQUESTED})),
        ("events/stream", json!({"name": EVENT_REQUESTED})),
        ("events/subscribe", json!({"name": EVENT_REQUESTED})),
    ] {
        let r = old.request(m, p).await;
        assert_eq!(r["error"]["code"], -32601, "{m}: {r}");
    }
}

async fn open_stream(c: &mut Client, extra: Value) -> u64 {
    let mut params = json!({"name": EVENT_REQUESTED});
    for (k, v) in extra.as_object().unwrap() {
        params[k] = v.clone();
    }
    c.start_request("events/stream", params).await
}

#[tokio::test]
async fn a_stream_sends_active_first_then_events_with_the_subscription_id() {
    let shared = fresh();
    let mut c = start(&shared, "a").modern(true);
    let id = open_stream(&mut c, json!({})).await;
    let active = c.notification(Duration::from_secs(10)).await.unwrap();
    assert_eq!(active["method"], "notifications/events/active", "{active}");
    assert_eq!(active["params"]["truncated"], false);
    assert_eq!(active["params"]["heartbeatMs"], 30_000);
    assert_eq!(
        active["params"]["_meta"]["io.modelcontextprotocol/subscriptionId"],
        json!(id)
    );
    shared.create_work("w-stream");
    let ev = c.notification(Duration::from_secs(10)).await.unwrap();
    assert_eq!(ev["method"], "notifications/events/event", "{ev}");
    assert_eq!(ev["params"]["name"], EVENT_REQUESTED);
    assert_eq!(ev["params"]["data"]["workId"], "w-stream");
    assert!(ev["params"]["cursor"].is_string());
    assert_eq!(
        ev["params"]["_meta"]["io.modelcontextprotocol/subscriptionId"],
        json!(id)
    );
}

#[tokio::test]
async fn an_idle_stream_beats_at_the_granted_interval_and_a_cancel_stops_delivery() {
    let shared = fresh();
    let mut c = start(&shared, "a").modern(true);
    let id = open_stream(&mut c, json!({"heartbeatMs": 5_000})).await;
    let active = c.notification(Duration::from_secs(10)).await.unwrap();
    assert_eq!(active["params"]["heartbeatMs"], 5_000);
    let started = Instant::now();
    let beat = c.notification(Duration::from_secs(12)).await.unwrap();
    assert_eq!(beat["method"], "notifications/events/heartbeat", "{beat}");
    assert!(
        started.elapsed() >= Duration::from_millis(4_500),
        "beat after {:?}: not faster than the granted interval",
        started.elapsed()
    );
    assert!(beat["params"]["cursor"].is_string());

    // FR-025: cancelling reads nothing and writes nothing of the outbox.
    let marks = shared.log.lock().unwrap().outbox_marks;
    c.send(json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId": id}}))
        .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    shared.create_work("after-cancel");
    // Nothing more arrives for the cancelled stream (a final result may).
    let end = Instant::now() + Duration::from_secs(2);
    while Instant::now() < end {
        match c.read(Duration::from_millis(500)).await {
            None => {}
            Some(v) => assert!(
                v.get("method").is_none(),
                "a notification arrived after the cancel: {v}"
            ),
        }
    }
    assert_eq!(shared.log.lock().unwrap().outbox_marks, marks);
    assert!(!c.server.is_finished());
}

#[tokio::test]
async fn a_truncated_cursor_gets_a_fresh_active() {
    let shared = fresh();
    shared.create_work("old");
    let mut c = start(&shared, "a").modern(true);
    let _ = open_stream(&mut c, json!({"cursor": "c99"})).await;
    let active = c.notification(Duration::from_secs(10)).await.unwrap();
    assert_eq!(active["method"], "notifications/events/active");
    assert_eq!(active["params"]["truncated"], true, "{active}");
    assert_eq!(active["params"]["cursor"], "c1");
}

#[tokio::test]
async fn revoking_ends_a_stream_with_terminated_and_a_waiting_poll_with_forbidden() {
    let shared = fresh();
    let mut s = start(&shared, "a").modern(true);
    let id = open_stream(&mut s, json!({})).await;
    let _active = s.notification(Duration::from_secs(10)).await.unwrap();
    let mut p = start(&shared, "a").modern(true);
    shared.revoke();
    let done = s.notification(Duration::from_secs(10)).await.unwrap();
    assert_eq!(done["method"], "notifications/events/terminated", "{done}");
    assert_eq!(done["params"]["error"]["code"], -32024);
    assert_eq!(
        done["params"]["_meta"]["io.modelcontextprotocol/subscriptionId"],
        json!(id)
    );
    // StreamEventsResult: `resultType` and the subscription id, nothing else.
    let fin = s.response(id, Duration::from_secs(10)).await;
    assert_eq!(
        fin["result"],
        json!({"resultType": "complete", "_meta": {"io.modelcontextprotocol/subscriptionId": id}}),
        "{fin}"
    );
    let r = p
        .request(
            "events/poll",
            json!({"name": EVENT_REQUESTED, "cursor": "c0"}),
        )
        .await;
    assert_eq!(r["error"]["code"], -32024, "{r}");
}

#[tokio::test]
async fn two_agents_hear_the_same_event_and_only_one_claim_wins() {
    let shared = fresh();
    let mut a = start(&shared, "alpha").modern(true);
    let mut b = start(&shared, "beta").modern(true);
    let ca = a
        .request("events/poll", json!({"name": EVENT_REQUESTED}))
        .await["result"]["cursor"]
        .clone();
    let cb = b
        .request("events/poll", json!({"name": EVENT_REQUESTED}))
        .await["result"]["cursor"]
        .clone();
    shared.create_work("w-race");
    let ra = a
        .request(
            "events/poll",
            json!({"name": EVENT_REQUESTED, "cursor": ca}),
        )
        .await;
    let rb = b
        .request(
            "events/poll",
            json!({"name": EVENT_REQUESTED, "cursor": cb}),
        )
        .await;
    assert_eq!(
        ra["result"]["events"][0]["eventId"], rb["result"]["events"][0]["eventId"],
        "both principals receive the same event"
    );
    let (x, y) = tokio::join!(claim(&mut a), claim(&mut b));
    let wins = [&x, &y]
        .iter()
        .filter(|r| r["result"]["isError"] == false)
        .count();
    assert_eq!(wins, 1, "{x} {y}");
    let loser = if x["result"]["isError"] == true {
        &x
    } else {
        &y
    };
    assert_eq!(
        loser["result"]["structuredContent"]["code"], "ALREADY_CLAIMED",
        "{loser}"
    );
}

#[tokio::test]
async fn the_server_tells_the_app_what_the_host_negotiated_once() {
    let shared = fresh();
    let mut c = start(&shared, "a").modern(true);
    let _ = c.request("tools/list", json!({})).await;
    let _ = c.request("tools/list", json!({})).await;
    let _ = c.request("skills/list", json!({})).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let seen = shared.log.lock().unwrap().observations.clone();
    assert!(!seen.is_empty());
    let first = &seen[0];
    assert_eq!(first.client_name, "harness");
    assert_eq!(first.client_version, "1.2.3");
    assert_eq!(first.profile, "2026-07-28");
    assert!(first.events);
    // Repeating the same request sends nothing new; using skills is a change.
    assert!(seen.len() <= 2, "{seen:?}");
    assert!(seen.last().unwrap().skills);
    let _ = EVENT_UPDATED;
}

async fn claim(c: &mut Client) -> Value {
    c.request(
        "tools/call",
        json!({"name": "musaic_work_claim", "arguments": {"workId": "w-race", "idempotencyKey": "key-12345678"}}),
    )
    .await
}

#[tokio::test]
async fn the_ninth_concurrent_stream_is_refused_and_a_slot_frees_on_cancel() {
    let shared = fresh();
    let mut c = start(&shared, "a").modern(true);
    let mut ids = Vec::new();
    for _ in 0..8 {
        ids.push(open_stream(&mut c, json!({"heartbeatMs": 5_000})).await);
    }
    // Each opens with an `active`.
    for _ in 0..8 {
        let n = c.notification(Duration::from_secs(10)).await.unwrap();
        assert_eq!(n["method"], "notifications/events/active");
    }
    let ninth = c
        .request("events/stream", json!({"name": EVENT_REQUESTED}))
        .await;
    assert_eq!(ninth["error"]["code"], -32025, "{ninth}");
    assert_eq!(
        ninth["error"]["data"],
        json!({"limit": "streams", "max": 8}),
        "{ninth}"
    );
    c.send(
        json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId": ids[0]}}),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let again = c
        .start_request(
            "events/stream",
            json!({"name": EVENT_REQUESTED, "heartbeatMs": 5_000}),
        )
        .await;
    let n = c.notification(Duration::from_secs(10)).await.unwrap();
    assert_eq!(n["method"], "notifications/events/active", "{n}");
    let _ = again;
}
