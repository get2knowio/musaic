/**
 * Thin EventSource wrapper that delivers typed events to a callback and
 * cleans up on completion. Used for /api/scan/stream (and later for the
 * apply + analysis endpoints, which need a fetch-based parser since
 * EventSource is GET-only — see `postSse`).
 */

import type { ScanEvent } from "./types";
import { errorDetailText } from "./errors";

export interface SseHandle {
  close: () => void;
}

export function openScanStream(
  url: string,
  onEvent: (event: ScanEvent) => void,
): SseHandle {
  const es = new EventSource(url);

  // Backend emits named events; EventSource handlers attach per-name.
  es.addEventListener("start", (ev) => {
    const data = JSON.parse((ev as MessageEvent).data);
    onEvent({ type: "start", ...data });
  });
  es.addEventListener("album", (ev) => {
    const data = JSON.parse((ev as MessageEvent).data);
    onEvent({ type: "album", album: data });
  });
  es.addEventListener("progress", (ev) => {
    const data = JSON.parse((ev as MessageEvent).data);
    onEvent({ type: "progress", ...data });
  });
  es.addEventListener("marked_missing", (ev) => {
    const data = JSON.parse((ev as MessageEvent).data);
    onEvent({ type: "marked_missing", ...data });
  });
  es.addEventListener("done", (ev) => {
    const data = JSON.parse((ev as MessageEvent).data);
    onEvent({ type: "done", ...data });
    es.close();
  });
  // Backend emits a custom 'error' event on failure (with a JSON detail).
  es.addEventListener("error", (ev) => {
    const messageEv = ev as MessageEvent;
    let detail = "Scan stream error";
    if (messageEv.data) {
      try {
        const data = JSON.parse(messageEv.data);
        if (data.detail) detail = data.detail;
      } catch {
        /* connection-level error, no payload */
      }
    }
    onEvent({ type: "error", detail });
    es.close();
  });

  return { close: () => es.close() };
}

/**
 * SSE over POST. EventSource only supports GET, so for endpoints that need
 * a request body (apply, audio analysis), we use fetch() + a hand-rolled
 * SSE parser over the response body's stream.
 *
 * `events` maps event names → handler functions; events not listed are
 * silently ignored.
 */
export async function postSse(
  url: string,
  body: unknown,
  events: Record<string, (data: unknown) => void>,
  signal?: AbortSignal,
): Promise<void> {
  // Detect FormData / Blob bodies and skip JSON encoding + Content-Type
  // (the browser sets the right multipart boundary automatically).
  const isFormData = typeof FormData !== "undefined" && body instanceof FormData;
  const headers: Record<string, string> = { Accept: "text/event-stream" };
  let payload: BodyInit;
  if (isFormData) {
    payload = body as FormData;
  } else {
    headers["Content-Type"] = "application/json";
    payload = JSON.stringify(body);
  }
  const resp = await fetch(url, {
    method: "POST",
    headers,
    body: payload,
    signal,
  });
  if (!resp.ok) {
    let detail = `${resp.status} ${resp.statusText}`;
    try {
      const err = await resp.json();
      detail = errorDetailText(err?.detail) ?? detail;
    } catch {
      /* ignore */
    }
    // Carry the status so callers can tell a refusal (409: restore
    // running, album Missing, folder gone) from a transport failure.
    throw Object.assign(new Error(detail), { status: resp.status });
  }
  if (!resp.body) throw new Error("SSE response has no body");

  const reader = resp.body.getReader();
  const decoder = new TextDecoder();
  let buffer = "";
  let event = "message";
  let data = "";
  // Captured handler-thrown error. We finish reading the SSE stream so
  // the connection closes cleanly, then re-throw so the caller's await
  // rejects. (Throwing mid-loop would orphan the connection.)
  let handlerError: unknown = null;

  const dispatch = (eventName: string, rawData: string): void => {
    if (handlerError !== null) return;  // already aborting
    const handler = events[eventName];
    if (!handler) return;
    let parsed: unknown;
    try {
      parsed = JSON.parse(rawData);
    } catch (e) {
      // Malformed payload — log + drop; the stream itself stays alive.
      console.warn("SSE parse error", e, rawData);
      return;
    }
    try {
      handler(parsed);
    } catch (e) {
      // Handler decided this event is fatal (typically an `error`
      // event whose handler throws). Capture and propagate after the
      // stream drains.
      handlerError = e;
    }
  };

  for (;;) {
    const { value, done } = await reader.read();
    if (done) break;
    buffer += decoder.decode(value, { stream: true });
    const lines = buffer.split("\n");
    buffer = lines.pop() ?? ""; // last line may be incomplete
    for (const line of lines) {
      if (line === "") {
        if (data) dispatch(event, data);
        event = "message";
        data = "";
      } else if (line.startsWith("event: ")) {
        event = line.slice(7);
      } else if (line.startsWith("data: ")) {
        data = data ? data + "\n" + line.slice(6) : line.slice(6);
      }
    }
    // Bail early if a handler signaled fatal failure — keep reading
    // would just queue more work for nothing.
    if (handlerError !== null) {
      try { reader.cancel(); } catch { /* ignore */ }
      break;
    }
  }

  if (handlerError !== null) {
    throw handlerError instanceof Error
      ? handlerError
      : new Error(String(handlerError));
  }
}
