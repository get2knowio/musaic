//! The production sources the routes hand to musaic-services. They live in
//! [`musaic_services::production`] (the network transport, the
//! fingerprinter, the resolver's owned parts, the checks' resolution source,
//! the cover downloader and the check-fix sources); this module re-exports
//! them under their old paths, with [`cached_album`] and [`file_name`].

pub use musaic_services::production::*;
pub use musaic_services::sources::{cached_album, file_name};
