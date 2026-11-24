//! `musaic-cli doctor`: what an assistant host needs to work, reported
//! without a secret and without starting any agent run. The only tool it
//! calls is `capabilities`.

use std::path::Path;

use musaic_commands::descriptor::DescriptorError;
use musaic_commands::{PROTOCOL_PROFILES, ToolErrorCode};
use serde_json::{Value, json};

use crate::client::Client;

/// Best effort, Unix and Windows: whether a process with this id exists. A
/// hint only; nothing is decided by it.
pub fn pid_alive(pid: u32) -> Option<bool> {
    #[cfg(unix)]
    {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(std::process::Stdio::null())
            .status()
            .ok()
            .map(|s| s.success())
    }
    #[cfg(windows)]
    {
        let out = std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .output()
            .ok()?;
        Some(String::from_utf8_lossy(&out.stdout).contains(&pid.to_string()))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        None
    }
}

/// The report, and whether everything is healthy.
pub fn report(
    client: &Client,
    config_dir: &Path,
    executable: &str,
    paths: &crate::setup::HostPaths,
) -> (Value, bool) {
    let mut healthy = true;
    let mut descriptor = json!({});
    let mut version_note: Option<String> = None;
    let mut hello_ok = false;
    let mut profiles: Vec<String> = Vec::new();
    match client.descriptor() {
        Ok(d) => {
            descriptor["instanceId"] = json!(d.instance_id);
            descriptor["port"] = json!(d.port);
            descriptor["pidAlive"] = json!(pid_alive(d.pid));
            descriptor["appVersion"] = json!(d.app_version);
            if d.app_version != env!("CARGO_PKG_VERSION") {
                version_note = Some(version_mismatch_text(
                    env!("CARGO_PKG_VERSION"),
                    &d.app_version,
                ));
            }
            profiles = d.protocol_profiles.clone();
            match client.hello(&d) {
                Ok(h) => match crate::client::check_contract(&h.contract_version) {
                    Ok(()) => {
                        descriptor["status"] = json!("ok");
                        hello_ok = true;
                    }
                    Err(e) => {
                        descriptor["status"] = json!("contract_mismatch");
                        descriptor["reason"] = json!(e.message);
                        healthy = false;
                    }
                },
                Err(_) => {
                    descriptor["status"] = json!("stale");
                    healthy = false;
                }
            }
        }
        Err(e) => {
            healthy = false;
            let absent = e.code == ToolErrorCode::AppNotRunning;
            descriptor["status"] = json!(if absent { "absent" } else { "invalid" });
            if !absent {
                descriptor["reason"] = json!(e.message);
            }
        }
    }
    if profiles.is_empty() {
        profiles = PROTOCOL_PROFILES.iter().map(|p| p.to_string()).collect();
    }

    let mut pairing = json!({"status": "unknown"});
    let mut scopes: Vec<String> = Vec::new();
    let mut extensions: Vec<String> = Vec::new();
    let mut session = json!({"hostReported": null});
    match client.choose_pairing() {
        Ok(id) => {
            pairing["id"] = json!(id);
            if hello_ok {
                match client.call("musaic_capabilities", &json!({})) {
                    Ok(caps) => {
                        pairing["status"] = json!("active");
                        extensions = caps["extensions"]
                            .as_array()
                            .map(|ids| {
                                ids.iter()
                                    .filter_map(Value::as_str)
                                    .filter(|id| {
                                        musaic_commands::events::OFFERED_EXTENSIONS.contains(id)
                                    })
                                    .map(str::to_string)
                                    .collect()
                            })
                            .unwrap_or_default();
                        session = match client.session_status() {
                            Ok(s) => json!({
                                "hostReported": s.observation.as_ref().map(|o| json!({
                                    "clientName": o.client_name,
                                    "clientVersion": o.client_version,
                                    "profile": o.profile,
                                    "tasks": o.tasks,
                                    "skills": o.skills,
                                    "events": o.events,
                                })),
                                "subscriptionActive": s.subscription_active,
                                "appSkillVersion": s.skill_version,
                            }),
                            Err(_) => json!({"hostReported": null}),
                        };
                        scopes = caps["scopes"]
                            .as_array()
                            .map(|a| {
                                a.iter()
                                    .filter_map(|s| s.as_str().map(str::to_string))
                                    .collect()
                            })
                            .unwrap_or_default();
                    }
                    Err(e) => {
                        healthy = false;
                        pairing["status"] = json!(match e.code {
                            ToolErrorCode::NotPaired => "not_paired",
                            ToolErrorCode::LockedOut => "locked_out",
                            ToolErrorCode::InstanceMismatch => "instance_mismatch",
                            _ => "error",
                        });
                    }
                }
            } else {
                pairing["status"] = json!("not_checked");
            }
        }
        Err(e) => {
            healthy = false;
            pairing["status"] = json!("none");
            pairing["reason"] = json!(e.message);
        }
    }
    let _ = DescriptorError::Absent;
    let mut report = json!({
        "executable": executable,
        "version": env!("CARGO_PKG_VERSION"),
        "configDir": config_dir.display().to_string(),
        "descriptor": descriptor,
        "pairing": pairing,
        "scopes": scopes,
        "transport": "stdio",
        "profiles": profiles,
        "extensions": extensions,
        "skill": crate::skill_install::status(paths),
        "session": session,
        "claudeDesktop": claude_desktop_line(paths),
    });
    if let Some(note) = version_note {
        report["versionNote"] = json!(note);
    }
    (report, healthy)
}

/// The words for a program and an app that are not the same version.
pub fn version_mismatch_text(cli: &str, app: &str) -> String {
    format!("musaic-cli {cli} is not the same version as the running app ({app}).")
}

/// One line about Claude Desktop's configuration file: whether it holds a
/// `musaic` entry and whose. Read-only; a connection made through the
/// extension does not appear there and is managed in Claude itself.
fn claude_desktop_line(paths: &crate::setup::HostPaths) -> Value {
    use musaic_commands::claude_desktop::{EntryOwner, entry_owner};
    // As the app's report: only a missing file is "no entry"; one that exists
    // but cannot be read says nothing about what it holds.
    let owner = match std::fs::read_to_string(&paths.claude_desktop) {
        Ok(text) => entry_owner(Some(&text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => EntryOwner::Absent,
        Err(_) => EntryOwner::Unreadable,
    };
    let (state, words) = match owner {
        EntryOwner::Absent => (
            "none",
            "no musaic entry in Claude Desktop's configuration file",
        ),
        EntryOwner::Ours => ("ours", "the musaic entry was written by Musaic"),
        EntryOwner::Foreign => ("foreign", "the musaic entry was not written by Musaic"),
        EntryOwner::Unreadable => (
            "unreadable",
            "Claude Desktop's configuration file cannot be read",
        ),
    };
    let extension = crate::setup::extension_state(paths);
    // "Not detected" says only that the folder was not found; it is never
    // worded as proof that no extension is installed.
    let ext_words = match extension {
        musaic_commands::claude_desktop::ExtensionState::Detected => {
            "The Musaic extension is installed in Claude Desktop; it is managed in Claude Desktop's extension settings."
        }
        _ => {
            "A Musaic extension installed from the app is managed in Claude Desktop's extension settings."
        }
    };
    json!({
        "entry": state,
        "extension": extension.as_str(),
        "message": format!("{words}. {ext_words}"),
    })
}
