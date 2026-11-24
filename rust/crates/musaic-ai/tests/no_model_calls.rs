//! FR-001, FR-028, FR-033: Musaic holds no model provider and starts no
//! agent program. `musaic-ai` depends on no HTTP client and has no test-only
//! provider feature; no source file of `musaic-ai`, `musaic-app` or
//! `musaic-services` names a model provider or its key, or spawns a process.
//! (No listener beyond loopback is pinned by `musaic-app`'s
//! `security_agent_events.rs`.)

use std::path::{Path, PathBuf};

fn crates() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn sources(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

fn rel(p: &Path) -> String {
    p.strip_prefix(crates())
        .unwrap()
        .to_string_lossy()
        .replace('\\', "/")
}

#[test]
fn musaic_ai_has_no_http_client_and_no_replay_feature() {
    let toml =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml")).unwrap();
    for needle in [
        "reqwest",
        "ureq",
        "hyper",
        "isahc",
        "curl",
        "musaic-net",
        "[features]",
        "replay",
    ] {
        assert!(
            !toml.contains(needle),
            "musaic-ai/Cargo.toml names {needle}"
        );
    }
}

#[test]
fn no_source_names_a_provider_or_spawns_a_process() {
    // Lower-cased needles. The log redaction list in musaic-app's
    // logging.rs keeps credential header names and key prefixes on purpose
    // (no key reaches the log), so that file is checked for the process
    // spawn only.
    let providers = [
        "anthropic",
        "openai",
        "firecrawl",
        "bedrock",
        "gemini",
        "ollama",
        "airframe",
        "copilot",
        "\"claude",
        "claude-",
        "ai_provider",
        "model_unavailable",
        "dyn provider",
    ];
    let mut hits = Vec::new();
    for krate in ["musaic-ai", "musaic-app", "musaic-services"] {
        for p in sources(&crates().join(krate).join("src")) {
            let text = std::fs::read_to_string(&p).unwrap();
            let name = rel(&p);
            for (i, line) in text.lines().enumerate() {
                if line.contains("Command::new") || line.contains("process::Command") {
                    hits.push(format!("{name}:{}: spawns a process", i + 1));
                }
                // The redaction list (see above), and a test's assertion that
                // a text is free of a name.
                if name == "musaic-app/src/logging.rs" || line.contains("assert!(!") {
                    continue;
                }
                // Claude Desktop is an assistant host the person may connect
                // (spec 007), named in the connection code; it is not a model
                // provider, and its per-user install folder is `AnthropicClaude`.
                let lower = line
                    .to_lowercase()
                    .replace("claude desktop", "")
                    .replace("claude-desktop", "")
                    .replace("claude_desktop", "")
                    .replace("anthropicclaude", "")
                    .replace("\"claude.app\"", "");
                for n in providers {
                    if lower.contains(n) {
                        hits.push(format!("{name}:{}: names {n}", i + 1));
                    }
                }
            }
        }
    }
    assert!(hits.is_empty(), "{hits:#?}");
}
