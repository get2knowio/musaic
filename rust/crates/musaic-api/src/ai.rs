//! The research requests' host side ([`AiQueue`], [`AppEnv`], job ids)
//! lives in [`musaic_app::ai`]
//! so a non-HTTP face shares it; this module re-exports it under its old
//! path.

pub use musaic_app::ai::*;
