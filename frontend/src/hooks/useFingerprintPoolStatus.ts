/**
 * Thin cache reader for the fingerprint-pool state.
 *
 * The cache is populated + kept fresh by `useFingerprintPoolStream`
 * (mounted once at App level). This hook just reads the same cache key,
 * so any component that wants the current pool state can call it
 * without triggering its own fetch loop.
 *
 * `staleTime: Infinity` because the SSE stream is the single source
 * of truth — refetching on focus/mount would race with it.
 */
import { useQuery } from "@tanstack/react-query";

import { getFingerprintPoolStatus } from "@/lib/api";
import type { FingerprintPoolStatus } from "@/lib/types";

export const FINGERPRINT_POOL_STATUS_KEY = ["fingerprint-pool-status"] as const;

export function useFingerprintPoolStatus() {
  return useQuery<FingerprintPoolStatus>({
    queryKey: FINGERPRINT_POOL_STATUS_KEY,
    queryFn: getFingerprintPoolStatus,
    staleTime: Infinity, // SSE keeps it fresh
    refetchOnWindowFocus: false,
    retry: false,
  });
}
