/**
 * useChecksStream — single subscriber to /api/checks/stream.
 *
 * Mounted at the App level alongside useJobsStream. The backend stream
 * emits a "state" event whenever a check's run state, finding set, or
 * ignored/resolved status changes; the payload is the full check-summary
 * list. We mirror that into the ["checks"] cache and also invalidate
 * ["findings"] so the per-album / per-check finding views refetch.
 *
 * The same stream carries a "sweep" event with the background check
 * sweep's progress (after the first "state", then on every change); it is
 * written into the sweep-status cache the Activity panel reads.
 *
 * The library-events stream also reports a finished rerun
 * (ChecksRerunFinished, see useLibraryEventsStream) and invalidates the
 * same findings caches. This stream stays for what the bus does not carry:
 * a running check's state, ignore / unignore, and the sweep's progress.
 *
 * Why invalidate findings rather than push them in the SSE payload?
 * Findings can grow into the hundreds; the summary already gives us a
 * change signal, and a single invalidation triggers one targeted REST
 * call rather than streaming a potentially-large list on every change.
 */
import { useEffect } from "react";
import { useQueryClient } from "@tanstack/react-query";

import { noteStreamLive, trackEventSource } from "@/lib/streamHealth";

import { CHECKS_QUERY_KEY, FINDINGS_QUERY_KEY } from "./useChecks";
import { CHECK_SWEEP_STATUS_KEY } from "./useCheckSweepStatus";
import type { CheckSummary, CheckSweepStatus } from "@/lib/types";

export function useChecksStream() {
  const qc = useQueryClient();

  useEffect(() => {
    const es = new EventSource("/api/checks/stream");
    trackEventSource("checks", es);

    es.addEventListener("state", (ev) => {
      noteStreamLive("checks");
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

    es.addEventListener("sweep", (ev) => {
      noteStreamLive("checks");
      let payload: CheckSweepStatus;
      try {
        payload = JSON.parse((ev as MessageEvent).data);
      } catch {
        return;
      }
      qc.setQueryData(CHECK_SWEEP_STATUS_KEY, payload);
    });

    es.addEventListener("error", () => {
      // EventSource auto-reconnects; nothing to do.
    });

    return () => es.close();
  }, [qc]);
}
