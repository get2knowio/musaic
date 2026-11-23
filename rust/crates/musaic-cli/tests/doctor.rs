//! `doctor` (T040): healthy, no app, a revoked pairing, and never a secret.

mod common;

use common::World;
use musaic_cli::client::Client;
use musaic_cli::commands::{self, EXIT_OK, EXIT_PROBLEMS};
use musaic_cli::credentials::Memory;
use musaic_commands::PermissionKind as K;

#[test]
fn a_healthy_setup_reports_everything_and_no_secret() {
    let w = World::with(false, false);
    let id = w.pair(&[K::LibraryRead, K::ProposalSubmit]);
    let client = w.client(&id);
    let out = commands::doctor(&client, &w.config);
    assert_eq!(out.code, EXIT_OK, "{}", out.json);
    let j = &out.json;
    assert_eq!(j["descriptor"]["status"], "ok");
    assert!(j["descriptor"]["port"].as_u64().unwrap() > 0);
    assert!(
        j["descriptor"]["instanceId"]
            .as_str()
            .unwrap()
            .starts_with("i_")
    );
    assert_eq!(j["pairing"]["status"], "active");
    assert_eq!(j["pairing"]["id"], id);
    assert_eq!(
        j["scopes"],
        serde_json::json!(["library.read", "proposal.submit"])
    );
    assert_eq!(j["transport"], "stdio");
    assert_eq!(
        j["profiles"],
        serde_json::json!(["mcp-2026-07-28", "mcp-2025-11-25"])
    );
    assert_eq!(
        j["extensions"],
        serde_json::json!(["io.modelcontextprotocol/tasks"])
    );
    assert_eq!(j["version"], env!("CARGO_PKG_VERSION"));
    assert!(j["executable"].as_str().is_some());
    let text = j.to_string();
    assert!(
        !text.contains(&w.secret_hex(&id)),
        "doctor printed the secret"
    );
}

#[test]
fn without_the_app_doctor_says_so_and_exits_1() {
    let w = World::with(false, false);
    let id = w.pair(&[K::LibraryRead]);
    let client = w.client(&id);
    let mut w = w;
    w.stop();
    let out = commands::doctor(&client, &w.config);
    assert_eq!(out.code, EXIT_PROBLEMS);
    assert_eq!(out.json["descriptor"]["status"], "absent");
    assert_eq!(out.json["pairing"]["status"], "not_checked");
    assert_eq!(out.json["scopes"], serde_json::json!([]));
}

#[test]
fn a_revoked_pairing_is_reported_not_paired() {
    let w = World::with(false, false);
    let id = w.pair(&[K::LibraryRead]);
    let client = w.client(&id);
    w.state.ops().agent().revoke_pairing(&id).unwrap();
    let out = commands::doctor(&client, &w.config);
    assert_eq!(out.code, EXIT_PROBLEMS);
    assert_eq!(out.json["descriptor"]["status"], "ok");
    assert_eq!(out.json["pairing"]["status"], "not_paired");
}

#[test]
fn with_no_credential_doctor_names_the_problem() {
    let w = World::with(false, false);
    let _id = w.pair(&[K::LibraryRead]);
    let client = Client::new(&w.config, Box::new(Memory::new()), None);
    let out = commands::doctor(&client, &w.config);
    assert_eq!(out.code, EXIT_PROBLEMS);
    assert_eq!(out.json["pairing"]["status"], "none");
    assert!(
        out.json["pairing"]["reason"]
            .as_str()
            .unwrap()
            .contains("credential")
    );
}

#[test]
fn a_dev_credential_file_is_refused_outside_a_scratch_directory() {
    let home_like = std::path::Path::new("/Users/someone/.musaic");
    if cfg!(unix) && !home_like.starts_with(std::env::temp_dir()) {
        let e = commands::credential_source(home_like, true)
            .err()
            .expect("refused");
        assert_eq!(e.code, musaic_commands::ToolErrorCode::NotPaired);
    }
}
