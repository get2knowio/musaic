//! Exercise the real stdio CLI, rmcp and authenticated loopback bridge.

mod common;

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use common::World;
use musaic_commands::PermissionKind as K;
use serde_json::{Value, json};

struct Host {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    next: u64,
}

impl Host {
    fn new(world: &World, pairing: &str) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_musaic-cli"))
            .arg("--config-dir")
            .arg(&world.config)
            .args(["--dev-credentials", "--pairing", pairing, "mcp"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        Self {
            input: child.stdin.take().unwrap(),
            output: BufReader::new(child.stdout.take().unwrap()),
            child,
            next: 1,
        }
    }

    fn send(&mut self, request: &Value) {
        writeln!(self.input, "{request}").unwrap();
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next;
        self.next += 1;
        self.send(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}));
        let mut line = String::new();
        assert!(
            self.output.read_line(&mut line).unwrap() > 0,
            "{method}: no reply"
        );
        let reply: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(reply["id"], id, "{method}: {reply}");
        reply
    }

    fn legacy(&mut self) {
        let reply = self.request(
            "initialize",
            json!({
                "protocolVersion": "2025-11-25", "capabilities": {},
                "clientInfo": {"name":"musaic-test-host", "version":"1"}
            }),
        );
        assert_eq!(reply["result"]["protocolVersion"], "2025-11-25", "{reply}");
        self.send(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
    }

    fn call(&mut self, name: &str, args: Value, capabilities: Option<bool>) -> Value {
        let mut params = json!({"name":name,"arguments":args});
        if let Some(tasks) = capabilities {
            params["_meta"] = meta(tasks);
        }
        self.request("tools/call", params)
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn meta(tasks: bool) -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientInfo": {"name":"musaic-test-host", "version":"1"},
        "io.modelcontextprotocol/clientCapabilities": if tasks {
            json!({"extensions":{"io.modelcontextprotocol/tasks":{}}})
        } else {
            json!({})
        }
    })
}

fn new_scan(world: &World, pairing: &str, key: &str) -> Value {
    let capabilities = world
        .client(pairing)
        .call("musaic_capabilities", &json!({}))
        .unwrap();
    json!({"configuredRootId": capabilities["configuredRoots"][0]["rootId"], "idempotencyKey":key})
}

fn status(host: &mut Host, task: &Value) -> Value {
    host.request("tasks/get", json!({"taskId":task,"_meta":meta(true)}))
}

#[test]
fn modern_scan_replays_its_task_after_cli_disconnect_and_app_restart() {
    let mut world = World::with(true, true);
    let pairing = world.pair(&[K::LibraryRead, K::ComputeScan]);
    let other = world.pair(&[K::LibraryRead, K::ComputeScan]);
    let args = new_scan(&world, &pairing, "task-scan-restart");
    let mut first = Host::new(&world, &pairing);
    let discover = first.request("server/discover", json!({"_meta":meta(true)}));
    assert!(
        discover
            .to_string()
            .contains("io.modelcontextprotocol/tasks"),
        "{discover}"
    );
    let called = first.call("musaic_scan_start", args.clone(), Some(true));
    let result = &called["result"];
    assert_eq!(result["resultType"], "task", "{called}");
    assert_eq!(result["status"], "working");
    assert_eq!(result["ttlMs"], Value::Null);
    let task = result["taskId"].clone();
    let operation = result["_meta"]["musaic/operationId"].clone();
    assert!(task.is_string() && operation.is_string() && task != operation);
    let omitted = first.call("musaic_scan_start", args.clone(), Some(false));
    assert_eq!(
        omitted["result"]["structuredContent"]["operationId"], operation,
        "{omitted}"
    );
    assert_eq!(omitted["result"]["resultType"], "complete");
    let no_capability = first.request("tasks/get", json!({"taskId":task,"_meta":meta(false)}));
    assert_eq!(no_capability["error"]["code"], -32021, "{no_capability}");
    let scoped = status(&mut Host::new(&world, &other), &task);
    assert_eq!(scoped["error"]["data"]["code"], "NOT_FOUND", "{scoped}");
    drop(first);
    world.restart();

    let mut resumed = Host::new(&world, &pairing);
    let replay = resumed.call("musaic_scan_start", args, Some(true));
    assert_eq!(replay["result"]["taskId"], task, "{replay}");
    let waiting = status(&mut resumed, &task);
    assert_eq!(waiting["result"]["status"], "working", "{waiting}");
    let unknown = resumed.request(
        "tasks/update",
        json!({
            "taskId":task, "inputResponses":{"approve":{"action":"accept"}}, "_meta":meta(true)
        }),
    );
    assert_eq!(unknown["result"]["resultType"], "complete", "{unknown}");
    let op = world
        .client(&pairing)
        .call("musaic_operation_get", &json!({"operationId":operation}))
        .unwrap();
    let approval = op["approvalId"].as_str().unwrap();
    world
        .state
        .ops()
        .agent()
        .decide_approval(approval, true)
        .unwrap();
    let end = Instant::now() + Duration::from_secs(15);
    let completed = loop {
        let reply = status(&mut resumed, &task);
        assert!(reply.get("error").is_none(), "{reply}");
        if reply["result"]["status"] == "completed" {
            break reply;
        }
        assert!(Instant::now() < end, "{reply}");
        std::thread::sleep(Duration::from_millis(25));
    };
    assert_eq!(
        completed["result"]["result"]["isError"], false,
        "{completed}"
    );
    assert_eq!(
        completed["result"]["result"]["structuredContent"]["operationId"],
        operation
    );
    assert!(completed["result"]["result"]["structuredContent"]["result"]["scanAlbums"].is_number());
    assert!(completed["result"]["ttlMs"].as_u64().unwrap() >= 604_800_000);
    trace("modern seed", &called);
    trace("modern replay after restart", &replay);
    trace("modern unknown input acknowledgement", &unknown);
    trace("modern final scan result", &completed);
    trace("modern unnegotiated call", &omitted);
    world.stop();
    let stopped = status(&mut resumed, &task);
    assert_eq!(
        stopped["error"]["data"]["code"], "APP_NOT_RUNNING",
        "{stopped}"
    );
}

#[test]
fn native_cancel_acknowledges_and_legacy_polling_observes_the_same_operation() {
    let world = World::with(true, true);
    let pairing = world.pair(&[K::LibraryRead, K::ComputeScan]);
    let mut legacy = Host::new(&world, &pairing);
    legacy.legacy();
    let args = new_scan(&world, &pairing, "task-scan-cancel");
    let start = legacy.call("musaic_scan_start", args.clone(), None);
    assert!(start["result"].get("resultType").is_none(), "{start}");
    let handle = &start["result"]["structuredContent"];
    let op = &handle["operationId"];
    assert_eq!(handle["state"], "awaiting_approval", "{start}");
    assert!(handle["pollIntervalMs"].is_number(), "{start}");

    let mut modern = Host::new(&world, &pairing);
    let called = modern.call("musaic_scan_start", args, Some(true));
    let task = &called["result"]["taskId"];
    assert_eq!(
        called["result"]["_meta"]["musaic/operationId"], *op,
        "{called}"
    );
    let cancel = modern.request("tasks/cancel", json!({"taskId":task,"_meta":meta(true)}));
    assert_eq!(cancel["result"]["resultType"], "complete", "{cancel}");
    assert!(cancel["result"].get("status").is_none(), "{cancel}");
    let terminal = status(&mut modern, task);
    assert_eq!(terminal["result"]["status"], "cancelled", "{terminal}");
    let legacy_status = legacy.call("musaic_operation_get", json!({"operationId":op}), None);
    assert_eq!(
        legacy_status["result"]["structuredContent"]["state"], "cancelled",
        "{legacy_status}"
    );
    trace("legacy start", &start);
    trace("modern cancel acknowledgement", &cancel);
    trace("modern terminal cancellation", &terminal);
    trace("legacy operation poll", &legacy_status);
}

#[test]
fn input_requests_replay_once_and_denied_tools_complete_with_an_error_result() {
    let world = World::with(true, true);
    let pairing = world.pair(&[K::LibraryRead, K::ComputeScan]);
    let mut modern = Host::new(&world, &pairing);
    let args = new_scan(&world, &pairing, "task-scan-input-test");
    let called = modern.call("musaic_scan_start", args, Some(true));
    let task = &called["result"]["taskId"];
    let op = &called["result"]["_meta"]["musaic/operationId"];
    let conn = musaic_db::open_connection(world.state.db_path()).unwrap();
    // The production starts have all required input. Seed an application
    // input request to exercise transport and persistence without asking the
    // host for information the actual scan does not need.
    conn.execute(
        "INSERT INTO mcp_task_inputs(task_id,input_key,request) VALUES (?1,'roots',?2)",
        [task.as_str().unwrap(), r#"{"method":"roots/list"}"#],
    )
    .unwrap();
    let waiting = status(&mut modern, task);
    assert_eq!(waiting["result"]["status"], "input_required", "{waiting}");
    assert_eq!(
        waiting["result"]["inputRequests"]["roots"]["method"],
        "roots/list"
    );
    let request =
        json!({"taskId": task, "inputResponses": {"roots": {"roots":[]}}, "_meta":meta(true)});
    let update = modern.request("tasks/update", request.clone());
    assert_eq!(update["result"]["resultType"], "complete", "{update}");
    let replay = modern.request("tasks/update", request);
    assert_eq!(replay["result"]["resultType"], "complete", "{replay}");
    let active = status(&mut modern, task);
    assert_eq!(active["result"]["status"], "working", "{active}");
    let operation = world
        .client(&pairing)
        .call("musaic_operation_get", &json!({"operationId":op}))
        .unwrap();
    assert_eq!(operation["state"], "awaiting_approval");
    let approval = operation["approvalId"].as_str().unwrap();
    world
        .state
        .ops()
        .agent()
        .decide_approval(approval, false)
        .unwrap();
    let denied = status(&mut modern, task);
    assert_eq!(denied["result"]["status"], "completed", "{denied}");
    assert_eq!(denied["result"]["result"]["isError"], true, "{denied}");
    assert_eq!(
        denied["result"]["result"]["structuredContent"]["code"], "DENIED",
        "{denied}"
    );
    trace("modern fixture input request", &waiting);
    trace("modern input acknowledgement", &update);
    trace("modern identical input replay", &replay);
    trace("modern desktop denial result", &denied);
}

fn trace(label: &str, reply: &Value) {
    if std::env::var_os("MUSAIC_TASK_TRANSCRIPT").is_none() {
        return;
    }
    // Serialize only replies, replacing opaque scratch identifiers and their
    // repeated copies inside CallToolResult text. No handshake is captured.
    let text = reply.to_string();
    let mut output = text.clone();
    for word in text.split(|c: char| !c.is_ascii_alphanumeric() && c != '_') {
        for (prefix, redacted) in [
            ("task_", "<task>"),
            ("op_", "<operation>"),
            ("ap_", "<approval>"),
        ] {
            if word
                .strip_prefix(prefix)
                .is_some_and(|id| id.len() >= 16 && id.bytes().all(|c| c.is_ascii_hexdigit()))
            {
                output = output.replace(word, redacted);
            }
        }
    }
    println!("TRANSCRIPT {label} {output}");
}

#[test]
fn negotiated_analysis_returns_a_handle_and_reports_its_actual_file_work() {
    let world = World::with(true, true);
    let pairing = world.pair(&[K::LibraryRead, K::ComputeAnalyze, K::TagsWrite]);
    let client = world.client(&pairing);
    let albums = client
        .call("musaic_library_list", &json!({"limit":1}))
        .unwrap();
    let album = &albums["items"][0]["albumId"];
    let mut host = Host::new(&world, &pairing);
    let started = host.call(
        "musaic_analysis_start",
        json!({"albumIds":[album], "kinds":["replaygain"], "idempotencyKey":"task-analysis-fixture"}),
        Some(true),
    );
    assert_eq!(started["result"]["resultType"], "task", "{started}");
    assert_eq!(started["result"]["status"], "working", "{started}");
    let task = &started["result"]["taskId"];
    let operation = &started["result"]["_meta"]["musaic/operationId"];
    let legacy = client
        .call("musaic_operation_get", &json!({"operationId":operation}))
        .unwrap();
    assert_eq!(legacy["state"], "awaiting_approval");
    world
        .state
        .ops()
        .agent()
        .decide_approval(legacy["approvalId"].as_str().unwrap(), true)
        .unwrap();
    let end = Instant::now() + Duration::from_secs(15);
    let completed = loop {
        let reply = status(&mut host, task);
        assert!(reply.get("error").is_none(), "{reply}");
        if reply["result"]["status"] == "completed" {
            break reply;
        }
        assert!(Instant::now() < end, "{reply}");
        std::thread::sleep(Duration::from_millis(25));
    };
    assert_eq!(
        completed["result"]["result"]["isError"], false,
        "{completed}"
    );
    let result = &completed["result"]["result"]["structuredContent"];
    assert_eq!(
        result["result"]["analysis"],
        json!([{"albumId":album, "kind":"replaygain", "status":"ok"}])
    );
    assert_eq!(result["counts"], json!({"done":1, "total":1, "failed":0}));
}
