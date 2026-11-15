//! Resolver rules for the Rust build, as pure functions with no I/O.
//!
//! * [`acoustid`] — serde types for an AcoustID `/v2/lookup` response asked with
//!   `meta=recordings releasegroups releases tracks compress`.
//! * [`vote`] — the album-anchoring rule that replaces the Python app's
//!   "Tier 0c fingerprint vote". This is a deliberate change from the Python
//!   app's `musaic/services/resolver.py`: the rule was chosen by measuring
//!   candidate rules against a real library (see the module docs).
//! * [`similarity`] — a faithful port of Python's `difflib.SequenceMatcher.ratio()`
//!   and the title normalisation the tie-break uses.
//! * [`name_search`] — the Tier 0b acceptance check (a deliberate addition).
//! * [`find_release`] — `mb_lookup.find_release`: query, blocklist, hint
//!   reranking, year fallback, CAA check, tracklist parse.
//! * [`resolution`] — `AlbumResolution.to_dict()` and Tier 1 alignment.
//! * [`resolver`] — the cascade, memo and invalidation.
//! * [`source`] — the traits the cascade reads the world through; this crate
//!   cannot depend on `musaic-net` (that crate depends on this one).
//! * [`db_store`] — [`source::AnchorStore`] over the library database.

pub mod acoustid;
pub mod db_store;
pub mod find_release;
pub mod musicbrainz;
pub mod name_search;
pub mod pyfmt;
pub mod resolution;
pub mod resolver;
pub mod similarity;
pub mod source;
pub mod vote;

pub use acoustid::{LookupResponse, LookupResult};
pub use resolution::{AlbumResolution, FieldFact, TrackResolution};
pub use resolver::{AnchorSetEvent, Env, ResolveError, Resolved, Resolver, content_hash};
pub use vote::{
    AlbumContext, Anchor, Candidate, Decision, Outcome, RejectReason, Rule, Winner, candidates,
    decide, matched_tracks, pick,
};
