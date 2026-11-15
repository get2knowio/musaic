/**
 * Remote cover previews are proxied through the backend. The desktop
 * window's CSP allows `img-src 'self' data: blob:` only, so a proposed
 * remote cover (Cover Art Archive or any other host) cannot be an
 * `<img src>` directly. The backend serves it same-origin instead: the
 * client names a job or an album, never the URL, and the server fetches the
 * cover URL it stored for that job or derives for that album
 * (`rust/crates/musaic-api/src/routes/preview.rs`; routes the Python
 * version did not have).
 */

/** The cover a finished job proposes (`GET /api/ai/jobs/{id}/cover-preview`). */
export function jobCoverPreviewUrl(jobId: string): string {
  return `/api/ai/jobs/${encodeURIComponent(jobId)}/cover-preview`;
}

/** The cover the resolver proposes for an album, with no job
 *  (`GET /api/albums/{id}/cover-preview`). */
export function albumCoverPreviewUrl(albumId: string): string {
  return `/api/albums/${encodeURIComponent(albumId)}/cover-preview`;
}

/**
 * What a preview request answered:
 *
 * - `ok`: the image, as a `data:` URL (allowed by the CSP, and nothing to
 *   revoke when the row goes away).
 * - `none`: 404 — nothing to preview (no anchor, no front cover, or the
 *   job has no cover).
 * - `missing`: 409 — the album is Missing; its cover cannot change.
 * - `unavailable`: 502 or any other failure — the fetch was refused or
 *   failed (always so offline).
 */
export type CoverPreview =
  | { status: "ok"; dataUrl: string }
  | { status: "none" }
  | { status: "missing" }
  | { status: "unavailable" };

export async function fetchCoverPreview(src: string, signal?: AbortSignal): Promise<CoverPreview> {
  let res: Response;
  try {
    res = await fetch(src, { signal });
  } catch {
    return { status: "unavailable" };
  }
  if (res.status === 404) return { status: "none" };
  if (res.status === 409) return { status: "missing" };
  if (!res.ok) return { status: "unavailable" };
  try {
    // The server answers only allow-listed raster types; anything else is
    // not shown.
    const type = (res.headers.get("content-type") ?? "").split(";")[0].trim();
    if (!type.startsWith("image/")) return { status: "unavailable" };
    return { status: "ok", dataUrl: dataUrl(type, new Uint8Array(await res.arrayBuffer())) };
  } catch {
    return { status: "unavailable" };
  }
}

function dataUrl(type: string, bytes: Uint8Array): string {
  let binary = "";
  // Chunked: String.fromCharCode takes its bytes as arguments.
  for (let i = 0; i < bytes.length; i += 0x8000) {
    binary += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  }
  return `data:${type};base64,${btoa(binary)}`;
}
