/**
 * Typed fetch wrappers for the FastAPI backend. All requests go through
 * the Vite dev proxy (or hit the same origin in production).
 */

import type {
  Album,
  AlbumListResponse,
  AlbumResolution,
  AppConfig,
  CheckSweepStatus,
  FingerprintPoolStatus,
  Job,
  JobStatus,
  JobType,
  ScanCancelResponse,
  ScanStatusResponse,
  TracksResponse,
} from "./types";
import { errorDetailText } from "./errors";

class ApiError extends Error {
  status: number;
  /** The body's `detail` as sent (string, object or list), for callers
   *  that branch on a structured conflict such as `active_job_exists`. */
  detail: unknown;
  /** The whole JSON body, for the few refusals that carry keys beside
   *  `detail` (the rename's `user_message`). */
  body: unknown;
  constructor(message: string, status: number, detail?: unknown, body?: unknown) {
    super(message);
    this.status = status;
    this.detail = detail;
    this.body = body;
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
    throw new ApiError(detail, res.status, raw, body);
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
 * search / research persist all skip it on the next run. Returns the MBID
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

/** The scan's state now (one read; its progress arrives on the scan stream). */
export function getScanStatus(): Promise<ScanStatusResponse> {
  return request<ScanStatusResponse>("/api/scan/status");
}

/** Ask the running scan to stop; nothing it read is saved. */
export function cancelScan(): Promise<ScanCancelResponse> {
  return request("/api/scan/cancel", { method: "POST" });
}

export function getFingerprintPoolStatus(): Promise<FingerprintPoolStatus> {
  return request<FingerprintPoolStatus>("/api/fingerprint-pool/status");
}

/** Whether the one-time "add an AcoustID key" prompt was dismissed. */
export function getAcoustidPrompt(): Promise<{ dismissed: boolean }> {
  return request("/api/prompts/acoustid");
}

export function dismissAcoustidPrompt(): Promise<{ dismissed: boolean }> {
  return request("/api/prompts/acoustid/dismiss", { method: "POST" });
}

/** The light/dark theme and Library tile size; null: never chosen. */
export interface UiPrefs {
  theme: "light" | "dark" | null;
  tile_columns: number | null;
}

export function getUiPrefs(): Promise<UiPrefs> {
  return request<UiPrefs>("/api/ui-prefs");
}

/** Set the named preferences; null forgets one, a field left out is untouched. */
export function putUiPrefs(prefs: Partial<UiPrefs>): Promise<UiPrefs> {
  return request<UiPrefs>("/api/ui-prefs", { method: "PUT", body: JSON.stringify(prefs) });
}

export function getCheckSweepStatus(): Promise<CheckSweepStatus> {
  return request<CheckSweepStatus>("/api/checks/sweep");
}

// --- Research requests (job cards) ---
//
// The original `suggestMetadata` / `suggestCover` wrappers (POST /api/ai/
// suggest-metadata, POST /api/ai/suggest-cover) were removed when the
// sync endpoints they wrapped were deleted server-side. All research-assisted
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

export function deleteJob(jobId: string): Promise<{ deleted: boolean; id: string }> {
  return request(`/api/ai/jobs/${encodeURIComponent(jobId)}`, { method: "DELETE" });
}

export function bulkDeleteJobs(statuses: JobStatus[]): Promise<{ deleted_count: number }> {
  return request(`/api/ai/jobs?status=${statuses.join(",")}`, { method: "DELETE" });
}

/** Cancel every waiting, held or running research request behind one job
 *  card (`POST /api/ai/jobs/{id}/cancel`). 409 when nothing is left to
 *  cancel (all awaiting review or finished). */
export function cancelResearchGroup(jobId: string): Promise<{ id: string; cancelled: number }> {
  return request(`/api/ai/jobs/${encodeURIComponent(jobId)}/cancel`, { method: "POST" });
}

// --- Settings ---

export interface AppSettings {
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
  acoustid_api_key?: string;
  snapshot_retention_count?: number;
  snapshot_min_interval_hours?: number;
  filename_template_single_artist?: string;
  filename_template_compilation?: string;
  dir_template_single_artist?: string;
  dir_template_compilation?: string;
}

/** The research queue's counts, as the jobs stream's `queue_state` sends
 *  them (the frame also carries older keys the UI does not read). */
export interface QueueState {
  running: number;
  queued: number;
  /** How many research requests "Clear applied or discarded" removes. */
  clearable: number;
}

export function getSettings(): Promise<AppSettings> {
  return request("/api/settings");
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
  args: { scope: "all" | "albums"; albumIds?: string[] } = { scope: "all" },
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
  /** Distinct albums the cluster touches (older servers omit it). */
  distinct_albums?: number;
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


/** Why a recording lookup gave no title. Stable codes; the sentence is in
 *  `reason`. `unreachable`, `rate_limited`, `service_error` and
 *  `unreadable_answer` are temporary: asking again later can succeed. */
export type CanonicalReasonCode =
  | "no_mbid"
  | "not_found"
  | "unreachable"
  | "rate_limited"
  | "service_error"
  | "unreadable_answer";

/** What MusicBrainz says the cluster's recording_mbid actually is.
 *  Title + artist are null when no MBID was attached, MB has no such
 *  recording, or the lookup failed. `reason` is a plain sentence for the
 *  user and `reason_code` says which case it is; both are null when a
 *  title is given. */
export interface CanonicalRecording {
  title: string | null;
  artist: string | null;
  mbid: string | null;
  reason: string | null;
  reason_code: CanonicalReasonCode | null;
}

/** True when the lookup failed for a reason that may go away (no
 *  connection, rate limit, service trouble), so the answer is not worth
 *  keeping. */
export function isTemporaryCanonicalFailure(c: CanonicalRecording | undefined): boolean {
  return (
    c?.reason_code === "unreachable" ||
    c?.reason_code === "rate_limited" ||
    c?.reason_code === "service_error" ||
    c?.reason_code === "unreadable_answer"
  );
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

export async function applyFilenameRename(
  findingId: number,
  args: {
    plan: { old: string; new: string }[];
    dirRename?: { old: string; new: string };
  },
): Promise<FilenameRenameResult> {
  try {
    return await request<FilenameRenameResult>(
      `/api/checks/filename-mismatch/findings/${findingId}/apply`,
      {
        method: "POST",
        body: JSON.stringify({
          plan: args.plan,
          dir_rename: args.dirRename ?? null,
        }),
      },
    );
  } catch (e) {
    // A rename the server refused (409) explains itself beside `detail`:
    // hand the dialog a result, so it shows the sentence for the user and
    // stays open, instead of a toast with the technical text.
    if (e instanceof ApiError && e.status === 409) {
      const b = e.body as { user_message?: unknown; reason_code?: unknown } | null;
      if (b && typeof b.user_message === "string") {
        return {
          ok: false,
          renamed: 0,
          errors: [],
          message: e.message,
          user_message: b.user_message,
          reason_code: typeof b.reason_code === "string" ? b.reason_code : undefined,
        };
      }
    }
    throw e;
  }
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

/** Re-derive what converting a multi-disc set would do, against current
 *  disk + DB state. Read-only. */
import type {
  MultiDiscBlocker,
  MultiDiscConversionPreview,
  MultiDiscConvertResult,
  MultiDiscProgress,
} from "./types";
import { postSse } from "./sse";

export function fetchMultiDiscConversionPreview(
  findingId: number,
): Promise<MultiDiscConversionPreview> {
  return request(
    `/api/checks/multi-disc-layout/findings/${findingId}/conversion-preview`,
  );
}

/** Convert a multi-disc set kept in one folder exactly as the preview with
 *  `digest` showed it. The server re-derives the plan and refuses (409)
 *  when it changed, when something stops it, or while a scan runs; such a
 *  refusal comes back as a result (`refused`), so the dialog says why and
 *  stays open. */
export async function convertMultiDisc(
  findingId: number,
  digest: string,
): Promise<MultiDiscConvertResult> {
  try {
    return await request<MultiDiscConvertResult>(
      `/api/checks/multi-disc-layout/findings/${findingId}/convert`,
      { method: "POST", body: JSON.stringify({ digest }) },
    );
  } catch (e) {
    if (e instanceof ApiError && e.status === 409) {
      const b = e.body as {
        user_message?: unknown;
        reason_code?: unknown;
        blockers?: unknown;
      } | null;
      if (b && typeof b.user_message === "string") {
        return {
          ok: false,
          refused: true,
          user_message: b.user_message,
          reason_code: typeof b.reason_code === "string" ? b.reason_code : undefined,
          blockers: Array.isArray(b.blockers) ? (b.blockers as MultiDiscBlocker[]) : undefined,
        };
      }
    }
    throw e;
  }
}

/** A refusal body turned into a result the dialog shows (nothing changed). */
function refusedResult(b: unknown): MultiDiscConvertResult | null {
  const r = b as { user_message?: unknown; reason_code?: unknown; blockers?: unknown } | null;
  if (!r || typeof r.user_message !== "string") return null;
  return {
    ok: false,
    refused: true,
    user_message: r.user_message,
    reason_code: typeof r.reason_code === "string" ? r.reason_code : undefined,
    blockers: Array.isArray(r.blockers) ? (r.blockers as MultiDiscBlocker[]) : undefined,
  };
}

/**
 * Convert a set kept in several folders, with its progress: the same POST
 * as {@link convertMultiDisc}, asked for a stream (moving the files of a
 * big box set over a network share takes minutes). `onProgress` gets each
 * `{step, done, total}`; the promise resolves with the result, or with a
 * refusal turned into one.
 */
export async function convertMultiDiscStreaming(
  findingId: number,
  digest: string,
  onProgress: (p: MultiDiscProgress) => void,
): Promise<MultiDiscConvertResult> {
  let result: MultiDiscConvertResult | null = null;
  try {
    await postSse(
      `/api/checks/multi-disc-layout/findings/${findingId}/convert`,
      { digest },
      {
        progress: (d) => onProgress(d as MultiDiscProgress),
        result: (d) => {
          result = d as MultiDiscConvertResult;
        },
        refused: (d) => {
          result = refusedResult(d) ?? {
            ok: false,
            refused: true,
            user_message: "Nothing was converted.",
          };
        },
      },
    );
  } catch (e) {
    // A refusal before the stream starts (a restore running).
    const status = (e as { status?: number }).status;
    if (status === 409) {
      return { ok: false, refused: true, user_message: (e as Error).message };
    }
    throw e;
  }
  if (!result) throw new Error("The conversion ended without saying how it went. Open the dialog again to see where it got.");
  return result;
}

/** Stop treating an unfinished conversion as one: every file stays where
 *  it is, the record is kept. */
export function abandonMultiDiscConversion(
  findingId: number,
): Promise<{ ok: boolean; user_message: string }> {
  return request(`/api/checks/multi-disc-layout/findings/${findingId}/abandon-conversion`, {
    method: "POST",
  });
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

// --- Agent pairings and approvals ---

export const AGENT_KINDS = [
  "library.read",
  "proposal.submit",
  "compute.scan",
  "compute.analyze",
  "tags.write",
  "covers.write",
  "files.rename",
  "work.cancel",
] as const;
export type AgentKind = (typeof AGENT_KINDS)[number];
export type AgentDecision = "allow" | "ask" | "refuse";

export interface AgentPairing {
  id: string;
  name: string;
  scopes: string[];
  policy: Partial<Record<AgentKind, AgentDecision>>;
  createdAt: string;
  lastUsedAt: string | null;
  revokedAt: string | null;
}

/** How to register a new pairing with an assistant; never carries a secret. */
export interface AgentPairingCreated extends AgentPairing {
  connect: { command: string; args: string[] };
}

export interface AgentActivityRecord {
  at: string;
  tool: string;
  outcome: string;
}

export type AgentApprovalState = "pending" | "approved" | "denied" | "expired";
export type AgentApprovalAction = "fix_apply" | "scan_start" | "checks_start" | "analysis_start";

export interface AgentApprovalDiff {
  tags: { scope: string; field: string; before: string | null; after: string | null }[];
  cover?: string;
  files: { from: string; to: string }[];
  folder?: { from: string; to: string };
}

export interface AgentApproval {
  id: string;
  pairingId: string;
  pairingName: string;
  action: AgentApprovalAction;
  albumId: string | null;
  albumLabel: string | null;
  kinds: string[];
  state: AgentApprovalState;
  reason: string | null;
  createdAt: string;
  expiresAt: string;
  decidedAt: string | null;
  diff: AgentApprovalDiff | null;
}

export function listAgentPairings(): Promise<AgentPairing[]> {
  return request("/api/agent/pairings");
}

export function createAgentPairing(body: { name: string; scopes: string[] }): Promise<AgentPairingCreated> {
  return request("/api/agent/pairings", { method: "POST", body: JSON.stringify(body) });
}

export function revokeAgentPairing(id: string): Promise<AgentPairing> {
  return request(`/api/agent/pairings/${encodeURIComponent(id)}`, { method: "DELETE" });
}

export function setAgentPolicy(
  id: string,
  policy: Partial<Record<AgentKind, AgentDecision>>,
): Promise<AgentPairing> {
  return request(`/api/agent/pairings/${encodeURIComponent(id)}/policy`, {
    method: "PUT",
    body: JSON.stringify(policy),
  });
}

export function listAgentActivity(id: string): Promise<AgentActivityRecord[]> {
  return request(`/api/agent/pairings/${encodeURIComponent(id)}/activity`);
}

export function listAgentApprovals(state: "pending" | "all"): Promise<AgentApproval[]> {
  return request(`/api/agent/approvals?state=${state}`);
}

export function decideAgentApproval(id: string, decision: "approve" | "deny"): Promise<AgentApproval> {
  return request(`/api/agent/approvals/${encodeURIComponent(id)}/${decision}`, { method: "POST" });
}

export type AgentChangeState = "recorded" | "applied" | "partial" | "failed" | "undone";

/** One change an assistant applied (or the undo of one). */
export interface AgentChange {
  id: string;
  pairingId: string | null;
  pairingName: string;
  /** The album's id now (a folder rename changes it). */
  albumId: string;
  albumLabel: string | null;
  createdAt: string;
  state: AgentChangeState;
  /** What it did, in a few words; a label only. */
  summary: string;
  /** For an undo's own record: the change it put back. */
  undoes: string | null;
  /** Whether Undo would run now; when not, `reason` says why. */
  undoable: boolean;
  reason: string | null;
}

export interface AgentUndoResult {
  change: AgentChange;
  undoId: string;
  complete: boolean;
  message: string | null;
}

export function listAgentChanges(pairingId: string): Promise<AgentChange[]> {
  return request(`/api/agent/changes?pairing=${encodeURIComponent(pairingId)}`);
}

export function undoAgentChange(id: string): Promise<AgentUndoResult> {
  return request(`/api/agent/changes/${encodeURIComponent(id)}/undo`, { method: "POST" });
}

export { ApiError };

export interface ResearchItem {
  id: string;
  kind: string;
  state: string;
  revision: number;
  validation: string | null;
  digest: string | null;
  note: string | null;
  historicalResult: string | null;
  applyOutcome?: {
    files: {fileName: string; status: string; message?: string}[];
    cover?: {status: string; message?: string};
    renamed: boolean;
  } | null;
  changeId?: string | null;
  result: {
    outcome: string;
    evidence: {source:string; retrievedAt:string; claim:string; limitations:string[]}[];
    confidence: number;
    uncertainties: string[];
    recommendation: {
      type: string;
      rationale?: string;
      rightsUncertainty?: string;
      question?: string;
      explanation?: string;
      url?: string;
      identifiers?: {type:string;id:string}[];
    };
  } | null;
}

export interface ResearchPreview {
  coverThumbnail?: string;
  digest:string;
  revision:number;
  diff: AgentApprovalDiff;
}
export function researchItems(group:string) {
  return request<ResearchItem[]>(`/api/research/groups/${encodeURIComponent(group)}`);
}
export function previewResearch(item:ResearchItem) {
  return request<ResearchPreview>(`/api/research/${encodeURIComponent(item.id)}/preview/${encodeURIComponent(item.digest ?? "")}`,{method:"POST"});
}
export function decideResearch(item:ResearchItem, action:"apply"|"discard"|"retry"|"cancel", preview?:ResearchPreview) {
  return request<{ok:boolean}>(`/api/research/${encodeURIComponent(item.id)}/decision`,{
    method:"POST",body:JSON.stringify({revision:preview?.revision ?? item.revision,action,digest:item.digest,previewDigest:preview?.digest}),
  });
}

// --- Agent hosts and delegated work (spec 005) ---

/** What a paired host reported about itself ("reported by the host"). */
export interface AgentHostReport {
  clientName: string;
  clientVersion: string;
  profile: string;
  tasks: boolean;
  skills: boolean;
  events: boolean;
  observedAt: string;
}

/** Four separate facts about one pairing, each with the time it was last
 * true; none of them says "ready". */
export interface AgentDiagnostics {
  pairingId: string;
  name: string;
  scopes: AgentKind[];
  revoked: boolean;
  paired: { at: string };
  reachable: { at: string | null };
  subscription: { active: boolean; at: string | null };
  lastClaim: { at: string | null; workId: string | null };
  host: AgentHostReport | null;
}

export function getAgentDiagnostics(): Promise<AgentDiagnostics[]> {
  return request("/api/agent/diagnostics");
}

export type DelegatedWorkState = "queued" | "claimed" | "running" | "awaiting_review" | "failed" | "cancelled";

export interface DelegatedWorkItem {
  workId: string;
  albumId: string;
  albumLabel: string | null;
  kind: string;
  state: DelegatedWorkState;
  revision: number;
  holder: string | null;
  lastHeartbeatAt: string | null;
  leaseExpiresAt: string | null;
  attempts: number;
  maxAttempts: number;
  lastError: string | null;
  waitingForAgent: boolean;
  /** The job card (group) the request belongs to. */
  groupId?: string;
  /** The Fix all batch this request belongs to; null (or absent) for a
   *  request made on its own. */
  batchId?: string | null;
}

export function listDelegatedWork(): Promise<DelegatedWorkItem[]> {
  return request("/api/research/work");
}

// --- Research batches (spec 006): the requests one Fix all run made ---

/** Every state a batch item can be in, finished ones included. */
export type ResearchBatchState = DelegatedWorkState | "applied" | "discarded";

/** One batch: where it came from, how big it is and its items by state.
 * Counts are derived from the items; a batch has no state of its own. */
export interface BatchSummary {
  batchId: string;
  checkId: string;
  startedAt: string;
  finishedAt: string | null;
  /** Stopped part-way (or the app quit) and no run holds it now. */
  unfinished: boolean;
  size: number;
  counts: Partial<Record<ResearchBatchState, number>>;
}

/** A batch with its items. Items of a Missing album are listed too. */
export interface BatchDetail extends BatchSummary {
  items: (Omit<DelegatedWorkItem, "state"> & { state: ResearchBatchState })[];
}

export function listResearchBatches(): Promise<BatchSummary[]> {
  return request("/api/research/batches");
}

export function getResearchBatch(batchId: string): Promise<BatchDetail> {
  return request(`/api/research/batches/${encodeURIComponent(batchId)}`);
}

/** Cancel every waiting or held item of a batch; items awaiting review or
 * finished are left as they are. */
export function cancelResearchBatch(batchId: string): Promise<BatchSummary & { cancelled: number }> {
  return request(`/api/research/batches/${encodeURIComponent(batchId)}/cancel`, { method: "POST" });
}

/** Retry failed or cancelled delegated work through the existing decision call. */
export function retryDelegatedWork(item: Pick<DelegatedWorkItem, "workId" | "revision">) {
  return request<{ ok: boolean }>(`/api/research/${encodeURIComponent(item.workId)}/decision`, {
    method: "POST",
    body: JSON.stringify({ revision: item.revision, action: "retry" }),
  });
}
