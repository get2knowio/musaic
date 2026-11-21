import { Navigate, useLocation, useParams } from "react-router-dom";

import { activitySearch } from "@/lib/activityPanel";
import { legacyAttentionSearch, reviewUrl } from "@/lib/reviewLinks";

/**
 * Old addresses of what is now Review. Each lands on the unified inbox
 * with the nearest filter, so a bookmark, a toast's link or a link in an
 * older build never ends on an empty or missing page.
 *
 *  - `/health`, `/health/:checkId` (the old Health page): the check filter;
 *  - `/jobs` (the old Jobs page): the Activity panel's jobs, over Review;
 *  - `/jobs/:jobId`, `/jobs?job=<id>`: the job, highlighted where it now
 *    lives (Review, or the Activity panel);
 *  - `/attention?…` (Needs attention): `legacyAttentionSearch`.
 */
export function HealthRedirect() {
  const { checkId } = useParams<{ checkId?: string }>();
  return <Navigate replace to={reviewUrl({ check: checkId })} />;
}

export function JobsRedirect() {
  const { jobId } = useParams<{ jobId?: string }>();
  const { search } = useLocation();
  const job = jobId ?? new URLSearchParams(search).get("job") ?? undefined;
  // One job: Review lists it if it asks something of the user, else opens
  // Activity at it. The job list itself is Activity's.
  return <Navigate replace to={job ? reviewUrl({ job }) : `/review?${activitySearch("jobs")}`} />;
}

export function AttentionRedirect() {
  const { search } = useLocation();
  const qs = legacyAttentionSearch(search);
  return <Navigate replace to={`/review${qs ? `?${qs}` : ""}`} />;
}
