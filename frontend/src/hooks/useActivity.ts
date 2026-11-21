/**
 * Everything the Activity panel shows, from the caches and stores the
 * App-level streams already keep current (no fetch of its own, no polling):
 *
 *  - the scan: lib/scanActivity.ts (Library's scan stream, or the follower);
 *  - the background check sweep: useCheckSweepStatus (checks stream);
 *  - fingerprinting: useFingerprintPoolStatus (fingerprint-pool stream);
 *  - bulk fixes: lib/bulkRuns.ts;
 *  - AI jobs: useJobs / useQueueState (jobs stream).
 */
import { useMemo } from "react";

import { useCheckSweepStatus } from "./useCheckSweepStatus";
import { useFingerprintPoolStatus } from "./useFingerprintPoolStatus";
import { useJobs, useQueueState } from "./useJobs";
import {
  bulkSummary,
  jobsSummary,
  poolSummary,
  scanSummary,
  sweepSummary,
  toolbarText,
  type Summary,
} from "@/lib/activityModel";
import { useBulkRuns, type BulkRun } from "@/lib/bulkRuns";
import { useScanActivity, type ScanActivity } from "@/lib/scanActivity";
import { useStreamHealth } from "@/lib/streamHealth";
import type { CheckSweepStatus, FingerprintPoolStatus, Job } from "@/lib/types";

export interface Activity {
  scan: { data: ScanActivity; summary: Summary };
  sweep: { data: CheckSweepStatus | undefined; summary: Summary };
  pool: { data: FingerprintPoolStatus | undefined; summary: Summary };
  bulk: { run: BulkRun; summary: Summary }[];
  jobs: {
    all: Job[];
    inProgress: Job[];
    /** Finished jobs, newest first: proposals, failures, applied, discarded. */
    finished: Job[];
    paused: boolean;
    summary: Summary;
    /** The jobs list could not be loaded at all. */
    unavailable: boolean;
  };
  /** The toolbar control's text. */
  toolbar: ReturnType<typeof toolbarText>;
}

const newestFirst = (a: Job, b: Job) => {
  const ka = a.completed_at ?? a.created_at;
  const kb = b.completed_at ?? b.created_at;
  return ka < kb ? 1 : ka > kb ? -1 : 0;
};

export function useActivity(): Activity {
  const scan = useScanActivity();
  const sweepQ = useCheckSweepStatus();
  const poolQ = useFingerprintPoolStatus();
  const jobsQ = useJobs();
  const { data: queue } = useQueueState();
  const runs = useBulkRuns();
  const health = useStreamHealth();

  const sweep = sweepQ.data;
  const pool = poolQ.data;
  const jobs = jobsQ.data;
  const sweepFailed = sweepQ.isError && !sweep;
  const poolFailed = poolQ.isError && !pool;
  const jobsFailed = jobsQ.isError && !jobs;
  const paused = !!queue?.paused;

  return useMemo(() => {
    const all = jobs ?? [];
    const scanS = scanSummary(scan);
    const sweepS = sweepSummary(sweep, { failed: sweepFailed, health: health.checks });
    const poolS = poolSummary(pool, { failed: poolFailed, health: health.fingerprint });
    const bulk = runs.map((run) => ({ run, summary: bulkSummary(run) }));
    const jobsS = jobsFailed
      ? { state: "unavailable" as const, verb: "AI jobs", progress: null, detail: "The job list could not be loaded." }
      : jobsSummary(all, paused, { failed: jobsFailed, health: health.jobs });
    return {
      scan: { data: scan, summary: scanS },
      sweep: { data: sweep, summary: sweepS },
      pool: { data: pool, summary: poolS },
      bulk,
      jobs: {
        all,
        inProgress: all.filter((j) => j.status === "running" || j.status === "queued").sort(newestFirst),
        finished: all.filter((j) => j.status !== "running" && j.status !== "queued").sort(newestFirst),
        paused,
        summary: jobsS,
        unavailable: jobsFailed,
      },
      // Most noticeable first: what the user started, then what runs on its own.
      toolbar: toolbarText([scanS, ...bulk.map((b) => b.summary), poolS, sweepS, jobsS]),
    };
  }, [scan, sweep, pool, runs, jobs, paused, sweepFailed, poolFailed, jobsFailed, health]);
}
