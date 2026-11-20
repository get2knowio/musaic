//! The one error type of the operations surface.
//!
//! A variant says what went wrong in the application's terms; it never carries a
//! status code. A face decides how to show it (the HTTP face maps each variant to
//! one status, in `musaic-api`). See `specs/001-operations-without-http/contracts/errors.md`.

use crate::gate::Busy;

/// What a [`OpError::NotFound`] could not find.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Subject {
    Album,
    Track,
    Finding,
    Check,
    Job,
    Snapshot,
    Cover,
}

/// Which "no library" it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoLibraryKind {
    /// No library folder is configured.
    NotConfigured,
    /// One is configured and is not there.
    Missing,
}

/// The outside service an [`OpError::Upstream`] failed on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Service {
    MusicBrainz,
    AcoustId,
    CoverArtArchive,
}

/// One reason a plan cannot be carried out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Blocker {
    pub code: String,
    pub message: String,
}

/// Why a rename / conversion / abandon was refused, with the state to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    /// Stable code (`reason_code`).
    pub reason_code: String,
    /// A sentence for the user (`user_message`).
    pub user_message: String,
    pub blockers: Vec<Blocker>,
    /// The digest of the freshly derived plan, when the refusal is that the
    /// plan changed.
    pub digest: Option<String>,
}

/// What a refusal carries besides its message. Typed, so no face has to parse a
/// document to read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Details {
    /// Boxed so the error stays small enough to return by value.
    Refusal(Box<Refusal>),
    /// A job enqueue / retry that met the album's existing active job.
    ExistingJob { job_id: String, status: String },
}

/// An operation's failure. Every variant carries `message`, a plain sentence
/// (for `Internal`, the cause: a face shows something generic).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpError {
    /// A value's meaning is not acceptable.
    InvalidInput {
        field: String,
        message: String,
    },
    /// Understood, but its content is refused.
    Rejected {
        message: String,
        details: Option<Details>,
    },
    NotFound {
        what: Subject,
        id: String,
        message: String,
        /// Why, when the refusal explains itself (a multi-disc conversion's
        /// `reason_code` and sentence); `None` for a plain lookup.
        details: Option<Details>,
    },
    /// A restore is running, or work is and a restore was asked for.
    Busy {
        reason: Busy,
        message: String,
    },
    NoLibrary {
        kind: NoLibraryKind,
        message: String,
    },
    /// The current state forbids it.
    Conflict {
        message: String,
        details: Option<Details>,
    },
    Gone {
        message: String,
    },
    TooLarge {
        limit: u64,
        message: String,
    },
    Upstream {
        service: Service,
        message: String,
    },
    Unavailable {
        message: String,
    },
    Internal {
        message: String,
    },
    /// An unexpected failure whose cause today's route shows in the body
    /// (`{"detail": cause}`, status 500) instead of the plain-text 500: the
    /// album lookup that reads the database, and a multi-disc conversion that
    /// could not start or finish its record (which also says why, in
    /// `details`). Raised nowhere else.
    Failed {
        message: String,
        details: Option<Details>,
    },
}

impl OpError {
    pub fn message(&self) -> &str {
        match self {
            OpError::InvalidInput { message, .. }
            | OpError::Rejected { message, .. }
            | OpError::NotFound { message, .. }
            | OpError::Busy { message, .. }
            | OpError::NoLibrary { message, .. }
            | OpError::Conflict { message, .. }
            | OpError::Gone { message }
            | OpError::TooLarge { message, .. }
            | OpError::Upstream { message, .. }
            | OpError::Unavailable { message }
            | OpError::Internal { message }
            | OpError::Failed { message, .. } => message,
        }
    }

    /// The typed extra a refusal carries, if any.
    pub fn details(&self) -> Option<&Details> {
        match self {
            OpError::Rejected { details, .. }
            | OpError::NotFound { details, .. }
            | OpError::Conflict { details, .. }
            | OpError::Failed { details, .. } => details.as_ref(),
            _ => None,
        }
    }

    /// An unexpected failure whose cause the face shows ([`OpError::Failed`]).
    pub fn failed(cause: impl std::fmt::Display) -> Self {
        OpError::Failed {
            message: cause.to_string(),
            details: None,
        }
    }

    pub fn internal(cause: impl std::fmt::Display) -> Self {
        OpError::Internal {
            message: cause.to_string(),
        }
    }

    /// [`OpError::Failed`] as [`OpError::Internal`], anything else unchanged: for
    /// the callers whose route answered a failed database read with the
    /// plain-text 500 rather than a body that shows the cause.
    pub fn failed_as_internal(self) -> Self {
        match self {
            OpError::Failed { message, .. } => OpError::Internal { message },
            other => other,
        }
    }
}

impl std::fmt::Display for OpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for OpError {}

impl From<Busy> for OpError {
    fn from(b: Busy) -> Self {
        OpError::Busy {
            reason: b,
            message: b.detail().to_string(),
        }
    }
}

impl From<musaic_db::Error> for OpError {
    fn from(e: musaic_db::Error) -> Self {
        OpError::internal(e)
    }
}

impl From<rusqlite::Error> for OpError {
    fn from(e: rusqlite::Error) -> Self {
        OpError::internal(e)
    }
}
