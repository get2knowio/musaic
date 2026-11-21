/**
 * The library scan as the Activity panel sees it, whatever page is open.
 *
 * The scan's events arrive on `/api/scan/stream`. While the Library page is
 * mounted its `useScanStream` reads that stream (it also builds the album
 * list from it) and passes each event here (`noteScanEvent`), as the
 * stream's *owner*. Without an owner, `useScanFollower` (mounted once in
 * App) joins the same stream to keep this record current: a rescan started
 * from Review or Settings, or a scan still running after the user left
 * Library. Opening the stream starts a scan when none runs, so the follower
 * opens it only for a scan known to be running (`requestScanFollow`, or
 * `GET /api/scan/status` at startup), never to look.
 *
 * `lost` is the honest state for a stream that broke mid-scan: the scan may
 * still be running, or may have ended; nothing here pretends to know.
 */
import { useSyncExternalStore } from "react";

import { cancelScan } from "@/lib/api";
import type { ScanEvent } from "@/lib/types";

export type ScanPhase =
  | "idle"
  | "starting"
  | "running"
  | "cancelling"
  | "done"
  | "cancelled"
  | "aborted"
  | "error"
  | "lost";

export interface ScanActivity {
  phase: ScanPhase;
  scanned: number;
  /** 0 until the scan has counted the folders it will read. */
  total: number;
  fresh: number;
  markedMissing: number;
  /** The scan's own words for how it ended (or why it stopped). */
  message: string | null;
  startedAt: number | null;
  endedAt: number | null;
  /** When the last event arrived; progress older than this is what it is. */
  updatedAt: number | null;
}

export const IDLE_SCAN: ScanActivity = {
  phase: "idle",
  scanned: 0,
  total: 0,
  fresh: 0,
  markedMissing: 0,
  message: null,
  startedAt: null,
  endedAt: null,
  updatedAt: null,
};

/** A scan is in progress (or about to be). */
export function isScanActive(phase: ScanPhase): boolean {
  return phase === "starting" || phase === "running" || phase === "cancelling";
}

/** One stream event applied to the record. Pure. */
export function reduceScan(prev: ScanActivity, ev: ScanEvent, now: number): ScanActivity {
  const at = { updatedAt: now };
  switch (ev.type) {
    case "start":
      return {
        ...prev,
        ...at,
        // A cancel accepted during discovery precedes `start`.
        phase: prev.phase === "cancelling" ? "cancelling" : "running",
        total: ev.total,
        scanned: 0,
        fresh: 0,
        markedMissing: 0,
        message: null,
        startedAt: isScanActive(prev.phase) && prev.startedAt ? prev.startedAt : now,
        endedAt: null,
      };
    case "album":
      return {
        ...prev,
        ...at,
        phase: isScanActive(prev.phase) ? prev.phase : "running",
        scanned: prev.scanned + 1,
        fresh: ev.album._status === "fresh" ? prev.fresh + 1 : prev.fresh,
      };
    case "progress":
      return { ...prev, ...at, scanned: ev.scanned, total: ev.total };
    case "marked_missing":
      return { ...prev, ...at, markedMissing: prev.markedMissing + 1 };
    case "adopted":
      return { ...prev, ...at };
    case "cancelling":
      return { ...prev, ...at, phase: "cancelling", scanned: ev.scanned, total: ev.total };
    case "cancelled":
      return { ...prev, ...at, phase: "cancelled", message: ev.detail, endedAt: now };
    case "aborted":
      return { ...prev, ...at, phase: "aborted", message: ev.detail, endedAt: now };
    case "done": {
      const changed =
        prev.fresh === 0 && ev.marked_missing_count === 0
          ? "Library up to date"
          : `${prev.fresh} new or updated` +
            (ev.marked_missing_count ? `, ${ev.marked_missing_count} missing` : "");
      return {
        ...prev,
        ...at,
        phase: "done",
        total: Math.max(prev.total, ev.count),
        scanned: Math.max(prev.scanned, ev.count),
        markedMissing: ev.marked_missing_count,
        message: changed,
        endedAt: now,
      };
    }
    case "error":
      return ev.lost
        ? // Progress stays as last seen; when it ended (if it did) is unknown.
          { ...prev, phase: "lost", message: "Lost the connection to the scan." }
        : { ...prev, ...at, phase: "error", message: ev.detail, endedAt: now };
  }
}

// ---- the store ----

let scan: ScanActivity = IDLE_SCAN;
let owners = 0;
let followWanted = false;
const listeners = new Set<() => void>();

function emit() {
  for (const l of listeners) l();
}

export function subscribeScanActivity(l: () => void): () => void {
  listeners.add(l);
  return () => {
    listeners.delete(l);
  };
}

export function getScanActivity(): ScanActivity {
  return scan;
}

export function useScanActivity(): ScanActivity {
  return useSyncExternalStore(subscribeScanActivity, getScanActivity, getScanActivity);
}

/** A stream event of the current scan, from whichever reader has it. */
export function noteScanEvent(ev: ScanEvent): void {
  scan = reduceScan(scan, ev, Date.now());
  if (!isScanActive(scan.phase)) followWanted = false;
  emit();
}

/** A scan was just asked for (its stream is about to open). */
export function noteScanStarting(): void {
  scan = { ...IDLE_SCAN, phase: "starting", startedAt: Date.now(), updatedAt: Date.now() };
  emit();
}

/** Replace the record from `GET /api/scan/status` (one read, not a poll). */
export function noteScanStatus(status: { status: string; scanned?: number; total?: number; error?: string | null }): void {
  const phase: ScanPhase | null =
    status.status === "running"
      ? "running"
      : status.status === "cancelling"
        ? "cancelling"
        : status.status === "done" || status.status === "cancelled" || status.status === "aborted" || status.status === "error"
          ? status.status
          : null;
  if (!phase) return;
  // A finished scan the panel never saw is not news; only a running one,
  // or the end of one it lost track of, is recorded.
  if (!isScanActive(phase) && scan.phase !== "lost") return;
  scan = {
    ...scan,
    phase,
    scanned: status.scanned ?? scan.scanned,
    total: status.total ?? scan.total,
    message: phase === "error" ? (status.error ?? scan.message) : isScanActive(phase) ? null : scan.message,
    startedAt: scan.startedAt ?? Date.now(),
    endedAt: isScanActive(phase) ? null : Date.now(),
    updatedAt: Date.now(),
  };
  if (isScanActive(phase)) followWanted = true;
  emit();
}

/** The Library page reads the scan stream itself; returns the detach. */
export function attachScanOwner(): () => void {
  owners++;
  emit();
  let done = false;
  return () => {
    if (done) return;
    done = true;
    owners--;
    // Left mid-scan: the follower takes over.
    if (owners === 0 && isScanActive(scan.phase)) followWanted = true;
    emit();
  };
}

export function scanHasOwner(): boolean {
  return owners > 0;
}

/** A scan is running (started from outside Library): follow it. */
export function requestScanFollow(): void {
  followWanted = true;
  emit();
}

/** The follower should be reading the stream now. */
export function scanFollowWanted(): boolean {
  return followWanted && owners === 0;
}

export function clearScanFollow(): void {
  followWanted = false;
}

/**
 * Ask the running scan to stop (`POST /api/scan/cancel`). The stream says
 * `cancelling`, then `cancelled`; this only says so sooner.
 */
export async function cancelRunningScan(): Promise<"requested" | "too_late" | "not_running"> {
  const r = await cancelScan();
  if (r.outcome === "requested" || r.outcome === "already_requested") {
    if (isScanActive(scan.phase)) {
      scan = { ...scan, phase: "cancelling", updatedAt: Date.now() };
      emit();
    }
    return "requested";
  }
  return r.outcome;
}

/** Test hook. */
export function resetScanActivity(): void {
  scan = IDLE_SCAN;
  owners = 0;
  followWanted = false;
  emit();
}
