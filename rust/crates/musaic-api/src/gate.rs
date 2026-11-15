//! The restore gate lives in
//! [`musaic_services::gate`] so a non-HTTP face shares it; this module
//! re-exports it and maps a refusal to the HTTP answer: [`Busy`] becomes a
//! 409 with [`Busy::detail`] (before every check a handler makes
//! itself).

pub use musaic_services::gate::*;

use crate::http::ApiError;

impl From<Busy> for ApiError {
    fn from(b: Busy) -> Self {
        ApiError::new(409, b.detail())
    }
}
