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
pub fn report(client: &Client, config_dir: &Path, executable: &str) -> (Value, bool) {
    let mut healthy = true;
    let mut descriptor = json!({});
    let mut hello_ok = false;
    let mut profiles: Vec<String> = Vec::new();
    match client.descriptor() {
        Ok(d) => {
            descriptor["instanceId"] = json!(d.instance_id);
            descriptor["port"] = json!(d.port);
            descriptor["pidAlive"] = json!(pid_alive(d.pid));
            profiles = d.protocol_profiles.clone();
            match client.hello(&d) {
                Ok(_) => {
                    descriptor["status"] = json!("ok");
                    hello_ok = true;
                }
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
    let mut extensions = Vec::new();
    match client.choose_pairing() {
        Ok(id) => {
            pairing["id"] = json!(id);
            if hello_ok {
                match client.call("musaic_capabilities", &json!({})) {
                    Ok(caps) => {
                        pairing["status"] = json!("active");
                        let tasks = musaic_commands::tools::task::EXTENSION_ID;
                        if caps["extensions"]
                            .as_array()
                            .is_some_and(|ids| ids.iter().any(|id| id.as_str() == Some(tasks)))
                        {
                            extensions.push(tasks);
                        }
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
    (
        json!({
            "executable": executable,
            "version": env!("CARGO_PKG_VERSION"),
            "configDir": config_dir.display().to_string(),
            "descriptor": descriptor,
            "pairing": pairing,
            "scopes": scopes,
            "transport": "stdio",
            "profiles": profiles,
            "extensions": extensions,
        }),
        healthy,
    )
}
