//! The app's log lives in [`musaic_app::logging`] so a non-HTTP
//! face shares it; this module re-exports it under its old path.

pub use musaic_app::logging::*;
