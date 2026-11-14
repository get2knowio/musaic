/**
 * Remote cover previews are proxied through the backend. The desktop
 * window's CSP allows `img-src 'self' data: blob:` only, so a job's
 * suggested remote cover (Cover Art Archive or any other host) cannot be an
 * `<img src>` directly. The backend serves it same-origin instead: the
 * client names the job, never the URL, and the server fetches the cover URL
 * it stored for that job (`GET /api/ai/jobs/{job_id}/cover-preview`, a
 * route the Python version did not have).
 */
export function jobCoverPreviewUrl(jobId: string): string {
  return `/api/ai/jobs/${encodeURIComponent(jobId)}/cover-preview`;
}
