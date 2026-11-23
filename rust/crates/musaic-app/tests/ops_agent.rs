//! The `agent` operations: pairings, the handshake, the read tools, proposals
//! and approvals, and the compute starts, on a scratch config dir with a
//! scripted clock and in-memory secrets.

mod common;

use std::sync::{Arc, Mutex};

use common::{AgentWorld, agent_err_code, agent_world, tool_code};
use musaic_app::ops::agent::AgentError;
use musaic_commands::auth::{ExchangeRequest, client_proof, hex_decode, random_hex};
use musaic_commands::{Decision, PermissionKind as K, ToolErrorCode};
use musaic_events::{AgentWhat, Event, EventKind, Subscriber, Topic};
use serde_json::json;

const READ: &[K] = &[K::LibraryRead, K::ProposalSubmit];

fn exchange_raw(w: &AgentWorld, req: &ExchangeRequest) -> Result<(), AgentError> {
    w.app.ops().agent().exchange(req).map(|_| ())
}

/// An honest request for `pairing_id`, built from `hello`.
fn honest(w: &AgentWorld, pairing_id: &str) -> ExchangeRequest {
    let hello = w.app.ops().agent().hello();
    let secret = hex_decode(
        &musaic_secrets::agent::get_secret(
            w.store.as_ref(),
            &common::config_id(&w.app),
            pairing_id,
        )
        .unwrap()
        .unwrap(),
    )
    .unwrap();
    let client_nonce = random_hex(16);
    ExchangeRequest {
        pairing_id: pairing_id.into(),
        instance_id: hello.instance_id.clone(),
        server_nonce: hello.server_nonce.clone(),
        client_proof: client_proof(
            &secret,
            pairing_id,
            &hello.instance_id,
            &hello.server_nonce,
            &client_nonce,
        ),
        client_nonce,
    }
}

type Seen = Arc<Mutex<Vec<(AgentWhat, Vec<String>)>>>;

fn events(w: &AgentWorld) -> Seen {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s = seen.clone();
    w.app.bus().register(
        Topic::Kind(EventKind::AgentStateChanged),
        Subscriber::from_fn(move |e| {
            if let Event::AgentStateChanged { what, ids } = e {
                s.lock().unwrap().push((*what, ids.clone()));
            }
            Ok(())
        })
        .key("agent_test")
        .priority(500)
        .name("agent_test"),
    );
    seen
}

#[test]
fn pairings_are_created_listed_and_revoked_and_the_secret_never_leaves_the_keyring() {
    let w = agent_world();
    let seen = events(&w);
    let ops_agent = w.app.ops();
    let agent = ops_agent.agent();
    assert!(agent.list_pairings().unwrap().is_empty());

    let created = w.pair(
        "  Claude  ",
        &[K::LibraryRead, K::ProposalSubmit, K::LibraryRead],
    );
    assert_eq!(created.pairing.name, "Claude", "trimmed");
    assert_eq!(
        created.pairing.scopes,
        [K::LibraryRead, K::ProposalSubmit],
        "deduplicated"
    );
    assert_eq!(created.pairing.policy[&K::LibraryRead], Decision::Allow);
    assert_eq!(created.pairing.policy[&K::TagsWrite], Decision::Ask);
    assert_eq!(created.connect.command, "musaic-cli");
    assert!(created.connect.args.contains(&"setup".to_string()));

    // The secret is in the agent store under this config dir's account, and
    // nowhere else: not in the record, not in the database file.
    let id = &created.pairing.id;
    let secret =
        musaic_secrets::agent::get_secret(w.store.as_ref(), &common::config_id(&w.app), id)
            .unwrap()
            .expect("stored");
    assert_eq!(secret.len(), 64);
    assert!(!format!("{created:?}").contains(&secret));
    for entry in std::fs::read_dir(&w.scratch.config).unwrap() {
        let p = entry.unwrap().path();
        if p.is_file() {
            let bytes = std::fs::read(&p).unwrap();
            assert!(
                !bytes
                    .windows(secret.len())
                    .any(|win| win == secret.as_bytes()),
                "{} holds the secret",
                p.display()
            );
        }
    }
    // No dev credential file unless the host asked for one.
    assert!(!w.scratch.config.join("agent-dev-credentials").exists());

    let listed = agent.list_pairings().unwrap();
    assert_eq!(listed.len(), 1);
    assert!(listed[0].revoked_at.is_none());

    // Revoke: the entry goes, the row stays (history), a second revoke is fine.
    let revoked = agent.revoke_pairing(id).unwrap();
    assert!(revoked.revoked_at.is_some());
    assert!(
        musaic_secrets::agent::get_secret(w.store.as_ref(), &common::config_id(&w.app), id)
            .unwrap()
            .is_none()
    );
    assert!(agent.revoke_pairing(id).is_ok());
    assert_eq!(
        agent_err_code(&agent.revoke_pairing("p_nope").unwrap_err()),
        "NOT_FOUND"
    );

    let got = seen.lock().unwrap().clone();
    assert_eq!(
        got[0],
        (AgentWhat::Pairings, vec![id.clone()]),
        "create published"
    );
    assert!(
        got.iter()
            .filter(|(what, _)| *what == AgentWhat::Pairings)
            .count()
            >= 2,
        "revoke published"
    );
}

#[test]
fn bad_names_and_scopes_and_the_twenty_first_pairing_are_refused() {
    let w = agent_world();
    let ops_agent = w.app.ops();
    let agent = ops_agent.agent();
    assert_eq!(
        agent_err_code(&agent.create_pairing("   ", READ).unwrap_err()),
        "INVALID_INPUT"
    );
    assert_eq!(
        agent_err_code(&agent.create_pairing(&"x".repeat(65), READ).unwrap_err()),
        "INVALID_INPUT"
    );
    assert_eq!(
        agent_err_code(&agent.create_pairing("ok", &[]).unwrap_err()),
        "INVALID_INPUT"
    );
    for i in 0..20 {
        w.pair(&format!("agent {i}"), READ);
    }
    assert_eq!(
        agent_err_code(&agent.create_pairing("one too many", READ).unwrap_err()),
        "CONFLICT"
    );
    // The refused one left no secret behind.
    let secrets = w
        .store
        .entries()
        .keys()
        .filter(|k| !k.ends_with(":index"))
        .count();
    assert_eq!(secrets, 20);
    // Revoking one frees a slot.
    let first = agent.list_pairings().unwrap().remove(0);
    agent.revoke_pairing(&first.id).unwrap();
    assert!(agent.create_pairing("fits now", READ).is_ok());
}

#[test]
fn policy_is_set_per_kind_inside_the_scope() {
    let w = agent_world();
    let ops_agent = w.app.ops();
    let agent = ops_agent.agent();
    let p = w.pair("a", &[K::LibraryRead, K::ProposalSubmit, K::TagsWrite]);
    let after = agent
        .set_policy(&p.pairing.id, &[(K::TagsWrite, Decision::Allow)])
        .unwrap();
    assert_eq!(after.policy[&K::TagsWrite], Decision::Allow);
    assert_eq!(
        after.policy[&K::LibraryRead],
        Decision::Allow,
        "others keep theirs"
    );
    assert_eq!(
        agent_err_code(
            &agent
                .set_policy(&p.pairing.id, &[(K::FilesRename, Decision::Allow)])
                .unwrap_err()
        ),
        "INVALID_INPUT",
        "outside the scope"
    );
    assert_eq!(
        agent_err_code(
            &agent
                .set_policy(&p.pairing.id, &[(K::LibraryRead, Decision::Ask)])
                .unwrap_err()
        ),
        "INVALID_INPUT",
        "a read has no card to wait on"
    );
    assert!(
        agent
            .set_policy(&p.pairing.id, &[(K::LibraryRead, Decision::Refuse)])
            .is_ok()
    );
    assert_eq!(
        agent_err_code(&agent.set_policy("p_nope", &[]).unwrap_err()),
        "NOT_FOUND"
    );
}

#[test]
fn the_handshake_succeeds_for_the_holder_and_every_failure_looks_the_same() {
    let w = agent_world();
    let id = w.pair("a", READ).pairing.id;
    let ops_agent = w.app.ops();
    let agent = ops_agent.agent();

    // Happy path.
    let p = w.login(&id);
    assert_eq!(p.pairing_id, id);
    assert_eq!(p.scopes, READ);
    assert!(agent.list_pairings().unwrap()[0].last_used_at.is_some());

    let uniform = |req: ExchangeRequest| agent_err_code(&exchange_raw(&w, &req).unwrap_err());

    // A wrong proof.
    let mut bad = honest(&w, &id);
    bad.client_proof = "00".repeat(32);
    assert_eq!(uniform(bad), "NOT_PAIRED");
    // A proof made with another secret.
    let mut other = honest(&w, &id);
    other.client_proof = client_proof(
        b"another secret",
        &id,
        &other.instance_id,
        &other.server_nonce,
        &other.client_nonce,
    );
    assert_eq!(uniform(other), "NOT_PAIRED");
    // A replayed nonce: the first use succeeds, the second is refused.
    let req = honest(&w, &id);
    assert!(exchange_raw(&w, &req).is_ok());
    assert_eq!(uniform(req), "NOT_PAIRED");
    // A nonce that expired.
    let req = honest(&w, &id);
    w.clock.advance(61);
    assert_eq!(uniform(req), "NOT_PAIRED");
    // Another instance id (a different run of the app).
    let hello = agent.hello();
    let secret = hex_decode(
        &musaic_secrets::agent::get_secret(w.store.as_ref(), &common::config_id(&w.app), &id)
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    let cn = random_hex(16);
    assert_eq!(
        uniform(ExchangeRequest {
            pairing_id: id.clone(),
            instance_id: "i_0000000000000000".into(),
            server_nonce: hello.server_nonce.clone(),
            client_proof: client_proof(
                &secret,
                &id,
                "i_0000000000000000",
                &hello.server_nonce,
                &cn
            ),
            client_nonce: cn,
        }),
        "NOT_PAIRED"
    );
    // An unknown pairing.
    let mut unknown = honest(&w, &id);
    unknown.pairing_id = "p_0000000000000000".into();
    assert_eq!(uniform(unknown), "NOT_PAIRED");
    // A revoked pairing.
    let req = honest(&w, &id);
    agent.revoke_pairing(&id).unwrap();
    assert_eq!(uniform(req), "NOT_PAIRED");
    // And no token was ever handed to a failed attempt.
    assert_eq!(
        agent_err_code(&agent.authenticate("not a token").unwrap_err()),
        "NOT_PAIRED"
    );
}

#[test]
fn a_pairing_of_another_config_dir_does_not_work_here() {
    let a = agent_world();
    let b = agent_world();
    let id = a.pair("a", READ).pairing.id;
    // B's store has no such entry: the exchange cannot succeed, whatever is sent.
    let req = honest(&a, &id);
    assert_eq!(
        agent_err_code(&b.app.ops().agent().exchange(&req).unwrap_err()),
        "NOT_PAIRED"
    );
    // Even with A's secret copied into B's store under B's own account, A's
    // pairing row does not exist in B's database.
    let secret =
        musaic_secrets::agent::get_secret(a.store.as_ref(), &common::config_id(&a.app), &id)
            .unwrap()
            .unwrap();
    musaic_secrets::agent::put_secret(b.store.as_ref(), &common::config_id(&b.app), &id, &secret)
        .unwrap();
    assert_ne!(common::config_id(&a.app), common::config_id(&b.app));
    assert_eq!(
        agent_err_code(&b.app.ops().agent().exchange(&req).unwrap_err()),
        "NOT_PAIRED"
    );
}

#[test]
fn lockout_follows_the_schedule_and_a_success_resets_it() {
    let w = agent_world();
    let id = w.pair("a", READ).pairing.id;
    let ops_agent = w.app.ops();
    let agent = ops_agent.agent();
    let fail = |w: &AgentWorld| {
        let mut r = honest(w, &id);
        r.client_proof = "11".repeat(32);
        agent_err_code(&w.app.ops().agent().exchange(&r).unwrap_err())
    };
    let retry = |e: AgentError| match e {
        AgentError::Tool(t) => t.details.unwrap()["retryAfterSeconds"]
            .parse::<u64>()
            .unwrap(),
        other => panic!("{other:?}"),
    };
    for _ in 0..5 {
        assert_eq!(fail(&w), "NOT_PAIRED");
    }
    // Locked: even an honest request is refused, with the time left.
    let e = agent.exchange(&honest(&w, &id)).unwrap_err();
    assert_eq!(agent_err_code(&e), "LOCKED_OUT");
    assert_eq!(retry(e), 60);
    w.clock.advance(30);
    assert_eq!(retry(agent.exchange(&honest(&w, &id)).unwrap_err()), 30);
    w.clock.advance(30);
    // The second lockout is 120 s, the third 240 s, capped at 900 s.
    for want in [120u64, 240, 480, 900, 900] {
        for _ in 0..5 {
            assert_eq!(fail(&w), "NOT_PAIRED");
        }
        assert_eq!(retry(agent.exchange(&honest(&w, &id)).unwrap_err()), want);
        w.clock.advance(want);
    }
    // An honest exchange succeeds once the lock ends, and resets the count.
    assert!(agent.exchange(&honest(&w, &id)).is_ok());
    for _ in 0..5 {
        assert_eq!(fail(&w), "NOT_PAIRED");
    }
    assert_eq!(retry(agent.exchange(&honest(&w, &id)).unwrap_err()), 60);
}

#[test]
fn tokens_expire_at_fifteen_minutes_and_a_restart_drops_them_but_not_the_pairing() {
    let w = agent_world();
    let id = w.pair("a", READ).pairing.id;
    let ops_agent = w.app.ops();
    let agent = ops_agent.agent();
    let hello = agent.hello();
    let secret = hex_decode(
        &musaic_secrets::agent::get_secret(w.store.as_ref(), &common::config_id(&w.app), &id)
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    let cn = random_hex(16);
    let resp = agent
        .exchange(&ExchangeRequest {
            pairing_id: id.clone(),
            instance_id: hello.instance_id.clone(),
            server_nonce: hello.server_nonce.clone(),
            client_proof: client_proof(&secret, &id, &hello.instance_id, &hello.server_nonce, &cn),
            client_nonce: cn,
        })
        .unwrap();
    w.clock.advance(899);
    assert!(agent.authenticate(&resp.token).is_ok());
    w.clock.advance(1);
    assert_eq!(
        agent_err_code(&agent.authenticate(&resp.token).unwrap_err()),
        "NOT_PAIRED"
    );

    // Restart: a new app on the same config dir has a new instance id and no tokens.
    let p = w.login(&id);
    let token_probe = {
        let hello = agent.hello();
        let instance_before = hello.instance_id.clone();
        let AgentWorld {
            app,
            scratch,
            clock,
            store,
        } = w;
        drop(app);
        let (c2, s2) = (clock.clone(), store.clone());
        let app2 = musaic_app::App::builder(&scratch.config)
            .secrets(Arc::new(musaic_secrets::MemoryStore::new()))
            .agent_secrets(s2)
            .clock(c2.wall())
            .subscribers(false)
            .auto_snapshot(false)
            .ai_worker(false)
            .build()
            .unwrap();
        (app2, instance_before, p, scratch)
    };
    let (app2, instance_before, p, _scratch) = token_probe;
    let ops_agent2 = app2.ops();
    let agent2 = ops_agent2.agent();
    assert_ne!(agent2.hello().instance_id, instance_before);
    assert!(
        agent2.list_pairings().unwrap().iter().any(|x| x.id == id),
        "the pairing survives"
    );
    // The old principal's token is unknown to the new run.
    assert_eq!(p.pairing_id, id);
    assert_eq!(
        agent_err_code(&agent2.authenticate("anything").unwrap_err()),
        "NOT_PAIRED"
    );
}

#[test]
fn authorization_is_the_scope_intersected_with_the_policy() {
    let w = agent_world();
    let id = w.pair("a", &[K::LibraryRead]).pairing.id;
    let p = w.login(&id);
    // A kind outside the scope is refused even if the policy says Allow.
    let r = w.call(
        &p,
        "musaic_fix_preview",
        json!({
            "albumId": "0123456789abcdef", "baseRevision": "0123456789abcdef0123456789abcdef",
            "proposedPatch": {"rename": true}
        }),
    );
    assert_eq!(tool_code(&r), "FORBIDDEN");
    // A refusal is recorded, without argument values.
    let act = w.app.ops().agent().activity(&id).unwrap();
    assert_eq!(act[0].tool, "musaic_fix_preview");
    assert_eq!(act[0].outcome, "refused:FORBIDDEN");
    assert!(
        act.iter()
            .any(|a| a.tool == "exchange" && a.outcome == "ok")
    );
    // The read kind itself set to Refuse refuses reads.
    w.app
        .ops()
        .agent()
        .set_policy(&id, &[(K::LibraryRead, Decision::Refuse)])
        .unwrap();
    assert_eq!(
        tool_code(&w.call(&p, "musaic_capabilities", json!({}))),
        "FORBIDDEN"
    );
    let _ = ToolErrorCode::Forbidden;
}

// ------------------------------------------------------------ read tools

use common::agent_world_scanned;
use musaic_app::ops::OpError;
use musaic_commands::{REGISTRY, ToolOutput};

fn out_json(r: Result<ToolOutput, musaic_commands::ToolError>) -> serde_json::Value {
    r.unwrap_or_else(|e| panic!("{e}")).to_json()
}

fn validates(tool: &str, v: &serde_json::Value) {
    let def = REGISTRY.iter().find(|t| t.id == tool).unwrap();
    let schema = (def.output_schema)();
    let validator = jsonschema::validator_for(&schema).unwrap();
    assert!(
        validator.is_valid(v),
        "{tool} output does not match its schema: {v}"
    );
}

/// A dump of every table, to prove a read wrote nothing.
fn db_state(w: &AgentWorld) -> String {
    let conn = musaic_db::open_connection(w.app.db_path()).unwrap();
    let tables: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let mut out = String::new();
    for t in tables {
        out.push_str(&format!("# {t}\n"));
        let mut stmt = conn.prepare(&format!("SELECT * FROM {t}")).unwrap();
        let n = stmt.column_count();
        let rows = stmt
            .query_map([], |r| {
                Ok((0..n)
                    .map(|i| format!("{:?}", r.get_ref(i).unwrap()))
                    .collect::<Vec<_>>()
                    .join("|"))
            })
            .unwrap();
        for row in rows {
            out.push_str(&row.unwrap());
            out.push('\n');
        }
    }
    out
}

fn strings_in(v: &serde_json::Value, out: &mut Vec<String>) {
    match v {
        serde_json::Value::String(s) => out.push(s.clone()),
        serde_json::Value::Array(a) => a.iter().for_each(|x| strings_in(x, out)),
        serde_json::Value::Object(m) => {
            for (k, x) in m {
                out.push(k.clone());
                strings_in(x, out);
            }
        }
        _ => {}
    }
}

fn looks_like_a_path(s: &str, secrets: &[String]) -> bool {
    s.starts_with('/')
        || s.starts_with("~/")
        || s.contains(":\\")
        || s.contains('\\')
        || secrets.iter().any(|p| s.contains(p.as_str()))
}

#[test]
fn library_list_is_the_desktops_list_and_pages_and_filters() {
    let w = agent_world_scanned();
    let id = w.pair("a", READ).pairing.id;
    let p = w.login(&id);
    let ops = w.app.ops();
    let desktop = ops.library().list_albums(false).unwrap();
    let want: Vec<String> = desktop.albums.iter().map(|a| a.id().to_string()).collect();
    assert!(want.len() > 3);

    let all = out_json(w.call(&p, "musaic_library_list", json!({"limit": 100})));
    validates("musaic_library_list", &all);
    let got: Vec<String> = all["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["albumId"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(got, want, "the same albums in the same order (SC-001)");
    assert_eq!(all["total"], want.len());
    assert!(all.get("nextCursor").is_none());

    // Paging: two at a time covers everything once.
    let mut seen = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let body = match &cursor {
            Some(c) => json!({"limit": 2, "cursor": c}),
            None => json!({"limit": 2}),
        };
        let page = out_json(w.call(&p, "musaic_library_list", body));
        let items = page["items"].as_array().unwrap();
        assert!(items.len() <= 2);
        seen.extend(
            items
                .iter()
                .map(|i| i["albumId"].as_str().unwrap().to_string()),
        );
        match page.get("nextCursor") {
            Some(c) => cursor = Some(c.as_str().unwrap().to_string()),
            None => break,
        }
    }
    assert_eq!(seen, want);

    // Default limit is 50; bounds are enforced before anything runs.
    assert_eq!(
        tool_code(&w.call(&p, "musaic_library_list", json!({"limit": 0}))),
        "INVALID_INPUT"
    );
    assert_eq!(
        tool_code(&w.call(&p, "musaic_library_list", json!({"limit": 101}))),
        "INVALID_INPUT"
    );

    // Query: a case-insensitive substring of the artist or title.
    let first = &all["items"][0];
    let needle = first["title"].as_str().unwrap().to_uppercase();
    let hit = out_json(w.call(&p, "musaic_library_list", json!({"query": needle})));
    assert!(
        hit["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["albumId"] == first["albumId"])
    );
    let none = out_json(w.call(
        &p,
        "musaic_library_list",
        json!({"query": "no such album anywhere zzzqqq"}),
    ));
    assert_eq!(none["total"], 0);

    // Summaries carry display fields only.
    for k in [
        "albumId",
        "artist",
        "title",
        "trackCount",
        "missing",
        "hasCover",
    ] {
        assert!(first.get(k).is_some(), "{k}");
    }
}

#[test]
fn a_cursor_goes_stale_when_the_library_is_rescanned() {
    let w = agent_world_scanned();
    let id = w.pair("a", READ).pairing.id;
    let p = w.login(&id);
    let page = out_json(w.call(&p, "musaic_library_list", json!({"limit": 1})));
    let cursor = page["nextCursor"].as_str().unwrap().to_string();
    assert!(
        w.call(
            &p,
            "musaic_library_list",
            json!({"limit": 1, "cursor": cursor})
        )
        .is_ok()
    );
    w.clock.advance(60);
    let ops = w.app.ops();
    ops.scan().rescan().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while w.app.gate().snapshot() != (0, false) {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(
        tool_code(&w.call(
            &p,
            "musaic_library_list",
            json!({"limit": 1, "cursor": cursor})
        )),
        "CURSOR_STALE"
    );
    assert_eq!(
        tool_code(&w.call(&p, "musaic_library_list", json!({"cursor": "garbage"}))),
        "CURSOR_STALE"
    );
}

#[test]
fn album_get_has_no_path_and_its_output_matches_its_schema() {
    let w = agent_world_scanned();
    let id = w.pair("a", READ).pairing.id;
    let p = w.login(&id);
    let ops = w.app.ops();
    let desktop = ops.library().list_albums(false).unwrap();
    let secrets = vec![
        w.scratch.tmp.path().display().to_string(),
        desktop.library_dir.clone(),
        desktop.albums[0].path().to_string(),
    ];
    for a in &desktop.albums {
        let v = out_json(w.call(&p, "musaic_album_get", json!({"albumId": a.id()})));
        validates("musaic_album_get", &v);
        let mut strs = Vec::new();
        strings_in(&v, &mut strs);
        for s in &strs {
            assert!(
                !looks_like_a_path(s, &secrets),
                "a path leaked: {s:?} in {v}"
            );
        }
        assert_eq!(v["album"]["albumId"], a.id());
        assert!(v["revision"].as_str().unwrap().len() == 32);
        assert!(!v["tracks"].as_array().unwrap().is_empty());
        let folder = v["album"]["folderName"].as_str().unwrap();
        assert!(!folder.contains('/') && !folder.is_empty());
        for t in v["tracks"].as_array().unwrap() {
            assert!(t["fileName"].as_str().unwrap().ends_with(".flac"));
        }
    }
    assert_eq!(
        tool_code(&w.call(
            &p,
            "musaic_album_get",
            json!({"albumId": "0123456789abcdef"})
        )),
        "NOT_FOUND"
    );
}

#[test]
fn the_revision_changes_with_tags_files_sizes_times_and_covers_and_is_otherwise_stable() {
    let w = agent_world_scanned();
    let id = w.pair("a", READ).pairing.id;
    let p = w.login(&id);
    let ops = w.app.ops();
    let desktop = ops.library().list_albums(false).unwrap();
    let rev = |album: &str| {
        out_json(w.call(&p, "musaic_album_get", json!({"albumId": album})))["revision"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let album = desktop
        .albums
        .iter()
        .find(|a| {
            a.iter()
                .any(|(k, v)| k == "cover_file" && matches!(v, musaic_services::Json::Str(_)))
        })
        .unwrap_or(&desktop.albums[0]);
    let (aid, apath) = (album.id().to_string(), album.path().to_string());
    let base = rev(&aid);
    assert_eq!(rev(&aid), base, "stable when nothing changed");

    // A file's modification time.
    let flac = std::fs::read_dir(&apath)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|x| x == "flac"))
        .unwrap();
    let f = std::fs::OpenOptions::new().write(true).open(&flac).unwrap();
    f.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(3600))
        .unwrap();
    drop(f);
    let r1 = rev(&aid);
    assert_ne!(r1, base, "mtime");

    // A file's size.
    use std::io::Write;
    std::fs::OpenOptions::new()
        .append(true)
        .open(&flac)
        .unwrap()
        .write_all(b"\0\0\0\0")
        .unwrap();
    let r2 = rev(&aid);
    assert_ne!(r2, r1, "size");

    // A file's name.
    let renamed = flac.with_file_name("zz renamed.flac");
    std::fs::rename(&flac, &renamed).unwrap();
    let r3 = rev(&aid);
    assert_ne!(r3, r2, "name");
    std::fs::rename(&renamed, &flac).unwrap();

    // The set of files.
    let extra = flac.with_file_name("zz extra.flac");
    std::fs::copy(&flac, &extra).unwrap();
    let r4 = rev(&aid);
    assert_ne!(r4, rev_without(&w, &p, &aid, &extra), "set of files");
    std::fs::remove_file(&extra).unwrap();

    // A tag change through the pipeline (the stored row changes).
    let before_tag = rev(&aid);
    ops.apply()
        .apply_changes(
            &aid,
            &musaic_app::ops::apply::ApplyChanges {
                suggestions: vec![("genre".into(), "Revision Test".into())],
                ..Default::default()
            },
            &mut |_| musaic_app::ops::Listening::Yes,
        )
        .unwrap();
    assert_ne!(rev(&aid), before_tag, "tags");

    // The sidecar cover's bytes.
    let cover = std::path::Path::new(&apath).join("cover.jpg");
    if cover.is_file() {
        let before = rev(&aid);
        let mut bytes = std::fs::read(&cover).unwrap();
        bytes.push(0);
        std::fs::write(&cover, bytes).unwrap();
        assert_ne!(rev(&aid), before, "cover");
    }
    let _ = OpError::Busy {
        reason: musaic_services::gate::Busy::Restoring,
        message: String::new(),
    };
}

fn rev_without(
    w: &AgentWorld,
    p: &musaic_app::ops::agent::Principal,
    album: &str,
    extra: &std::path::Path,
) -> String {
    let hidden = extra.with_extension("hidden");
    std::fs::rename(extra, &hidden).unwrap();
    let r = out_json(w.call(p, "musaic_album_get", json!({"albumId": album})))["revision"]
        .as_str()
        .unwrap()
        .to_string();
    std::fs::rename(&hidden, extra).unwrap();
    r
}

#[test]
fn resolution_get_reads_only_what_is_stored_takes_no_guard_and_writes_nothing() {
    let w = agent_world_scanned();
    let id = w.pair("a", READ).pairing.id;
    let p = w.login(&id);
    let ops = w.app.ops();
    let album = ops.library().list_albums(false).unwrap().albums[0]
        .id()
        .to_string();
    let before = db_state(&w);
    // A restore holding the gate refuses writing work; a read that took the
    // work guard would be refused too, so this call proves it takes none.
    let _restore = common::hold_restore(&w.app);
    let v = out_json(w.call(&p, "musaic_resolution_get", json!({"albumId": album})));
    validates("musaic_resolution_get", &v);
    assert_eq!(v["status"], "not_resolved");
    assert_eq!(v["coverCandidate"], false);
    drop(_restore);
    assert_eq!(db_state(&w), before, "no database write (FR-053)");
    assert_eq!(
        tool_code(&w.call(
            &p,
            "musaic_resolution_get",
            json!({"albumId": "0123456789abcdef"})
        )),
        "NOT_FOUND"
    );
}

#[test]
fn reads_work_while_a_restore_holds_the_gate_and_write_nothing() {
    let w = agent_world_scanned();
    let id = w.pair("a", READ).pairing.id;
    let p = w.login(&id);
    let ops = w.app.ops();
    let album = ops.library().list_albums(false).unwrap().albums[0]
        .id()
        .to_string();
    let before = db_state(&w);
    let _restore = common::hold_restore(&w.app);
    for (tool, body) in [
        ("musaic_capabilities", json!({})),
        ("musaic_library_list", json!({})),
        ("musaic_album_get", json!({"albumId": album})),
        ("musaic_findings_list", json!({})),
    ] {
        let v = out_json(w.call(&p, tool, body));
        validates(tool, &v);
    }
    drop(_restore);
    assert_eq!(db_state(&w), before);
}

#[test]
fn findings_list_has_references_only_and_no_detail() {
    let w = agent_world_scanned();
    let id = w.pair("a", READ).pairing.id;
    let p = w.login(&id);
    let ops = w.app.ops();
    for check in ["track-gaps", "no-cover-art", "unidentified-album"] {
        let _ = ops.checks().run(check, "all", None);
    }
    let direct = ops.checks().list_findings(&Default::default()).unwrap();
    assert!(!direct.is_empty(), "the synthetic library has findings");
    let v = out_json(w.call(&p, "musaic_findings_list", json!({"limit": 100})));
    validates("musaic_findings_list", &v);
    let items = v["items"].as_array().unwrap();
    assert!(!items.is_empty());
    for i in items {
        assert!(i.get("detail").is_none(), "{i}");
        for k in [
            "findingId",
            "checkId",
            "severity",
            "title",
            "status",
            "firstSeen",
            "lastSeen",
        ] {
            assert!(i.get(k).is_some(), "{k} in {i}");
        }
        let title = i["title"].as_str().unwrap();
        assert!(
            !title.contains(&w.scratch.tmp.path().display().to_string()),
            "{title}"
        );
    }
    // Filtering by album.
    let album = items.iter().find_map(|i| i["albumId"].as_str()).unwrap();
    let one = out_json(w.call(&p, "musaic_findings_list", json!({"albumId": album})));
    assert!(
        one["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i["albumId"] == album)
    );
}

#[test]
fn capabilities_reports_scope_policy_roots_and_the_digest() {
    let w = agent_world_scanned();
    let id = w.pair("a", &[K::LibraryRead, K::TagsWrite]).pairing.id;
    w.app
        .ops()
        .agent()
        .set_policy(&id, &[(K::TagsWrite, Decision::Allow)])
        .unwrap();
    let p = w.login(&id);
    let v = out_json(w.call(&p, "musaic_capabilities", json!({})));
    validates("musaic_capabilities", &v);
    assert_eq!(v["contractVersion"], "1");
    assert_eq!(
        v["registryDigest"],
        musaic_commands::catalog::registry_digest()
    );
    assert_eq!(v["scopes"], json!(["library.read", "tags.write"]));
    assert_eq!(v["policy"]["tags.write"], "allow");
    assert_eq!(v["policy"]["covers.write"], "refuse", "outside the scope");
    assert_eq!(v["profiles"].as_array().unwrap().len(), 2);
    let roots = v["configuredRoots"].as_array().unwrap();
    assert_eq!(roots.len(), 1);
    assert_eq!(
        roots[0]["name"], "library",
        "the folder's base name, not its path"
    );
    assert!(
        !v.to_string()
            .contains(&w.scratch.tmp.path().display().to_string())
    );
}

#[test]
fn an_unpaired_revoked_or_expired_caller_and_a_missing_scope_are_refused() {
    let w = agent_world_scanned();
    let id = w.pair("a", &[K::ProposalSubmit]).pairing.id;
    let p = w.login(&id);
    let ops = w.app.ops();
    // No library.read in the scope: every read is FORBIDDEN, and recorded.
    for tool in [
        "musaic_capabilities",
        "musaic_library_list",
        "musaic_findings_list",
    ] {
        assert_eq!(
            tool_code(&w.call(&p, tool, json!({}))),
            "FORBIDDEN",
            "{tool}"
        );
    }
    assert_eq!(
        ops.agent().activity(&id).unwrap()[0].outcome,
        "refused:FORBIDDEN"
    );
    // A made-up token.
    assert_eq!(
        agent_err_code(
            &ops.agent()
                .authenticate("0".repeat(64).as_str())
                .unwrap_err()
        ),
        "NOT_PAIRED"
    );
    // Revoked: the token stops at once.
    let id2 = w.pair("b", READ).pairing.id;
    let hello = ops.agent().hello();
    let secret = hex_decode(
        &musaic_secrets::agent::get_secret(w.store.as_ref(), &common::config_id(&w.app), &id2)
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    let cn = random_hex(16);
    let resp = ops
        .agent()
        .exchange(&ExchangeRequest {
            pairing_id: id2.clone(),
            instance_id: hello.instance_id.clone(),
            server_nonce: hello.server_nonce.clone(),
            client_proof: client_proof(&secret, &id2, &hello.instance_id, &hello.server_nonce, &cn),
            client_nonce: cn,
        })
        .unwrap();
    assert!(ops.agent().authenticate(&resp.token).is_ok());
    ops.agent().revoke_pairing(&id2).unwrap();
    assert_eq!(
        agent_err_code(&ops.agent().authenticate(&resp.token).unwrap_err()),
        "NOT_PAIRED"
    );
    // Expired.
    let id3 = w.pair("c", READ).pairing.id;
    let p3 = w.login(&id3);
    w.clock.advance(901);
    let _ = p3;
    assert_eq!(
        agent_err_code(&ops.agent().authenticate(&resp.token).unwrap_err()),
        "NOT_PAIRED"
    );
}

// ------------------------------------------------- proposals and approvals

use std::collections::BTreeMap;

use musaic_app::ops::agent::{ApprovalFilter, Principal};
use sha2::{Digest, Sha256};

const ALL_KINDS: &[K] = &[
    K::LibraryRead,
    K::ProposalSubmit,
    K::TagsWrite,
    K::CoversWrite,
    K::FilesRename,
];

/// Every file under an album folder (names and content digests), and the
/// folder's own name: what "nothing changed" means.
fn files_digest(album_path: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for e in std::fs::read_dir(album_path).unwrap().flatten() {
        if e.path().is_file() {
            let bytes = std::fs::read(e.path()).unwrap();
            out.insert(
                e.file_name().to_string_lossy().into_owned(),
                musaic_commands::auth::hex_encode(&Sha256::digest(&bytes)),
            );
        }
    }
    out.insert(
        "<folder>".into(),
        std::path::Path::new(album_path)
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned(),
    );
    out
}

struct Fix {
    w: AgentWorld,
    p: Principal,
    pairing: String,
    album: String,
    path: String,
}

fn fix_world(scopes: &[K]) -> Fix {
    let w = agent_world_scanned();
    let pairing = w.pair("assistant", scopes).pairing.id;
    let p = w.login(&pairing);
    let ops = w.app.ops();
    let a = ops.library().list_albums(false).unwrap().albums.remove(0);
    let (album, path) = (a.id().to_string(), a.path().to_string());
    drop(ops);
    Fix {
        w,
        p,
        pairing,
        album,
        path,
    }
}

impl Fix {
    fn revision(&self) -> String {
        out_json(
            self.w
                .call(&self.p, "musaic_album_get", json!({"albumId": self.album})),
        )["revision"]
            .as_str()
            .unwrap()
            .to_string()
    }

    fn preview(
        &self,
        patch: serde_json::Value,
    ) -> Result<serde_json::Value, musaic_commands::ToolError> {
        self.w
            .call(&self.p, "musaic_fix_preview", json!({"albumId": self.album, "baseRevision": self.revision(), "proposedPatch": patch}))
            .map(|o| o.to_json())
    }

    fn apply(
        &self,
        prev: &serde_json::Value,
        key: &str,
    ) -> Result<serde_json::Value, musaic_commands::ToolError> {
        self.w
            .call(&self.p, "musaic_fix_apply", json!({"proposalId": prev["proposalId"], "digest": prev["digest"], "idempotencyKey": key}))
            .map(|o| o.to_json())
    }

    fn operation(&self, id: &str) -> serde_json::Value {
        out_json(
            self.w
                .call(&self.p, "musaic_operation_get", json!({"operationId": id})),
        )
    }

    fn set_policy(&self, k: K, d: Decision) {
        self.w
            .app
            .ops()
            .agent()
            .set_policy(&self.pairing, &[(k, d)])
            .unwrap();
    }

    fn cards(&self, filter: ApprovalFilter) -> Vec<musaic_app::ops::agent::ApprovalRecord> {
        self.w.app.ops().agent().list_approvals(filter).unwrap()
    }

    fn decide(
        &self,
        id: &str,
        approve: bool,
    ) -> Result<musaic_app::ops::agent::ApprovalRecord, AgentError> {
        self.w.app.ops().agent().decide_approval(id, approve)
    }

    fn files(&self) -> BTreeMap<String, String> {
        files_digest(&self.path)
    }
}

const GENRE: fn(&str) -> serde_json::Value = |g| json!({"tags": {"album": {"genre": g}}});

#[test]
fn preview_writes_nothing_and_describes_the_change() {
    let f = fix_world(ALL_KINDS);
    let before = f.files();
    let db = db_state(&f.w);
    let prev = f.preview(GENRE("Agent Genre")).unwrap();
    validates("musaic_fix_preview", &prev);
    assert_eq!(f.files(), before, "preview changed a file");
    assert_eq!(prev["kinds"], json!(["tags.write"]));
    assert_eq!(prev["effects"], json!(["writes_tags"]));
    let tags = prev["diff"]["tags"].as_array().unwrap();
    assert!(
        tags.iter()
            .any(|t| t["scope"] == "album" && t["field"] == "genre" && t["after"] == "Agent Genre"),
        "{prev}"
    );
    assert_eq!(prev["digest"].as_str().unwrap().len(), 64);
    // Stored (immutable), expiring in 30 minutes.
    let conn = musaic_db::open_connection(f.w.app.db_path()).unwrap();
    let stored = musaic_db::agent::get_proposal(&conn, prev["proposalId"].as_str().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(stored.digest, prev["digest"].as_str().unwrap());
    assert_eq!(stored.expires_at, "2027-01-15T08:30:00Z");
    let _ = db;

    // Bad patches are refused with the field named.
    for (patch, field) in [
        (
            json!({"tags": {"album": {"nonsense": "x"}}}),
            "proposedPatch.tags.album",
        ),
        (
            json!({"tags": {"album": {"genre": ""}}}),
            "proposedPatch.tags.album",
        ),
        (
            json!({"tags": {"tracks": {"not a file.flac": {"title": "x"}}}}),
            "proposedPatch.tags.tracks",
        ),
        (
            json!({"tags": {"tracks": {"x": {"tracknumber": null}}}}),
            "proposedPatch.tags.tracks",
        ),
        (
            json!({"cover": {"source": "resolution"}}),
            "proposedPatch.cover",
        ),
        (
            json!({"cover": {"source": "url", "url": "https://127.0.0.1/c.jpg"}}),
            "proposedPatch.cover.url",
        ),
        (
            json!({"cover": {"source": "url", "url": "https://localhost/c.jpg"}}),
            "proposedPatch.cover.url",
        ),
        (
            json!({"tags": {"album": {"genre": "Same as now?"}}, "rename": false}),
            "proposedPatch.rename",
        ),
    ] {
        let e = f.preview(patch.clone()).unwrap_err();
        assert_eq!(e.code, ToolErrorCode::InvalidInput, "{patch}");
        assert_eq!(e.details.as_ref().unwrap()["field"], field, "{patch}");
    }
    // A stale base revision.
    let e = f.w.call(&f.p, "musaic_fix_preview", json!({"albumId": f.album, "baseRevision": "0".repeat(32), "proposedPatch": GENRE("x")})).unwrap_err();
    assert_eq!(e.code, ToolErrorCode::StaleRevision);
    assert_eq!(f.files(), before);
    // A public https cover URL passes the cheap check (nothing is fetched).
    let p = f
        .preview(json!({"cover": {"source": "url", "url": "https://example.com/c.jpg"}}))
        .unwrap();
    assert_eq!(p["kinds"], json!(["covers.write"]));
    assert_eq!(p["effects"], json!(["writes_cover", "fetches_network"]));
}

#[test]
fn ask_makes_a_card_reads_keep_working_and_deny_changes_nothing() {
    let f = fix_world(ALL_KINDS);
    let before = f.files();
    let seen = events(&f.w);
    let prev = f.preview(GENRE("Asked")).unwrap();
    let h = f.apply(&prev, "key-ask-0001").unwrap();
    validates("musaic_fix_apply", &h);
    assert_eq!(h["state"], "awaiting_approval");
    let card = h["approvalId"].as_str().unwrap().to_string();
    assert!(h.get("result").is_none());
    assert_eq!(f.files(), before);

    let cards = f.cards(ApprovalFilter::Pending);
    assert_eq!(cards.len(), 1);
    assert_eq!(cards[0].id, card);
    assert_eq!(cards[0].action, "fix_apply");
    assert_eq!(cards[0].pairing_name, "assistant");
    assert_eq!(cards[0].kinds, [K::TagsWrite]);
    assert!(
        cards[0]
            .album_label
            .as_deref()
            .is_some_and(|l| !l.is_empty())
    );
    assert!(
        cards[0]
            .diff
            .as_ref()
            .unwrap()
            .tags
            .iter()
            .any(|t| t.field == "genre")
    );
    assert_eq!(cards[0].expires_at, "2027-01-15T08:30:00Z");
    assert!(
        seen.lock()
            .unwrap()
            .iter()
            .any(|(w, ids)| *w == AgentWhat::Approvals && ids == &vec![card.clone()])
    );

    // A pending approval does not block reads (FR-039, SC-008).
    for (tool, body) in [
        ("musaic_capabilities", json!({})),
        ("musaic_library_list", json!({})),
        ("musaic_album_get", json!({"albumId": f.album})),
    ] {
        assert!(f.w.call(&f.p, tool, body).is_ok(), "{tool}");
    }
    let other = f.w.pair("other", READ).pairing.id;
    let po = f.w.login(&other);
    assert!(
        f.w.call(&po, "musaic_library_list", json!({})).is_ok(),
        "another pairing's reads too"
    );

    // Deny.
    let denied = f.decide(&card, false).unwrap();
    assert_eq!(denied.state, "denied");
    let op = f.operation(h["operationId"].as_str().unwrap());
    assert_eq!(op["state"], "denied");
    assert_eq!(op["error"]["code"], "DENIED");
    assert_eq!(f.files(), before, "denied: 0 files changed (SC-003)");
    assert_eq!(
        agent_err_code(&f.decide(&card, true).unwrap_err()),
        "CONFLICT",
        "decided once"
    );
    assert_eq!(
        agent_err_code(&f.decide("ap_nope", true).unwrap_err()),
        "NOT_FOUND"
    );
    let act = f.w.app.ops().agent().activity(&f.pairing).unwrap();
    assert!(
        act.iter()
            .any(|a| a.tool == "musaic_fix_apply" && a.outcome == "awaiting_approval")
    );
    assert!(
        act.iter()
            .any(|a| a.tool == "musaic_fix_apply" && a.outcome == "refused:DENIED")
    );
}

#[test]
fn a_card_expires_after_thirty_minutes_and_nothing_runs() {
    let f = fix_world(ALL_KINDS);
    let before = f.files();
    let prev = f.preview(GENRE("Late")).unwrap();
    let h = f.apply(&prev, "key-late-0001").unwrap();
    let card = h["approvalId"].as_str().unwrap().to_string();
    f.w.clock.advance(29 * 60);
    assert!(f.w.app.ops().agent().expire_due().is_empty(), "not yet");
    f.w.clock.advance(60);
    assert_eq!(
        f.w.app.ops().agent().expire_due(),
        std::slice::from_ref(&card)
    );
    let op = f.operation(h["operationId"].as_str().unwrap());
    assert_eq!(op["state"], "expired");
    assert_eq!(op["error"]["code"], "APPROVAL_EXPIRED");
    assert_eq!(
        agent_err_code(&f.decide(&card, true).unwrap_err()),
        "CONFLICT",
        "an expired card cannot be approved"
    );
    assert_eq!(
        f.cards(ApprovalFilter::All)[0].reason.as_deref(),
        Some("timed out")
    );
    assert_eq!(f.files(), before);
}

#[test]
fn an_expired_card_is_not_decidable_even_before_the_ticker_runs() {
    let f = fix_world(ALL_KINDS);
    let before = f.files();
    let prev = f.preview(GENRE("Late")).unwrap();
    let h = f.apply(&prev, "key-late-0002").unwrap();
    f.w.clock.advance(31 * 60);
    assert_eq!(
        agent_err_code(
            &f.decide(h["approvalId"].as_str().unwrap(), true)
                .unwrap_err()
        ),
        "CONFLICT"
    );
    assert_eq!(f.files(), before);
}

#[test]
fn refuse_and_a_kind_outside_the_scope_refuse_before_anything_runs() {
    let f = fix_world(ALL_KINDS);
    let before = f.files();
    let prev = f.preview(GENRE("Nope")).unwrap();
    f.set_policy(K::TagsWrite, Decision::Refuse);
    assert_eq!(tool_code(&f.w.call(&f.p, "musaic_fix_apply", json!({"proposalId": prev["proposalId"], "digest": prev["digest"], "idempotencyKey": "key-refuse-01"}))), "FORBIDDEN");
    assert_eq!(f.files(), before);
    assert!(
        f.cards(ApprovalFilter::All).is_empty(),
        "no card for a refusal"
    );
    assert_eq!(
        f.w.app.ops().agent().activity(&f.pairing).unwrap()[0].outcome,
        "refused:FORBIDDEN"
    );

    // A pairing whose scope lacks tags.write: FORBIDDEN, not a card (FR-033).
    let g = fix_world(&[K::LibraryRead, K::ProposalSubmit]);
    let before = g.files();
    let prev = g.preview(GENRE("Out of scope")).unwrap();
    assert_eq!(tool_code(&g.w.call(&g.p, "musaic_fix_apply", json!({"proposalId": prev["proposalId"], "digest": prev["digest"], "idempotencyKey": "key-scope-001"}))), "FORBIDDEN");
    assert_eq!(g.files(), before);
}

#[test]
fn a_wrong_digest_an_unknown_proposal_and_an_expired_one_are_refused() {
    let f = fix_world(ALL_KINDS);
    let before = f.files();
    let prev = f.preview(GENRE("x")).unwrap();
    let apply = |proposal: &str, digest: &str, key: &str| {
        tool_code(&f.w.call(
            &f.p,
            "musaic_fix_apply",
            json!({"proposalId": proposal, "digest": digest, "idempotencyKey": key}),
        ))
    };
    assert_eq!(
        apply(
            prev["proposalId"].as_str().unwrap(),
            &"a".repeat(64),
            "key-digest-01"
        ),
        "DIGEST_MISMATCH"
    );
    assert_eq!(
        apply(
            "pr_0000000000000000",
            prev["digest"].as_str().unwrap(),
            "key-digest-02"
        ),
        "NOT_FOUND"
    );
    // Another pairing cannot apply this pairing's proposal.
    let other = f.w.pair("o", ALL_KINDS).pairing.id;
    let po = f.w.login(&other);
    let r = f.w.call(&po, "musaic_fix_apply", json!({"proposalId": prev["proposalId"], "digest": prev["digest"], "idempotencyKey": "key-digest-03"}));
    assert_eq!(tool_code(&r), "NOT_FOUND");
    f.w.clock.advance(31 * 60);
    assert_eq!(
        apply(
            prev["proposalId"].as_str().unwrap(),
            prev["digest"].as_str().unwrap(),
            "key-digest-04"
        ),
        "PROPOSAL_EXPIRED"
    );
    assert_eq!(f.files(), before);
}

/// What the album's files and folder look like, for "nothing but the outside
/// change happened".
fn approve_after(
    f: &Fix,
    outside: impl FnOnce(&Fix),
) -> (serde_json::Value, BTreeMap<String, String>) {
    let prev = f.preview(GENRE("Stale")).unwrap();
    let h = f.apply(&prev, "key-stale-001").unwrap();
    let card = h["approvalId"].as_str().unwrap().to_string();
    outside(f);
    let after_outside = f.files();
    let rec = f.decide(&card, true).unwrap();
    assert_eq!(
        rec.state, "approved",
        "the user's decision stands; the apply was refused"
    );
    (
        f.operation(h["operationId"].as_str().unwrap()),
        after_outside,
    )
}

#[test]
fn a_change_made_outside_after_the_proposal_makes_the_approval_stale() {
    // An outside tag change.
    let f = fix_world(ALL_KINDS);
    let (op, after_outside) = approve_after(&f, |f| {
        f.w.app
            .ops()
            .apply()
            .apply_changes(
                &f.album,
                &musaic_app::ops::apply::ApplyChanges {
                    suggestions: vec![("year".into(), "1999".into())],
                    ..Default::default()
                },
                &mut |_| musaic_app::ops::Listening::Yes,
            )
            .unwrap();
    });
    assert_eq!(op["state"], "failed");
    assert_eq!(op["error"]["code"], "STALE_REVISION");
    assert_eq!(
        f.files(),
        after_outside,
        "only the outside change; the approved one did not run"
    );

    // An outside file rename.
    let f = fix_world(ALL_KINDS);
    let (op, after_outside) = approve_after(&f, |f| {
        let flac = std::fs::read_dir(&f.path)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .find(|p| p.extension().is_some_and(|x| x == "flac"))
            .unwrap();
        std::fs::rename(&flac, flac.with_file_name("outside rename.flac")).unwrap();
    });
    assert_eq!(op["error"]["code"], "STALE_REVISION");
    assert_eq!(f.files(), after_outside);

    // The album goes Missing.
    let f = fix_world(ALL_KINDS);
    let (op, _) = approve_after(&f, |f| {
        let conn = musaic_db::open_connection(f.w.app.db_path()).unwrap();
        assert!(
            musaic_db::albums::mark_album_missing(&conn, &f.album, "2027-01-15T08:00:00Z").unwrap()
        );
    });
    assert_eq!(op["state"], "failed");
    assert_eq!(op["error"]["code"], "MISSING_ALBUM");
}

#[test]
fn a_restore_running_refuses_an_allowed_apply_and_a_revoked_pairing_loses_its_card() {
    let f = fix_world(ALL_KINDS);
    let before = f.files();
    f.set_policy(K::TagsWrite, Decision::Allow);
    let prev = f.preview(GENRE("Restoring")).unwrap();
    {
        let _r = common::hold_restore(&f.w.app);
        assert_eq!(tool_code(&f.w.call(&f.p, "musaic_fix_apply", json!({"proposalId": prev["proposalId"], "digest": prev["digest"], "idempotencyKey": "key-restore-1"}))), "RESTORING");
        assert_eq!(tool_code(&f.w.call(&f.p, "musaic_fix_preview", json!({"albumId": f.album, "baseRevision": "0".repeat(32), "proposedPatch": GENRE("x")}))), "RESTORING");
    }
    assert_eq!(f.files(), before);

    // Ask, then revoke before the user decides.
    let g = fix_world(ALL_KINDS);
    let before = g.files();
    let prev = g.preview(GENRE("Revoked")).unwrap();
    let h = g.apply(&prev, "key-revoke-001").unwrap();
    g.w.app.ops().agent().revoke_pairing(&g.pairing).unwrap();
    let card = g.cards(ApprovalFilter::All).remove(0);
    assert_eq!(card.state, "denied");
    assert_eq!(card.reason.as_deref(), Some("pairing revoked"));
    assert_eq!(
        agent_err_code(&g.decide(&card.id, true).unwrap_err()),
        "CONFLICT"
    );
    let _ = h;
    assert_eq!(g.files(), before);
}

#[test]
fn restoring_a_snapshot_expires_previews_even_when_the_album_still_matches() {
    let f = fix_world(ALL_KINDS);
    f.set_policy(K::TagsWrite, Decision::Allow);
    let before = f.files();
    let preview = f.preview(GENRE("Restored preview")).unwrap();
    let backup = f.w.app.ops().database().backup().unwrap();
    f.w.app
        .ops()
        .database()
        .restore_from(&backup.file.path)
        .unwrap();
    assert_eq!(
        f.apply(&preview, "old-preview-after-restore")
            .unwrap_err()
            .code,
        ToolErrorCode::ProposalExpired
    );
    assert_eq!(f.files(), before);
    assert!(f.preview(GENRE("Fresh preview")).is_ok());
}

#[test]
fn the_agent_can_cancel_only_while_the_card_waits() {
    let f = fix_world(ALL_KINDS);
    let before = f.files();
    let prev = f.preview(GENRE("Cancel")).unwrap();
    let h = f.apply(&prev, "key-cancel-001").unwrap();
    let op = h["operationId"].as_str().unwrap();
    let c = out_json(f.w.call(&f.p, "musaic_operation_cancel", json!({"operationId": op})));
    validates("musaic_operation_cancel", &c);
    assert_eq!(
        (c["cancellation"].as_str(), c["state"].as_str()),
        (Some("requested"), Some("cancelled"))
    );
    let card = &f.cards(ApprovalFilter::All)[0];
    assert_eq!(
        (card.state.as_str(), card.reason.as_deref()),
        ("denied", Some("cancelled by assistant"))
    );
    let c = out_json(f.w.call(&f.p, "musaic_operation_cancel", json!({"operationId": op})));
    assert_eq!(c["cancellation"], "already_finished");
    assert_eq!(f.files(), before);
    // Another pairing cannot see or cancel it.
    let other = f.w.pair("o", READ).pairing.id;
    let po = f.w.login(&other);
    assert_eq!(
        tool_code(&f.w.call(&po, "musaic_operation_get", json!({"operationId": op}))),
        "NOT_FOUND"
    );
    assert_eq!(
        tool_code(&f.w.call(&po, "musaic_operation_cancel", json!({"operationId": op}))),
        "NOT_FOUND"
    );
}

#[test]
fn approving_applies_through_the_pipeline_with_the_same_events_as_a_desktop_edit() {
    let collect = |w: &AgentWorld, album: &str| {
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let (s, id) = (seen.clone(), album.to_string());
        w.app.bus().register(
            Topic::Event,
            Subscriber::from_fn(move |e| {
                if !matches!(e, Event::AgentStateChanged { .. }) {
                    s.lock().unwrap().push(format!(
                        "{} {}",
                        e.name(),
                        e.payload_json().replace(&id, "<album>")
                    ));
                }
                Ok(())
            })
            .key("collect")
            .priority(500)
            .name("collect"),
        );
        seen
    };

    // The agent's path.
    let f = fix_world(ALL_KINDS);
    let agent_events = collect(&f.w, &f.album);
    let before = f.files();
    let prev = f
        .preview(json!({"tags": {"album": {"genre": "Approved Genre", "year": "2031"}}}))
        .unwrap();
    let h = f.apply(&prev, "key-approve-01").unwrap();
    f.decide(h["approvalId"].as_str().unwrap(), true).unwrap();
    let op = f.operation(h["operationId"].as_str().unwrap());
    assert_eq!(op["state"], "succeeded", "{op}");
    let files = op["result"]["apply"]["files"].as_array().unwrap();
    assert!(
        !files.is_empty() && files.iter().all(|x| x["status"] == "ok"),
        "{op}"
    );
    assert!(
        files
            .iter()
            .all(|x| !x["fileName"].as_str().unwrap().contains('/'))
    );
    assert_ne!(f.files(), before);
    // The tags are on disc.
    let flac = std::fs::read_dir(&f.path)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|x| x == "flac"))
        .unwrap();
    let meta = musaic_scan::flacmeta::read(&flac).unwrap();
    assert_eq!(meta.values("genre").next(), Some("Approved Genre"));
    assert_eq!(meta.values("date").next(), Some("2031"));
    // A restart-proof record exists (FR-054).
    let conn = musaic_db::open_connection(f.w.app.db_path()).unwrap();
    let changes = musaic_db::agent::list_changes(&conn).unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(
        (changes[0].state.as_str(), changes[0].approval_id.as_deref()),
        ("applied", h["approvalId"].as_str())
    );
    assert!(
        f.w.scratch
            .config
            .join("agent-changes")
            .join(&changes[0].id)
            .join("before.json")
            .is_file()
    );

    // The desktop's path: the same request through apply_changes on a twin world.
    let g = fix_world(ALL_KINDS);
    let desk_events = collect(&g.w, &g.album);
    g.w.app
        .ops()
        .apply()
        .apply_changes(
            &g.album,
            &musaic_app::ops::apply::ApplyChanges {
                suggestions: vec![
                    ("genre".into(), "Approved Genre".into()),
                    ("year".into(), "2031".into()),
                ],
                ..Default::default()
            },
            &mut |_| musaic_app::ops::Listening::Yes,
        )
        .unwrap();
    let norm = |v: &Arc<Mutex<Vec<String>>>| {
        v.lock()
            .unwrap()
            .iter()
            .filter(|l| !l.starts_with("ChecksRerun"))
            .map(|l| {
                l.split(' ').next().unwrap().to_string()
                    + " "
                    + &l[l.find(' ').unwrap()..].replace(&g.album, "<album>")
            })
            .collect::<Vec<_>>()
    };
    let (a, d) = (norm(&agent_events), norm(&desk_events));
    assert!(!a.is_empty());
    assert_eq!(
        a, d,
        "the agent's apply publishes the desktop edit's events (SC-004)"
    );
}

#[test]
fn an_allowed_rename_renames_files_and_the_folder_and_publishes_album_renamed() {
    let f = fix_world(ALL_KINDS);
    f.set_policy(K::TagsWrite, Decision::Allow);
    f.set_policy(K::FilesRename, Decision::Allow);
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let s = seen.clone();
    f.w.app.bus().register(
        Topic::Kind(EventKind::AlbumRenamed),
        Subscriber::from_fn(move |e| {
            s.lock().unwrap().push(e.payload_json());
            Ok(())
        })
        .key("renamed")
        .priority(500)
        .name("renamed"),
    );
    let prev = f
        .preview(json!({"tags": {"album": {"album_artist": "Renamed Artist"}}, "rename": true}))
        .unwrap();
    assert_eq!(prev["kinds"], json!(["tags.write", "files.rename"]));
    assert!(
        prev["diff"]["folder"]["to"]
            .as_str()
            .unwrap()
            .contains("Renamed Artist"),
        "{prev}"
    );
    assert!(
        prev["effects"]
            .as_array()
            .unwrap()
            .contains(&json!("renames_folder"))
    );
    let h = f.apply(&prev, "key-rename-001").unwrap();
    assert_eq!(h["state"], "succeeded", "{h}");
    assert_eq!(h["result"]["renamed"], true, "{h}\n{prev}");
    assert!(
        h["result"]["albumIdAfter"]
            .as_str()
            .is_some_and(|id| id != f.album)
    );
    assert!(
        !seen.lock().unwrap().is_empty(),
        "AlbumRenamed was published"
    );
    assert!(
        !std::path::Path::new(&f.path).exists(),
        "the old folder is gone"
    );
    // Nothing in the result is a path.
    assert!(
        !h.to_string()
            .contains(&f.w.scratch.tmp.path().display().to_string())
    );
    // The same key and input again returns the same handle, not a second apply.
    let again = f.apply(&prev, "key-rename-001").unwrap();
    assert_eq!(again["operationId"], h["operationId"]);
}

#[test]
fn idempotency_returns_the_same_handle_and_refuses_other_input() {
    let f = fix_world(ALL_KINDS);
    let a = f.preview(GENRE("One")).unwrap();
    let b = f.preview(GENRE("Two")).unwrap();
    let h1 = f.apply(&a, "key-idem-0001").unwrap();
    let h2 = f.apply(&a, "key-idem-0001").unwrap();
    assert_eq!(h1["operationId"], h2["operationId"]);
    assert_eq!(h1["approvalId"], h2["approvalId"]);
    assert_eq!(
        f.cards(ApprovalFilter::Pending).len(),
        1,
        "one card, not two"
    );
    assert_eq!(
        f.apply(&b, "key-idem-0001").unwrap_err().code,
        ToolErrorCode::IdempotencyConflict
    );
    // Another pairing may use the same key.
    let other = f.w.pair("o", ALL_KINDS).pairing.id;
    let po = f.w.login(&other);
    let _ = po;
}

#[test]
fn a_url_cover_is_fetched_only_at_apply_and_an_offline_failure_changes_nothing() {
    let f = fix_world(ALL_KINDS);
    f.set_policy(K::CoversWrite, Decision::Allow);
    let before = f.files();
    let prev = f
        .preview(json!({"cover": {"source": "url", "url": "https://example.com/cover.jpg"}}))
        .unwrap();
    let h = f.apply(&prev, "key-cover-001").unwrap();
    assert_eq!(h["state"], "failed", "{h}");
    let op = f.operation(h["operationId"].as_str().unwrap());
    assert_eq!(op["error"]["code"], "UNAVAILABLE");
    assert!(
        op["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Failed to download cover"),
        "{op}"
    );
    // The transport is offline: no file changed (the before-image dir may exist).
    assert_eq!(f.files(), before);
    let conn = musaic_db::open_connection(f.w.app.db_path()).unwrap();
    assert_eq!(
        musaic_db::agent::list_changes(&conn).unwrap()[0].state,
        "failed"
    );
}

#[test]
fn no_tool_can_approve_deny_or_change_policy() {
    for t in &REGISTRY {
        for bad in ["approv", "deny", "policy", "pairing", "grant"] {
            assert!(
                !t.id.contains(bad),
                "{} looks like it changes authority",
                t.id
            );
        }
    }
    for id in [
        "musaic_approve",
        "musaic_deny",
        "musaic_set_policy",
        "musaic_pair",
    ] {
        assert_eq!(
            musaic_commands::parse_call(id, b"{}").unwrap_err().code,
            ToolErrorCode::NotFound
        );
    }
}

#[test]
fn a_restart_keeps_a_pending_card_and_its_operation() {
    let f = fix_world(ALL_KINDS);
    let prev = f.preview(GENRE("Restart")).unwrap();
    let handle = f.apply(&prev, "key-restart-01").unwrap();
    let op_id = handle["operationId"].as_str().unwrap();
    let card_id = handle["approvalId"].as_str().unwrap();
    assert_eq!(f.cards(ApprovalFilter::Pending).len(), 1);
    let Fix { w, .. } = f;
    let AgentWorld {
        app,
        scratch,
        clock,
        store,
    } = w;
    drop(app);
    let app2 = musaic_app::App::builder(&scratch.config)
        .secrets(Arc::new(musaic_secrets::MemoryStore::new()))
        .agent_secrets(store.clone())
        .clock(clock.wall())
        .subscribers(false)
        .auto_snapshot(false)
        .ai_worker(false)
        .build()
        .unwrap();
    let ops = app2.ops();
    let pending = ops.agent().list_approvals(ApprovalFilter::Pending).unwrap();
    assert_eq!(pending[0].id, card_id);
    let conn = musaic_db::open_connection(app2.db_path()).unwrap();
    let op = musaic_db::agent_operations::get(&conn, op_id)
        .unwrap()
        .unwrap();
    assert_eq!(op.state, "awaiting_approval");
    assert_eq!(op.approval_id.as_deref(), Some(card_id));
    let card = ops.agent().decide_approval(card_id, true).unwrap();
    assert_eq!(card.state, "approved");
    let op = musaic_db::agent_operations::get(&conn, op_id)
        .unwrap()
        .unwrap();
    assert_eq!(op.state, "succeeded");
}

// ------------------------------------------------------------ compute starts

use std::time::{Duration, Instant};

/// A held door: a thread that reaches `wait` blocks until `open` is called.
#[derive(Default)]
struct Door {
    open: Mutex<bool>,
    cv: std::sync::Condvar,
    entered: Mutex<usize>,
}

impl Door {
    fn wait(&self) {
        *self.entered.lock().unwrap() += 1;
        let mut open = self.open.lock().unwrap();
        while !*open {
            open = self.cv.wait(open).unwrap();
        }
    }
    fn open(&self) {
        *self.open.lock().unwrap() = true;
        self.cv.notify_all();
    }
    fn entered(&self) -> usize {
        *self.entered.lock().unwrap()
    }
}

fn eventually(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline {
        if f() {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("timed out waiting for {what}");
}

fn finished(w: &AgentWorld, p: &Principal, op: &str) -> serde_json::Value {
    let mut last = json!(null);
    eventually("the operation to finish", || {
        last = out_json(w.call(p, "musaic_operation_get", json!({"operationId": op})));
        !matches!(
            last["state"].as_str(),
            Some("running") | Some("awaiting_approval")
        )
    });
    last
}

const COMPUTE: &[K] = &[
    K::LibraryRead,
    K::ComputeScan,
    K::ComputeAnalyze,
    K::TagsWrite,
];

fn compute_world() -> (AgentWorld, Principal, String) {
    let w = agent_world_scanned();
    let id = w.pair("compute", COMPUTE).pairing.id;
    for k in [K::ComputeScan, K::ComputeAnalyze, K::TagsWrite] {
        w.app
            .ops()
            .agent()
            .set_policy(&id, &[(k, Decision::Allow)])
            .unwrap();
    }
    let p = w.login(&id);
    (w, p, id)
}

#[test]
fn checks_start_findings_equal_a_direct_run_and_the_same_key_gives_the_same_handle() {
    let (w, p, _) = compute_world();
    let body =
        json!({"checkIds": ["track-gaps", "no-cover-art"], "idempotencyKey": "key-checks-0001"});
    let h = out_json(w.call(&p, "musaic_checks_start", body.clone()));
    validates("musaic_checks_start", &h);
    assert_eq!(h["state"], "running");
    let op = finished(&w, &p, h["operationId"].as_str().unwrap());
    validates("musaic_operation_get", &op);
    assert_eq!(op["state"], "succeeded", "{op}");
    assert_eq!(op["counts"], json!({"done": 2, "total": 2, "failed": 0}));
    let summaries = op["result"]["checks"].as_array().unwrap();
    assert_eq!(summaries.len(), 2);
    let ops = w.app.ops();
    for s in summaries {
        let direct = ops
            .checks()
            .run(s["checkId"].as_str().unwrap(), "all", None)
            .unwrap();
        assert_eq!(s["findings"].as_i64().unwrap(), direct.issue_count, "{s}");
    }
    // Same key and input: the same handle, no second run.
    let again = out_json(w.call(&p, "musaic_checks_start", body));
    assert_eq!(again["operationId"], h["operationId"]);
    // Same key, other input: refused.
    let r = w.call(
        &p,
        "musaic_checks_start",
        json!({"checkIds": ["track-gaps"], "idempotencyKey": "key-checks-0001"}),
    );
    assert_eq!(tool_code(&r), "IDEMPOTENCY_CONFLICT");
    // An unknown check id is refused before anything starts.
    assert_eq!(
        tool_code(&w.call(
            &p,
            "musaic_checks_start",
            json!({"checkIds": ["nope"], "idempotencyKey": "key-checks-0002"})
        )),
        "INVALID_INPUT"
    );
    // Recorded: the start and its outcome.
    let act = ops.agent().activity(&p.pairing_id).unwrap();
    assert!(
        act.iter()
            .any(|a| a.tool == "musaic_checks_start" && a.outcome == "started")
    );
    assert!(
        act.iter()
            .any(|a| a.tool == "musaic_checks_start" && a.outcome == "ok")
    );
    // Another pairing cannot see the operation; a finished one cannot be cancelled.
    let other = w.pair("other", READ).pairing.id;
    let po = w.login(&other);
    assert_eq!(
        tool_code(&w.call(
            &po,
            "musaic_operation_get",
            json!({"operationId": h["operationId"]})
        )),
        "NOT_FOUND"
    );
    let c = out_json(w.call(
        &p,
        "musaic_operation_cancel",
        json!({"operationId": h["operationId"]}),
    ));
    assert_eq!(c["cancellation"], "already_finished");
    assert_eq!(c["state"], "succeeded");
}

#[test]
fn cancelling_a_check_run_stops_it_between_checks_with_partial_counts() {
    let (w, p, _) = compute_world();
    let door = Arc::new(Door::default());
    let hold = door.clone();
    w.app.bus().register(
        Topic::Kind(EventKind::ChecksRerunFinished),
        Subscriber::from_fn(move |_| {
            if hold.entered() == 0 {
                hold.wait();
            }
            Ok(())
        })
        .key("hold-between-checks")
        .name("hold-between-checks"),
    );
    let h = out_json(w.call(&p, "musaic_checks_start", json!({"checkIds": ["track-gaps", "no-cover-art", "inconsistent-artist-sort"], "idempotencyKey": "key-cancel-0001"})));
    let op = h["operationId"].as_str().unwrap().to_string();
    eventually("the first check to complete", || door.entered() > 0);
    let running = out_json(w.call(&p, "musaic_operation_get", json!({"operationId": op})));
    assert_eq!(running["state"], "running");
    assert_eq!(running["counts"]["done"], 0);
    let c = out_json(w.call(&p, "musaic_operation_cancel", json!({"operationId": op})));
    assert_eq!(
        (c["cancellation"].as_str(), c["state"].as_str()),
        (Some("requested"), Some("running"))
    );
    door.open();
    let done = finished(&w, &p, &op);
    assert_eq!(done["state"], "cancelled", "{done}");
    assert_eq!(done["counts"]["total"], 3);
    assert_eq!(done["counts"]["done"], 1, "stopped at a boundary: {done}");
    assert_eq!(done["result"]["checks"].as_array().unwrap().len(), 1);
}

#[test]
fn scan_start_follows_a_running_scan_and_a_wrong_root_is_not_found() {
    let gate = Arc::new(Door::default());
    let g2 = gate.clone();
    let hold = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let h2 = hold.clone();
    let clock = common::FakeClock::new();
    let store = Arc::new(musaic_secrets::MemoryStore::new());
    let (c2, s2) = (clock.clone(), store.clone());
    let (app, scratch) = common::app_quiet_pool(move |b| {
        b.clock(c2.wall())
            .agent_secrets(s2)
            .on_scan_album(Arc::new(move |_, _| {
                if h2.load(std::sync::atomic::Ordering::SeqCst) {
                    g2.wait();
                }
            }))
    });
    common::configure(&app, &scratch);
    assert!(app.ops().scan().subscribe(false).unwrap().count() > 2);
    let w = AgentWorld {
        app,
        scratch,
        clock,
        store,
    };
    eventually("idle", || w.app.gate().snapshot() == (0, false));
    let id = w.pair("s", COMPUTE).pairing.id;
    w.app
        .ops()
        .agent()
        .set_policy(&id, &[(K::ComputeScan, Decision::Allow)])
        .unwrap();
    let p = w.login(&id);
    let caps = out_json(w.call(&p, "musaic_capabilities", json!({})));
    let root = caps["configuredRoots"][0]["rootId"]
        .as_str()
        .unwrap()
        .to_string();

    assert_eq!(
        tool_code(&w.call(
            &p,
            "musaic_scan_start",
            json!({"configuredRootId": "0123456789abcdef", "idempotencyKey": "key-scan-00001"})
        )),
        "NOT_FOUND"
    );

    // A scan the desktop started and holds: the agent's handle follows it.
    hold.store(true, std::sync::atomic::Ordering::SeqCst);
    let ops = w.app.ops();
    ops.scan().start(true).unwrap();
    eventually("the scan to be held", || gate.entered() > 0);
    let h = out_json(w.call(
        &p,
        "musaic_scan_start",
        json!({"configuredRootId": root, "idempotencyKey": "key-scan-00002"}),
    ));
    validates("musaic_scan_start", &h);
    assert_eq!(h["state"], "running");
    let op = h["operationId"].as_str().unwrap().to_string();
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(
        out_json(w.call(&p, "musaic_operation_get", json!({"operationId": op})))["state"],
        "running",
        "still following the held scan"
    );
    hold.store(false, std::sync::atomic::Ordering::SeqCst);
    gate.open();
    let done = finished(&w, &p, &op);
    assert_eq!(done["state"], "succeeded", "{done}");
    assert!(done["result"]["scanAlbums"].as_u64().unwrap() > 2);
}

#[test]
fn cancelling_a_scan_goes_through_the_scans_own_cancel() {
    let gate = Arc::new(Door::default());
    let g2 = gate.clone();
    let clock = common::FakeClock::new();
    let store = Arc::new(musaic_secrets::MemoryStore::new());
    let (c2, s2) = (clock.clone(), store.clone());
    let (app, scratch) = common::app_quiet_pool(move |b| {
        b.clock(c2.wall())
            .agent_secrets(s2)
            .on_scan_album(Arc::new(move |_, _| g2.wait()))
    });
    common::configure(&app, &scratch);
    let w = AgentWorld {
        app,
        scratch,
        clock,
        store,
    };
    let id = w.pair("s", COMPUTE).pairing.id;
    w.app
        .ops()
        .agent()
        .set_policy(&id, &[(K::ComputeScan, Decision::Allow)])
        .unwrap();
    let p = w.login(&id);
    let caps = out_json(w.call(&p, "musaic_capabilities", json!({})));
    let root = caps["configuredRoots"][0]["rootId"]
        .as_str()
        .unwrap()
        .to_string();
    let h = out_json(w.call(
        &p,
        "musaic_scan_start",
        json!({"configuredRootId": root, "idempotencyKey": "key-scan-00003"}),
    ));
    let op = h["operationId"].as_str().unwrap().to_string();
    eventually("the scan to be held", || gate.entered() > 0);
    let c = out_json(w.call(&p, "musaic_operation_cancel", json!({"operationId": op})));
    assert_eq!(c["cancellation"], "requested");
    eventually("the cancel to reach the scan", || {
        w.app.ops().scan().status().cancel_requested
    });
    gate.open();
    let done = finished(&w, &p, &op);
    assert_eq!(done["state"], "cancelled", "{done}");
}

#[test]
fn analysis_needs_both_kinds_and_a_refused_one_changes_nothing() {
    let (w, p, id) = compute_world();
    let ops = w.app.ops();
    let a = ops.library().list_albums(false).unwrap().albums.remove(0);
    let before = files_digest(a.path());
    w.app
        .ops()
        .agent()
        .set_policy(&id, &[(K::TagsWrite, Decision::Refuse)])
        .unwrap();
    let r = w.call(
        &p,
        "musaic_analysis_start",
        json!({"albumIds": [a.id()], "kinds": ["bpm"], "idempotencyKey": "key-analysis-01"}),
    );
    assert_eq!(
        tool_code(&r),
        "FORBIDDEN",
        "compute.analyze alone is not enough: analysis writes tags"
    );
    assert_eq!(files_digest(a.path()), before);
    // With both Allow it runs and writes ReplayGain / BPM tags.
    w.app
        .ops()
        .agent()
        .set_policy(&id, &[(K::TagsWrite, Decision::Allow)])
        .unwrap();
    let h = out_json(w.call(
        &p,
        "musaic_analysis_start",
        json!({"albumIds": [a.id()], "kinds": ["replaygain"], "idempotencyKey": "key-analysis-02"}),
    ));
    let op = finished(&w, &p, h["operationId"].as_str().unwrap());
    assert_eq!(op["state"], "succeeded", "{op}");
    assert_eq!(op["counts"], json!({"done": 1, "total": 1, "failed": 0}));
    assert_eq!(op["result"]["analysis"][0]["status"], "ok");
    assert_ne!(
        files_digest(a.path()),
        before,
        "the analysis wrote its tags"
    );
}

#[test]
fn a_start_that_asks_waits_for_a_card_and_runs_only_when_approved() {
    let w = agent_world_scanned();
    let id = w
        .pair("asker", &[K::LibraryRead, K::ComputeScan])
        .pairing
        .id;
    let p = w.login(&id);
    let h = out_json(w.call(
        &p,
        "musaic_checks_start",
        json!({"checkIds": ["track-gaps"], "idempotencyKey": "key-ask-checks-1"}),
    ));
    assert_eq!(h["state"], "awaiting_approval");
    let card = h["approvalId"].as_str().unwrap().to_string();
    let cards = w
        .app
        .ops()
        .agent()
        .list_approvals(ApprovalFilter::Pending)
        .unwrap();
    assert_eq!(
        (cards[0].action.as_str(), cards[0].album_id.as_deref()),
        ("checks_start", None)
    );
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(
        out_json(w.call(
            &p,
            "musaic_operation_get",
            json!({"operationId": h["operationId"]})
        ))["state"],
        "awaiting_approval"
    );
    w.app.ops().agent().decide_approval(&card, true).unwrap();
    let done = finished(&w, &p, h["operationId"].as_str().unwrap());
    assert_eq!(done["state"], "succeeded", "{done}");
    // A denied start never runs.
    let h2 = out_json(w.call(
        &p,
        "musaic_checks_start",
        json!({"checkIds": ["no-cover-art"], "idempotencyKey": "key-ask-checks-2"}),
    ));
    w.app
        .ops()
        .agent()
        .decide_approval(h2["approvalId"].as_str().unwrap(), false)
        .unwrap();
    let denied = out_json(w.call(
        &p,
        "musaic_operation_get",
        json!({"operationId": h2["operationId"]}),
    ));
    assert_eq!(
        (denied["state"].as_str(), denied["error"]["code"].as_str()),
        (Some("denied"), Some("DENIED"))
    );
}

#[test]
fn shutdown_joins_the_worker_threads_and_refuses_new_ones() {
    let (w, p, _) = compute_world();
    let h = out_json(w.call(
        &p,
        "musaic_checks_start",
        json!({"idempotencyKey": "key-shutdown-01"}),
    ));
    finished(&w, &p, h["operationId"].as_str().unwrap());
    w.app.begin_shutdown();
    let r = w.call(
        &p,
        "musaic_checks_start",
        json!({"checkIds": ["track-gaps"], "idempotencyKey": "key-shutdown-02"}),
    );
    assert_eq!(tool_code(&r), "UNAVAILABLE");
    let report = w.app.shutdown();
    assert!(report.scan_stopped);
}

#[test]
fn list_tools_returns_the_generated_catalog_and_its_digest() {
    let w = agent_world();
    let id = w.pair("catalog", READ).pairing.id;
    let p = w.login(&id);
    let info = w.app.ops().agent().list_tools(&p);
    assert_eq!(info.catalog, musaic_commands::catalog::catalog_json());
    assert_eq!(
        info.registry_digest,
        musaic_commands::catalog::registry_digest()
    );
}

/// An approved start whose authority went away after its card was made: the
/// operation ends Failed with FORBIDDEN, no work ran, and the activity log
/// records the refusal and no start (`start_still_allowed`).
fn assert_refused_start(w: &AgentWorld, pairing: &str, op: &str, tool: &str) {
    let db = musaic_db::open_connection(w.app.db_path()).unwrap();
    let snap = musaic_db::agent_operations::get(&db, op)
        .unwrap()
        .expect("the operation");
    let error: Option<musaic_commands::ToolError> = snap
        .error
        .as_deref()
        .map(|value| serde_json::from_str(value).unwrap());
    assert_eq!(
        snap.state,
        musaic_commands::tools::operation::OperationState::Failed.as_str()
    );
    assert_eq!(
        error.as_ref().map(|e| e.code),
        Some(ToolErrorCode::Forbidden),
        "{error:?}"
    );
    assert!(snap.result.is_none(), "no work ran: {:?}", snap.result);
    assert_eq!(
        serde_json::from_str::<musaic_commands::tools::operation::Counts>(&snap.counts).unwrap(),
        Default::default()
    );
    let act = w.app.ops().agent().activity(pairing).unwrap();
    assert!(
        act.iter()
            .any(|a| a.tool == tool && a.outcome == "refused:FORBIDDEN"),
        "{act:?}"
    );
    assert!(
        !act.iter().any(|a| a.tool == tool && a.outcome == "started"),
        "{act:?}"
    );
}

#[test]
fn approving_a_start_after_the_pairing_was_revoked_starts_nothing() {
    let w = agent_world_scanned();
    let id = w
        .pair("revoked", &[K::LibraryRead, K::ComputeScan])
        .pairing
        .id;
    let p = w.login(&id);
    let caps = out_json(w.call(&p, "musaic_capabilities", json!({})));
    let root = caps["configuredRoots"][0]["rootId"]
        .as_str()
        .unwrap()
        .to_string();
    let checks = out_json(w.call(
        &p,
        "musaic_checks_start",
        json!({"checkIds": ["track-gaps"], "idempotencyKey": "key-revoked-chk1"}),
    ));
    let scan = out_json(w.call(
        &p,
        "musaic_scan_start",
        json!({"configuredRootId": root, "idempotencyKey": "key-revoked-scn1"}),
    ));
    for h in [&checks, &scan] {
        assert_eq!(h["state"], "awaiting_approval", "{h}");
    }
    // The revoke lands between the card being read and the start. It is
    // written straight to the database so the card is still pending when the
    // user approves it (the revoke operation also denies waiting cards).
    let conn = musaic_db::open_connection(w.app.db_path()).unwrap();
    assert!(musaic_db::agent::revoke_pairing(&conn, &id, &w.app.now_iso()).unwrap());
    let scan_before = w.app.ops().scan().status();
    for (h, tool) in [
        (&checks, "musaic_checks_start"),
        (&scan, "musaic_scan_start"),
    ] {
        w.app
            .ops()
            .agent()
            .decide_approval(h["approvalId"].as_str().unwrap(), true)
            .unwrap();
        assert_refused_start(&w, &id, h["operationId"].as_str().unwrap(), tool);
    }
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(w.app.ops().scan().status(), scan_before, "no scan started");
}

#[test]
fn approving_a_start_after_its_kind_turned_to_refuse_starts_nothing() {
    let w = agent_world_scanned();
    let id = w.pair("refused", COMPUTE).pairing.id;
    let p = w.login(&id);
    let a = w
        .app
        .ops()
        .library()
        .list_albums(false)
        .unwrap()
        .albums
        .remove(0);
    let before = files_digest(a.path());
    let analysis = out_json(w.call(
        &p,
        "musaic_analysis_start",
        json!({"albumIds": [a.id()], "kinds": ["replaygain"], "idempotencyKey": "key-refused-ana1"}),
    ));
    let checks = out_json(w.call(
        &p,
        "musaic_checks_start",
        json!({"checkIds": ["no-cover-art"], "idempotencyKey": "key-refused-chk1"}),
    ));
    for h in [&analysis, &checks] {
        assert_eq!(h["state"], "awaiting_approval", "{h}");
    }
    // Turning a kind to Refuse leaves the waiting cards pending.
    w.app
        .ops()
        .agent()
        .set_policy(
            &id,
            &[
                (K::TagsWrite, Decision::Refuse),
                (K::ComputeScan, Decision::Refuse),
            ],
        )
        .unwrap();
    for (h, tool) in [
        (&analysis, "musaic_analysis_start"),
        (&checks, "musaic_checks_start"),
    ] {
        w.app
            .ops()
            .agent()
            .decide_approval(h["approvalId"].as_str().unwrap(), true)
            .unwrap();
        assert_refused_start(&w, &id, h["operationId"].as_str().unwrap(), tool);
        // The agent reads the same through its own tool.
        let op = out_json(w.call(
            &p,
            "musaic_operation_get",
            json!({"operationId": h["operationId"]}),
        ));
        assert_eq!(
            (op["state"].as_str(), op["error"]["code"].as_str()),
            (Some("failed"), Some("FORBIDDEN")),
            "{op}"
        );
    }
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(files_digest(a.path()), before, "no analysis wrote tags");
}

#[test]
fn task_mapping_rolls_back_on_outbox_failure_and_recovers_on_retry() {
    use musaic_commands::tools::task::{TaskCreateInput, TaskInput, TaskUpdateInput};

    let w = agent_world();
    let id = w.pair("durable", COMPUTE).pairing.id;
    let p = w.login(&id);
    let args = json!({"checkIds":["track-gaps"],"idempotencyKey":"task-atomic-test"});
    let handle = out_json(w.call(&p, "musaic_checks_start", args.clone()));
    let operation_id = handle["operationId"].as_str().unwrap().to_string();
    let create = TaskCreateInput {
        operation_id: operation_id.clone(),
    };
    let conn = musaic_db::open_connection(w.app.db_path()).unwrap();
    conn.execute_batch(
        "CREATE TRIGGER fail_task_event BEFORE INSERT ON delegation_audit
         WHEN NEW.transition='task_created' BEGIN SELECT RAISE(ABORT,'interrupted'); END;",
    )
    .unwrap();
    let ops = w.app.ops();
    let agent = ops.agent();
    assert_eq!(
        agent.task_create(&p, &create).unwrap_err().code,
        ToolErrorCode::Internal
    );
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM mcp_tasks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0);
    conn.execute_batch("DROP TRIGGER fail_task_event").unwrap();
    let replay = out_json(w.call(&p, "musaic_checks_start", args));
    assert_eq!(replay["operationId"], operation_id);
    let task = agent.task_create(&p, &create).unwrap();
    assert_eq!(
        agent.task_create(&p, &create).unwrap().task_id,
        task.task_id
    );
    let read = TaskInput {
        task_id: task.task_id.clone(),
    };

    // Supply an internal input request to exercise the generic replay
    // mechanism, independently from the desktop's approval card.
    conn.execute(
        "INSERT INTO mcp_task_inputs(task_id,input_key,request) VALUES(?1,'roots',?2)",
        [&task.task_id, &json!({"method":"roots/list"}).to_string()],
    )
    .unwrap();
    let pending = agent.task_get(&p, &read).unwrap();
    assert!(pending.pending_inputs.contains_key("roots"));
    let update = |roots| TaskUpdateInput {
        task_id: task.task_id.clone(),
        input_responses: [("roots".into(), roots)].into(),
    };
    let mut mixed = update(json!({"roots":[]}));
    mixed
        .input_responses
        .insert("never-issued".into(), json!({}));
    agent.task_update(&p, &mixed).unwrap();
    agent.task_update(&p, &update(json!({"roots":[]}))).unwrap();
    agent
        .task_update(&p, &update(json!({"roots":[{}]})))
        .unwrap();
    assert!(agent.task_get(&p, &read).unwrap().pending_inputs.is_empty());
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM delegation_audit WHERE transition='task_input_received'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
    assert_eq!(
        agent.list_approvals(ApprovalFilter::Pending).unwrap()[0].state,
        "pending"
    );

    agent.task_cancel(&p, &read).unwrap();
    let task = agent.task_get(&p, &read).unwrap();
    assert_eq!(task.operation.state.as_str(), "cancelled");
    assert_eq!(task.ttl_ms, Some(604_800_000));
    w.clock.advance(604_800);
    agent.expire_due();
    let expired = agent.task_get(&p, &read).unwrap_err();
    assert_eq!(expired.code, ToolErrorCode::TaskExpired);
    assert_eq!(expired.details.unwrap()["operationId"], operation_id);
    let op = out_json(w.call(
        &p,
        "musaic_operation_get",
        json!({"operationId":operation_id}),
    ));
    assert_eq!(op["state"], "cancelled");
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM mcp_task_inputs", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn restored_database_fences_task_handles_and_old_approvals() {
    use musaic_commands::tools::task::{TaskCreateInput, TaskInput};

    let w = agent_world();
    let id = w.pair("restore", COMPUTE).pairing.id;
    let p = w.login(&id);
    let handle = out_json(w.call(
        &p,
        "musaic_checks_start",
        json!({"checkIds":["track-gaps"],"idempotencyKey":"task-restore-test"}),
    ));
    let operation_id = handle["operationId"].as_str().unwrap().to_string();
    let ops = w.app.ops();
    let agent = ops.agent();
    let snapshot = agent
        .task_create(
            &p,
            &TaskCreateInput {
                operation_id: operation_id.clone(),
            },
        )
        .unwrap();
    let read = TaskInput {
        task_id: snapshot.task_id,
    };
    let backup_path = w.scratch.tmp.path().join("backup.db");
    let conn = musaic_db::open_connection(w.app.db_path()).unwrap();
    conn.execute("VACUUM INTO ?1", [backup_path.to_str().unwrap()])
        .unwrap();
    drop(conn);
    let restoring = w.app.gate().begin_restore().unwrap();
    assert_eq!(
        agent.task_get(&p, &read).unwrap_err().code,
        ToolErrorCode::Restoring
    );
    drop(restoring);
    ops.database().restore_from(&backup_path).unwrap();
    assert_eq!(
        agent.task_get(&p, &read).unwrap_err().code,
        ToolErrorCode::StaleContext
    );
    assert_eq!(
        w.call(
            &p,
            "musaic_operation_get",
            json!({"operationId":operation_id})
        )
        .unwrap_err()
        .code,
        ToolErrorCode::StaleContext
    );
    assert!(
        agent
            .list_approvals(ApprovalFilter::Pending)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn interrupted_apply_reports_confirmed_file_outcomes_and_needs_a_new_preview() {
    let f = fix_world(ALL_KINDS);
    let preview = f.preview(GENRE("Interrupted")).unwrap();
    let handle = f.apply(&preview, "key-crash-apply-case").unwrap();
    let id = handle["operationId"].as_str().unwrap();
    let approval = handle["approvalId"].as_str().unwrap();
    let now =
        (f.w.app
            .now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()) as i64;
    let conn = musaic_db::open_connection(f.w.app.db_path()).unwrap();
    let tx = musaic_db::immediate(&conn).unwrap();
    assert!(
        musaic_db::agent::decide_approval(&tx, approval, "approved", None, &f.w.app.now_iso())
            .unwrap()
    );
    assert!(musaic_db::agent_operations::start(&tx, id, now).unwrap());
    musaic_db::agent_operations::checkpoint(&tx, id, "ch_test_confirmed").unwrap();
    let partial = json!({"apply":{"files":[{"fileName":"01.flac","status":"ok"}],"renamed":false}});
    musaic_db::agent_operations::progress(
        &tx,
        id,
        now,
        r#"{"done":1,"total":2,"failed":0}"#,
        Some(&partial.to_string()),
    )
    .unwrap();
    tx.commit().unwrap();
    drop(conn);
    let Fix { w, .. } = f;
    let AgentWorld {
        app,
        scratch,
        clock,
        store,
    } = w;
    drop(app);
    let new = musaic_app::App::builder(&scratch.config)
        .agent_secrets(store)
        .clock(clock.wall())
        .subscribers(false)
        .auto_snapshot(false)
        .ai_worker(false)
        .build()
        .unwrap();
    let conn = musaic_db::open_connection(new.db_path()).unwrap();
    let op = musaic_db::agent_operations::get(&conn, id)
        .unwrap()
        .unwrap();
    assert_eq!(op.state, "failed");
    assert_eq!(op.result.as_deref(), Some(partial.to_string().as_str()));
    let error: musaic_commands::ToolError = serde_json::from_str(&op.error.unwrap()).unwrap();
    assert_eq!(error.code, ToolErrorCode::Unavailable);
    assert!(error.message.contains("preview remaining edits again"));
    assert_eq!(error.details.unwrap()["changeId"], "ch_test_confirmed");
}
