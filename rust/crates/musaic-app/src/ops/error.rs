//! The surface's error type; defined in `musaic-services` so services can return it.

pub use musaic_services::error::{
    Blocker, Details, NoLibraryKind, OpError, Refusal, Service, Subject,
};
