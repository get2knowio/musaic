/**
 * What the Activity panel and its toolbar control say about each kind of
 * background work, as plain functions of what the streams already deliver.
 *
 * Every entry has one state from a small, fixed set, so "waiting",
 * "stopped" and "failed" never read alike, and a state for not knowing:
 * `stale` (the last progress seen, but the stream that carries it is down)
 * and `unavailable` (never loaded).
 */
import type { BulkRun } from "@/lib/bulkRuns";
import type { ScanActivity } from "@/lib/scanActivity";
import type { StreamHealth } from "@/lib/streamHealth";
import type { CheckSweepStatus, FingerprintPoolStatus, Job } from "@/lib/types";

export type ActivityState =
  | "running"
  | "waiting"
  | "stopped"
  | "failed"
  | "completed"
  | "idle"
  | "stale"
  | "unavailable";

export const STATE_LABEL: Record<ActivityState, string> = {
  running: "Running",
  waiting: "Waiting",
  stopped: "Stopped",
  failed: "Failed",
  completed: "Completed",
  idle: "Idle",
  stale: "Out of date",
  unavailable: "Unavailable",
};

/** Busy states: the toolbar control names them. */
export function isBusy(s: ActivityState): boolean {
  return s === "running" || s === "waiting";
}

export interface Progress {
  done: number;
  /** null: the total is not known (yet). */
  total: number | null;
}

/** "42 of 180", or "42 so far" without a total. */
export function progressText(p: Progress): string {
  return p.total !== null && p.total > 0 ? `${Math.min(p.done, p.total)} of ${p.total}` : `${p.done} so far`;
}

export interface Summary {
  state: ActivityState;
  /** The verb for the toolbar: "Scanning", "Checking", … */
  verb: string;
  progress: Progress | null;
  /** One sentence for the panel. */
  detail: string;
  /** Out of date but, as far as anyone knows, still going (counts as busy). */
  ongoing?: boolean;
}

/** The query or stream that feeds an entry. */
export interface Feed {
  /** The first read failed and nothing is known. */
  failed: boolean;
  health: StreamHealth;
}

const lost = (f: Feed) => f.health.state === "lost";

// ---- scan ----

export function scanSummary(s: ScanActivity): Summary {
  const progress = s.total > 0 || s.scanned > 0 ? { done: s.scanned, total: s.total || null } : null;
  switch (s.phase) {
    case "starting":
      return { state: "running", verb: "Scanning", progress: null, detail: "Starting the scan…" };
    case "running":
      return {
        state: "running",
        verb: "Scanning",
        progress,
        detail: s.total > 0 ? "Reading album folders." : "Counting album folders…",
      };
    case "cancelling":
      return { state: "running", verb: "Stopping scan", progress, detail: "Stopping. Nothing this scan read will be saved." };
    case "done":
      return { state: "completed", verb: "Scan", progress, detail: s.message ?? "Library up to date." };
    case "cancelled":
      return {
        state: "stopped",
        verb: "Scan",
        progress,
        detail: "Cancelled. Nothing was saved; the library keeps its previous scan.",
      };
    case "aborted":
      return {
        state: "stopped",
        verb: "Scan",
        progress,
        detail: `Stopped without saving${s.message ? `: ${s.message}` : "."}`,
      };
    case "error":
      return { state: "failed", verb: "Scan", progress, detail: s.message ?? "The scan failed." };
    case "lost":
      return {
        state: "stale",
        ongoing: true,
        verb: "Scan",
        progress,
        detail:
          "Lost the connection to the scan. It may still be running or may have finished; the progress shown is the last seen.",
      };
    case "idle":
      return { state: "idle", verb: "Scan", progress: null, detail: "No scan since Musaic started." };
  }
}

// ---- the background check sweep ----

export function sweepSummary(s: CheckSweepStatus | undefined, feed: Feed): Summary {
  if (!s) {
    return feed.failed
      ? { state: "unavailable", verb: "Checks", progress: null, detail: "The background checks' status could not be loaded." }
      : { state: "idle", verb: "Checks", progress: null, detail: "Loading…" };
  }
  const progress = s.total > 0 ? { done: s.completed, total: s.total } : null;
  if (s.status !== "idle" && lost(feed)) {
    return {
      state: "stale",
      ongoing: true,
      verb: "Checking",
      progress,
      detail: "The connection to the checks was lost; the progress shown is the last seen.",
    };
  }
  switch (s.status) {
    case "running":
      return {
        state: "running",
        verb: "Checking",
        progress,
        detail: `Rerunning the checks whose inputs changed${s.current_check ? ` (now: ${s.current_check})` : ""}.`,
      };
    case "waiting":
      return {
        state: "waiting",
        verb: "Checks waiting",
        progress,
        detail: "Waiting for a scan or other work to finish, then continuing.",
      };
    case "idle":
      return {
        state: s.completed_at ? "completed" : "idle",
        verb: "Checks",
        progress: null,
        detail: s.completed_at ? "Every check is up to date." : "Nothing to check right now.",
      };
  }
}

// ---- fingerprinting ----

export function poolSummary(p: FingerprintPoolStatus | undefined, feed: Feed): Summary {
  if (!p) {
    return feed.failed
      ? { state: "unavailable", verb: "Fingerprinting", progress: null, detail: "Fingerprinting status could not be loaded." }
      : { state: "idle", verb: "Fingerprinting", progress: null, detail: "Loading…" };
  }
  const progress = p.total > 0 || p.completed > 0 ? { done: p.completed, total: p.total || null } : null;
  const skipped = p.errors > 0 ? ` ${p.errors} skipped.` : "";
  if (p.status === "running" && lost(feed)) {
    return {
      state: "stale",
      ongoing: true,
      verb: "Fingerprinting",
      progress,
      detail: "The connection to fingerprinting was lost; the progress shown is the last seen.",
    };
  }
  switch (p.status) {
    case "running":
      return { state: "running", verb: "Fingerprinting", progress, detail: `Computing audio fingerprints.${skipped}` };
    case "done":
      return { state: "completed", verb: "Fingerprinting", progress, detail: `Finished.${skipped}` };
    case "cancelled":
      return {
        state: "stopped",
        verb: "Fingerprinting",
        progress,
        detail: "Stopped before it finished. It starts again after the next scan.",
      };
    case "unavailable":
      return {
        state: "failed",
        verb: "Fingerprinting",
        progress: null,
        detail: `Could not run${p.skip_reason ? `: ${p.skip_reason}` : "."}`,
      };
    case "idle":
      return { state: "idle", verb: "Fingerprinting", progress: null, detail: "Starts after a library scan." };
  }
}

// ---- bulk fixes ----

export function bulkSummary(r: BulkRun): Summary {
  const progress = { done: r.items.length, total: r.total };
  switch (r.phase) {
    case "running":
      return { state: "running", verb: r.actionLabel, progress, detail: `${r.checkLabel}: ${r.counts.fixed} fixed so far.` };
    case "completed":
      return {
        state: "completed",
        verb: r.actionLabel,
        progress,
        detail: `${r.checkLabel}: ${r.summary?.fixed ?? r.counts.fixed} of ${r.total} fixed.`,
      };
    case "stopped":
      return {
        state: "stopped",
        verb: r.actionLabel,
        progress,
        detail: `${r.checkLabel}: stopped after ${r.items.length} of ${r.total}; ${r.counts.fixed} fixed.`,
      };
    case "interrupted":
      return {
        state: "stale",
        verb: r.actionLabel,
        progress,
        detail: `${r.checkLabel}: the connection was lost after ${r.items.length} of ${r.total}; ${r.counts.fixed} fixed. The rest is not known.`,
      };
    case "failed":
      return {
        state: "failed",
        verb: r.actionLabel,
        progress: null,
        detail: `${r.checkLabel}: could not start${r.error ? `: ${r.error}` : "."}`,
      };
  }
}

// ---- research requests (job cards) ----

export function jobState(j: Job): ActivityState {
  switch (j.status) {
    case "running":
      return "running";
    case "queued":
      return "waiting";
    case "failed":
      return "failed";
    case "completed":
    case "acted_on":
      return "completed";
  }
}

export function jobsSummary(jobs: readonly Job[], feed: Feed): Summary {
  const running = jobs.filter((j) => j.status === "running").length;
  const queued = jobs.filter((j) => j.status === "queued").length;
  if ((running || queued) && lost(feed)) {
    return { state: "stale", ongoing: true, verb: "Research requests", progress: null, detail: "The connection to the research queue was lost." };
  }
  if (running) {
    return {
      state: "running",
      verb: running === 1 ? "1 research request running" : `${running} research requests running`,
      progress: null,
      detail: `${running} running${queued ? `, ${queued} queued` : ""}.`,
    };
  }
  if (queued) {
    return {
      state: "waiting",
      verb: `${queued} ${queued === 1 ? "research request" : "research requests"} queued`,
      progress: null,
      detail: `${queued} queued.`,
    };
  }
  return { state: "idle", verb: "Research requests", progress: null, detail: "None in progress." };
}

/**
 * The toolbar control's words: the first busy thing, most noticeable first
 * ("Scanning · 42 of 180"), with "+N" for the others; "Activity" when
 * nothing is busy. A busy entry whose stream is down still counts, marked.
 */
export function toolbarText(entries: readonly Summary[]): { text: string; busy: number; stale: boolean } {
  const busy = entries.filter((e) => isBusy(e.state) || e.ongoing);
  if (busy.length === 0) return { text: "Activity", busy: 0, stale: false };
  const first = busy[0];
  const p = first.progress ? ` · ${progressText(first.progress)}` : "";
  const more = busy.length > 1 ? ` +${busy.length - 1}` : "";
  return { text: `${first.verb}${p}${more}`, busy: busy.length, stale: first.state === "stale" };
}
