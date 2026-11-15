/**
 * Turning an error response body's `detail` into text a person can read.
 *
 * FastAPI-style bodies carry `detail` in three shapes: a string (most
 * refusals), an object (the job-queue conflict:
 * `{error, message, existing_job_id, existing_status}`), or a list of
 * validation errors (`[{loc, msg, type}, ...]`, the 422 shape). Showing
 * only the string form left the other two as a bare "409 Conflict".
 */

/** The readable text in a `detail` value, or null when it carries none. */
export function errorDetailText(detail: unknown): string | null {
  if (typeof detail === "string") {
    return detail.trim() ? detail : null;
  }
  if (Array.isArray(detail)) {
    const parts = detail
      .map((d) => {
        if (typeof d === "string") return d;
        if (d && typeof d === "object" && typeof (d as { msg?: unknown }).msg === "string") {
          return (d as { msg: string }).msg;
        }
        return null;
      })
      .filter((s): s is string => !!s && s.trim() !== "");
    return parts.length ? parts.join("; ") : null;
  }
  if (detail && typeof detail === "object") {
    const o = detail as Record<string, unknown>;
    for (const key of ["message", "detail", "error"]) {
      const v = o[key];
      if (typeof v === "string" && v.trim()) return v;
    }
  }
  return null;
}

/** The message of an error a request threw, or `fallback` when it has none. */
export function errorText(err: unknown, fallback: string): string {
  const m = err instanceof Error ? err.message : typeof err === "string" ? err : "";
  return m && m.trim() ? m : fallback;
}
