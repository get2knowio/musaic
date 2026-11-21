/**
 * useFingerprintPoolStream — single subscriber to
 * /api/fingerprint-pool/stream.
 *
 * Mounted at the App level. The backend emits a `state` event whenever
 * the pool transitions (idle → running → done/cancelled/unavailable,
 * plus per-album progress while running). We mirror the payload into
 * the FINGERPRINT_POOL_STATUS_KEY cache so `useFingerprintPoolStatus`
 * readers stay current without polling.
 *
 * Replaces the per-component polling that `useFingerprintPoolStatus`
 * used to do — the pool was the last poller in the app.
 */
import { useEffect } from "react";
import { useQueryClient } from "@tanstack/react-query";

import { noteStreamLive, trackEventSource } from "@/lib/streamHealth";

import { FINGERPRINT_POOL_STATUS_KEY } from "./useFingerprintPoolStatus";
import type { FingerprintPoolStatus } from "@/lib/types";

export function useFingerprintPoolStream() {
  const qc = useQueryClient();

  useEffect(() => {
    const es = new EventSource("/api/fingerprint-pool/stream");
    trackEventSource("fingerprint", es);

    es.addEventListener("state", (ev) => {
      noteStreamLive("fingerprint");
      let payload: FingerprintPoolStatus;
      try {
        payload = JSON.parse((ev as MessageEvent).data);
      } catch {
        return;
      }
      qc.setQueryData(FINGERPRINT_POOL_STATUS_KEY, payload);
    });

    es.addEventListener("error", () => {
      // EventSource auto-reconnects; nothing to do.
    });

    return () => es.close();
  }, [qc]);
}
