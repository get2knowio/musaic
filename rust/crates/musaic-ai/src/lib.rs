//! Research synthesis and the shapes kept from the old job queue.
//!
//! * [`pyjson`]: an ordered JSON value with Python's `json.loads` /
//!   `json.dumps` / `str()` / `repr()`, so stored `result_json` and stream
//!   frames are byte-identical to the Python app's.
//! * [`parse`]: the suggestion types a research result carries.
//! * [`synth`]: the resolver synthesizers and the rule that says when the
//!   resolver settled an album on its own (positional matches count as
//!   resolved, [`synth::unresolved_for_skip_rule`]).
//! * [`env`]: [`env::ResolverEnv`], what the synthesis needs from the host.
//! * [`jobs`]: the job card shape (`job_to_py`) and the change [`jobs::Signal`].
//!
//! Musaic has no built-in model: whatever the resolver cannot settle becomes
//! a research request for a paired assistant (`musaic-app`).

pub mod env;
pub mod jobs;
pub mod parse;
pub mod pyjson;
pub mod synth;

pub use env::ResolverEnv;
pub use jobs::{Signal, job_to_py};
pub use pyjson::Py;
