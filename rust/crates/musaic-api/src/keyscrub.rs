//! The secret-settings migration lives
//! in [`musaic_services::keyscrub`] so a non-HTTP face shares it; this
//! module re-exports it. It logs through the app's log
//! ([`crate::logging::AppLog`] implements [`musaic_services::host_log::HostLog`]).
//! Scheduling the timed retry against the app state, the retry after a
//! restore and the shutdown call stay in `crate::state`.

pub use musaic_services::keyscrub::*;
