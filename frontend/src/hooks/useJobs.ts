/**
 * Research request (job card) hooks.
 *
 * The single source of truth is the "jobs" query in TanStack Query, kept
 * fresh by `useJobsStream` (mounted at App level) which subscribes to
 * /api/ai/jobs/stream via SSE and writes the latest snapshot into the cache
 * on every change. All other components consume that cache via `useJobs()`.
 */
import { useMemo } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  bulkDeleteJobs,
  cancelResearchGroup,
  deleteJob as deleteJobApi,
  enqueueJob,
  getSettings,
  listJobs,
  patchSettings,
  type AppSettings,
  type AppSettingsPatch,
  type QueueState,
} from "@/lib/api";
import type { Job, JobStatus, JobType } from "@/lib/types";
import { batchedActiveJobs } from "@/lib/batchedResearch";
import { isHistoryOnlyJob } from "@/lib/filters";
import { useDelegatedWork } from "@/hooks/useDelegatedWork";

export const JOBS_QUERY_KEY = ["jobs"] as const;
export const QUEUE_STATE_QUERY_KEY = ["jobs", "queueState"] as const;
export const SETTINGS_QUERY_KEY = ["settings"] as const;

/**
 * All jobs, sorted newest first. Hydrated initially via REST, then kept
 * up-to-date by useJobsStream (which writes via setQueryData).
 */
export function useJobs() {
  return useQuery({
    queryKey: JOBS_QUERY_KEY,
    queryFn: async () => (await listJobs()).jobs,
    staleTime: Infinity,  // SSE keeps it fresh
  });
}

/**
 * Find the most recent active (queued/running/completed-not-acted-on) job
 * for an album. Returns null if none. Reads from the cached jobs list, so
 * it updates live as the SSE stream pushes changes.
 */
export function useActiveJobForAlbum(albumId: string | null | undefined): Job | null {
  const { data: jobs = [] } = useJobs();
  if (!albumId) return null;
  // Active = queued | running | completed (NOT acted_on or failed-without-action)
  const active = jobs
    .filter(
      (j) =>
        j.album_id === albumId &&
        !isHistoryOnlyJob(j) &&
        (j.status === "queued" || j.status === "running" || j.status === "completed"),
    )
    .sort((a, b) => (a.created_at < b.created_at ? 1 : -1));
  return active[0] ?? null;
}

/**
 * Map of album_id → most-recent active job for that album. "Active" matches
 * `useActiveJobForAlbum` (queued | running | completed-not-yet-acted-on).
 *
 * Computed once per jobs-cache update so the library tiles can render a
 * status overlay without each Tile doing its own filter pass over the full
 * jobs list.
 */
export function useActiveJobAlbumMap(): Map<string, Job> {
  const { data: jobs = [] } = useJobs();
  // Requests of a Fix all batch have no job card; the delegated work list
  // (["research", "work"], kept fresh by the `work` event family) carries
  // them, so an album with one gets the same pill.
  const { data: work } = useDelegatedWork();
  // Memoize on the array references. TanStack Query gives us a fresh
  // array only when the query data actually changes, so this map is stable
  // across unrelated parent re-renders (e.g. typing in the search box).
  return useMemo(() => {
    const sorted = [...jobs].sort((a, b) => (a.created_at < b.created_at ? 1 : -1));
    const map = new Map<string, Job>();
    for (const j of sorted) {
      if (j.status !== "queued" && j.status !== "running" && j.status !== "completed") continue;
      if (isHistoryOnlyJob(j)) continue;
      if (!map.has(j.album_id)) map.set(j.album_id, j);
    }
    // A job card wins over a batched request of the same album.
    for (const [albumId, j] of batchedActiveJobs(Array.isArray(work) ? work : [])) {
      if (!map.has(albumId)) map.set(albumId, j);
    }
    return map;
  }, [jobs, work]);
}

/**
 * Prior cover-art lookup attempts for an album that ended without a usable
 * cover. Used to surface "we've tried this N times before" context in the
 * HintDialog so the user doesn't unintentionally re-request a cover that
 * research or MusicBrainz already couldn't find.
 *
 * Qualifying jobs:
 *   - job_type is "cover" or "metadata+cover"
 *   - terminal state (failed, OR acted_on with no cover_url in the result)
 *
 * Currently-active jobs (queued / running / completed-not-yet-acted-on) are
 * excluded — those are surfaced separately by useActiveJobForAlbum and would
 * double-count if listed here.
 */
export function usePriorCoverAttempts(albumId: string | null | undefined): Job[] {
  const { data: jobs = [] } = useJobs();
  return useMemo(() => {
    if (!albumId) return [];
    const out = jobs.filter((j) => {
      if (j.album_id !== albumId) return false;
      if (j.job_type !== "cover" && j.job_type !== "metadata+cover") return false;
      if (j.status === "failed") return true;
      if (j.status === "acted_on" && !j.result?.cover?.cover_url) return true;
      return false;
    });
    return out.sort((a, b) => (a.created_at < b.created_at ? 1 : -1));
  }, [albumId, jobs]);
}


// ---- Mutations ----

export function useEnqueueJob() {
  return useMutation({
    mutationFn: (args: {
      albumId: string;
      jobType: JobType;
      hint?: string;
      // v16: opt-out from cover-attach. Passed through to
      // `enqueueJob` (api.ts) which serializes to the backend's
      // `attach_cover` field. Defaults true backend-side.
      attachCover?: boolean;
    }) => enqueueJob(args),
  });
}

// A delete or Clear wakes /api/ai/jobs/stream (useJobsStream writes the
// new list into the cache) and, for research requests, publishes
// AgentStateChanged(work) (rule 5); the handlers refresh nothing themselves.
export function useDeleteJob() {
  return useMutation({
    mutationFn: (jobId: string) => deleteJobApi(jobId),
  });
}

export function useBulkDeleteJobs() {
  return useMutation({
    mutationFn: (statuses: JobStatus[]) => bulkDeleteJobs(statuses),
  });
}

/**
 * Cancel the research requests behind one job card (every waiting, held or
 * running item; items awaiting review or finished are left alone). The
 * `work` event family refreshes the jobs and research caches (rule 5), so
 * the handler invalidates nothing.
 */
export function useCancelResearchGroup() {
  return useMutation({
    mutationFn: (jobId: string) => cancelResearchGroup(jobId),
  });
}

// ---- Queue state + settings ----

/**
 * Live research queue counts (running, queued, and how many requests
 * "Clear applied or discarded" and "Clear failed" remove). The jobs stream
 * writes every snapshot into this cache; until its first frame the counts
 * are zero.
 */
export function useQueueState() {
  return useQuery<QueueState>({
    queryKey: QUEUE_STATE_QUERY_KEY,
    queryFn: async (): Promise<QueueState> => ({ running: 0, queued: 0, clearable: 0, clearable_failed: 0 }),
    staleTime: Infinity,  // SSE keeps it fresh
  });
}

export function useSettings() {
  return useQuery<AppSettings>({
    queryKey: SETTINGS_QUERY_KEY,
    queryFn: getSettings,
    // Settings change only via useUpdateSettings (which calls
    // setQueryData on success). 30s refetch catches another tab
    // editing the same settings.json out-of-band.
    staleTime: 30_000,
  });
}

export function useUpdateSettings() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (patch: AppSettingsPatch) => patchSettings(patch),
    onSuccess: (data) => {
      qc.setQueryData(SETTINGS_QUERY_KEY, data);
    },
  });
}
