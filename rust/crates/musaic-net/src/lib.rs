//! Outbound HTTP for the resolver.
//!
//! Everything goes through a [`Transport`]. Two exist: [`ReplayTransport`],
//! which serves responses recorded from the real services
//! (the recorded replay set (private test data)), and [`LiveTransport`], blocking HTTPS over `ureq`
//! (rustls). Clients take either; neither knows which it has.
//!
//! The API is blocking. Pacing and retry are written against a [`Clock`] so
//! tests run on virtual time. A tokio host calls the clients from
//! `spawn_blocking`; see [`live`] for how to build them. Background work
//! that should yield to the user's own requests goes through
//! [`BackgroundPaced`] (see [`background`]).
//!
//! No secret (the AcoustID key, a form field) appears in any `Display` or
//! `Debug` output of this crate: URLs in errors are redacted and free text
//! that could echo a request is scrubbed of the request's secrets.

pub mod background;
mod caa;
mod clients;
pub mod live;
mod pacing;
pub mod preview;
mod replay;
mod request;

pub use background::{BACKGROUND_POLL, BACKGROUND_QUIET, BackgroundPaced};
pub use caa::{
    CAA_TIMEOUT, COVER_ACCEPT, COVER_DOWNLOAD_TIMEOUT, COVER_MAX_BYTES, CoverArtClient, CoverError,
    CoverImage, FrontCover,
};
pub use clients::{
    ACOUSTID_META, ACOUSTID_TIMEOUT, AcoustIdClient, ClientError, FailureClass,
    MB_RECORDING_TIMEOUT, MB_TIMEOUT, MalformedRecording, MusicBrainzClient, RecordingCanonical,
    RecordingLookup, USER_AGENT, join_artist_credits, name_search_terms, parse_recording_canonical,
};
pub use live::{LiveClients, LiveTransport};
pub use pacing::{
    Clock, Limiter, MAX_RETRY_WAIT, Paced, PacingPolicy, RetryPolicy, SystemClock, process_limiter,
};
pub use replay::{FollowRedirects, ReplayTransport};
pub use request::{Method, Request, Response};

/// Why a request produced no response. URLs are always redacted.
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    /// Replay only: nothing was recorded for this request.
    #[error("unobserved in replay set (key {key}): {method} {url}")]
    Unobserved {
        key: String,
        method: &'static str,
        url: String,
    },
    /// Refused before any I/O: only HTTPS may leave the machine (a
    /// deliberate change: the Python app also sent plain HTTP).
    #[error("refused to send over plain HTTP: {url}")]
    InsecureScheme { url: String },
    /// The host name did not resolve (offline, DNS down).
    #[error("could not resolve host {host}")]
    Dns { host: String },
    /// No connection, or the connection was refused / reset before an answer.
    #[error("connection failed for {url}: {detail}")]
    Connect { url: String, detail: String },
    /// Connect or read took longer than the request's timeout.
    #[error("timed out after {seconds:.1} s: {url}")]
    Timeout { url: String, seconds: f64 },
    #[error("TLS failure for {url}: {detail}")]
    Tls { url: String, detail: String },
    /// The answer was cut short or was not valid HTTP.
    #[error("broken response from {url}: {detail}")]
    Protocol { url: String, detail: String },
    #[error("more than {limit} redirects from {url}")]
    TooManyRedirects { url: String, limit: u32 },
    #[error("response body from {url} is larger than {limit} bytes")]
    BodyTooLarge { url: String, limit: u64 },
    /// Refused before connecting: a [`Request::public_only`] request for a
    /// URL or an address that is not on the public internet.
    #[error("refused to fetch {url}: {reason}")]
    Refused { url: String, reason: String },
    /// A [`BackgroundPaced`] request was stopped by its caller while it
    /// waited for a quiet slot; nothing was sent.
    #[error("background request stopped before it was sent")]
    Stopped,
    #[error("transport: {0}")]
    Other(String),
}

impl TransportError {
    /// The host was unreachable in a way that says "offline" (DNS, refused
    /// connection, timeout), as opposed to a server that answered badly.
    pub fn is_offline(&self) -> bool {
        matches!(
            self,
            TransportError::Dns { .. }
                | TransportError::Connect { .. }
                | TransportError::Timeout { .. }
        )
    }
}

pub trait Transport {
    fn send(&self, request: &Request) -> Result<Response, TransportError>;
}

impl<T: Transport + ?Sized> Transport for &T {
    fn send(&self, request: &Request) -> Result<Response, TransportError> {
        (**self).send(request)
    }
}

impl<T: Transport + ?Sized> Transport for std::sync::Arc<T> {
    fn send(&self, request: &Request) -> Result<Response, TransportError> {
        (**self).send(request)
    }
}

impl<T: Transport + ?Sized> Transport for Box<T> {
    fn send(&self, request: &Request) -> Result<Response, TransportError> {
        (**self).send(request)
    }
}
