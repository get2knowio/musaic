/**
 * Bulk fix runs ("Find covers", "Apply canonical values", …) for the
 * session: each run's progress and every finding it handled, kept apart
 * from the dialog that started it, so that
 *
 *  - closing the dialog does not stop the run (it carries on; Activity
 *    shows it);
 *  - Stop keeps what was done: the run ends `stopped` with its confirmed
 *    outcomes, and the summary stays in the dialog and in Activity (#186);
 *  - a stream that breaks mid-run ends `interrupted`, again with what was
 *    confirmed, rather than as if nothing happened.
 *
 * The store owns the run's stream (`streamQuickFixAll`); the findings
 * caches are refreshed by the BulkBatchCompleted event, as before.
 */
import { useSyncExternalStore } from "react";

import { streamQuickFixAll, type QuickFixSummary } from "@/hooks/useChecks";
import { plainReason, type BulkOutcome } from "@/lib/bulkFixReasons";

export type BulkPhase =
  /** Started, nothing reported yet or still reporting. */
  | "running"
  /** Every finding reported and `done` received. */
  | "completed"
  /** The user pressed Stop. */
  | "stopped"
  /** The stream broke after it started: the rest is unknown. */
  | "interrupted"
  /** Refused or failed before or while starting (e.g. a restore running). */
  | "failed";

/** One handled finding. */
export interface BulkResultItem {
  findingId: number;
  albumId: string | null;
  /** The album as the user saw it when the run started. */
  album: string;
  outcome: BulkOutcome;
  /** Plain-language reason; empty for a fixed finding. */
  reason: string;
}

export interface BulkCounts {
  fixed: number;
  skipped: number;
  errors: number;
  not_applicable: number;
}

export interface BulkRun {
  id: string;
  checkId: string;
  checkLabel: string;
  actionLabel: string;
  fixKind: "none" | "ai-job" | "deterministic";
  phase: BulkPhase;
  /** Findings the run set out to handle (the stream's `start`). */
  total: number;
  /** The stream's `start` arrived. */
  started: boolean;
  counts: BulkCounts;
  items: BulkResultItem[];
  summary: QuickFixSummary | null;
  /** Why it failed or was interrupted. */
  error: string | null;
  startedAt: number;
  endedAt: number | null;
}

export function isBulkRunning(r: BulkRun): boolean {
  return r.phase === "running";
}

/** Handled so far. */
export function processed(r: BulkRun): number {
  return r.items.length;
}

/** How many the run set out to do that have no confirmed outcome. */
export function unconfirmed(r: BulkRun): number {
  return Math.max(0, r.total - r.items.length);
}

/** Runs kept for the session, newest first; older ones drop off. */
export const KEPT_RUNS = 10;

let runs: BulkRun[] = [];
const cancels = new Map<string, () => void>();
const listeners = new Set<() => void>();
let seq = 0;

function emit() {
  for (const l of listeners) l();
}

function update(id: string, f: (r: BulkRun) => BulkRun) {
  runs = runs.map((r) => (r.id === id ? f(r) : r));
  emit();
}

function end(id: string, phase: BulkPhase, patch: Partial<BulkRun> = {}) {
  cancels.delete(id);
  update(id, (r) => (r.phase === "running" ? { ...r, ...patch, phase, endedAt: Date.now() } : r));
}

export interface StartBulkRun {
  checkId: string;
  checkLabel: string;
  actionLabel: string;
  fixKind: BulkRun["fixKind"];
  total: number;
  /** The album of a finding, as the page knew it when the run started. */
  albumOf: (findingId: number) => { id: string | null; label: string };
}

/** Start a run; returns its id. */
export function startBulkRun(opts: StartBulkRun): string {
  const id = `bulk-${Date.now()}-${++seq}`;
  const run: BulkRun = {
    id,
    checkId: opts.checkId,
    checkLabel: opts.checkLabel,
    actionLabel: opts.actionLabel,
    fixKind: opts.fixKind,
    phase: "running",
    total: opts.total,
    started: false,
    counts: { fixed: 0, skipped: 0, errors: 0, not_applicable: 0 },
    items: [],
    summary: null,
    error: null,
    startedAt: Date.now(),
    endedAt: null,
  };
  runs = [run, ...runs].slice(0, KEPT_RUNS);
  emit();

  const cancel = streamQuickFixAll(opts.checkId, {
    onStart: (total) => update(id, (r) => ({ ...r, total, started: true })),
    onProgress: (ev) => {
      const album = opts.albumOf(ev.finding_id);
      const item: BulkResultItem = {
        findingId: ev.finding_id,
        albumId: album.id,
        album: album.label,
        outcome: ev.outcome,
        reason: plainReason(ev.outcome, ev.message),
      };
      update(id, (r) => {
        if (r.phase !== "running") return r;
        const counts = { ...r.counts };
        if (ev.outcome === "fixed") counts.fixed++;
        else if (ev.outcome === "skipped") counts.skipped++;
        else if (ev.outcome === "error") counts.errors++;
        else counts.not_applicable++;
        return { ...r, items: [...r.items, item], counts };
      });
    },
    onDone: (summary) => end(id, "completed", { summary }),
    onError: (detail) => {
      const r = getBulkRun(id);
      end(id, r?.started ? "interrupted" : "failed", { error: detail });
    },
    // The response ended without `done`: the connection, not the run.
    onClosed: () => end(id, "interrupted", { error: "The connection to the run closed before it finished." }),
  });
  cancels.set(id, cancel);
  return id;
}

/**
 * Stop a run: no further finding is started. What was reported stands
 * (each fix is written as it happens); the one in progress may still finish.
 */
export function stopBulkRun(id: string): void {
  cancels.get(id)?.();
  end(id, "stopped");
}

export function getBulkRun(id: string): BulkRun | undefined {
  return runs.find((r) => r.id === id);
}

export function getBulkRuns(): BulkRun[] {
  return runs;
}

function subscribe(l: () => void): () => void {
  listeners.add(l);
  return () => {
    listeners.delete(l);
  };
}

export function useBulkRuns(): BulkRun[] {
  return useSyncExternalStore(subscribe, getBulkRuns, getBulkRuns);
}

export function useBulkRun(id: string | null): BulkRun | undefined {
  const all = useBulkRuns();
  return id ? all.find((r) => r.id === id) : undefined;
}

/** Forget the finished runs (Activity's "Clear"). Running ones stay. */
export function clearFinishedBulkRuns(): void {
  runs = runs.filter(isBulkRunning);
  emit();
}

/** Test hook: stop everything and forget it. */
export function resetBulkRuns(): void {
  for (const c of cancels.values()) c();
  cancels.clear();
  runs = [];
  emit();
}
