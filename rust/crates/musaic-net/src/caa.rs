//! Cover Art Archive: the front-cover check and the cover
//! download.
//!
//! The check is the Python app's `_head_cover` (mb_lookup.py:651-664): HEAD the
//! release-group `/front` URL with redirects followed (CAA answers 307 to
//! archive.org, which answers 302 to a storage node), MusicBrainz
//! User-Agent, `find_release`'s 15 s timeout. 200 offers the URL, 404 is "no
//! front-cover for this release", anything else is "HTTP <status>", and a
//! failed request is reported, never raised. The HEAD follows the download's
//! rule for every hop: `https` only, public hosts and addresses
//! only ([`crate::preview::check_public_url`] on each hop's URL before it is
//! sent, and [`Request::public_only`] for the resolved address), at most
//! [`crate::preview::PREVIEW_MAX_REDIRECTS`] redirects, followed here one
//! hop at a time (the Python app's `requests` followed up to 30, to any host).
//! A refused hop or a sixth redirect is a failed check
//! ([`FrontCover::Failed`] with [`TransportError::Refused`] /
//! [`TransportError::TooManyRedirects`]): no cover is offered and the
//! reason is the error's text, as for any other failed request.
//!
//! The download (a deliberate change from the Python app) is the public-image fetch
//! ([`crate::preview::fetch_public_image`]), the same rule as the cover
//! preview: `https` only, public hosts and addresses only (checked on the
//! URL of every hop and on the address actually connected to), at most
//! [`crate::preview::PREVIEW_MAX_REDIRECTS`] redirects, each re-checked, at
//! most 8 MiB (`cover_cache.MAX_BYTES_PER_ENTRY`, cover_cache.py:36), a
//! `Content-Type` among [`crate::preview::PREVIEW_TYPES`] and a body that
//! starts with that format's signature. `Accept: image/*,*/*;q=0.5`
//! (ai.py:1508), the full User-Agent and the apply path's 30 s
//! timeout (`tags.download_cover`, tags.py:373). Every caller's URL comes
//! from a model's suggestion, the user, or the resolver (a CAA URL, which
//! passes: coverartarchive.org redirects twice, to archive.org and then to
//! a storage node).
//!
//! The Python app downloaded any URL: plain HTTP, loopback and LAN hosts, up to
//! 30 redirects, an absent `Content-Type` taken as JPEG, and the apply path
//! took any status-200 body (ai.py:1483-1531, tags.py:392-397). A refused URL fails like any failed download:
//! the apply stream's `error` "Failed to download cover from {url}", the AI
//! verification's reason (this error's text), and a WARNING
//! `cover download failed: {reason}` on `musaic.tags` (the Python app's line
//! for a failed download, tags.py:400).

use std::fmt;
use std::time::Duration;

use crate::clients::{MB_TIMEOUT, USER_AGENT, rebase};
use crate::preview::{
    PREVIEW_MAX_REDIRECTS, PreviewError, check_public_url, fetch_public_image, public_image_request,
};
use crate::{Method, Request, Transport, TransportError};

const CAA: &str = "https://coverartarchive.org";

/// `find_release`'s timeout, which `_head_cover` inherits (mb_lookup.py:266).
pub const CAA_TIMEOUT: Duration = MB_TIMEOUT;
/// `tags.download_cover(timeout=30)` (tags.py:373).
pub const COVER_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30);
/// `cover_cache.MAX_BYTES_PER_ENTRY` (cover_cache.py:36), enforced by
/// `_verify_cover_url` (ai.py:1525).
pub const COVER_MAX_BYTES: u64 = 8 * 1024 * 1024;
/// ai.py:1508.
pub const COVER_ACCEPT: &str = "image/*,*/*;q=0.5";

/// Outcome of the front-cover check. Only `Verified` offers a cover URL.
#[derive(Debug)]
pub enum FrontCover {
    Verified {
        url: String,
    },
    /// 404 after redirects.
    Missing,
    /// Any other status after redirects.
    Status(u16),
    /// No answer (offline, timeout, more than
    /// [`crate::preview::PREVIEW_MAX_REDIRECTS`] redirects, a hop refused by
    /// the public rule, ...).
    Failed(TransportError),
}

impl FrontCover {
    /// The URL to offer as `cover_url`, only when the HEAD returned 200.
    pub fn cover_url(&self) -> Option<&str> {
        match self {
            FrontCover::Verified { url } => Some(url),
            _ => None,
        }
    }

    /// The Python app's reason string (mb_lookup.py:659-664).
    pub fn reason(&self) -> String {
        match self {
            FrontCover::Verified { .. } => "ok".into(),
            FrontCover::Missing => "no front-cover for this release".into(),
            FrontCover::Status(s) => format!("HTTP {s}"),
            FrontCover::Failed(e) => e.to_string(),
        }
    }
}

/// A downloaded cover. `mime` is the `Content-Type` without parameters,
/// lower-cased, one of [`crate::preview::PREVIEW_TYPES`] (the Python app took an
/// absent one as `image/jpeg`, ai.py:1527; this build refuses it).
#[derive(Clone, PartialEq, Eq)]
pub struct CoverImage {
    pub bytes: Vec<u8>,
    pub mime: String,
}

impl fmt::Debug for CoverImage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CoverImage")
            .field("bytes", &self.bytes.len())
            .field("mime", &self.mime)
            .finish()
    }
}

/// Why a cover was not downloaded. `Display` follows the Python app's reasons
/// (ai.py:1516-1544) where the Python app had one.
#[derive(Debug, thiserror::Error)]
pub enum CoverError {
    #[error("not a cover URL: {0}")]
    BadUrl(String),
    #[error("HTTP {0}")]
    Status(u16),
    #[error("content-type {0}")]
    ContentType(String),
    #[error("image too large ({0} bytes)")]
    TooLarge(u64),
    /// The URL (or a redirect, or the address it resolved to) is
    /// not a public `https` one.
    #[error("refused: {0}")]
    Refused(String),
    /// More than [`crate::preview::PREVIEW_MAX_REDIRECTS`].
    #[error("more than {0} redirects")]
    TooManyRedirects(u32),
    /// The body does not start with its type's signature.
    #[error("not an image: the body does not start with the {0} signature")]
    NotAnImage(String),
    #[error(transparent)]
    Transport(TransportError),
}

impl From<PreviewError> for CoverError {
    fn from(e: PreviewError) -> Self {
        match e {
            PreviewError::Refused(r) => CoverError::Refused(r),
            PreviewError::TooManyRedirects(n) => CoverError::TooManyRedirects(n),
            PreviewError::Status(s) => CoverError::Status(s),
            PreviewError::ContentType(ct) if ct.is_empty() => {
                CoverError::ContentType("absent".into())
            }
            PreviewError::ContentType(ct) => CoverError::ContentType(ct),
            // The Python app reports the length it read; this build stops one
            // byte over the cap.
            PreviewError::TooLarge(cap) => CoverError::TooLarge(cap + 1),
            PreviewError::NotAnImage(ct) => CoverError::NotAnImage(ct),
            PreviewError::Transport(t) => CoverError::Transport(t),
        }
    }
}

#[derive(Clone)]
pub struct CoverArtClient<T> {
    transport: T,
    origin: String,
}

impl<T> fmt::Debug for CoverArtClient<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CoverArtClient")
            .field("origin", &self.origin)
            .finish_non_exhaustive()
    }
}

impl<T: Transport> CoverArtClient<T> {
    pub fn new(transport: T) -> Self {
        Self::with_origin(transport, CAA)
    }

    /// Same requests against another origin (a test server).
    pub fn with_origin(transport: T, origin: &str) -> Self {
        Self {
            transport,
            origin: origin.trim_end_matches('/').to_string(),
        }
    }

    /// `CAA_RELEASE_GROUP_FRONT` (mb_lookup.py:58) as a HEAD: the first
    /// hop of [`Self::check_front`] (public addresses only, redirects
    /// returned to the check rather than followed by the transport).
    pub fn front_request(release_group: &str) -> Request {
        Request::head(CAA, &format!("/release-group/{release_group}/front"))
            .header("User-Agent", USER_AGENT)
            .timeout(CAA_TIMEOUT)
            .no_redirects()
            .public_only()
    }

    /// A later hop of [`Self::check_front`]: the redirect target, same
    /// headers, timeout and flags as the first. `None` when it is not an
    /// absolute URL.
    pub fn front_hop_request(url: &str) -> Option<Request> {
        Some(
            Request::from_url(Method::Head, url)?
                .header("User-Agent", USER_AGENT)
                .timeout(CAA_TIMEOUT)
                .no_redirects()
                .public_only(),
        )
    }

    /// The URL offered as `cover_url` on success (the CAA URL, not the
    /// archive.org one the redirects end at; mb_lookup.py:267-268).
    pub fn front_url(&self, release_group: &str) -> String {
        rebase(Self::front_request(release_group), &self.origin).url()
    }

    /// HEAD the front cover, redirects followed hop by hop under
    /// the public rule (module docs). The status is the last hop's.
    pub fn check_front(&self, release_group: &str) -> FrontCover {
        let first = rebase(Self::front_request(release_group), &self.origin);
        let offered = first.url();
        let mut req = first;
        for hop in 0..=PREVIEW_MAX_REDIRECTS {
            let url = req.url();
            if let Err(reason) = check_public_url(&url) {
                return FrontCover::Failed(TransportError::Refused { url, reason });
            }
            let resp = match self.transport.send(&req) {
                Ok(r) => r,
                Err(e) => return FrontCover::Failed(e),
            };
            let location = resp
                .header("location")
                .filter(|_| matches!(resp.status, 301 | 302 | 303 | 307 | 308));
            let Some(location) = location else {
                return match resp.status {
                    200 => FrontCover::Verified { url: offered },
                    404 => FrontCover::Missing,
                    s => FrontCover::Status(s),
                };
            };
            if hop == PREVIEW_MAX_REDIRECTS {
                return FrontCover::Failed(TransportError::TooManyRedirects {
                    url: offered,
                    limit: PREVIEW_MAX_REDIRECTS,
                });
            }
            let next = crate::live::resolve_location(&url, location);
            req = match Self::front_hop_request(&next) {
                Some(r) => r,
                None => {
                    return FrontCover::Failed(TransportError::Refused {
                        url: next,
                        reason: "not an absolute URL".into(),
                    });
                }
            };
        }
        unreachable!("the loop returns on its last hop")
    }

    /// The GET for one hop of a cover download. `None` when it is not an
    /// absolute URL.
    pub fn download_request(url: &str) -> Option<Request> {
        public_image_request(url, COVER_DOWNLOAD_TIMEOUT)
    }

    /// [`Self::download`] without its log line: the AI cover verification
    /// (`ai._verify_cover_url`, which logged nothing) uses it.
    pub fn fetch(&self, url: &str) -> Result<CoverImage, CoverError> {
        if Request::from_url(crate::Method::Get, url).is_none() {
            return Err(CoverError::BadUrl(url.into()));
        }
        fetch_public_image(&self.transport, url, COVER_DOWNLOAD_TIMEOUT)
            .map(|p| CoverImage {
                bytes: p.bytes,
                mime: p.mime,
            })
            .map_err(CoverError::from)
    }

    /// Download a cover under the public-image rule (module docs):
    /// `tags.download_cover`, with its log line.
    pub fn download(&self, url: &str) -> Result<CoverImage, CoverError> {
        match self.fetch(url) {
            Ok(img) => Ok(img),
            Err(e) => {
                // tags.py:400 `log.warning("cover download failed: %s", e)`,
                // which the Python app wrote when `requests.get` raised: no
                // answer (connection, timeout) or too many redirects. Also
                // for a URL the public rule refuses. A status other than
                // 200 returned None silently there, as here; the type, size
                // and signature refusals are not logged either.
                // `e` is this build's text, not requests'.
                if matches!(
                    e,
                    CoverError::Refused(_)
                        | CoverError::TooManyRedirects(_)
                        | CoverError::Transport(_)
                ) {
                    musaic_log::warning("musaic.tags", &format!("cover download failed: {e}"));
                }
                Err(e)
            }
        }
    }
}
