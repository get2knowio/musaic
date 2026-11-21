/**
 * Tiny module-level registry that lets the Titlebar (above the routes) trigger
 * a rescan owned by the Library page (one of the routes). The Library page's
 * `useScanStream` hook owns the SSE-driven album state; we don't want to lift
 * that state into App just to make the Titlebar's Rescan button work.
 *
 * Library calls `registerRescan(fn)` on mount and `registerRescan(null)` on
 * unmount. The Titlebar calls `triggerRescan()`. Both subscribe to the
 * scanning flag so the button can disable / spin while a scan is in flight.
 *
 * `triggerRescan` returns the result of the underlying rescan when one is
 * registered, or rejects with `{ kind: "no_library_mounted" }` so the caller
 * can fall back to the API directly (and show an explanatory toast).
 */
import { rescanLibrary } from "@/lib/api";
import { noteScanStarting, requestScanFollow } from "@/lib/scanActivity";

type RescanFn = () => Promise<void>;

let rescanFn: RescanFn | null = null;
let scanning = false;
const listeners = new Set<() => void>();

export function registerRescan(fn: RescanFn | null): void {
  rescanFn = fn;
}

export function setScanning(value: boolean): void {
  if (scanning === value) return;
  scanning = value;
  for (const l of listeners) l();
}

export function getScanning(): boolean {
  return scanning;
}

export function subscribeScanning(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

/**
 * Kick off a rescan. Prefers the registered Library function (so the SSE
 * stream updates the grid live); when no Library is mounted (Review,
 * Settings) it falls back to a bare POST and the Activity panel's follower
 * reads the scan's stream, so the progress shows where the user is.
 */
export async function triggerRescan(): Promise<{ via: "library" | "api" }> {
  if (rescanFn) {
    await rescanFn();
    return { via: "library" };
  }
  await rescanLibrary();
  // Library is not open to read the scan's stream: the Activity panel's
  // follower does, so the progress shows wherever the user is.
  noteScanStarting();
  requestScanFollow();
  return { via: "api" };
}
