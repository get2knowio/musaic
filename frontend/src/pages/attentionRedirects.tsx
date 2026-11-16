import { Navigate, useLocation, useParams } from "react-router-dom";

/**
 * The old Health routes. `/health/:checkId` lands on Needs attention
 * filtered to that check; a check filter shows all of the check's open
 * findings, informational ones included, so the deep link never lands on
 * an empty list that hides them.
 */
export function HealthRedirect() {
  const { checkId } = useParams<{ checkId?: string }>();
  return <Navigate replace to={attentionUrl({ check: checkId })} />;
}

/**
 * The old Jobs routes: `/jobs`, `/jobs/:jobId` and `/jobs?job=<id>` land on
 * Needs attention with that job shown and highlighted.
 */
export function JobsRedirect() {
  const { jobId } = useParams<{ jobId?: string }>();
  const { search } = useLocation();
  const job = jobId ?? new URLSearchParams(search).get("job") ?? undefined;
  return <Navigate replace to={attentionUrl({ job })} />;
}

export function attentionUrl(opts: { check?: string | null; job?: string | null } = {}): string {
  const p = new URLSearchParams();
  if (opts.check) p.set("check", opts.check);
  if (opts.job) p.set("job", opts.job);
  const qs = p.toString();
  return `/attention${qs ? `?${qs}` : ""}`;
}
