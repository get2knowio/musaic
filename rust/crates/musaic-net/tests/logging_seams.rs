//! Log lines the Python app's `mb_lookup.get_recording_release_groups` writes
//! (`musaic.mb_lookup`; server log follow-up: the lower crates log through
//! `musaic-log`). network failures: the result is unchanged (`Err`), only the line
//! is new. A canned transport answers; nothing leaves the machine.

use std::sync::Mutex;

use musaic_log::{Level, capture};
use musaic_net::{MusicBrainzClient, Request, Response, Transport, TransportError};

/// Answers every request with one canned result.
struct Canned(Mutex<Option<Result<Response, TransportError>>>);

impl Canned {
    fn status(status: u16) -> Self {
        Canned(Mutex::new(Some(Ok(Response {
            status,
            headers: Vec::new(),
            body: Vec::new(),
        }))))
    }
}

impl Transport for Canned {
    fn send(&self, _: &Request) -> Result<Response, TransportError> {
        self.0.lock().unwrap().take().expect("one request")
    }
}

const REC: &str = "00000000-0000-0000-0000-000000000001";

// Python app: musaic/mb_lookup.py:513
// `log.warning("MB recording release-groups lookup failed for %s: %s", mbid, e)`.
// `e` is this build's TransportError text (URL redacted), not requests'.
#[test]
fn failed_request_is_logged() {
    let t = Canned(Mutex::new(Some(Err(TransportError::Dns {
        host: "musicbrainz.org".into(),
    }))));
    let (r, lines) = capture(|| MusicBrainzClient::new(&t).recording_release_groups(REC));
    assert!(r.is_err());
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0].level, Level::Warning);
    assert_eq!(lines[0].target, "musaic.mb_lookup");
    assert_eq!(
        lines[0].message,
        format!(
            "MB recording release-groups lookup failed for {REC}: could not resolve host musicbrainz.org"
        )
    );
}

// Python app: musaic/mb_lookup.py:518
// `log.warning("MB recording release-groups HTTP %s for %s", resp.status_code, mbid)`;
// a 404 returns None without a line (mb_lookup.py:515-516).
#[test]
fn bad_status_is_logged_but_not_404() {
    let t = Canned::status(503);
    let (r, lines) = capture(|| MusicBrainzClient::new(&t).recording_release_groups(REC));
    assert!(r.is_err());
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(
        (
            lines[0].level,
            lines[0].target.as_str(),
            lines[0].message.as_str()
        ),
        (
            Level::Warning,
            "musaic.mb_lookup",
            format!("MB recording release-groups HTTP 503 for {REC}").as_str()
        )
    );

    let t = Canned::status(404);
    let (r, lines) = capture(|| MusicBrainzClient::new(&t).recording_release_groups(REC));
    assert!(r.is_err());
    assert!(lines.is_empty(), "{lines:?}");
}
