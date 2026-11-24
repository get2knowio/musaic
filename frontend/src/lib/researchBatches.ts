/**
 * Words and counts for a Fix all batch of research requests, shared by the
 * Review and Activity entries (components/ResearchBatch.tsx).
 */
import type { BatchSummary, ResearchBatchState } from "./api";

/** Items of an open batch listed before "Show all N". */
export const BATCH_ITEM_LIMIT = 50;

export const KIND_LABEL: Record<string, string> = {
  "metadata.identify": "Identify album",
  "metadata.correct": "Correct metadata",
  "cover.research": "Find cover",
};

/** The states a batch counts, in the order they are shown, in plain words. */
const COUNT_ORDER: { state: ResearchBatchState; label: string }[] = [
  { state: "queued", label: "waiting" },
  { state: "claimed", label: "claimed" },
  { state: "running", label: "investigating" },
  { state: "awaiting_review", label: "waiting for you" },
  { state: "applied", label: "applied" },
  { state: "discarded", label: "discarded" },
  { state: "failed", label: "failed" },
  { state: "cancelled", label: "cancelled" },
];

export const BATCH_STATE_LABEL: Record<ResearchBatchState, string> = {
  queued: "Waiting",
  claimed: "Claimed",
  running: "Investigating",
  awaiting_review: "Result waiting for you",
  applied: "Applied",
  discarded: "Discarded",
  failed: "Failed",
  cancelled: "Cancelled",
};

/** "120 research requests from Fix all · Unidentified album". */
export function batchTitle(batch: BatchSummary, checkLabel: string): string {
  const n = batch.size;
  return `${n} research ${n === 1 ? "request" : "requests"} from Fix all · ${checkLabel}`;
}

/** The non-zero counts, e.g. "100 waiting · 5 claimed · 10 waiting for you". */
export function batchCountsText(batch: BatchSummary): string {
  return COUNT_ORDER.filter(({ state }) => (batch.counts[state] ?? 0) > 0)
    .map(({ state, label }) => `${batch.counts[state]} ${label}`)
    .join(" · ");
}

/** How many items a cancel would cancel: those waiting or held. */
export function cancellableCount(batch: BatchSummary): number {
  const c = batch.counts;
  return (c.queued ?? 0) + (c.claimed ?? 0) + (c.running ?? 0);
}
