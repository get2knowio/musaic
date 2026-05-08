/**
 * useChecksStream — single subscriber to /api/checks/stream.
 *
 * Mounted at the App level alongside useJobsStream. The backend stream
 * emits a "state" event whenever a check's run state, finding set, or
 * ignored/resolved status changes; the payload is the full check-summary
 * list. We mirror that into the ["checks"] cache and also invalidate
 * ["findings"] so the per-album / per-check finding views refetch.
 *
 * Why invalidate findings rather than push them in the SSE payload?
 * Findings can grow into the hundreds; the summary already gives us a
 * change signal, and a single invalidation triggers one targeted REST
 * call rather than streaming a potentially-large list on every change.
 */
import { useEffect } from "react";
import { useQueryClient } from "@tanstack/react-query";

import { CHECKS_QUERY_KEY, FINDINGS_QUERY_KEY } from "./useChecks";
import type { CheckSummary } from "@/lib/types";

export function useChecksStream() {
  const qc = useQueryClient();

  useEffect(() => {
    const es = new EventSource("/api/checks/stream");

    es.addEventListener("state", (ev) => {
      let payload: { checks: CheckSummary[] };
      try {
        payload = JSON.parse((ev as MessageEvent).data);
      } catch {
        return;
      }
      qc.setQueryData(CHECKS_QUERY_KEY, payload.checks ?? []);
      // Findings may have changed; invalidate so consumers refetch lazily.
      qc.invalidateQueries({ queryKey: FINDINGS_QUERY_KEY });
      // The Health detail page reads findings off the per-check detail
      // response (CHECK_DETAIL_KEY), not the global findings cache —
      // invalidate that too so an open detail page refreshes when a
      // finding gets fixed/ignored from a dialog elsewhere.
      qc.invalidateQueries({ queryKey: ["check"] });
    });

    es.addEventListener("error", () => {
      // EventSource auto-reconnects; nothing to do.
    });

    return () => es.close();
  }, [qc]);
}
