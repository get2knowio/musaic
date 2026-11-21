/**
 * Whether the App-level SSE streams are delivering, so the Activity panel
 * can say "out of date" instead of showing frozen progress as if it were
 * live. Each stream hook reports here: `noteStreamLive` on open and on
 * every event, `noteStreamLost` on a connection error (EventSource keeps
 * reconnecting on its own; the next event marks it live again).
 *
 * No polling: this only records what the streams already see.
 */
import { useSyncExternalStore } from "react";

export type StreamName = "checks" | "fingerprint" | "jobs" | "scan";

export interface StreamHealth {
  /** `unknown` until the stream first opens or fails. */
  state: "unknown" | "live" | "lost";
  /** When the stream last delivered anything (ms since epoch), or null. */
  lastEventAt: number | null;
}

const UNKNOWN: StreamHealth = { state: "unknown", lastEventAt: null };

let health: Record<StreamName, StreamHealth> = {
  checks: UNKNOWN,
  fingerprint: UNKNOWN,
  jobs: UNKNOWN,
  scan: UNKNOWN,
};
const listeners = new Set<() => void>();

function emit() {
  for (const l of listeners) l();
}

export function noteStreamLive(name: StreamName): void {
  health = { ...health, [name]: { state: "live", lastEventAt: Date.now() } };
  emit();
}

export function noteStreamLost(name: StreamName): void {
  if (health[name].state === "lost") return;
  health = { ...health, [name]: { ...health[name], state: "lost" } };
  emit();
}

function subscribe(l: () => void): () => void {
  listeners.add(l);
  return () => {
    listeners.delete(l);
  };
}

const get = () => health;

export function useStreamHealth(): Record<StreamName, StreamHealth> {
  return useSyncExternalStore(subscribe, get, get);
}

/**
 * Wire an EventSource's connection to the health of `name`. A closed
 * source (the server ended a finite stream) is not a loss.
 */
export function trackEventSource(name: StreamName, es: EventSource): void {
  es.addEventListener("open", () => noteStreamLive(name));
  es.addEventListener("error", () => {
    // CLOSED (2) after a deliberate close is not a lost connection.
    if (es.readyState !== 2) noteStreamLost(name);
  });
}

/** Test hook. */
export function resetStreamHealth(): void {
  health = { checks: UNKNOWN, fingerprint: UNKNOWN, jobs: UNKNOWN, scan: UNKNOWN };
  emit();
}
