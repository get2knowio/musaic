//! The skill's text may name only what exists, grants nothing, and holds no
//! credential, account or subscription wording (FR-003, FR-004).

use musaic_commands::events::EVENT_NAMES;
use musaic_commands::skill::{bundle, parse_frontmatter};
use musaic_commands::{REGISTRY, find_tool};

fn words<'a>(text: &'a str, prefix: &'a str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(i) = rest.find(prefix) {
        let tail = &rest[i..];
        let end = tail
            .find(|c: char| !(c.is_ascii_lowercase() || c == '_' || c == '.' || c.is_ascii_digit()))
            .unwrap_or(tail.len());
        out.push(tail[..end].trim_end_matches('.').to_string());
        rest = &tail[end..];
    }
    out
}

#[test]
fn no_file_uses_credential_account_or_subscription_wording() {
    let banned = [
        "credential",
        "account",
        "subscription",
        "login",
        "log in",
        "sign in",
        "password",
        "api key",
        "api-key",
        "bearer",
        "oauth",
    ];
    for e in &bundle().entries {
        let text = e.text().to_lowercase();
        for word in banned {
            assert!(!text.contains(word), "{} contains {word:?}", e.path);
        }
    }
}

#[test]
fn no_file_names_a_filesystem_path() {
    for e in &bundle().entries {
        for token in e
            .text()
            .split(|c: char| c.is_whitespace() || "`()[]\",".contains(c))
        {
            let absolute = token.len() > 1
                && token.starts_with('/')
                && token[1..].starts_with(|c: char| c.is_ascii_alphabetic());
            assert!(!absolute, "{}: {token}", e.path);
            assert!(!token.starts_with("~/"), "{}: {token}", e.path);
            assert!(!token.contains(":\\"), "{}: {token}", e.path);
            assert!(
                !token.contains("/Users/") && !token.contains("/home/"),
                "{}: {token}",
                e.path
            );
        }
    }
}

#[test]
fn every_tool_and_event_name_exists() {
    for e in &bundle().entries {
        for name in words(e.text(), "musaic_") {
            assert!(
                find_tool(&name).is_some(),
                "{} names {name}, which is not in the registry",
                e.path
            );
        }
        for name in words(e.text(), "musaic.work.") {
            assert!(
                EVENT_NAMES.contains(&name.as_str()),
                "{} names the event {name}",
                e.path
            );
        }
    }
}

#[test]
fn the_guide_grants_nothing_and_every_tool_it_needs_is_cited() {
    let md = bundle().skill_md().text();
    assert!(md.contains("grants no scope and no permission"));
    for tool in [
        "musaic_capabilities",
        "musaic_work_list",
        "musaic_work_get",
        "musaic_work_claim",
        "musaic_work_renew",
        "musaic_work_progress",
        "musaic_work_complete",
        "musaic_work_release",
        "musaic_work_fail",
        "musaic_operation_get",
    ] {
        assert!(md.contains(tool), "{tool}");
        assert!(REGISTRY.iter().any(|t| t.id == tool));
    }
}

#[test]
fn skill_md_has_the_required_frontmatter() {
    let fm = parse_frontmatter(bundle().skill_md().text()).unwrap();
    assert_eq!(fm["name"], "musaic-work-delegation");
    assert!(!fm["description"].as_str().unwrap().is_empty());
    assert!(fm["metadata"]["version"].as_str().is_some());
}

#[test]
fn the_examples_include_a_success_an_ambiguity_and_a_no_match() {
    let text = bundle()
        .lookup("skill://musaic-work-delegation/references/metadata-evidence.md")
        .unwrap()
        .text()
        .to_string();
    for needle in ["**Successful.**", "**Ambiguous.**", "**No match.**"] {
        assert!(text.contains(needle), "{needle}");
    }
    assert!(text.contains("synthetic"));
}
