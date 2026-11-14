/**
 * Typed fetch wrappers for the FastAPI backend. All requests go through
 * the Vite dev proxy (or hit the same origin in production).
 */

import type {
  Album,
  AlbumListResponse,
  AlbumResolution,
  AppConfig,
  FingerprintPoolStatus,
  Job,
  JobStatus,
  JobType,
  TracksResponse,
} from "./types";
import { errorDetailText } from "./errors";

class ApiError extends Error {
  status: number;
  /** The body's `detail` as sent (string, object or list), for callers
   *  that branch on a structured conflict such as `active_job_exists`. */
  detail: unknown;
  constructor(message: string, status: number, detail?: unknown) {
    super(message);
    this.status = status;
    this.detail = detail;
    this.name = "ApiError";
  }
}

function bodyDetail(body: unknown): unknown {
  return body && typeof body === "object" && "detail" in body
    ? (body as { detail?: unknown }).detail
    : undefined;
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const res = await fetch(path, {
    headers: { "Content-Type": "application/json", ...(init?.headers ?? {}) },
    ...init,
  });
  let body: unknown = null;
  const contentType = res.headers.get("content-type") ?? "";
  if (contentType.includes("application/json")) {
    try {
      body = await res.json();
    } catch {
      /* empty body */
    }
  }
  if (!res.ok) {
    const raw = bodyDetail(body);
    const detail = errorDetailText(raw) ?? `${res.status} ${res.statusText}`;
    throw new ApiError(detail, res.status, raw);
  }
  return body as T;
}

// --- Config ---

export function getConfig(): Promise<AppConfig> {
  return request<AppConfig>("/api/config");
}

export function setLibraryDir(libraryDir: string): Promise<AppConfig> {
  return request<AppConfig>("/api/config", {
    method: "POST",
    body: JSON.stringify({ library_dir: libraryDir }),
  });
}

// --- Albums ---

export function listAlbums(opts?: { refresh?: boolean }): Promise<AlbumListResponse> {
  const qs = opts?.refresh ? "?refresh=true" : "";
  return request<AlbumListResponse>(`/api/albums${qs}`);
}

export function getAlbum(id: string): Promise<Album> {
  return request<Album>(`/api/albums/${encodeURIComponent(id)}`);
}

export function getTracks(id: string): Promise<TracksResponse> {
  return request<TracksResponse>(`/api/albums/${encodeURIComponent(id)}/tracks`);
}

export function getAlbumResolution(
  id: string,
  opts?: { force?: boolean; hint?: string },
): Promise<AlbumResolution> {
  const qs = new URLSearchParams();
  if (opts?.force) qs.set("force", "true");
  if (opts?.hint) qs.set("hint", opts.hint);
  const suffix = qs.toString() ? `?${qs.toString()}` : "";
  return request<AlbumResolution>(
    `/api/albums/${encodeURIComponent(id)}/resolution${suffix}`,
  );
}

/**
 * Explicit user-initiated removal of an album from the cache. Drops
 * the row + cascade-deletes its fingerprints, track tags, and
 * per-album MB blocklists. The only post-v15 path that reaches
 * `cache.delete_album` — scans never get here, they mark albums
 * Missing instead. Intended use is the "Remove from library" button
 * on Missing albums.
 *
 * 404 when the album_id isn't in the cache (already removed,
 * never cached, or wrong id) — callers should treat this as
 * "already gone" rather than an error.
 */
export function removeAlbumFromLibrary(
  id: string,
): Promise<{ ok: boolean; deleted_album_id: string }> {
  return request(
    `/api/albums/${encodeURIComponent(id)}`,
    { method: "DELETE" },
  );
}


/**
 * Wipe the cached MusicBrainz anchor for an album AND blocklist the
 * rejected release-group so the resolver's fingerprint vote / dir
 * search / AI persist all skip it on the next run. Returns the MBID
 * that was just blocklisted (null if the album wasn't anchored to
 * anything).
 *
 * Resolver re-derivation happens lazily on the next
 * `['album-resolution', id]` refetch (triggered by the AlbumTagsChanged
 * event this endpoint publishes). Concentrating the resolver call
 * there avoids the double-publish + concurrent-rerun race where both
 * this endpoint and the resolver's side-effect fired for one click.
 */
export function clearAlbumMbAnchor(
  id: string,
): Promise<{ ok: boolean; blocked_mbid: string | null }> {
  return request(
    `/api/albums/${encodeURIComponent(id)}/mb-anchor/clear`,
    { method: "POST" },
  );
}

// --- Scan ---

export function rescanLibrary(): Promise<{ status: string; library_dir: string }> {
  return request("/api/scan/rescan", { method: "POST" });
}

export function getFingerprintPoolStatus(): Promise<FingerprintPoolStatus> {
  return request<FingerprintPoolStatus>("/api/fingerprint-pool/status");
}

// --- AI Jobs ---
//
// The original `suggestMetadata` / `suggestCover` wrappers (POST /api/ai/
// suggest-metadata, POST /api/ai/suggest-cover) were removed when the
// sync endpoints they wrapped were deleted server-side. All AI-assisted
// suggestion flows now go through the job queue (`enqueueJob` + the SSE
// stream + the per-album active-job hooks). The MetadataSuggestionResponse
// / CoverSuggestionResponse types are still exported from `types.ts`
// because the worker emits the same shapes inside JobResult.

export function enqueueJob(
  args: {
    albumId: string;
    jobType: JobType;
    hint?: string;
    // v16: opt-out from the cover-attach path on suggest_metadata.
    // Defaults true on the backend; pass false from the hint
    // dialog's "Include cover art" unchecked state.
    attachCover?: boolean;
  },
): Promise<Job> {
  return request("/api/ai/jobs", {
    method: "POST",
    body: JSON.stringify({
      album_id: args.albumId,
      job_type: args.jobType,
      hint: args.hint ?? "",
      attach_cover: args.attachCover ?? true,
    }),
  });
}

export function listJobs(statuses?: JobStatus[]): Promise<{ jobs: Job[] }> {
  const qs = statuses?.length ? `?status=${statuses.join(",")}` : "";
  return request(`/api/ai/jobs${qs}`);
}

export function getActiveJobForAlbum(albumId: string): Promise<Job | null> {
  return request(`/api/ai/jobs/active/${encodeURIComponent(albumId)}`);
}

export function markJobActedOn(jobId: string, action: "applied" | "discarded"): Promise<Job> {
  return request(`/api/ai/jobs/${encodeURIComponent(jobId)}/acted-on`, {
    method: "POST",
    body: JSON.stringify({ action }),
  });
}

export function deleteJob(jobId: string): Promise<{ deleted: boolean; id: string }> {
  return request(`/api/ai/jobs/${encodeURIComponent(jobId)}`, { method: "DELETE" });
}

export function bulkDeleteJobs(statuses: JobStatus[]): Promise<{ deleted_count: number }> {
  return request(`/api/ai/jobs?status=${statuses.join(",")}`, { method: "DELETE" });
}

export function retryJob(jobId: string): Promise<Job> {
  return request(`/api/ai/jobs/${encodeURIComponent(jobId)}/retry`, { method: "POST" });
}

export function resumeQueue(): Promise<{ queue_state: QueueState }> {
  return request(`/api/ai/queue/resume`, { method: "POST" });
}

// --- Settings ---

export interface AppSettings {
  ai_max_parallel: number;
  ai_queue_paused: boolean;
  /** Selected airframe provider id (e.g. "claude"). Defaults to "claude". */
  ai_provider_id: string;
  /** Vendor model id passed to the selected provider. */
  ai_model_id: string;
  /** Whether a provider API key is configured. Write-only, like the other
   *  keys — the value itself is never returned by GET /api/settings. */
  ai_api_key_set: boolean;
  /** Whether the AcoustID API key is configured. The actual key is
   *  write-only — never returned by GET /api/settings. */
  acoustid_api_key_set: boolean;
  /** False when the system keychain that holds the API keys can't be
   *  reached: keys then read as not set and saving one fails. Absent from
   *  backends that don't report it. */
  keyring_available?: boolean;
  /** True while API keys from an earlier version are still present in the
   *  database file (or its backup, or an older snapshot) and removal is
   *  being retried. Absent
   *  from backends that don't report it. */
  keys_pending_removal?: boolean;
  /** Whether the FireCrawl API key is configured. Same write-only
   *  treatment as AcoustID. When set, ai.py mounts the FireCrawl MCP
   *  server into Claude Agent SDK calls as a last-resort web fallback. */
  firecrawl_api_key_set: boolean;
  /** How many automatic DB snapshots to keep (0 disables snapshotting). */
  snapshot_retention_count: number;
  /** Minimum hours between automatic snapshots; 0 = always snapshot on
   *  app start. */
  snapshot_min_interval_hours: number;
  /** Filename template applied to single-artist albums. */
  filename_template_single_artist: string;
  /** Filename template applied to compilation albums. */
  filename_template_compilation: string;
  /** Album-directory template for single-artist albums. */
  dir_template_single_artist: string;
  /** Album-directory template for compilations. */
  dir_template_compilation: string;
}

/** PATCH-time payload. *_api_key fields are write-only and only included
 *  when the user is changing them (empty string clears, omitted = no change). */
export interface AppSettingsPatch {
  ai_max_parallel?: number;
  ai_queue_paused?: boolean;
  /** airframe provider id. Empty string resets to the default ("claude");
   *  a non-empty value must be an installed provider or the API 400s. */
  ai_provider_id?: string;
  ai_model_id?: string;
  /** Provider API key (write-only). Empty string clears it. */
  ai_api_key?: string;
  acoustid_api_key?: string;
  firecrawl_api_key?: string;
  snapshot_retention_count?: number;
  snapshot_min_interval_hours?: number;
  filename_template_single_artist?: string;
  filename_template_compilation?: string;
  dir_template_single_artist?: string;
  dir_template_compilation?: string;
}

export interface QueueState {
  paused: boolean;
  max_parallel: number;
  running: number;
  queued: number;
}

export function getSettings(): Promise<AppSettings> {
  return request("/api/settings");
}

/** One installed, selectable airframe provider. */
export interface AiProvider {
  id: string;
  /** True if the provider takes a pasted API key; false if it authenticates
   *  via its own env / CLI chain (in which case `auth_hint` explains how). */
  accepts_api_key: boolean;
  auth_hint: string | null;
  /** Whether the provider's external prerequisite (a CLI it shells out to,
   *  e.g. `claude` / `gh`) is present. False → AI calls will fail until the
   *  user installs it or switches providers; deterministic matching still
   *  works. Key/SDK-chain providers are always `true` here. */
  ready: boolean;
  /** What's required, e.g. "the `claude` CLI". Null when ready. */
  requirement: string | null;
  /** Actionable fix when not ready. Null when ready. */
  remedy: string | null;
}

export interface AiProvidersResponse {
  selected: { provider_id: string; model_id: string; api_key_set: boolean };
  /** Installed (selectable) providers. */
  providers: AiProvider[];
  /** Known-but-not-installed provider ids (add via `pip install
   *  airframe-agents[<extra>]`). Informational only. */
  uninstalled: string[];
}

export function getAiProviders(): Promise<AiProvidersResponse> {
  return request("/api/ai/providers");
}

export function patchSettings(patch: AppSettingsPatch): Promise<AppSettings> {
  return request("/api/settings", {
    method: "PATCH",
    body: JSON.stringify(patch),
  });
}

// --- Health checks ---

import type {
  CheckSummary,
  CheckDetail,
  Finding,
  FixFindingResult,
  CheckRun,
} from "./types";

export function listChecks(): Promise<{ checks: CheckSummary[] }> {
  return request("/api/checks");
}

export function getCheck(checkId: string): Promise<CheckDetail> {
  return request(`/api/checks/${encodeURIComponent(checkId)}`);
}

export function listFindings(opts?: {
  albumId?: string;
  checkId?: string;
  includeIgnored?: boolean;
  includeResolved?: boolean;
}): Promise<{ findings: Finding[] }> {
  const qs = new URLSearchParams();
  if (opts?.albumId) qs.set("album_id", opts.albumId);
  if (opts?.checkId) qs.set("check_id", opts.checkId);
  if (opts?.includeIgnored) qs.set("include_ignored", "true");
  if (opts?.includeResolved) qs.set("include_resolved", "true");
  const tail = qs.toString();
  return request(`/api/checks/findings${tail ? "?" + tail : ""}`);
}

export function runCheck(
  checkId: string,
  args: { scope: "all" | "incremental" | "albums"; albumIds?: string[] } = { scope: "all" },
): Promise<CheckRun> {
  return request(`/api/checks/${encodeURIComponent(checkId)}/run`, {
    method: "POST",
    body: JSON.stringify({ scope: args.scope, album_ids: args.albumIds }),
  });
}

export function ignoreFinding(checkId: string, findingId: number, reason?: string): Promise<Finding> {
  return request(
    `/api/checks/${encodeURIComponent(checkId)}/findings/${findingId}/ignore`,
    { method: "POST", body: JSON.stringify({ reason: reason ?? null }) },
  );
}

export function unignoreFinding(checkId: string, findingId: number): Promise<Finding> {
  return request(
    `/api/checks/${encodeURIComponent(checkId)}/findings/${findingId}/unignore`,
    { method: "POST" },
  );
}

export function fixFinding(
  checkId: string,
  findingId: number,
  hint: string = "",
): Promise<FixFindingResult> {
  // Empty hint omits the body entirely so the existing /fix consumers
  // (no-body POSTs) keep their request shape — server treats absent
  // payload identically to {hint: ""}.
  const init: RequestInit = { method: "POST" };
  if (hint) {
    init.body = JSON.stringify({ hint });
  }
  return request(
    `/api/checks/${encodeURIComponent(checkId)}/findings/${findingId}/fix`,
    init,
  );
}

/** Custom action for the duplicate-tracks check: delete one of the
 *  duplicates the user picked. Server validates the filename is among
 *  the finding's listed duplicates before unlinking. */
export function deleteDuplicateFile(
  findingId: number,
  filename: string,
): Promise<{ ok: boolean; message: string }> {
  return request(
    `/api/checks/duplicate-tracks/findings/${findingId}/delete-file`,
    {
      method: "POST",
      body: JSON.stringify({ filename }),
    },
  );
}

/** Bulk-apply a sort value across every album in an
 *  inconsistent-artist-sort cluster. Server validates that the
 *  finding belongs to that check, then writes ALBUMARTISTSORT into
 *  the FLAC tags of every cluster member via the apply_direct path. */
export function applyInconsistentArtistSort(
  findingId: number,
  sortValue: string,
): Promise<{
  ok: boolean;
  applied: number;
  cluster_size: number;
  sort_value: string;
  message: string;
}> {
  return request(
    `/api/checks/inconsistent-artist-sort/findings/${findingId}/apply-sort`,
    {
      method: "POST",
      body: JSON.stringify({ sort_value: sortValue }),
    },
  );
}


/** What MusicBrainz says the cluster's recording_mbid actually is.
 *  Title + artist are null when no MBID was attached, MB returned 404,
 *  or the request failed — `reason` carries the human explanation. */
export interface CanonicalRecording {
  title: string | null;
  artist: string | null;
  mbid: string | null;
  reason: string | null;
}

/** Lazy MB lookup for a duplicate-tracks finding. Backend caches in-process
 *  (recordings are immutable) so repeat opens cost one HTTP call. */
export function getDuplicateFindingCanonical(
  findingId: number,
): Promise<CanonicalRecording> {
  return request(
    `/api/checks/duplicate-tracks/findings/${findingId}/canonical`,
  );
}

/** Custom action for the filename-mismatch check: rename files
 *  according to the user-confirmed plan. Two-pass rename, track-tag
 *  migration, post-rename rescan all happen server-side. */
import type {
  FilenameMismatchPlanPreview,
  FilenameRenameResult,
} from "./types";

export function applyFilenameRename(
  findingId: number,
  args: {
    plan: { old: string; new: string }[];
    dirRename?: { old: string; new: string };
  },
): Promise<FilenameRenameResult> {
  return request(
    `/api/checks/filename-mismatch/findings/${findingId}/apply`,
    {
      method: "POST",
      body: JSON.stringify({
        plan: args.plan,
        dir_rename: args.dirRename ?? null,
      }),
    },
  );
}

/** Re-derive the rename plan for a finding against current disk + DB
 *  state. The Resolve dialog calls this on open so it never shows a
 *  stale snapshot from when the check ran. */
export function fetchFilenameRenamePreview(
  findingId: number,
): Promise<FilenameMismatchPlanPreview> {
  return request(
    `/api/checks/filename-mismatch/findings/${findingId}/plan-preview`,
  );
}

/** Re-derive the duplicate cluster for a finding against current
 *  track_fingerprints. Mirrors fetchFilenameRenamePreview — the
 *  Resolve dialog calls this on open + after every successful delete
 *  so the file list never lags behind disk state. */
import type { DuplicateClusterPreview } from "./types";

export function fetchDuplicateClusterPreview(
  findingId: number,
): Promise<DuplicateClusterPreview> {
  return request(
    `/api/checks/duplicate-tracks/findings/${findingId}/cluster-preview`,
  );
}

// --- User-defined track tags (v9) ---

import type { AlbumTagsResponse, TagListResponse } from "./types";

/** Get the per-track tags + album-level intersection for an album. */
export function getAlbumTags(albumId: string): Promise<AlbumTagsResponse> {
  return request(`/api/albums/${encodeURIComponent(albumId)}/tags`);
}

/** Apply `tag` to every track on the album. Server fans out across
 *  every FLAC in the album dir when `filenames` is omitted. */
export function addAlbumTag(
  albumId: string,
  tag: string,
  filenames?: string[],
): Promise<AlbumTagsResponse> {
  return request(`/api/albums/${encodeURIComponent(albumId)}/tags`, {
    method: "POST",
    body: JSON.stringify(filenames ? { tag, filenames } : { tag }),
  });
}

/** Strip `tag` from every track on the album. */
export function removeAlbumTag(
  albumId: string,
  tag: string,
): Promise<AlbumTagsResponse> {
  return request(
    `/api/albums/${encodeURIComponent(albumId)}/tags/${encodeURIComponent(tag)}`,
    { method: "DELETE" },
  );
}

/** Add a tag to a single track. */
export function addTrackTag(
  albumId: string,
  filename: string,
  tag: string,
): Promise<AlbumTagsResponse> {
  return request(
    `/api/albums/${encodeURIComponent(albumId)}/tracks/${encodeURIComponent(filename)}/tags`,
    { method: "POST", body: JSON.stringify({ tag }) },
  );
}

/** Remove a tag from a single track. */
export function removeTrackTag(
  albumId: string,
  filename: string,
  tag: string,
): Promise<AlbumTagsResponse> {
  return request(
    `/api/albums/${encodeURIComponent(albumId)}/tracks/${encodeURIComponent(filename)}/tags/${encodeURIComponent(tag)}`,
    { method: "DELETE" },
  );
}

/** All tags the user has ever applied, with usage counts (autocomplete). */
export function listAllTags(): Promise<TagListResponse> {
  return request("/api/tags");
}

// --- Tag vocabulary (v10) ---

import type { VocabularyResponse } from "./types";

export function getVocabulary(): Promise<VocabularyResponse> {
  return request("/api/vocabulary");
}

export function addVocabularyEntry(args: {
  tag: string;
  description?: string;
  color?: string | null;
  sortOrder?: number;
}): Promise<VocabularyResponse> {
  return request("/api/vocabulary", {
    method: "POST",
    body: JSON.stringify({
      tag: args.tag,
      description: args.description ?? "",
      color: args.color ?? null,
      sort_order: args.sortOrder ?? null,
    }),
  });
}

export function patchVocabularyEntry(
  tag: string,
  patch: { description?: string; color?: string | null; sortOrder?: number },
): Promise<VocabularyResponse> {
  return request(`/api/vocabulary/${encodeURIComponent(tag)}`, {
    method: "PATCH",
    body: JSON.stringify({
      description: patch.description ?? null,
      color: patch.color ?? null,
      sort_order: patch.sortOrder ?? null,
    }),
  });
}

export function renameVocabularyEntry(
  tag: string,
  newTag: string,
): Promise<VocabularyResponse> {
  return request(`/api/vocabulary/${encodeURIComponent(tag)}/rename`, {
    method: "POST",
    body: JSON.stringify({ new_tag: newTag }),
  });
}

export function mergeVocabularyEntry(
  tag: string,
  intoTag: string,
): Promise<VocabularyResponse> {
  return request(`/api/vocabulary/${encodeURIComponent(tag)}/merge`, {
    method: "POST",
    body: JSON.stringify({ into_tag: intoTag }),
  });
}

export function deleteVocabularyEntry(
  tag: string,
  opts?: { stripFromTracks?: boolean },
): Promise<VocabularyResponse> {
  const qs = opts?.stripFromTracks ? "?strip_from_tracks=true" : "";
  return request(`/api/vocabulary/${encodeURIComponent(tag)}${qs}`, {
    method: "DELETE",
  });
}

export function promoteOrphanTag(tag: string): Promise<VocabularyResponse> {
  return request(
    `/api/vocabulary/orphans/${encodeURIComponent(tag)}/promote`,
    { method: "POST" },
  );
}

// --- DB backup + snapshots ---

import type {
  DbInfo,
  DbRestoreResponse,
  DbSnapshotCreateResponse,
  DbSnapshotListResponse,
} from "./types";

export function getDbInfo(): Promise<DbInfo> {
  return request("/api/db/info");
}

/** Trigger a download of a fresh DB snapshot. Bypasses the JSON
 *  request/response loop because the response is the raw .db file.
 *  Resolves once the browser has accepted the download. */
export async function downloadDbBackup(): Promise<void> {
  const res = await fetch("/api/db/backup");
  if (!res.ok) {
    let detail = `${res.status} ${res.statusText}`;
    try {
      const body = await res.json();
      detail = errorDetailText(bodyDetail(body)) ?? detail;
    } catch {
      /* keep default detail */
    }
    throw new ApiError(detail, res.status);
  }
  // Honor the server's Content-Disposition filename if present;
  // otherwise build a sensible default.
  const cd = res.headers.get("Content-Disposition") ?? "";
  const m = cd.match(/filename="?([^";]+)"?/);
  const filename = m?.[1] ?? `musaic-${new Date().toISOString().replace(/[:.]/g, "-")}.db`;
  const blob = await res.blob();
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = filename;
  document.body.appendChild(a);
  a.click();
  a.remove();
  URL.revokeObjectURL(url);
}

/** Upload a .db file to replace the live DB. Caller is responsible
 *  for confirmation prompts. */
export async function restoreDbFromFile(file: File): Promise<DbRestoreResponse> {
  const fd = new FormData();
  fd.append("file", file);
  const res = await fetch("/api/db/restore", { method: "POST", body: fd });
  let body: unknown = null;
  try {
    body = await res.json();
  } catch {
    /* empty body */
  }
  if (!res.ok) {
    const raw = bodyDetail(body);
    const detail = errorDetailText(raw) ?? `${res.status} ${res.statusText}`;
    throw new ApiError(detail, res.status, raw);
  }
  return body as DbRestoreResponse;
}

export function listDbSnapshots(): Promise<DbSnapshotListResponse> {
  return request("/api/db/snapshots");
}

export function createDbSnapshot(): Promise<DbSnapshotCreateResponse> {
  return request("/api/db/snapshots", { method: "POST" });
}

export function deleteDbSnapshot(name: string): Promise<{ deleted: boolean; name: string }> {
  return request(`/api/db/snapshots/${encodeURIComponent(name)}`, {
    method: "DELETE",
  });
}

export function restoreDbSnapshot(name: string): Promise<DbRestoreResponse> {
  return request(`/api/db/snapshots/${encodeURIComponent(name)}/restore`, {
    method: "POST",
  });
}

// --- Cover URL helper ---

/**
 * Build a cover image URL with a cache buster derived from the album's
 * directory mtime. When the underlying files change (and we re-scan and
 * pick up a new mtime), the URL changes too — so the browser refetches.
 *
 * Without this, the browser would happily keep showing a stale cover for
 * up to a day (our max-age). Lesson learned from the apply-flow bug
 * earlier.
 */
export function coverUrl(album: Album, opts?: { size?: "thumb" | "full" }): string {
  const params = new URLSearchParams();
  if (opts?.size === "full") params.set("size", "full");
  if (album.dir_mtime) params.set("v", String(Math.floor(album.dir_mtime)));
  const qs = params.toString();
  return `/api/albums/${encodeURIComponent(album.id)}/cover${qs ? "?" + qs : ""}`;
}

export { ApiError };
