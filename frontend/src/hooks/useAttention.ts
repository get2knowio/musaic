/**
 * What needs a decision from the user, derived from the caches the SSE
 * streams already keep fresh (findings, jobs): no new endpoint.
 *
 *  - open findings of severity warn or error (not ignored, not resolved);
 *  - jobs that are proposed (status "completed": a result awaiting review)
 *    or failed.
 *
 * Informational findings, applied/discarded jobs and queued/running jobs
 * are "other": listed on the page's second view, never counted. The
 * Sidebar badge is `count`.
 *
 * Each of the two sources (findings, jobs) is in one of four states, kept
 * apart so that a request that failed never reads as "nothing to do":
 * `loading` (no answer yet), `ok` (answered; an empty list is a confirmed
 * empty), `unavailable` (failed with nothing cached) and `stale` (the
 * last answer is kept but the latest refresh failed). Only when every
 * source is `ok` is a zero a verified zero.
 */
import { useMemo } from "react";

import { useFindings } from "./useChecks";
import { useJobs } from "./useJobs";
import type { Finding, Job } from "@/lib/types";

/** The findings endpoint's default row limit (10000). A list that reaches
 *  it may be missing findings, so the count is then a lower bound. */
export const FINDINGS_LIST_LIMIT = 10_000;

export function needsDecisionFinding(f: Finding): boolean {
  return !f.ignored_at && !f.resolved_at && (f.severity === "warn" || f.severity === "error");
}

export function needsDecisionJob(j: Job): boolean {
  return j.status === "completed" || j.status === "failed";
}

export type SourceState = "loading" | "ok" | "unavailable" | "stale";

export interface SourceStatus {
  /** What the source is, for copy: "findings", "jobs". */
  label: string;
  state: SourceState;
  /** Ask the source again (a refetch of its query). */
  retry: () => void;
  /** A retry (or any refetch) is in flight. */
  retrying: boolean;
}

/** The state of one query, as the four cases above. */
export function sourceStateOf(q: { status: "pending" | "error" | "success"; data: unknown }): SourceState {
  if (q.status === "success") return "ok";
  if (q.status === "error") return q.data === undefined ? "unavailable" : "stale";
  return "loading";
}

export interface Attention {
  findings: Finding[];
  /** Proposed (awaiting review) jobs, newest first. */
  proposed: Job[];
  /** Failed jobs, newest first. */
  failed: Job[];
  /** Informational findings (open, not ignored). */
  infoFindings: Finding[];
  /** Applied / discarded jobs. */
  finishedJobs: Job[];
  /** Queued or running jobs. */
  inProgress: Job[];
  count: number;
  /** `count` may be too low: the findings list hit its limit, or a source
   *  could not be read at all. */
  countIsLowerBound: boolean;
  sources: { findings: SourceStatus; jobs: SourceStatus };
  /** Every source has answered; nothing failed. */
  complete: boolean;
  /** Some source failed (no data, or only last-known data). */
  degraded: boolean;
  /** Nothing to decide and every source answered: a verified zero. */
  allClear: boolean;
  /** Every source is still waiting for its first answer. */
  isLoading: boolean;
}

const newestFirst = (a: Job, b: Job) => (a.created_at < b.created_at ? 1 : -1);

export function useAttention(): Attention {
  const findingsQ = useFindings();
  const jobsQ = useJobs();
  const all = findingsQ.data;
  const jobs = jobsQ.data;
  const findingsState = sourceStateOf(findingsQ);
  const jobsState = sourceStateOf(jobsQ);
  const findingsFetching = findingsQ.isFetching;
  const jobsFetching = jobsQ.isFetching;
  const refetchFindings = findingsQ.refetch;
  const refetchJobs = jobsQ.refetch;
  return useMemo(() => {
    const fs = all ?? [];
    const js = jobs ?? [];
    const findings = fs.filter(needsDecisionFinding);
    const proposed = js.filter((j) => j.status === "completed").sort(newestFirst);
    const failed = js.filter((j) => j.status === "failed").sort(newestFirst);
    const count = findings.length + proposed.length + failed.length;
    const states = [findingsState, jobsState];
    const complete = states.every((s) => s === "ok");
    return {
      findings,
      proposed,
      failed,
      infoFindings: fs.filter((f) => !f.ignored_at && !f.resolved_at && f.severity === "info"),
      finishedJobs: js.filter((j) => j.status === "acted_on").sort(newestFirst),
      inProgress: js.filter((j) => j.status === "queued" || j.status === "running").sort(newestFirst),
      count,
      countIsLowerBound: fs.length >= FINDINGS_LIST_LIMIT || states.includes("unavailable"),
      sources: {
        findings: {
          label: "findings",
          state: findingsState,
          retry: () => void refetchFindings(),
          retrying: findingsFetching,
        },
        jobs: { label: "jobs", state: jobsState, retry: () => void refetchJobs(), retrying: jobsFetching },
      },
      complete,
      degraded: states.some((s) => s === "unavailable" || s === "stale"),
      allClear: complete && count === 0,
      isLoading: states.every((s) => s === "loading"),
    };
  }, [
    all,
    jobs,
    findingsState,
    jobsState,
    findingsFetching,
    jobsFetching,
    refetchFindings,
    refetchJobs,
  ]);
}

/** The Sidebar badge text for a count. */
export function attentionBadgeText(count: number, lowerBound: boolean): string {
  return lowerBound ? `${count}+` : String(count);
}

/**
 * The Sidebar badge for the attention state, or null for no badge: while
 * nothing has answered yet, and at a verified zero. A source that failed
 * outright makes the count a lower bound ("2+", or "?" when nothing is
 * known to need attention); a source showing last-known data says so in
 * the title.
 */
export function attentionBadge(a: Attention): { text: string; title: string; uncertain: boolean } | null {
  if (a.isLoading) return null;
  const unavailable = Object.values(a.sources).some((s) => s.state === "unavailable");
  const stale = Object.values(a.sources).some((s) => s.state === "stale");
  if (a.count === 0 && unavailable) {
    return { text: "?", title: "? Some review data could not be loaded", uncertain: true };
  }
  if (a.count === 0) return null;
  const text = attentionBadgeText(a.count, a.countIsLowerBound);
  const note = unavailable
    ? " (some review data could not be loaded)"
    : stale
      ? " (last known; refreshing failed)"
      : "";
  return { text, title: `${text} to decide${note}`, uncertain: unavailable || stale };
}
