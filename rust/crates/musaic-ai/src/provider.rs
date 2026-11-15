//! The provider seam: what the worker hands a model backend
//! and what it gets back, in the shape of the Python app's airframe call
//! (`ai._run_agent` / `_agent_async`): a request (system prompt, user
//! message, optional cover image, MCP servers, max turns), a stream of
//! events (text / reasoning deltas, tool calls and results, the turn's
//! final text) and a typed failure, which [`ProviderError::message`] turns
//! into the Jobs-page text exactly as `_friendly_agent_error` does.
//!
//! Two providers ship: [`Unconfigured`] (this build has no model backend;
//! every call fails before any model is reached, the way the Python app's
//! preflight fails when its provider's CLI is missing) and, behind the
//! `replay` feature, [`replay::ReplayProvider`], which plays recorded /
//! scripted responses for tests. No network provider exists in this build.

use std::fmt;

use crate::pyjson::Py;

/// Which provider call this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallKind {
    /// `suggest_metadata`.
    Metadata,
    /// `suggest_cover`.
    Cover,
}

/// An image part of the prompt (`airframe.ImageInput(bytes_, media_type)`),
/// placed before the text, as the Python app does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImagePart {
    pub bytes: Vec<u8>,
    pub media_type: String,
}

/// `airframe.McpServerRef(name, transport, url)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServer {
    pub name: String,
    pub transport: String,
    pub url: String,
}

/// One provider call.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderRequest {
    pub kind: CallKind,
    /// The system prompt, with the FireCrawl paragraph appended when a key
    /// is configured (`_build_agent_extras`).
    pub system_prompt: String,
    /// `_format_user_message(payload, hint)`.
    pub user_message: String,
    /// The album payload the message embeds.
    pub payload: Py,
    /// The cover (metadata calls only, when `attach_cover` and one exists).
    pub image: Option<ImagePart>,
    pub mcp_servers: Vec<McpServer>,
    /// `_AGENT_MAX_TURNS`.
    pub max_turns: u32,
}

/// One streamed event (airframe's `RuntimeEvent`s the Python app reads).
#[derive(Debug, Clone, PartialEq)]
pub enum ProviderEvent {
    /// `TextDelta`.
    Text(String),
    /// `ReasoningDelta`.
    Reasoning(String),
    /// `ToolCallStart(tool_name, arguments_preview)`.
    ToolCallStart {
        tool_name: String,
        arguments_preview: String,
    },
    /// `ToolCallResult(output, is_error)`.
    ToolCallResult { output: Py, is_error: bool },
    /// `TurnComplete(result.text)`.
    TurnComplete { text: String },
}

/// A provider's failure (airframe's typed errors, plus the preflight).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderError {
    /// `RuntimeAuthError`.
    Auth(String),
    /// `RuntimeTransientError`.
    Transient(String),
    /// The provider cannot run at all (the Python app's CLI preflight):
    /// `requirement` and `remedy` as `provider_runtime_status` reports them.
    NotReady { requirement: String, remedy: String },
    /// Anything else: the message, and the `__cause__` (type name, text)
    /// when there is one.
    Other {
        message: String,
        cause: Option<(String, String)>,
    },
}

impl ProviderError {
    /// The job's error text (`_friendly_agent_error`; the preflight's
    /// `RuntimeError` passes through it unchanged). `provider_id` is what
    /// the Python app reads from `settings.ai_provider_id()`.
    pub fn message(&self, provider_id: &str) -> String {
        match self {
            ProviderError::Transient(_) => TRANSIENT_MESSAGE.to_string(),
            ProviderError::Auth(_) => format!(
                "The agent backend ({}) rejected the credentials. Check the auth for your \
                 selected provider (for Claude: CLAUDE_CODE_OAUTH_TOKEN / ANTHROPIC_API_KEY, \
                 or the Claude Code credential file).",
                musaic_fsops::py_repr(provider_id)
            ),
            ProviderError::NotReady {
                requirement,
                remedy,
            } => format!(
                "AI provider {} isn't ready \u{2014} it needs {requirement}. {remedy}",
                musaic_fsops::py_repr(provider_id)
            ),
            ProviderError::Other { message, cause } => match cause {
                Some((ty, text)) => format!("{message} (caused by {ty}: {text})"),
                None => message.clone(),
            },
        }
    }
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProviderError::Auth(m) | ProviderError::Transient(m) => f.write_str(m),
            ProviderError::NotReady { requirement, .. } => write!(f, "needs {requirement}"),
            ProviderError::Other { message, .. } => f.write_str(message),
        }
    }
}

impl std::error::Error for ProviderError {}

/// `_friendly_agent_error`'s text for `RuntimeTransientError`.
pub const TRANSIENT_MESSAGE: &str = "The agent backend reported a transient failure \u{2014} most often a rate-limit hit (subscriptions have shared rolling windows), a brief upstream outage, or a model/tool incompatibility. Hit Retry on the AI Jobs page after a moment. To triage, run `.venv/bin/python diagnose_claude.py <album_id>`.";

/// What `GET /api/ai/providers` reports for a provider
/// (`provider_runtime_status` + the auth model).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderInfo {
    pub id: String,
    pub accepts_api_key: bool,
    pub auth_hint: Option<String>,
    pub ready: bool,
    pub requirement: Option<String>,
    pub remedy: Option<String>,
}

/// A model backend. `run` blocks until the call ends (the worker runs it on
/// its own thread and enforces the wall-clock cap); every event is handed
/// to `on_event` as it arrives.
pub trait Provider: Send + Sync {
    /// The id used in messages (`settings.ai_provider_id()` in the Python app).
    fn id(&self) -> &str;
    fn info(&self) -> ProviderInfo;
    fn run(
        &self,
        request: &ProviderRequest,
        on_event: &mut dyn FnMut(ProviderEvent),
    ) -> Result<(), ProviderError>;
    /// Whether a model backend stands behind this provider at all.
    /// `false` only for [`Unconfigured`]: a job that needs a model then
    /// fails without pausing the queue, and a check fix answers
    /// [`model_unavailable`] instead of queueing a job. A configured backend
    /// that fails (even with [`ProviderError::NotReady`]) keeps the Python app's
    /// pause.
    fn has_backend(&self) -> bool {
        true
    }
}

/// The not-available message for work that needs a model when `provider` has no backend, i.e. the text a job that
/// needs a model fails with there and the text a check fix answers instead
/// of queueing one; `None` when a backend exists.
///
/// A provider without a backend is not ready, so the message names no AI: [`NOT_AVAILABLE`]. The user
/// meets it after a Fix the frontend labels MusicBrainz-only, or on a job
/// that turned out to need a model (Retry on the Jobs page, or a resolver
/// answer that changed since the job was queued), never after explicitly
/// choosing AI. (Were such a provider ever to report itself ready, the
/// provider's own `NotReady` message in the Python app's template is used.)
pub fn model_unavailable(provider: &dyn Provider) -> Option<String> {
    if provider.has_backend() {
        return None;
    }
    let info = provider.info();
    if !info.ready {
        return Some(NOT_AVAILABLE.to_string());
    }
    Some(
        ProviderError::NotReady {
            requirement: info.requirement.unwrap_or_default(),
            remedy: info.remedy.unwrap_or_default(),
        }
        .message(provider.id()),
    )
}

/// The not-available message without a ready provider (see
/// [`model_unavailable`]). It also follows "Wrote …; " when a tags fix wrote
/// resolver values first.
pub const NOT_AVAILABLE: &str =
    "MusicBrainz has nothing more for this album, and this build has no other source to try.";

/// No model backend: every call fails before any model is reached, with the
/// shape of the Python app's preflight failure (no excerpt). Unlike the
/// Python app's preflight failure, the queue is not paused.
#[derive(Debug, Default, Clone, Copy)]
pub struct Unconfigured;

/// [`Unconfigured`]'s id.
pub const UNCONFIGURED_ID: &str = "unconfigured";
/// [`Unconfigured`]'s requirement text.
pub const UNCONFIGURED_REQUIREMENT: &str = "an AI backend";
/// [`Unconfigured`]'s remedy text.
pub const UNCONFIGURED_REMEDY: &str = "This build has no AI backend configured, so AI suggestions are unavailable. Deterministic MusicBrainz matching keeps working without it.";

impl Provider for Unconfigured {
    fn id(&self) -> &str {
        UNCONFIGURED_ID
    }

    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: UNCONFIGURED_ID.into(),
            accepts_api_key: false,
            auth_hint: Some(UNCONFIGURED_REMEDY.into()),
            ready: false,
            requirement: Some(UNCONFIGURED_REQUIREMENT.into()),
            remedy: Some(UNCONFIGURED_REMEDY.into()),
        }
    }

    fn run(
        &self,
        _request: &ProviderRequest,
        _on_event: &mut dyn FnMut(ProviderEvent),
    ) -> Result<(), ProviderError> {
        Err(ProviderError::NotReady {
            requirement: UNCONFIGURED_REQUIREMENT.into(),
            remedy: UNCONFIGURED_REMEDY.into(),
        })
    }

    fn has_backend(&self) -> bool {
        false
    }
}

/// Test-only: a provider that plays scripted / recorded responses.
#[cfg(feature = "replay")]
pub mod replay {
    use std::collections::VecDeque;
    use std::sync::{Arc, Condvar, Mutex};

    use super::{Provider, ProviderError, ProviderEvent, ProviderInfo, ProviderRequest};

    /// How one scripted call ends after its events.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Ending {
        /// Returns normally.
        Done,
        /// Fails with this error.
        Fail(ProviderError),
        /// Never returns until [`ReplayProvider::release`] (the worker's
        /// wall-clock cap fires first).
        Hang,
    }

    /// One scripted call.
    #[derive(Debug, Clone, PartialEq)]
    pub struct Script {
        pub events: Vec<ProviderEvent>,
        pub ending: Ending,
    }

    #[derive(Default)]
    struct State {
        scripts: VecDeque<Script>,
        requests: Vec<ProviderRequest>,
        released: bool,
    }

    /// Plays its scripts in order, one per call, and records every request.
    /// A call with no script left fails (`Other("no scripted response")`).
    #[derive(Clone)]
    pub struct ReplayProvider {
        id: String,
        state: Arc<(Mutex<State>, Condvar)>,
    }

    impl ReplayProvider {
        pub fn new(id: &str) -> ReplayProvider {
            ReplayProvider {
                id: id.into(),
                state: Arc::new((Mutex::new(State::default()), Condvar::new())),
            }
        }

        fn lock(&self) -> std::sync::MutexGuard<'_, State> {
            self.state.0.lock().unwrap_or_else(|e| e.into_inner())
        }

        pub fn push(&self, s: Script) {
            self.lock().scripts.push_back(s);
        }

        /// Scripts not played yet.
        pub fn pending(&self) -> usize {
            self.lock().scripts.len()
        }

        /// The requests received so far (and forget them).
        pub fn take_requests(&self) -> Vec<ProviderRequest> {
            std::mem::take(&mut self.lock().requests)
        }

        /// Lets every hanging call return (with `Other("released")`).
        pub fn release(&self) {
            self.lock().released = true;
            self.state.1.notify_all();
        }
    }

    impl Provider for ReplayProvider {
        fn id(&self) -> &str {
            &self.id
        }

        fn info(&self) -> ProviderInfo {
            ProviderInfo {
                id: self.id.clone(),
                accepts_api_key: false,
                auth_hint: None,
                ready: true,
                requirement: None,
                remedy: None,
            }
        }

        fn run(
            &self,
            request: &ProviderRequest,
            on_event: &mut dyn FnMut(ProviderEvent),
        ) -> Result<(), ProviderError> {
            let script = {
                let mut st = self.lock();
                st.requests.push(request.clone());
                st.scripts.pop_front()
            };
            let Some(script) = script else {
                return Err(ProviderError::Other {
                    message: "no scripted response".into(),
                    cause: None,
                });
            };
            for e in script.events {
                on_event(e);
            }
            match script.ending {
                Ending::Done => Ok(()),
                Ending::Fail(e) => Err(e),
                Ending::Hang => {
                    let mut st = self.lock();
                    while !st.released {
                        st = self.state.1.wait(st).unwrap_or_else(|e| e.into_inner());
                    }
                    Err(ProviderError::Other {
                        message: "released".into(),
                        cause: None,
                    })
                }
            }
        }
    }
}
