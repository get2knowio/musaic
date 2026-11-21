//! The CLI against a real in-process app (T038): discovery, the mutual
//! handshake and the read tools, and every way the app can be missing or
//! something else can be on its port.

mod common;

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use common::World;
use musaic_cli::client::{Client, config_id_of};
use musaic_cli::credentials::Memory;
use musaic_commands::descriptor::{self, Descriptor};
use musaic_commands::{PermissionKind as K, ToolErrorCode as C};
use serde_json::json;

const READ: &[K] = &[K::LibraryRead, K::ProposalSubmit];

#[test]
fn the_cli_lists_the_albums_the_desktop_lists() {
    let w = World::new();
    let id = w.pair(READ);
    let client = w.client(&id);
    let r = client
        .call("musaic_library_list", &json!({"limit": 100}))
        .unwrap();
    let got: Vec<String> = r["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["albumId"].as_str().unwrap().to_string())
        .collect();
    let ops = w.state.ops();
    let want: Vec<String> = ops
        .library()
        .list_albums(false)
        .unwrap()
        .albums
        .iter()
        .map(|a| a.id().to_string())
        .collect();
    assert!(want.len() > 3);
    assert_eq!(got, want, "SC-001");
    // And one album, with its revision.
    let one = client
        .call("musaic_album_get", &json!({"albumId": want[0]}))
        .unwrap();
    assert_eq!(one["album"]["albumId"], want[0]);
    // A tool error carries its code.
    let e = client
        .call("musaic_album_get", &json!({"albumId": "0123456789abcdef"}))
        .unwrap_err();
    assert_eq!(e.code, C::NotFound);
}

#[test]
fn a_missing_or_stale_descriptor_means_the_app_is_not_running() {
    let w = World::with(false, false);
    let id = w.pair(READ);
    let client = w.client(&id);
    assert!(client.call("musaic_capabilities", &json!({})).is_ok());
    let real = descriptor::read_validated(&w.config).unwrap();

    // No descriptor at all.
    let other = tempfile::tempdir().unwrap();
    let c = Client::new(other.path(), Box::new(Memory::new()), None);
    assert_eq!(
        c.call("musaic_capabilities", &json!({})).unwrap_err().code,
        C::AppNotRunning
    );

    // A descriptor whose port nobody listens on.
    let dead = {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let d = Descriptor {
        port: u32::from(dead),
        ..real.clone()
    };
    descriptor::write_atomic_owner_only(other.path(), &d).unwrap();
    let c = Client::new(other.path(), Box::new(Memory::new()), None);
    assert_eq!(
        c.call("musaic_capabilities", &json!({})).unwrap_err().code,
        C::AppNotRunning
    );

    // A live port with another instance id.
    let d = Descriptor {
        instance_id: "i_ffffffffffffffff".into(),
        ..real
    };
    descriptor::write_atomic_owner_only(other.path(), &d).unwrap();
    let c = Client::new(other.path(), Box::new(Memory::new()), None);
    assert_eq!(
        c.call("musaic_capabilities", &json!({})).unwrap_err().code,
        C::AppNotRunning
    );
}

#[cfg(unix)]
#[test]
fn a_symlinked_descriptor_is_refused() {
    let w = World::with(false, false);
    let id = w.pair(READ);
    let real = w.config.join("agent-instance.json");
    let elsewhere = w.tmp.path().join("elsewhere.json");
    std::fs::rename(&real, &elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &real).unwrap();
    let e = w
        .client(&id)
        .call("musaic_capabilities", &json!({}))
        .unwrap_err();
    assert_eq!(e.code, C::DescriptorInvalid);
}

#[test]
fn a_squatter_on_the_port_learns_no_secret_and_is_not_believed() {
    let w = World::with(false, false);
    let id = w.pair(READ);
    let secret_hex = w.secret_hex(&id);
    // A server that answers hello with whatever instance the descriptor names
    // and then "accepts" any exchange with a made-up proof.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let dir = tempfile::tempdir().unwrap();
    let instance = "i_5555555555555555";
    descriptor::write_atomic_owner_only(
        dir.path(),
        &Descriptor {
            format_version: 1,
            instance_id: instance.into(),
            app_version: "0".into(),
            protocol_profiles: vec![],
            contract_version: "1".into(),
            host: "127.0.0.1".into(),
            port: u32::from(port),
            pid: 1,
        },
    )
    .unwrap();
    let received = Arc::new(Mutex::new(String::new()));
    let r2 = received.clone();
    let t = std::thread::spawn(move || {
        for _ in 0..2 {
            let (mut s, _) = listener.accept().unwrap();
            // Read the whole request: the body can arrive in a later segment than
            // the headers (it does on Windows), and a single read() would miss it.
            let mut raw = Vec::new();
            let mut buf = [0u8; 8192];
            loop {
                let n = s.read(&mut buf).unwrap();
                if n == 0 {
                    break;
                }
                raw.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&raw);
                if let Some(end) = text.find("\r\n\r\n") {
                    let want = text[..end]
                        .lines()
                        .find_map(|l| {
                            let (k, v) = l.split_once(':')?;
                            k.eq_ignore_ascii_case("content-length")
                                .then(|| v.trim().parse::<usize>().ok())?
                        })
                        .unwrap_or(0);
                    if raw.len() >= end + 4 + want {
                        break;
                    }
                }
            }
            let req = String::from_utf8_lossy(&raw).to_string();
            r2.lock().unwrap().push_str(&req);
            let body = if req.contains("/hello") {
                format!(
                    r#"{{"instanceId":"{instance}","serverNonce":"{}","profiles":[],"contractVersion":"1"}}"#,
                    "ab".repeat(16)
                )
            } else {
                format!(
                    r#"{{"token":"stolen","expiresAt":"x","scopes":["library.read"],"serverProof":"{}"}}"#,
                    "cd".repeat(32)
                )
            };
            let resp = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            s.write_all(resp.as_bytes()).unwrap();
        }
    });
    // The client holds the real secret in memory, and the squatter's dir as config.
    let m = Memory::new();
    m.put(&config_id_of(dir.path()), &id, w.secret(&id));
    let client = Client::new(dir.path(), Box::new(m), Some(id.clone()));
    let e = client.call("musaic_capabilities", &json!({})).unwrap_err();
    assert_eq!(e.code, C::InstanceMismatch);
    t.join().unwrap();
    let seen = received.lock().unwrap().clone();
    assert!(
        seen.contains("clientProof"),
        "the exchange was sent: {seen}"
    );
    assert!(
        !seen.contains(&secret_hex),
        "the squatter received the secret"
    );
    assert!(
        !seen.to_lowercase().contains("authorization"),
        "no token was sent"
    );
}

#[test]
fn a_restart_gives_a_new_instance_and_the_cli_finds_it() {
    let mut w = World::with(false, false);
    let id = w.pair(READ);
    let client = w.client(&id);
    let before = client.call("musaic_capabilities", &json!({})).unwrap()["instanceId"]
        .as_str()
        .unwrap()
        .to_string();
    w.restart();
    let after = client.call("musaic_capabilities", &json!({})).unwrap()["instanceId"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(before, after);
}

#[test]
fn another_config_dirs_credential_is_not_a_pairing_here() {
    let w = World::with(false, false);
    let id = w.pair(READ);
    // The credential is registered under another config dir's id.
    let m = Memory::new();
    m.put("ffffffffffffffff", &id, w.secret(&id));
    let c = Client::new(&w.config, Box::new(m), None);
    assert_eq!(
        c.call("musaic_capabilities", &json!({})).unwrap_err().code,
        C::NotPaired
    );
    // Naming the pairing does not help: there is no secret under this config dir.
    let m = Memory::new();
    m.put("ffffffffffffffff", &id, w.secret(&id));
    let c = Client::new(&w.config, Box::new(m), Some(id.clone()));
    assert_eq!(
        c.call("musaic_capabilities", &json!({})).unwrap_err().code,
        C::NotPaired
    );
}

#[test]
fn several_credentials_need_an_explicit_pairing_and_a_revoked_one_stops_working() {
    let w = World::with(false, false);
    let a = w.pair(READ);
    let b = w.pair(READ);
    let m = Memory::new();
    let cfg = config_id_of(&w.config);
    m.put(&cfg, &a, w.secret(&a));
    m.put(&cfg, &b, w.secret(&b));
    let c = Client::new(&w.config, Box::new(m), None);
    let e = c.call("musaic_capabilities", &json!({})).unwrap_err();
    assert_eq!(e.code, C::NotPaired);
    assert!(
        e.message.contains(&a) && e.message.contains(&b),
        "{}",
        e.message
    );
    // Revoked: a client that still holds a copy of the secret is refused
    // (NOT_PAIRED, and never a token).
    let client = w.client(&a);
    assert!(client.call("musaic_capabilities", &json!({})).is_ok());
    let copy = w.secret(&a);
    w.state.ops().agent().revoke_pairing(&a).unwrap();
    let fresh = w.client_with_copy(&a, &copy);
    assert_eq!(
        fresh
            .call("musaic_capabilities", &json!({}))
            .unwrap_err()
            .code,
        C::NotPaired
    );
    // The old session's token stopped at once too.
    assert!(client.call("musaic_capabilities", &json!({})).is_err());
}

impl World {
    fn client_with_copy(&self, id: &str, secret: &[u8]) -> Client {
        let m = Memory::new();
        m.put(&config_id_of(&self.config), id, secret.to_vec());
        Client::new(&self.config, Box::new(m), Some(id.to_string()))
    }
}
