//! The CLI's client of the running app: find it through the descriptor,
//! prove both sides hold the pairing secret, then call tools with the token.
//!
//! The secret never crosses the wire: the client proves it with an HMAC over
//! the handshake's nonces, and does not use the token until the server's own
//! proof verifies, so a process squatting on the port learns nothing and
//! gets no calls. Requests go to `http://127.0.0.1:<port>` with exactly that
//! Host and no Origin.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use musaic_commands::auth::{
    ExchangeRequest, ExchangeResponse, HelloResponse, RENEW_BEFORE_SECS, TOKEN_TTL_SECS,
    client_proof, config_dir_id, random_hex, verify_server_proof,
};
use musaic_commands::descriptor::{self, Descriptor};
use musaic_commands::tools::task::{TaskCreateInput, TaskInput, TaskSnapshot, TaskUpdateInput};
use musaic_commands::{ToolError, ToolErrorCode};
use serde_json::Value;

use crate::credentials::CredentialSource;

/// How long `hello` may take before the app counts as not running.
pub const HELLO_TIMEOUT: Duration = Duration::from_secs(2);
/// A tool call can run long (an apply under the work gate).
pub const CALL_TIMEOUT: Duration = Duration::from_secs(15 * 60);

struct Session {
    port: u16,
    instance_id: String,
    token: String,
    renew_at: Instant,
}

pub struct Client {
    config_dir: PathBuf,
    config_id: String,
    creds: Box<dyn CredentialSource>,
    pairing: Option<String>,
    session: Mutex<Option<Session>>,
}

fn err(code: ToolErrorCode, message: impl Into<String>) -> ToolError {
    ToolError::new(code, message)
}

fn not_running() -> ToolError {
    err(
        ToolErrorCode::AppNotRunning,
        "Musaic is not running. Start Musaic, then try again. The CLI never starts Musaic itself.",
    )
}

/// The id of a config dir, as the app computes it.
pub fn config_id_of(dir: &Path) -> String {
    let resolved = musaic_core::config::resolve_path(dir).unwrap_or_else(|_| dir.to_path_buf());
    config_dir_id(&resolved.to_string_lossy())
}

fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        // Never follow a redirect: nothing here should leave the app.
        .max_redirects(0)
        .proxy(None)
        .build()
        .into()
}

fn post_json(
    agent: &ureq::Agent,
    url: &str,
    bearer: Option<&str>,
    body: &Value,
) -> Result<(u16, Value), ureq::Error> {
    let mut req = agent.post(url).header("content-type", "application/json");
    if let Some(t) = bearer {
        req = req.header("authorization", format!("Bearer {t}"));
    }
    let mut resp = req.send(body.to_string().as_bytes())?;
    let status = resp.status().as_u16();
    let text = resp.body_mut().read_to_string().unwrap_or_default();
    Ok((status, serde_json::from_str(&text).unwrap_or(Value::Null)))
}

/// A `{ok:false, error}` body as the error it carries.
pub fn tool_error_of(v: &Value) -> ToolError {
    let e = &v["error"];
    let code = e["code"]
        .as_str()
        .and_then(ToolErrorCode::parse)
        .unwrap_or(ToolErrorCode::Internal);
    let mut te = ToolError::new(code, e["message"].as_str().unwrap_or("the request failed"));
    if let Some(d) = e["details"].as_object() {
        for (k, v) in d {
            te = te.with_detail(k, v.as_str().unwrap_or_default());
        }
    }
    te
}

impl Client {
    pub fn new(
        config_dir: &Path,
        creds: Box<dyn CredentialSource>,
        pairing: Option<String>,
    ) -> Self {
        Self {
            config_dir: config_dir.to_path_buf(),
            config_id: config_id_of(config_dir),
            creds,
            pairing,
            session: Mutex::new(None),
        }
    }

    pub fn config_id(&self) -> &str {
        &self.config_id
    }

    /// The descriptor, validated; its refusals are the contract's errors.
    pub fn descriptor(&self) -> Result<Descriptor, ToolError> {
        descriptor::read_validated(&self.config_dir).map_err(|e| e.to_tool_error())
    }

    /// The pairing to use: `--pairing`, else the only one with a credential.
    pub fn choose_pairing(&self) -> Result<String, ToolError> {
        if let Some(p) = &self.pairing {
            return Ok(p.clone());
        }
        let mut with_secret = Vec::new();
        for id in self.creds.pairings(&self.config_id)? {
            if self.creds.secret(&self.config_id, &id)?.is_some() {
                with_secret.push(id);
            }
        }
        match with_secret.len() {
            1 => Ok(with_secret.remove(0)),
            0 => Err(err(
                ToolErrorCode::NotPaired,
                "no pairing has a credential for this config dir; pair an assistant in Musaic (Settings, Agents)",
            )),
            _ => Err(err(
                ToolErrorCode::NotPaired,
                format!(
                    "several pairings have a credential; pass --pairing with one of: {}",
                    with_secret.join(", ")
                ),
            )),
        }
    }

    /// The `hello` of the running app, without any credential.
    pub fn hello(&self, d: &Descriptor) -> Result<HelloResponse, ToolError> {
        let url = format!("http://127.0.0.1:{}/__musaic/agent/v1/hello", d.port);
        let (status, v) = post_json(&agent(HELLO_TIMEOUT), &url, None, &serde_json::json!({}))
            .map_err(|_| not_running())?;
        if status != 200 {
            return Err(not_running());
        }
        let h: HelloResponse = serde_json::from_value(v).map_err(|_| not_running())?;
        if h.instance_id != d.instance_id {
            // The descriptor names an instance that is gone: stale.
            return Err(not_running());
        }
        Ok(h)
    }

    fn connect(&self) -> Result<Session, ToolError> {
        let d = self.descriptor()?;
        let hello = self.hello(&d)?;
        let pairing = self.choose_pairing()?;
        let secret = self
            .creds
            .secret(&self.config_id, &pairing)?
            .ok_or_else(|| {
                err(
                    ToolErrorCode::NotPaired,
                    "this pairing has no credential here",
                )
            })?;
        let client_nonce = random_hex(16);
        let req = ExchangeRequest {
            pairing_id: pairing.clone(),
            instance_id: hello.instance_id.clone(),
            server_nonce: hello.server_nonce.clone(),
            client_nonce: client_nonce.clone(),
            client_proof: client_proof(
                &secret,
                &pairing,
                &hello.instance_id,
                &hello.server_nonce,
                &client_nonce,
            ),
        };
        let url = format!("http://127.0.0.1:{}/__musaic/agent/v1/exchange", d.port);
        let (status, v) = post_json(
            &agent(HELLO_TIMEOUT),
            &url,
            None,
            &serde_json::to_value(&req).unwrap_or(Value::Null),
        )
        .map_err(|_| not_running())?;
        match status {
            200 => {}
            429 | 401 => return Err(tool_error_of(&v)),
            _ => {
                return Err(err(
                    ToolErrorCode::Internal,
                    "the app refused the handshake",
                ));
            }
        }
        let resp: ExchangeResponse = serde_json::from_value(v).map_err(|_| {
            err(
                ToolErrorCode::InstanceMismatch,
                "the app's answer is not a handshake answer",
            )
        })?;
        // The token is used only once the server has proved it holds the secret.
        if !verify_server_proof(
            &secret,
            &resp.server_proof,
            &pairing,
            &hello.instance_id,
            &hello.server_nonce,
            &client_nonce,
        ) {
            return Err(err(
                ToolErrorCode::InstanceMismatch,
                "whatever is listening there did not prove it is Musaic with this pairing",
            ));
        }
        Ok(Session {
            port: d.port as u16,
            instance_id: hello.instance_id,
            token: resp.token,
            renew_at: Instant::now() + Duration::from_secs(TOKEN_TTL_SECS - RENEW_BEFORE_SECS),
        })
    }

    fn session(&self, force: bool) -> Result<(u16, String), ToolError> {
        let mut guard = self
            .session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let stale = guard
            .as_ref()
            .is_none_or(|s| force || Instant::now() >= s.renew_at);
        if stale {
            *guard = Some(self.connect()?);
        }
        let s = guard.as_ref().expect("a session was just made");
        Ok((s.port, s.token.clone()))
    }

    /// The instance id of the current session, if connected.
    pub fn instance_id(&self) -> Option<String> {
        self.session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .map(|s| s.instance_id.clone())
    }

    /// Call a tool; the result object, or the tool's error. A 401 renews the
    /// token once and tries again.
    pub fn call(&self, tool_id: &str, input: &Value) -> Result<Value, ToolError> {
        self.post_authenticated(&format!("tools/{tool_id}"), input)
    }

    fn post_authenticated(&self, method: &str, input: &Value) -> Result<Value, ToolError> {
        for attempt in 0..2 {
            let (port, token) = self.session(attempt == 1)?;
            let url = format!("http://127.0.0.1:{port}/__musaic/agent/v1/{method}");
            let (status, v) = match post_json(&agent(CALL_TIMEOUT), &url, Some(&token), input) {
                Ok(r) => r,
                Err(_) => {
                    // The app may have restarted (a new port and instance):
                    // forget the session and discover it again once.
                    *self
                        .session
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
                    if attempt == 0 {
                        continue;
                    }
                    return Err(not_running());
                }
            };
            if status == 401 && attempt == 0 {
                continue;
            }
            return if v["ok"] == Value::Bool(true) {
                Ok(v["result"].clone())
            } else if v["ok"] == Value::Bool(false) {
                Err(tool_error_of(&v))
            } else {
                Err(err(
                    ToolErrorCode::Internal,
                    "the app's answer was not understood",
                ))
            };
        }
        Err(err(
            ToolErrorCode::NotPaired,
            "the app did not accept the token",
        ))
    }

    fn task_request<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        input: &impl serde::Serialize,
    ) -> Result<T, ToolError> {
        let input = serde_json::to_value(input).expect("a typed request is JSON");
        let result = self.post_authenticated(method, &input)?;
        serde_json::from_value(result).map_err(|_| {
            err(
                ToolErrorCode::Internal,
                "the app's Task answer was not understood",
            )
        })
    }
}

impl musaic_mcp::Backend for Client {
    fn call_tool(&self, tool_id: &str, input: Value) -> Result<Value, ToolError> {
        self.call(tool_id, &input)
    }

    fn supports_tasks(&self) -> bool {
        true
    }

    fn create_task(&self, input: TaskCreateInput) -> Result<TaskSnapshot, ToolError> {
        self.task_request("tasks/create", &input)
    }

    fn get_task(&self, input: TaskInput) -> Result<TaskSnapshot, ToolError> {
        self.task_request("tasks/get", &input)
    }

    fn update_task(&self, input: TaskUpdateInput) -> Result<(), ToolError> {
        self.task_request("tasks/update", &input)
    }

    fn cancel_task(&self, input: TaskInput) -> Result<(), ToolError> {
        self.task_request("tasks/cancel", &input)
    }
}
