//! The libjpeg-turbo-compatible JPEG decoder lives in
//! [`musaic_services::jpegdec`] so a non-HTTP face shares it; this module
//! re-exports it under its old path.

pub use musaic_services::jpegdec::*;
