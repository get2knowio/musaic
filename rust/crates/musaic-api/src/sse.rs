//! `StreamingResponse(media_type="text/event-stream")` with the Python app's
//! headers: a response body fed by a bounded channel. When the
//! client goes away hyper drops the body, the receiver closes, and the
//! producer notices on its next send or through [`SseSender::closed`], then
//! drops what it holds (a relay connection, a scan subscription).
//!
//! When the server begins shutting down
//! ([`crate::AppState::begin_shutdown`]) every body ends: the response
//! finishes with its terminating chunk, as if the producer had closed it,
//! instead of the connection being cut when the drain times out. Frames
//! already queued are not sent after that point. The producer sees a closed
//! channel on its next send, exactly as after a client disconnect.

use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use axum::body::Body;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::Response;
use bytes::Bytes;
use http_body::Frame;
use musaic_app::ops::{Listening, OpError, Ops};
use tokio::sync::{mpsc, oneshot, watch};

use crate::http::ApiError;

/// The Python app's SSE content type (Starlette appends the charset to `text/*`).
pub const CONTENT_TYPE: &str = "text/event-stream; charset=utf-8";
/// Comment frame sent after 30 s without an event.
pub const PING: &str = ": ping\n\n";

type Shutdown = Pin<Box<dyn Future<Output = ()> + Send>>;

struct SseBody {
    rx: mpsc::Receiver<Bytes>,
    /// Resolves when the server begins shutting down.
    shutdown: Option<Shutdown>,
}

impl http_body::Body for SseBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        if let Some(f) = self.shutdown.as_mut()
            && f.as_mut().poll(cx).is_ready()
        {
            self.shutdown = None;
            self.rx.close();
            return Poll::Ready(None);
        }
        match self.rx.poll_recv(cx) {
            Poll::Ready(Some(b)) => Poll::Ready(Some(Ok(Frame::data(b)))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

/// The producer side.
#[derive(Clone)]
pub struct SseSender {
    tx: mpsc::Sender<Bytes>,
}

impl SseSender {
    /// `false` once the client is gone.
    pub async fn send(&self, frame: impl Into<String>) -> bool {
        self.tx.send(Bytes::from(frame.into())).await.is_ok()
    }

    /// Blocking variant for producers on `spawn_blocking` threads.
    pub fn send_blocking(&self, frame: impl Into<String>) -> bool {
        self.tx.blocking_send(Bytes::from(frame.into())).is_ok()
    }

    /// Queue a frame only if there is room: for progress that may be
    /// dropped, so a slow client never holds the producer up. `false` when
    /// it was dropped or the client is gone.
    pub fn try_send_frame(&self, frame: impl Into<String>) -> bool {
        self.tx.try_send(Bytes::from(frame.into())).is_ok()
    }

    pub fn is_closed(&self) -> bool {
        self.tx.is_closed()
    }

    /// Resolves when the client has gone away.
    pub async fn closed(&self) {
        self.tx.closed().await
    }
}

/// A new SSE response and its sender, ending when `st` begins shutting down.
/// `capacity` 1 makes the producer wait for the consumer (at
/// most one frame in flight).
pub fn channel(st: &crate::AppState, capacity: usize) -> (SseSender, Response) {
    channel_until(Some(st.shutdown_signal()), capacity)
}

/// [`channel`] with an explicit shutdown flag (`None`: never ends on its
/// own).
pub fn channel_until(
    shutdown: Option<watch::Receiver<bool>>,
    capacity: usize,
) -> (SseSender, Response) {
    let (tx, rx) = mpsc::channel(capacity.max(1));
    let shutdown = shutdown.map(|mut w| -> Shutdown {
        Box::pin(async move {
            // A dropped sender (the state is gone) also ends the stream.
            let _ = w.wait_for(|on| *on).await;
        })
    });
    let mut r = Response::new(Body::new(SseBody { rx, shutdown }));
    *r.status_mut() = StatusCode::OK;
    let h = r.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(CONTENT_TYPE));
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-cache, no-transform"),
    );
    h.insert("x-accel-buffering", HeaderValue::from_static("no"));
    h.insert(header::CONNECTION, HeaderValue::from_static("keep-alive"));
    (SseSender { tx }, r)
}

/// `sse_event(name, data)` for a pre-rendered JSON payload.
pub fn frame(name: &str, data_json: &str) -> String {
    musaic_events::sse_frame(name, data_json)
}

/// An operation's run as the answer. `prepare` (the operation's checks: the
/// guard, the album lookup) runs on a blocking thread and is awaited, so a
/// client that leaves meanwhile drops the prepared run and nothing is written,
/// as before the operations existed; its refusal is answered as its status.
/// Then `run` goes on a blocking thread (in the caller's event context) with a
/// sink that renders each event with `render` and sends it down a capacity-1
/// stream (a slow client holds the run's reporting back; the run itself goes
/// on as the operation decides). Answers the stream once the first event is
/// out, or once the run ended without one; a refusal returned before any event
/// is answered as its status instead.
pub async fn run_stream<E, P, T, R>(
    st: &crate::AppState,
    render: fn(&E) -> String,
    prepare: impl FnOnce(Ops) -> Result<P, OpError> + Send + 'static,
    run: R,
) -> crate::http::ApiResult
where
    E: 'static,
    P: Send + 'static,
    R: FnOnce(P, &mut dyn FnMut(E) -> Listening) -> Result<T, OpError> + Send + 'static,
{
    let ops = st.ops();
    let prepared = crate::http::blocking(move || prepare(ops)).await??;
    let (tx, resp) = channel(st, 1);
    // Answers once: `Ok` when the first event is out (or the run ended without
    // one), `Err` when the operation refused before any.
    let (ready_tx, ready_rx) = oneshot::channel::<Result<(), ApiError>>();
    let ctx = musaic_events::context::current();
    tokio::task::spawn_blocking(move || {
        musaic_events::context::run_in(ctx, || {
            let mut ready = Some(ready_tx);
            let result = run(prepared, &mut |event| {
                if let Some(r) = ready.take() {
                    let _ = r.send(Ok(()));
                }
                if tx.send_blocking(render(&event)) {
                    Listening::Yes
                } else {
                    Listening::Gone
                }
            });
            if let Some(r) = ready.take() {
                let _ = r.send(result.map(|_| ()).map_err(ApiError::from));
            }
        })
    });
    match ready_rx.await {
        Ok(Ok(())) => Ok(resp),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(ApiError::internal("the run ended without an answer")),
    }
}
