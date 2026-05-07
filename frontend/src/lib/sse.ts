/**
 * Thin EventSource wrapper that delivers typed events to a callback and
 * cleans up on completion. Used for /api/scan/stream (and later for the
 * apply + analysis endpoints, which need a fetch-based parser since
 * EventSource is GET-only — see `postSse`).
 */

import type { ScanEvent } from "./types";

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
  es.addEventListener("removed", (ev) => {
    const data = JSON.parse((ev as MessageEvent).data);
    onEvent({ type: "removed", ...data });
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
  const resp = await fetch(url, {
    method: "POST",
    headers: { "Content-Type": "application/json", Accept: "text/event-stream" },
    body: JSON.stringify(body),
    signal,
  });
  if (!resp.ok) {
    let detail = `${resp.status} ${resp.statusText}`;
    try {
      const err = await resp.json();
      if (err.detail) detail = err.detail;
    } catch {
      /* ignore */
    }
    throw new Error(detail);
  }
  if (!resp.body) throw new Error("SSE response has no body");

  const reader = resp.body.getReader();
  const decoder = new TextDecoder();
  let buffer = "";
  let event = "message";
  let data = "";

  for (;;) {
    const { value, done } = await reader.read();
    if (done) break;
    buffer += decoder.decode(value, { stream: true });
    const lines = buffer.split("\n");
    buffer = lines.pop() ?? ""; // last line may be incomplete
    for (const line of lines) {
      if (line === "") {
        if (data) {
          const handler = events[event];
          if (handler) {
            try {
              handler(JSON.parse(data));
            } catch (e) {
              // Ignore malformed events; never crash the stream.
              console.warn("SSE parse error", e, data);
            }
          }
        }
        event = "message";
        data = "";
      } else if (line.startsWith("event: ")) {
        event = line.slice(7);
      } else if (line.startsWith("data: ")) {
        data = data ? data + "\n" + line.slice(6) : line.slice(6);
      }
    }
  }
}
