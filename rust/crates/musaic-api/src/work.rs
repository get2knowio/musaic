//! The quit-confirmation rule ([`WorkSnapshot`]) lives in
//! [`musaic_app::work`] so a non-HTTP face shares it; this module
//! re-exports it under its old path.

pub use musaic_app::work::*;
