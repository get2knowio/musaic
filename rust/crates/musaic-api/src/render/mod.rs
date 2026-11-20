//! Rendering the operations surface as HTTP: the one place an [`OpError`]
//! becomes a status (`contracts/errors.md`, rule 1: the status is a function of
//! the variant).

pub mod analyze;
pub mod apply;
pub mod checks;
pub mod fixes;
pub mod jobs;
pub mod library;
pub mod settings;
pub mod system;
pub mod tags;

use musaic_services::OpError;

use crate::http::ApiError;

/// The status of each variant. A variant that needed two would be split.
pub(crate) fn status(e: &OpError) -> u16 {
    match e {
        OpError::InvalidInput { .. } => 422,
        OpError::Rejected { .. } => 400,
        OpError::NotFound { .. } => 404,
        OpError::Busy { .. } | OpError::NoLibrary { .. } | OpError::Conflict { .. } => 409,
        OpError::Gone { .. } => 410,
        OpError::TooLarge { .. } => 413,
        OpError::Upstream { .. } => 502,
        OpError::Unavailable { .. } => 503,
        OpError::Internal { .. } | OpError::Failed { .. } => 500,
    }
}

/// `{"detail": message}` with the variant's status; `Internal` goes through
/// [`ApiError::internal`], which logs the cause and sends the plain-text 500.
/// `Failed` keeps the cause in `detail` (a 500 with a body). A refusal's typed
/// `details` are rendered into body keys by the slice that
/// converts a route carrying them.
impl From<OpError> for ApiError {
    fn from(e: OpError) -> Self {
        if let OpError::Internal { message } = &e {
            return ApiError::internal(message);
        }
        ApiError::new(status(&e), e.message())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use musaic_services::Subject;

    #[test]
    fn statuses_follow_the_variant() {
        let nf = OpError::NotFound {
            what: Subject::Job,
            id: "j".into(),
            message: "Job not found".into(),
        };
        let a = ApiError::from(nf);
        assert_eq!(a.status, 404);
        assert_eq!(a.detail, serde_json::json!("Job not found"));
        let a = ApiError::from(OpError::internal("boom"));
        assert_eq!(a.status, 500);
        assert!(a.detail.is_null());
        // The album lookup keeps its cause in the body.
        let a = ApiError::from(OpError::failed("disk I/O error"));
        assert_eq!(a.status, 500);
        assert_eq!(a.detail, serde_json::json!("disk I/O error"));
    }
}
