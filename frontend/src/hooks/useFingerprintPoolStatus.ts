/**
 * Polls /api/fingerprint-pool/status while the pool is running, idles
 * otherwise.
 *
 * The pool is fire-and-forget on the backend (kicked off by the scan
 * service after every successful scan). The user sees its progress in
 * the titlebar pill; once it transitions out of "running" the pill
 * unmounts and we stop polling. No SSE — the data is small and
 * polling at 2s while running is fine.
 */
import { useQuery } from "@tanstack/react-query";

import { getFingerprintPoolStatus } from "@/lib/api";
import type { FingerprintPoolStatus } from "@/lib/types";

export const FINGERPRINT_POOL_STATUS_KEY = ["fingerprint-pool-status"] as const;

export function useFingerprintPoolStatus() {
  return useQuery<FingerprintPoolStatus>({
    queryKey: FINGERPRINT_POOL_STATUS_KEY,
    queryFn: getFingerprintPoolStatus,
    // 30s baseline poll catches the pool flipping into "running" after
    // a fresh scan; bumps to 2s while we know it's running so the
    // counter feels live. refetchInterval can return false (off) or
    // a number (ms).
    refetchInterval: (query) => {
      const data = query.state.data as FingerprintPoolStatus | undefined;
      return data?.status === "running" ? 2_000 : 30_000;
    },
    refetchOnWindowFocus: false,
    retry: false,
  });
}
