//! Serve recorded responses from the recorded replay set (private test data).

use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use base64::Engine as _;

use crate::{Request, Response, Transport, TransportError};

/// A transport that never touches the network: a request is answered from
/// `<dir>/<replay key>.json` or fails as unobserved.
///
/// `Send + Sync`, so one replay transport can be shared (`Arc`) by handlers
/// that run on `spawn_blocking` threads, like the paced live transport.
pub struct ReplayTransport {
    dir: PathBuf,
    hits: Mutex<Vec<String>>,
}

impl ReplayTransport {
    pub fn new(dir: impl AsRef<Path>) -> Self {
        Self {
            dir: dir.as_ref().to_path_buf(),
            hits: Mutex::new(Vec::new()),
        }
    }

    /// Replay keys served so far, in order.
    pub fn hits(&self) -> Vec<String> {
        self.hits
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl std::fmt::Debug for ReplayTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReplayTransport")
            .field("dir", &self.dir)
            .finish_non_exhaustive()
    }
}

impl Transport for ReplayTransport {
    fn send(&self, request: &Request) -> Result<Response, TransportError> {
        let key = request.replay_key();
        let path = self.dir.join(format!("{key}.json"));
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Err(TransportError::Unobserved {
                key,
                method: request.method.as_str(),
                url: request.redacted_url(),
            });
        };
        let bad = |what: &str| TransportError::Other(format!("{}: {what}", path.display()));
        let rec: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| bad(&e.to_string()))?;
        let status = rec["status"].as_u64().ok_or_else(|| bad("no status"))? as u16;
        let headers = rec["headers"]
            .as_object()
            .map(|h| {
                h.iter()
                    .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        let body = base64::engine::general_purpose::STANDARD
            .decode(rec["content_b64"].as_str().unwrap_or(""))
            .map_err(|e| bad(&e.to_string()))?;
        self.hits
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(key);
        Ok(Response {
            status,
            headers,
            body,
        })
    }
}

/// A transport that follows redirects itself, the way the live transport
/// does (301, 302, 303, 307, 308 for GET and HEAD, never POST; each
/// hop a new request to the resolved `Location`, same method, headers and
/// timeout; only HTTPS or loopback hops; at most [`crate::live::MAX_REDIRECTS`]).
///
/// The replay transport answers each hop from its own recording and does
/// not follow redirects, so a host that serves the resolver from replay
/// wraps it in this to see what the live transport would return.
#[derive(Debug, Clone)]
pub struct FollowRedirects<T>(pub T);

impl<T: Transport> Transport for FollowRedirects<T> {
    fn send(&self, request: &Request) -> Result<Response, TransportError> {
        let limit = crate::live::MAX_REDIRECTS;
        if !request.follow_redirects {
            return self.0.send(request);
        }
        let mut req = request.clone();
        for hop in 0..=limit {
            let resp = self.0.send(&req)?;
            let redirect = matches!(resp.status, 301 | 302 | 303 | 307 | 308)
                && req.method != crate::Method::Post;
            let Some(location) = resp.header("location").filter(|_| redirect) else {
                return Ok(resp);
            };
            if hop == limit {
                return Err(TransportError::TooManyRedirects {
                    url: request.redacted_url(),
                    limit,
                });
            }
            let next = crate::live::resolve_location(&req.url(), location);
            if !crate::live::check_scheme(&next) {
                return Err(TransportError::InsecureScheme {
                    url: request.scrub(&next),
                });
            }
            let mut hop_req =
                Request::from_url(req.method, &next).ok_or_else(|| TransportError::Protocol {
                    url: request.redacted_url(),
                    detail: "unusable redirect Location".into(),
                })?;
            hop_req.headers = req.headers.clone();
            hop_req.timeout = req.timeout;
            hop_req.max_body = req.max_body;
            hop_req.public_only = req.public_only;
            req = hop_req;
        }
        unreachable!("the loop returns on its last hop")
    }
}
