/**
 * AI Jobs hooks.
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
  deleteJob as deleteJobApi,
  enqueueJob,
  getAiProviders,
  getSettings,
  listJobs,
  markJobActedOn,
  patchSettings,
  resumeQueue,
  retryJob,
  type AppSettings,
  type AppSettingsPatch,
  type QueueState,
} from "@/lib/api";
import type { Job, JobStatus, JobType } from "@/lib/types";

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
    .filter((j) => j.album_id === albumId && (j.status === "queued" || j.status === "running" || j.status === "completed"))
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
  // Memoize on the jobs array reference. TanStack Query gives us a fresh
  // array only when the query data actually changes, so this map is stable
  // across unrelated parent re-renders (e.g. typing in the search box).
  return useMemo(() => {
    const sorted = [...jobs].sort((a, b) => (a.created_at < b.created_at ? 1 : -1));
    const map = new Map<string, Job>();
    for (const j of sorted) {
      if (j.status !== "queued" && j.status !== "running" && j.status !== "completed") continue;
      if (!map.has(j.album_id)) map.set(j.album_id, j);
    }
    return map;
  }, [jobs]);
}

/**
 * Prior cover-art lookup attempts for an album that ended without a usable
 * cover. Used to surface "we've tried this N times before" context in the
 * HintDialog so the user doesn't unintentionally re-request a cover that
 * Claude / MusicBrainz already couldn't find.
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

/**
 * Counts of in-flight jobs (running + queued behind it) for the sidebar
 * activity indicator. The serial worker means at most one is "running" at
 * any moment, but additional jobs may be queued.
 */
export function useInFlightJobCounts(): { running: number; queued: number } {
  const { data: jobs = [] } = useJobs();
  let running = 0;
  let queued = 0;
  for (const j of jobs) {
    if (j.status === "running") running++;
    else if (j.status === "queued") queued++;
  }
  return { running, queued };
}

// ---- Mutations ----

export function useEnqueueJob() {
  const qc = useQueryClient();
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
    onSuccess: () => {
      // The SSE stream will push the updated list shortly, but invalidate
      // here so the UI updates instantly without waiting for the round trip.
      qc.invalidateQueries({ queryKey: JOBS_QUERY_KEY });
    },
  });
}

export function useMarkJobActedOn() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (args: { jobId: string; action: "applied" | "discarded" }) =>
      markJobActedOn(args.jobId, args.action),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: JOBS_QUERY_KEY });
    },
  });
}

export function useDeleteJob() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (jobId: string) => deleteJobApi(jobId),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: JOBS_QUERY_KEY });
    },
  });
}

export function useBulkDeleteJobs() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (statuses: JobStatus[]) => bulkDeleteJobs(statuses),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: JOBS_QUERY_KEY });
    },
  });
}

export function useRetryJob() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (jobId: string) => retryJob(jobId),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: JOBS_QUERY_KEY });
    },
  });
}

// ---- Queue state + settings ----

/**
 * Live queue state (paused flag, parallel cap, running/queued counts). The
 * SSE stream pushes this on every jobs change; we hydrate the initial value
 * via /api/settings → queue_state isn't there directly, so we derive a
 * sensible placeholder until the first SSE message arrives.
 */
export function useQueueState() {
  return useQuery<QueueState>({
    queryKey: QUEUE_STATE_QUERY_KEY,
    // Hydrate from /api/settings synchronously — counts will be 0 until SSE
    // delivers the first real snapshot, but paused + max_parallel are real.
    queryFn: async (): Promise<QueueState> => {
      const s = await getSettings();
      return {
        paused: s.ai_queue_paused,
        max_parallel: s.ai_max_parallel,
        running: 0,
        queued: 0,
      };
    },
    staleTime: Infinity,  // SSE keeps it fresh
  });
}

export function useResumeQueue() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: () => resumeQueue(),
    onSuccess: (data) => {
      qc.setQueryData(QUEUE_STATE_QUERY_KEY, data.queue_state);
    },
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

/** Installed airframe providers + current selection. Static-ish; the list
 *  only changes when an extra is pip-installed (a restart), so no SSE. */
export function useAiProviders() {
  return useQuery({
    queryKey: ["ai-providers"],
    queryFn: getAiProviders,
    staleTime: 5 * 60_000,
  });
}

export function useUpdateSettings() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (patch: AppSettingsPatch) => patchSettings(patch),
    onSuccess: (data) => {
      qc.setQueryData(SETTINGS_QUERY_KEY, data);
      // Provider/model/key live in the providers endpoint's "selected" too;
      // refetch so its view stays in sync after an edit here.
      qc.invalidateQueries({ queryKey: ["ai-providers"] });
      // Reflect in queue state immediately too — the next SSE will overwrite
      // with the authoritative counts but this avoids any UI lag.
      qc.setQueryData(QUEUE_STATE_QUERY_KEY, (prev: QueueState | undefined) =>
        prev
          ? { ...prev, paused: data.ai_queue_paused, max_parallel: data.ai_max_parallel }
          : { paused: data.ai_queue_paused, max_parallel: data.ai_max_parallel, running: 0, queued: 0 },
      );
    },
  });
}
