//! The AI job queue (the Python app's `musaic/jobs.py`, `musaic/services/worker.py`
//! and the deterministic half of `musaic/ai.py`).
//!
//! * [`pyjson`]: an ordered JSON value with Python's `json.loads` /
//!   `json.dumps` / `str()` / `repr()`, so stored `result_json`, prompt
//!   payloads and stream frames are byte-identical to the Python app's.
//! * [`prompts`], [`payload`]: the provider request.
//! * [`parse`]: a provider's final text into a suggestion.
//! * [`synth`]: the resolver synthesizers and the skip-the-provider rule
//!   (positional matches count as resolved,
//!   [`synth::unresolved_for_skip_rule`]).
//! * [`transcript`]: the failure-excerpt breadcrumbs.
//! * [`provider`]: the [`provider::Provider`] seam, [`provider::Unconfigured`],
//!   and (feature `replay`) the test-only replay provider.
//! * [`jobs`]: job / queue-state shapes and the change [`jobs::Signal`].
//! * [`worker`]: one job and the dispatcher loop.
//!
//! The table itself is `musaic_db::ai_jobs`.

pub mod jobs;
pub mod parse;
pub mod payload;
pub mod prompts;
pub mod provider;
pub mod pyjson;
pub mod synth;
pub mod transcript;
pub mod worker;

pub use jobs::{Signal, job_to_py, queue_state, stream_payload};
pub use provider::{
    Provider, ProviderError, ProviderEvent, ProviderRequest, Unconfigured, model_unavailable,
};
pub use pyjson::Py;
pub use worker::{RunOptions, Worker, WorkerEnv, run_next, run_one_job};
