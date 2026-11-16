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
  /** `count` may be too low: the findings list hit its limit. */
  countIsLowerBound: boolean;
}

const newestFirst = (a: Job, b: Job) => (a.created_at < b.created_at ? 1 : -1);

export function useAttention(): Attention & { isLoading: boolean } {
  const findingsQ = useFindings();
  const jobsQ = useJobs();
  const all = findingsQ.data;
  const jobs = jobsQ.data;
  return useMemo(() => {
    const fs = all ?? [];
    const js = jobs ?? [];
    const findings = fs.filter(needsDecisionFinding);
    const proposed = js.filter((j) => j.status === "completed").sort(newestFirst);
    const failed = js.filter((j) => j.status === "failed").sort(newestFirst);
    return {
      findings,
      proposed,
      failed,
      infoFindings: fs.filter((f) => !f.ignored_at && !f.resolved_at && f.severity === "info"),
      finishedJobs: js.filter((j) => j.status === "acted_on").sort(newestFirst),
      inProgress: js.filter((j) => j.status === "queued" || j.status === "running").sort(newestFirst),
      count: findings.length + proposed.length + failed.length,
      countIsLowerBound: fs.length >= FINDINGS_LIST_LIMIT,
      isLoading: findingsQ.isLoading || jobsQ.isLoading,
    };
  }, [all, jobs, findingsQ.isLoading, jobsQ.isLoading]);
}

/** The Sidebar badge text for a count. */
export function attentionBadgeText(count: number, lowerBound: boolean): string {
  return lowerBound ? `${count}+` : String(count);
}
