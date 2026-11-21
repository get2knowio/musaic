/**
 * Addresses of the Review page (`pages/Review.tsx`), for links, toasts and
 * redirects. The query string holds the page's state:
 *
 *  - `check=<id>`: one check's open findings;
 *  - `kind=proposed|failed`: proposed changes, or failed jobs;
 *  - `album=<id>`: narrows any of those (or All open) to one album;
 *  - `show=ignored`: ignored findings (beside the inbox, not counted);
 *  - `job=<id>`: a deep link, the job highlighted where it is listed (a job
 *    in progress or finished is in the Activity panel, which opens at it).
 *
 * No `check`, `kind` or `show` is All open. Jobs in progress and job
 * history are not Review's: `activitySearch` (lib/activityPanel.ts) links
 * to them.
 */
import { activitySearch } from "@/lib/activityPanel";

export type ReviewKind = "proposed" | "failed";
export type ReviewShow = "ignored";

export function reviewUrl(
  opts: {
    check?: string | null;
    kind?: ReviewKind | null;
    album?: string | null;
    show?: ReviewShow | null;
    job?: string | null;
  } = {},
): string {
  const p = new URLSearchParams();
  if (opts.check) p.set("check", opts.check);
  else if (opts.kind) p.set("kind", opts.kind);
  else if (opts.show) p.set("show", opts.show);
  if (opts.album) p.set("album", opts.album);
  if (opts.job) p.set("job", opts.job);
  const qs = p.toString();
  return `/review${qs ? `?${qs}` : ""}`;
}

/**
 * What a Review URL's query string shows, in the words its rail uses:
 * a check's label, "Proposed changes", "Failed jobs", "Ignored findings";
 * null for All open (with or without an album filter), which is just
 * "Review". Jobs in progress and history are in Activity, not Review.
 * For return links ("Back to …").
 */
export function reviewViewName(search: string, checkLabel: (id: string) => string): string | null {
  const p = new URLSearchParams(search);
  const show = p.get("show");
  if (show === "ignored") return "Ignored findings";
  const check = p.get("check");
  if (check) return checkLabel(check);
  const kind = p.get("kind");
  if (kind === "proposed") return "Proposed changes";
  if (kind === "failed") return "Failed jobs";
  return null;
}

/**
 * Needs attention's query string, as Review reads it. `check`, `job`,
 * `album` and `ignored` mean the same. `jobs=proposed|failed` (a job list)
 * becomes that type filter; `jobs=running|finished` opens the Activity
 * panel (jobs in progress, job history) over All open; `list=ignored` the
 * Ignored findings view. `view=suggestions|other` (the former Suggestions
 * tab) is All open, which now shows informational findings with the rest.
 */
export function legacyAttentionSearch(search: string): string {
  const old = new URLSearchParams(search);
  const jobs = old.get("jobs");
  if (!old.get("check") && (jobs === "running" || jobs === "finished")) {
    return activitySearch(jobs === "running" ? "jobs" : "history", old.get("job"));
  }
  const p = new URLSearchParams();
  const check = old.get("check");
  if (check) p.set("check", check);
  else if (jobs === "proposed" || jobs === "failed") p.set("kind", jobs);
  else if (old.get("list") === "ignored") p.set("show", "ignored");
  for (const k of ["album", "job", "ignored"]) {
    const v = old.get(k);
    if (v) p.set(k, v);
  }
  return p.toString();
}
