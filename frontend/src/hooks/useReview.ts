/**
 * The Review inbox: every open item that asks something of the user, in
 * one list, derived from the caches the SSE streams already keep fresh
 * (findings, jobs): no new endpoint.
 *
 * An item is one of
 *  - an open finding of any severity (not ignored, not resolved);
 *  - a proposed change (a job with status "completed": a result awaiting
 *    review);
 *  - a failed job;
 *  - a pending approval: an assistant asked to do something that needs the
 *    user's decision (it expires, so it is the most pressing after errors).
 *
 * Queued and running jobs and finished ones (applied / discarded) are
 * activity, not review items: they are returned apart (`inProgress`,
 * `finishedJobs`) and never counted. Ignored findings leave the count.
 *
 * `count` is the number of distinct items in the default view (All open):
 * the Sidebar badge and the page header use it, with `reviewBadge`.
 *
 * Each of the three sources (findings, jobs, assistant requests) is in one of four states, kept
 * apart so that a request that failed never reads as "nothing to do":
 * `loading` (no answer yet), `ok` (answered; an empty list is a confirmed
 * empty), `unavailable` (failed with nothing cached) and `stale` (the
 * last answer is kept but the latest refresh failed). Only when every
 * source is `ok` is a zero a verified zero.
 */
import { useMemo } from "react";

import { useChecks, useFindings } from "./useChecks";
import { useJobs } from "./useJobs";
import { useAgentApprovals } from "./useAgents";
import type { AgentApproval } from "@/lib/api";
import type { Finding, Job } from "@/lib/types";

/** The findings endpoint's default row limit (10000). A list that reaches
 *  it may be missing findings, so the count is then a lower bound. */
export const FINDINGS_LIST_LIMIT = 10_000;

/** How pressing an item is, most pressing first. */
export type Urgency = "error" | "approval" | "failed" | "warning" | "proposal" | "info";

export const URGENCY_ORDER: readonly Urgency[] = ["error", "approval", "failed", "warning", "proposal", "info"];

/** The short label a row carries. */
export const URGENCY_LABEL: Record<Urgency, string> = {
  error: "Error",
  approval: "Needs approval",
  failed: "Failed",
  warning: "Warning",
  proposal: "To review",
  info: "Suggestion",
};

export type ReviewItem =
  | { kind: "finding"; key: string; urgency: Urgency; albumId: string | null; finding: Finding }
  | { kind: "job"; key: string; urgency: Urgency; albumId: string; job: Job }
  | { kind: "approval"; key: string; urgency: Urgency; albumId: string | null; approval: AgentApproval };

export const findingKey = (id: number) => `finding:${id}`;
export const jobKey = (id: string) => `job:${id}`;
export const approvalKey = (id: string) => `approval:${id}`;

export function isOpenFinding(f: Finding): boolean {
  return !f.ignored_at && !f.resolved_at;
}

export function isReviewJob(j: Job): boolean {
  return j.status === "completed" || j.status === "failed";
}

function findingUrgency(f: Finding): Urgency {
  return f.severity === "error" ? "error" : f.severity === "warn" ? "warning" : "info";
}

/**
 * The inbox's order: most pressing first (`URGENCY_ORDER`); then findings
 * in the checks' display order and by id (oldest first), jobs newest
 * first; the key last, so equal items always come out the same way.
 */
export function sortReviewItems(items: ReviewItem[], checkOrder: readonly string[] = []): ReviewItem[] {
  const checkIndex = new Map(checkOrder.map((id, i) => [id, i]));
  const rank = (u: Urgency) => URGENCY_ORDER.indexOf(u);
  const group = (i: ReviewItem) =>
    i.kind === "finding" ? (checkIndex.get(i.finding.check_id) ?? checkOrder.length) : 0;
  return [...items].sort((a, b) => {
    if (a.urgency !== b.urgency) return rank(a.urgency) - rank(b.urgency);
    if (group(a) !== group(b)) return group(a) - group(b);
    if (a.kind === "finding" && b.kind === "finding" && a.finding.id !== b.finding.id) {
      return a.finding.id - b.finding.id;
    }
    if (a.kind === "job" && b.kind === "job" && a.job.created_at !== b.job.created_at) {
      return a.job.created_at < b.job.created_at ? 1 : -1;
    }
    // Approvals expire in the order they were asked: the oldest is first.
    if (a.kind === "approval" && b.kind === "approval" && a.approval.createdAt !== b.approval.createdAt) {
      return a.approval.createdAt < b.approval.createdAt ? -1 : 1;
    }
    return a.key < b.key ? -1 : a.key > b.key ? 1 : 0;
  });
}

/** The open items, sorted. */
export function buildReviewItems(
  findings: readonly Finding[],
  jobs: readonly Job[],
  checkOrder: readonly string[] = [],
  approvals: readonly AgentApproval[] = [],
): ReviewItem[] {
  const items: ReviewItem[] = [];
  for (const f of findings) {
    if (!isOpenFinding(f)) continue;
    items.push({ kind: "finding", key: findingKey(f.id), urgency: findingUrgency(f), albumId: f.album_id, finding: f });
  }
  for (const j of jobs) {
    if (!isReviewJob(j)) continue;
    items.push({
      kind: "job",
      key: jobKey(j.id),
      urgency: j.status === "failed" ? "failed" : "proposal",
      albumId: j.album_id,
      job: j,
    });
  }
  for (const a of approvals) {
    if (a.state !== "pending") continue;
    items.push({ kind: "approval", key: approvalKey(a.id), urgency: "approval", albumId: a.albumId, approval: a });
  }
  return sortReviewItems(items, checkOrder);
}

/** What the list can be narrowed to: one of these, plus an optional album. */
export type ReviewFilter =
  | { kind: "all" }
  | { kind: "check"; id: string }
  | { kind: "proposed" }
  | { kind: "failed" }
  | { kind: "approvals" };

export function filterReviewItems(
  items: readonly ReviewItem[],
  filter: ReviewFilter,
  albumId: string | null = null,
): ReviewItem[] {
  return items.filter((i) => {
    if (albumId && i.albumId !== albumId) return false;
    switch (filter.kind) {
      case "all":
        return true;
      case "check":
        return i.kind === "finding" && i.finding.check_id === filter.id;
      case "proposed":
        return i.kind === "job" && i.job.status === "completed";
      case "failed":
        return i.kind === "job" && i.job.status === "failed";
      case "approvals":
        return i.kind === "approval";
    }
  });
}

/** Distinct albums the items are about. */
export function albumCount(items: readonly ReviewItem[]): number {
  return new Set(items.map((i) => i.albumId).filter((a): a is string => !!a)).size;
}

export type SourceState = "loading" | "ok" | "unavailable" | "stale";

export interface SourceStatus {
  /** What the source is, for copy: "findings", "jobs", "assistant requests". */
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

export interface ReviewInbox {
  /** Every open item, most pressing first (All open). */
  items: ReviewItem[];
  /** Distinct items in All open: the badge. */
  count: number;
  /** `count` may be too low: the findings list hit its limit, or a source
   *  could not be read at all. */
  countIsLowerBound: boolean;
  /** Queued or running jobs, newest first (activity, not counted). */
  inProgress: Job[];
  /** Applied / discarded jobs, newest first (history, not counted). */
  finishedJobs: Job[];
  sources: { findings: SourceStatus; jobs: SourceStatus; approvals: SourceStatus };
  /** Every source has answered; nothing failed. */
  complete: boolean;
  /** Some source failed (no data, or only last-known data). */
  degraded: boolean;
  /** Nothing to review and every source answered: a verified zero. */
  allClear: boolean;
  /** Every source is still waiting for its first answer. */
  isLoading: boolean;
}

const newestFirst = (a: Job, b: Job) => (a.created_at < b.created_at ? 1 : -1);

export function useReview(): ReviewInbox {
  const findingsQ = useFindings();
  const jobsQ = useJobs();
  const approvalsQ = useAgentApprovals("pending");
  // Only for the order of equal items; the checks list is not a source.
  const { data: checks } = useChecks();
  const all = findingsQ.data;
  const jobs = jobsQ.data;
  const findingsState = sourceStateOf(findingsQ);
  const jobsState = sourceStateOf(jobsQ);
  const approvals = approvalsQ.data;
  const approvalsState = sourceStateOf(approvalsQ);
  const findingsFetching = findingsQ.isFetching;
  const jobsFetching = jobsQ.isFetching;
  const approvalsFetching = approvalsQ.isFetching;
  const refetchApprovals = approvalsQ.refetch;
  const refetchFindings = findingsQ.refetch;
  const refetchJobs = jobsQ.refetch;
  return useMemo(() => {
    const fs = all ?? [];
    const js = jobs ?? [];
    const items = buildReviewItems(fs, js, (checks ?? []).map((c) => c.meta.id), approvals ?? []);
    const states = [findingsState, jobsState, approvalsState];
    const complete = states.every((s) => s === "ok");
    return {
      items,
      count: items.length,
      countIsLowerBound: fs.length >= FINDINGS_LIST_LIMIT || states.includes("unavailable"),
      inProgress: js.filter((j) => j.status === "queued" || j.status === "running").sort(newestFirst),
      finishedJobs: js.filter((j) => j.status === "acted_on").sort(newestFirst),
      sources: {
        findings: {
          label: "findings",
          state: findingsState,
          retry: () => void refetchFindings(),
          retrying: findingsFetching,
        },
        jobs: { label: "jobs", state: jobsState, retry: () => void refetchJobs(), retrying: jobsFetching },
        approvals: {
          label: "assistant requests",
          state: approvalsState,
          retry: () => void refetchApprovals(),
          retrying: approvalsFetching,
        },
      },
      complete,
      degraded: states.some((s) => s === "unavailable" || s === "stale"),
      allClear: complete && items.length === 0,
      isLoading: states.every((s) => s === "loading"),
    };
  }, [
    all,
    jobs,
    approvals,
    checks,
    findingsState,
    jobsState,
    approvalsState,
    findingsFetching,
    jobsFetching,
    approvalsFetching,
    refetchFindings,
    refetchJobs,
    refetchApprovals,
  ]);
}

/** A count with its lower-bound mark: "7", or "7+". */
export function countText(count: number, lowerBound: boolean): string {
  return lowerBound ? `${count}+` : String(count);
}

/** "1 item", "3 items", "3+ items". */
export function itemsText(count: number, lowerBound = false): string {
  return `${countText(count, lowerBound)} ${count === 1 && !lowerBound ? "item" : "items"}`;
}

/**
 * The Sidebar badge, or null for no badge: while nothing has answered yet,
 * and at a verified zero. A source that failed outright makes the count a
 * lower bound ("2+", or "?" when nothing is known); a source showing
 * last-known data says so in the title.
 */
export function reviewBadge(r: ReviewInbox): { text: string; title: string; uncertain: boolean } | null {
  if (r.isLoading) return null;
  const unavailable = Object.values(r.sources).some((s) => s.state === "unavailable");
  const stale = Object.values(r.sources).some((s) => s.state === "stale");
  if (r.count === 0 && unavailable) {
    return { text: "?", title: "? Some review data could not be loaded", uncertain: true };
  }
  if (r.count === 0) return null;
  const note = unavailable
    ? " (some review data could not be loaded)"
    : stale
      ? " (last known; refreshing failed)"
      : "";
  return {
    text: countText(r.count, r.countIsLowerBound),
    title: `${itemsText(r.count, r.countIsLowerBound)} to review${note}`,
    uncertain: unavailable || stale,
  };
}
