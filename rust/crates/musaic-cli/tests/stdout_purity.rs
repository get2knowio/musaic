//! `musaic-cli mcp` writes protocol messages to stdout and nothing else
//! (T039, SC-006), and no credential reaches stdout, stderr or the app's log.

mod common;

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

use common::World;
use musaic_commands::PermissionKind as K;
use serde_json::{Value, json};

fn spawn(config: &std::path::Path, dev: bool) -> std::process::Child {
    let mut c = Command::new(env!("CARGO_BIN_EXE_musaic-cli"));
    c.arg("--config-dir").arg(config);
    if dev {
        c.arg("--dev-credentials");
    }
    c.arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    c.spawn().unwrap()
}

/// Send the requests, close stdin, and read every line of stdout and all of stderr.
fn talk(mut child: std::process::Child, requests: &[Value]) -> (Vec<String>, String) {
    let mut stdin = child.stdin.take().unwrap();
    for r in requests {
        writeln!(stdin, "{r}").unwrap();
    }
    drop(stdin);
    let stdout = BufReader::new(child.stdout.take().unwrap());
    let lines: Vec<String> = stdout.lines().map(|l| l.unwrap()).collect();
    let mut err = String::new();
    std::io::Read::read_to_string(&mut child.stderr.take().unwrap(), &mut err).unwrap();
    let status = child.wait().unwrap();
    assert!(status.success(), "exit {status:?}; stderr: {err}");
    (lines, err)
}

fn meta() -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientCapabilities": {},
        "io.modelcontextprotocol/clientInfo": {"name": "t", "version": "1"}
    })
}

fn legacy_session() -> Vec<Value> {
    vec![
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}),
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
        json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"musaic_library_list","arguments":{"limit":3}}}),
    ]
}

fn modern_session() -> Vec<Value> {
    vec![
        json!({"jsonrpc":"2.0","id":1,"method":"server/discover","params":{"_meta": meta()}}),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{"_meta": meta()}}),
        json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"musaic_library_list","arguments":{"limit":3},"_meta": meta()}}),
    ]
}

fn parse_all(lines: &[String]) -> Vec<Value> {
    lines
        .iter()
        .map(|l| {
            let v: Value = serde_json::from_str(l)
                .unwrap_or_else(|_| panic!("stdout line is not JSON: {l:?}"));
            assert_eq!(v["jsonrpc"], "2.0", "{l}");
            v
        })
        .collect()
}

#[test]
fn both_profiles_carry_protocol_messages_only_and_the_tools_work_end_to_end() {
    let w = World::with(true, true);
    let id = w.pair(&[K::LibraryRead, K::ProposalSubmit]);
    let secret_hex = w.secret_hex(&id);
    assert!(w.config.join("agent-dev-credentials").join(&id).is_file());

    for session in [legacy_session(), modern_session()] {
        let (lines, err) = talk(spawn(&w.config, true), &session);
        let msgs = parse_all(&lines);
        assert_eq!(
            msgs.len(),
            session.iter().filter(|r| r.get("id").is_some()).count(),
            "{lines:?}"
        );
        let call = msgs.iter().find(|m| m["id"] == 3).unwrap();
        assert_eq!(call["result"]["isError"], false, "{call}");
        let items = call["result"]["structuredContent"]["items"]
            .as_array()
            .unwrap();
        assert_eq!(items.len(), 3);
        let tools = msgs.iter().find(|m| m["id"] == 2).unwrap();
        assert_eq!(tools["result"]["tools"].as_array().unwrap().len(), 21);
        for text in [lines.join("\n"), err] {
            assert!(!text.contains(&secret_hex), "the secret reached the output");
        }
    }
    let log = w.log.lock().unwrap().join("\n");
    assert!(!log.contains(&secret_hex));
}

#[test]
fn with_the_app_missing_stdout_stays_protocol_only_and_diagnostics_go_to_stderr() {
    let mut w = World::with(true, false);
    let id = w.pair(&[K::LibraryRead]);
    let secret_hex = w.secret_hex(&id);
    w.stop();
    // No requests at all: stdout stays empty and the process ends cleanly.
    let (lines, _) = talk(spawn(&w.config, true), &[]);
    assert!(lines.is_empty(), "{lines:?}");
    // A call: the failure is a tool result, still a JSON-RPC message.
    let (lines, err) = talk(spawn(&w.config, true), &legacy_session());
    let msgs = parse_all(&lines);
    let call = msgs.iter().find(|m| m["id"] == 3).unwrap();
    assert_eq!(call["result"]["isError"], true);
    assert_eq!(
        call["result"]["structuredContent"]["code"],
        "APP_NOT_RUNNING"
    );
    assert!(!lines.join("\n").contains(&secret_hex) && !err.contains(&secret_hex));
}

#[test]
fn a_refused_dev_credential_flag_is_a_stderr_message_and_an_empty_stdout() {
    // A config dir outside any scratch directory cannot use --dev-credentials.
    let mut c = Command::new(env!("CARGO_BIN_EXE_musaic-cli"));
    c.args([
        "--config-dir",
        "/Users/nobody/.musaic-not-here",
        "--dev-credentials",
        "mcp",
    ])
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    let out = c.output().unwrap();
    assert!(
        out.stdout.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("dev-credentials"));
}

#[test]
fn the_call_command_prints_one_json_document_and_the_exit_codes_follow_the_contract() {
    let mut w = World::with(true, true);
    let id = w.pair(&[K::LibraryRead]);
    let config = w.config.clone();
    let run = |args: &[&str]| {
        let mut c = Command::new(env!("CARGO_BIN_EXE_musaic-cli"));
        c.arg("--config-dir")
            .arg(&config)
            .arg("--dev-credentials")
            .args(args)
            .stdin(Stdio::null());
        c.output().unwrap()
    };
    let out = run(&["library-list", "--input", r#"{"limit":2}"#]);
    assert_eq!(out.status.code(), Some(0));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["ok"], true);
    assert_eq!(v["result"]["items"].as_array().unwrap().len(), 2);
    // A tool error: 2.
    let out = run(&["album-get", "--input", r#"{"albumId":"0123456789abcdef"}"#]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(
        serde_json::from_slice::<Value>(&out.stdout).unwrap()["error"]["code"],
        "NOT_FOUND"
    );
    // Usage: 64.
    let out = run(&["call", "no-such-tool"]);
    assert_eq!(out.status.code(), Some(64));
    let out = run(&["library-list", "--input", "not json"]);
    assert_eq!(out.status.code(), Some(64));
    // The same call by `call <name>`, with the input from stdin.
    let mut c = Command::new(env!("CARGO_BIN_EXE_musaic-cli"));
    c.arg("--config-dir")
        .arg(&config)
        .arg("--dev-credentials")
        .args(["call", "capabilities"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    let mut child = c.spawn().unwrap();
    child.stdin.take().unwrap().write_all(b"{}").unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["result"]["scopes"], json!(["library.read"]));
    assert!(!String::from_utf8_lossy(&out.stdout).contains(&w.secret_hex(&id)));
    // Transport failure: 3, with the remedy.
    w.stop();
    let out = run(&["capabilities"]);
    assert_eq!(out.status.code(), Some(3));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["error"]["code"], "APP_NOT_RUNNING");
    assert!(
        v["error"]["remedy"]
            .as_str()
            .unwrap()
            .contains("never starts Musaic")
    );
    // `tools` needs no app.
    let out = run(&["tools"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        serde_json::from_slice::<Value>(&out.stdout).unwrap()["tools"]
            .as_array()
            .unwrap()
            .len(),
        21
    );
}
