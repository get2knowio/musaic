/**
 * Vitest global setup. Loaded once per test file via vitest.config.ts.
 *
 *  - Wires @testing-library/jest-dom matchers (`toBeInTheDocument`, …).
 *  - Auto-cleans the DOM between tests.
 *  - Installs a controllable `EventSource` mock — jsdom ships no
 *    EventSource, and every App-level SSE hook (useLibraryEventsStream,
 *    useJobsStream, useChecksStream, useFingerprintPoolStream) constructs
 *    one on mount. MockEventSource lets a test grab the instance and
 *    drive named events deterministically.
 */
import "@testing-library/jest-dom/vitest";
import { afterEach, beforeEach } from "vitest";
import { cleanup } from "@testing-library/react";

/** Minimal, fully-synchronous EventSource stand-in for tests. */
export class MockEventSource {
  /** Every instance constructed since the last reset, in order. */
  static instances: MockEventSource[] = [];
  /** The most recently constructed instance (the one under test). */
  static get last(): MockEventSource | undefined {
    return MockEventSource.instances.at(-1);
  }

  readonly url: string;
  readyState = 0; // CONNECTING
  closed = false;
  private readonly listeners: Record<string, Set<(ev: unknown) => void>> = {};

  constructor(url: string) {
    this.url = url;
    MockEventSource.instances.push(this);
  }

  addEventListener(type: string, cb: (ev: unknown) => void): void {
    (this.listeners[type] ??= new Set()).add(cb);
  }

  removeEventListener(type: string, cb: (ev: unknown) => void): void {
    this.listeners[type]?.delete(cb);
  }

  close(): void {
    this.closed = true;
    this.readyState = 2; // CLOSED
  }

  /**
   * Test helper — dispatch a named SSE event. `data` is JSON-encoded to
   * mirror the wire format the relay produces; pass a raw string to
   * exercise the malformed-payload path.
   */
  emit(type: string, data?: unknown): void {
    const payload =
      data === undefined
        ? ""
        : typeof data === "string"
          ? data
          : JSON.stringify(data);
    const ev = { type, data: payload };
    this.listeners[type]?.forEach((cb) => cb(ev));
  }
}

beforeEach(() => {
  MockEventSource.instances.length = 0;
  (globalThis as { EventSource?: unknown }).EventSource =
    MockEventSource as unknown as typeof EventSource;
});

afterEach(() => {
  cleanup();
});
