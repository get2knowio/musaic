/**
 * useJobsStream — single subscriber to /api/ai/jobs/stream.
 *
 * Mounted at the App level so it's always live. Updates the cached jobs
 * query (so all downstream `useJobs()` consumers stay in sync) and fires
 * Sonner toasts on meaningful state transitions:
 *
 *   queued → running   : silent
 *   * → completed      : success toast with "View" → /library/{album_id}
 *   * → failed         : error toast with "View" → /jobs
 *
 * The "enqueue" toast is fired from the click site (in DetailSheet), not
 * here — the SSE event arrives a beat after the enqueue mutation resolves
 * and we want immediate feedback at the click.
 */
import { useEffect, useRef } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { useNavigate } from "react-router-dom";
import { toast } from "sonner";

import { JOBS_QUERY_KEY, QUEUE_STATE_QUERY_KEY } from "./useJobs";
import type { QueueState } from "@/lib/api";
import type { Job } from "@/lib/types";

function jobLabel(j: Job): string {
  switch (j.job_type) {
    case "metadata":       return "Metadata";
    case "cover":          return "Cover art";
    case "metadata+cover": return "Metadata + cover";
  }
}

function shortAlbum(j: Job, maxLen = 36): string {
  const t = j.album_title || "album";
  return t.length > maxLen ? t.slice(0, maxLen - 1) + "…" : t;
}

export function useJobsStream() {
  const qc = useQueryClient();
  const navigate = useNavigate();
  const previousById = useRef<Map<string, Job>>(new Map());

  useEffect(() => {
    const es = new EventSource("/api/ai/jobs/stream");

    es.addEventListener("jobs", (ev) => {
      let payload: { jobs: Job[]; queue_state?: QueueState };
      try {
        payload = JSON.parse((ev as MessageEvent).data);
      } catch {
        return;
      }
      const jobs = payload.jobs ?? [];
      // Queue state ships in the same payload — keep its cache key fresh too,
      // so the Jobs page banner / sidebar indicator update without a separate
      // poll. Backend always emits queue_state; guard for older snapshots.
      if (payload.queue_state) {
        qc.setQueryData(QUEUE_STATE_QUERY_KEY, payload.queue_state);
      }

      // Diff against previous snapshot to surface meaningful transitions.
      const prevMap = previousById.current;
      const nextMap = new Map(jobs.map((j) => [j.id, j]));
      for (const j of jobs) {
        const prev = prevMap.get(j.id);
        if (!prev) continue;  // brand-new jobs are announced by the click-site toast
        if (prev.status === j.status) continue;

        // Defer navigation past Sonner's click handler / any open Radix
        // Dialog's focus trap. Without the setTimeout, the click on the
        // toast's action button can be swallowed by the dialog overlay
        // when the sheet is currently open.
        const goTo = (path: string) => () => {
          // eslint-disable-next-line no-console
          console.debug("[toast] navigate →", path);
          setTimeout(() => navigate(path), 0);
        };

        if (j.status === "completed") {
          toast.success(`${jobLabel(j)} ready for ${shortAlbum(j)}`, {
            action: {
              label: "View",
              onClick: goTo(`/library/${encodeURIComponent(j.album_id)}`),
            },
            duration: 5000,
          });
        } else if (j.status === "failed") {
          toast.error(`${jobLabel(j)} failed for ${shortAlbum(j)}`, {
            action: {
              label: "View",
              onClick: goTo("/jobs"),
            },
            duration: 6000,
          });
        } else if (
          j.status === "acted_on"
          && j.acted_on_action === "applied"
          && prev.status === "running"
        ) {
          // Server-side auto-acknowledged a no-op confirmation: MB
          // identified the album, nothing to apply. Distinguishable
          // from the user-driven "completed → Apply → acted_on"
          // path by prev.status (auto-ack jumps straight from
          // running). Quiet success toast — there's nothing for the
          // user to decide, just a heads-up that the album's now
          // identified.
          toast.success(`${shortAlbum(j)}: identified on MusicBrainz`, {
            action: {
              label: "View",
              onClick: goTo(`/library/${encodeURIComponent(j.album_id)}`),
            },
            duration: 4000,
          });
        }
      }
      previousById.current = nextMap;

      qc.setQueryData(JOBS_QUERY_KEY, jobs);
    });

    es.addEventListener("error", () => {
      // EventSource auto-reconnects on its own. Don't toast on every blip.
    });

    return () => es.close();
  }, [qc, navigate]);
}
