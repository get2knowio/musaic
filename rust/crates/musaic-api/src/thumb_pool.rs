//! The scan's thumbnail pool lives in
//! [`musaic_services::thumb_pool`] so a non-HTTP face shares it; this
//! module re-exports it under its old path. The scan's dependencies with
//! the pool are built by [`musaic_services::scan_run::scan_deps_with_thumbs`].

pub use musaic_services::thumb_pool::*;
