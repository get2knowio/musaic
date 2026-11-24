/**
 * Research requests made by a Fix all run belong to a batch and are left out
 * of the job cards (`GET /api/ai/jobs`), so the Library reads them from the
 * delegated work list instead: an album with a batched request gets the same
 * status pill as one with a card, and a batched result waiting for review is
 * found by "Jobs to review".
 */
import type { DelegatedWorkItem, DelegatedWorkState } from "./api";
import type { Job, JobStatus, JobType } from "./types";

/** The job-card status a batched item shows as, as the server maps a card's
 * requests (claimed and running are both "running"); null for a state the
 * pill does not show. */
export function batchedItemStatus(state: DelegatedWorkState): JobStatus | null {
  switch (state) {
    case "queued":
      return "queued";
    case "claimed":
    case "running":
      return "running";
    case "awaiting_review":
      return "completed";
    case "failed":
      return "failed";
    default:
      return null;
  }
}

function jobTypeOf(kinds: Set<string>): JobType {
  const cover = kinds.has("cover.research");
  const metadata = [...kinds].some((k) => k !== "cover.research");
  if (cover && metadata) return "metadata+cover";
  return cover ? "cover" : "metadata";
}

/** Higher wins when one album has several batched items: a result waiting
 * for review first, then work in progress, then waiting work. */
const RANK: Partial<Record<JobStatus, number>> = { completed: 3, running: 2, queued: 1 };

/**
 * One job-card-shaped entry per album for its batched items in an active
 * state (queued, claimed, running, awaiting review), for the status pill.
 * Items with no batch are already on a job card and are skipped.
 */
export function batchedActiveJobs(work: DelegatedWorkItem[] | undefined): Map<string, Job> {
  const byAlbum = new Map<string, { status: JobStatus; kinds: Set<string>; item: DelegatedWorkItem }>();
  for (const item of work ?? []) {
    if (!item.batchId) continue;
    const status = batchedItemStatus(item.state);
    if (!status || !RANK[status]) continue;
    const prev = byAlbum.get(item.albumId);
    if (!prev) {
      byAlbum.set(item.albumId, { status, kinds: new Set([item.kind]), item });
      continue;
    }
    prev.kinds.add(item.kind);
    if ((RANK[status] ?? 0) > (RANK[prev.status] ?? 0)) {
      prev.status = status;
      prev.item = item;
    }
  }
  const out = new Map<string, Job>();
  for (const [albumId, { status, kinds, item }] of byAlbum) {
    out.set(albumId, {
      id: item.workId,
      album_id: albumId,
      album_path: "",
      album_title: item.albumLabel ?? "",
      job_type: jobTypeOf(kinds),
      hint: "",
      status,
      result: null,
      error: null,
      acted_on_action: null,
      created_at: "",
      started_at: null,
      completed_at: null,
      acted_on_at: null,
    });
  }
  return out;
}

/** Per album, the job-card status its batched requests wait for the user
 * in, as a card would show it: "completed" when a result waits for review
 * (it wins), else "failed". Albums with neither are left out. */
export function batchedToReview(
  work: readonly DelegatedWorkItem[] | undefined,
): Map<string, "completed" | "failed"> {
  const out = new Map<string, "completed" | "failed">();
  for (const item of work ?? []) {
    if (!item.batchId) continue;
    if (item.state === "awaiting_review") out.set(item.albumId, "completed");
    else if (item.state === "failed" && !out.has(item.albumId)) out.set(item.albumId, "failed");
  }
  return out;
}
