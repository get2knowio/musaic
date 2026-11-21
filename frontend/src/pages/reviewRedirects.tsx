import { Navigate, useLocation, useParams } from "react-router-dom";

import { legacyAttentionSearch, reviewUrl } from "@/lib/reviewLinks";

/**
 * Old addresses of what is now Review. Each lands on the unified inbox
 * with the nearest filter, so a bookmark, a toast's link or a link in an
 * older build never ends on an empty or missing page.
 *
 *  - `/health`, `/health/:checkId` (the old Health page): the check filter;
 *  - `/jobs`, `/jobs/:jobId`, `/jobs?job=<id>` (the old Jobs page): the job,
 *    highlighted where it now lives;
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
  return <Navigate replace to={reviewUrl({ job })} />;
}

export function AttentionRedirect() {
  const { search } = useLocation();
  const qs = legacyAttentionSearch(search);
  return <Navigate replace to={`/review${qs ? `?${qs}` : ""}`} />;
}
