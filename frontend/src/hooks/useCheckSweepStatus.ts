/**
 * Thin cache reader for the background check sweep's progress.
 *
 * Kept fresh by `useChecksStream` (mounted once at App level), which writes
 * every `sweep` event of /api/checks/stream into this key. The first read
 * comes from GET /api/checks/sweep.
 *
 * `staleTime: Infinity` because the SSE stream is the single source of
 * truth — refetching on focus/mount would race with it.
 */
import { useQuery } from "@tanstack/react-query";

import { getCheckSweepStatus } from "@/lib/api";
import type { CheckSweepStatus } from "@/lib/types";

export const CHECK_SWEEP_STATUS_KEY = ["check-sweep-status"] as const;

export function useCheckSweepStatus() {
  return useQuery<CheckSweepStatus>({
    queryKey: CHECK_SWEEP_STATUS_KEY,
    queryFn: getCheckSweepStatus,
    staleTime: Infinity, // SSE keeps it fresh
    refetchOnWindowFocus: false,
    retry: false,
  });
}
