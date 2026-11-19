/**
 * Types matching the FastAPI response shapes in `musaic/main.py`.
 * Keep in sync with the Album dataclass in `musaic/scanner.py`.
 */

export interface Album {
  id: string;
  path: string;
  dir_name: string;
  title: string | null;
  artist: string | null;
  album_artist: string | null;
  year: string | null;
  genre: string | null;
  track_count: number;
  total_seconds: number;
  has_embedded_cover: boolean;
  cover_file: string | null;
  sample_track: string;
  dir_mtime: number;
  // v2 — directory-name parsed fields
  dir_artist: string | null;
  dir_album: string | null;
  is_compilation: boolean;
  disc_number: number | null;
  disc_total: number | null;
  // v3 — audio format from FLAC StreamInfo
  sample_rate: number | null;       // Hz
  bits_per_sample: number | null;   // e.g. 16, 24
  channels: number | null;          // 1 = mono, 2 = stereo
  // v8 — Vorbis sort variants. Populated by the scanner from the FLAC's
  // ALBUMARTISTSORT / ARTISTSORT / ALBUMSORT tags. Used by the library
  // sort key so "The Beatles" / "Ray Conniff & His Orchestra" / etc.
  // group under the canonical sort form rather than literal album_artist.
  album_artist_sort: string | null;
  artist_sort: string | null;
  album_sort: string | null;
  // MusicBrainz anchor state. Three meaningful shapes:
  //   { mbid: "<uuid>", checked_at: "<ts>" }     → identified
  //   { mbid: null,     checked_at: "<ts>" }     → tried, no match
  //                                                  found yet
  //   { mbid: null,     checked_at: null }       → never tried
  // The PropTable / status indicators key off the (mbid, checked_at)
  // pair to render the right state.
  mb_release_group_mbid?: string | null;
  mb_last_checked_at?: string | null;
  // v12 — timestamp of the most recent AI metadata job whose output
  // the user acted on (applied OR discarded). NULL = the album hasn't
  // been through an AI metadata pass that the user reviewed yet.
  metadata_ai_acted_on_at?: string | null;
  // v15 — presence marker. NULL when the album's directory is on disk;
  // ISO-8601 timestamp when the scanner didn't see it (drive unmount,
  // user moved it, FS hiccup, deleted from another tool). The Library
  // grid greys the tile, the DetailSheet shows a "Missing since X"
  // banner with a "Remove from library" button, and mutation
  // affordances (Apply, AI, cover upload) disable. The album row +
  // computed children (fingerprints, MB anchor, track tags) survive
  // until the user explicitly removes it via DELETE /api/albums/{id}.
  missing_since?: string | null;
  // SSE-stream-only field — populated when Album arrives via /api/scan/stream
  _status?: "fresh" | "cached";
}

export interface Track {
  filename: string;
  tracknumber: string;
  title: string;
  artist: string;
  /** The file's ALBUM tag ("" when absent). */
  album?: string;
  duration: number | null;
  // Audio analysis (null if not yet computed)
  bpm: number | null;
  replaygain_track_gain: string | null;  // e.g. "-2.34 dB"
  replaygain_track_peak: string | null;  // e.g. "0.987654"
  replaygain_album_gain: string | null;  // same value across all album tracks
  replaygain_album_peak: string | null;
  error?: string;
}

export interface AppConfig {
  library_dir: string | null;
  exists: boolean;
}

export interface AlbumListResponse {
  library_dir: string;
  count: number;
  scanned_at: string;
  albums: Album[];
}

export interface TracksResponse {
  album_id: string;
  tracks: Track[];
}

// ---- Check sweep ----

/** Progress of the background check sweep (GET /api/checks/sweep, and the
 * `sweep` event of /api/checks/stream). Every check runs on its own; the
 * sweep reruns the ones whose inputs changed. `total` / `completed` count
 * units of work (one album of one check, or one library-wide run).
 * "waiting": it has work but lets a scan, a restore or other work finish
 * first. */
export interface CheckSweepStatus {
  status: "idle" | "running" | "waiting";
  total: number;
  completed: number;
  current_check: string | null;
  started_at: string | null;
  completed_at: string | null;
}

// ---- Fingerprint pool ----

/** Status of the eager AcoustID-fingerprinting pool that runs after
 * each scan. Mirrors `services/fingerprint_pool._state`. */
export interface FingerprintPoolStatus {
  status: "idle" | "running" | "done" | "cancelled" | "unavailable";
  library_dir: string | null;
  total: number;
  completed: number;
  errors: number;
  current_album_id: string | null;
  started_at: string | null;
  completed_at: string | null;
  /** Populated when status === "unavailable" — usually "fpcalc not
   *  installed" or "AcoustID API key not set". */
  skip_reason: string | null;
}

// ---- Metadata resolver ----
//
// Mirrors `services/resolver.AlbumResolution.to_dict()`. A `FieldFact`
// carries the value plus the source ("musicbrainz", "fingerprint",
// "cache", etc.) and a 0-1 confidence so the UI can show provenance
// without re-deriving it.

export type ResolutionSource =
  | "cache"
  | "musicbrainz"
  | "fingerprint"
  | "fingerprint+mb"
  | "current_tag"
  | "ai"
  | "user";

export interface FieldFact<V = string> {
  value: V;
  source: ResolutionSource;
  confidence: number;
}

export interface TrackResolution {
  filename: string;
  recording_mbid: string | null;
  fingerprint_score: number | null;
  // True when a track_fingerprints row exists for this filename —
  // i.e. Chromaprint was computed. Independent from recording_mbid
  // (AcoustID match) and from fingerprint_score (AcoustID's
  // confidence value — NULL means either "service failed" OR "no
  // confident match" so it can't distinguish those from "never
  // fingerprinted"). Use THIS field for the per-track UI chip.
  fingerprinted: boolean;
  canonical_title: FieldFact | null;
  canonical_artist: FieldFact | null;
  canonical_tracknumber: FieldFact | null;
  canonical_duration_s: number | null;
}

export interface AlbumResolution {
  album_id: string;
  anchor: FieldFact | null;
  /** "cache" | "dir-search" | "fingerprint-vote" | "ai" | "user" | "none" */
  anchor_strategy: string;
  canonical_title: FieldFact | null;
  canonical_album_artist: FieldFact | null;
  canonical_year: FieldFact | null;
  canonical_album_artist_sort: FieldFact | null;
  canonical_genre: FieldFact | null;
  cover_url: FieldFact | null;
  tracks: TrackResolution[];
  unresolved_track_count: number;
  notes: string[];
}

// SSE events emitted by /api/scan/stream
export type ScanEvent =
  | { type: "start"; library_dir: string; total: number; cached_count: number; force: boolean }
  | { type: "album"; album: Album }
  | { type: "progress"; scanned: number; total: number }
  // v15: scan-observed albums that vanished from disk. The row +
  // cached metadata persist (with `missing_since` set); only an
  // explicit user action via DELETE /api/albums/{id} actually drops
  // the row. The old "removed" event name implied destruction it
  // never delivered.
  | { type: "marked_missing"; id: string; path: string }
  // Saved: the row of a vanished album moved to the folder found under
  // another spelling (the same folder on disk). Its `album` event carried
  // the new id; the old id no longer exists.
  | { type: "adopted"; old_id: string; id: string; old_path: string; path: string }
  // `skipped_non_utf8_count` is present only when folders or files were left
  // out because their names are not valid UTF-8.
  | {
      type: "done";
      count: number;
      marked_missing_count: number;
      scanned_at: string;
      skipped_non_utf8_count?: number;
    }
  // A cancel was accepted (POST /api/scan/cancel); the scan ends with
  // `cancelled` at its next album.
  | { type: "cancelling"; scanned: number; total: number }
  // The scan ended without saving anything: cancelled, or aborted by the
  // backend's sanity check (too many albums vanished at once, e.g. an
  // unmounted drive). The library keeps its previous scan.
  | { type: "cancelled"; reason: "cancelled"; phase: string; scanned: number; total: number; detail: string }
  | { type: "aborted"; reason: string; detail: string; vanish_count: number; cached_count: number }
  | { type: "error"; detail: string };

/** `POST /api/scan/cancel`. */
export interface ScanCancelResponse {
  outcome: "requested" | "already_requested" | "too_late" | "not_running";
  scan: { status: string };
}

export type ApiError = { status: number; detail: string };

// ----- AI suggestions -----

export interface TrackSuggestionEntry {
  filename: string;
  suggestions: Partial<Record<"title" | "artist" | "tracknumber", string>>;
}

export interface MetadataSuggestionResponse {
  album_id: string;
  suggestions: Partial<Record<"title" | "album_artist" | "artist" | "year" | "genre", string>>;
  tracks: TrackSuggestionEntry[];
  notes: string;
  confidence: "low" | "medium" | "high" | "unknown" | string;
  raw_excerpt: string;
  /** When AI confidently identified the album on MusicBrainz, this is the
   *  release-group MBID (already validated UUID + verified live). The MBID
   *  has been persisted server-side by the time the user sees the response;
   *  the field is here purely so the UI can surface "we identified it as X"
   *  alongside any tag suggestions. */
  mb_release_group_mbid?: string | null;
}

export interface CoverSuggestionResponse {
  album_id: string;
  cover_url: string | null;
  source: string;
  notes: string;
  confidence: "low" | "medium" | "high" | "unknown" | string;
  raw_excerpt: string;
}

// ----- AI Job queue -----

export type JobType = "metadata" | "cover" | "metadata+cover";
export type JobStatus = "queued" | "running" | "completed" | "failed" | "acted_on";

export interface JobResult {
  /** Captured Claude transcript leading up to a failed job. Populated
   *  by the worker when the SDK call throws — provides context that
   *  the bare exception message ("exit code 1 …") usually doesn't.
   *  Absent on completed jobs. */
  failure_excerpt?: string;
  metadata?: {
    suggestions: Partial<Record<"title" | "album_artist" | "artist" | "year" | "genre", string>>;
    tracks: TrackSuggestionEntry[];
    notes: string;
    confidence: string;
    /** Canonical MusicBrainz release-group MBID, when AI confidently
     *  identified the album. Validated to UUID format + verified live
     *  against MB before being included here, then persisted server-side
     *  via cache.set_album_mb_anchor as a side-effect of job completion
     *  (independent of whether the user accepts any tag suggestions).
     *  Surfaced in the suggestion review so the user can SEE that the
     *  MBID side-effect happened. */
    mb_release_group_mbid?: string | null;
  };
  cover?: {
    cover_url: string | null;
    source: string;
    notes: string;
    confidence: string;
  };
}

export interface Job {
  id: string;
  album_id: string;
  album_path: string;
  album_title: string;
  job_type: JobType;
  hint: string;
  status: JobStatus;
  result: JobResult | null;
  error: string | null;
  acted_on_action: "applied" | "discarded" | null;
  created_at: string;
  started_at: string | null;
  completed_at: string | null;
  acted_on_at: string | null;
  // v16: per-job toggle for whether the album cover art is sent
  // with the suggest_metadata prompt. User-set via the hint
  // dialog checkbox (default on). Optional in the client type
  // for backward-compat with any cached jobs from before v16.
  attach_cover?: boolean;
}

// 409 detail when an album already has an active job.
export interface JobConflictDetail {
  error: "active_job_exists";
  message: string;
  existing_job_id: string;
  existing_status: JobStatus;
}

// ----- Health checks -----

export type CheckCategory = "metadata" | "cover" | "audio" | "files" | "duplicates" | "external";
export type CheckCost = "cheap" | "medium" | "heavy";
export type CheckFixKind = "none" | "ai-job" | "deterministic";
export type FindingSeverity = "info" | "warn" | "error";
export type CheckRunStatus = "running" | "completed" | "failed" | "cancelled";

export interface CheckMeta {
  id: string;
  label: string;
  description: string;
  category: CheckCategory;
  cost: CheckCost;
  runs_during_scan: boolean;
  fix_kind: CheckFixKind;
  /** When non-null, the Health detail page renders a "<label> (N)" button
   *  that calls try_quick_fix on every open finding via the bulk SSE
   *  endpoint. Null/missing = no bulk button shown. */
  bulk_action_label: string | null;
}

export interface CheckRun {
  id: number;
  check_id: string;
  started_at: string;
  completed_at: string | null;
  status: CheckRunStatus;
  library_path: string;
  scope: string;             // "all" | JSON list (older runs may read "incremental")
  issue_count: number;
  error: string | null;
  duration_ms: number | null;
}

export interface Finding {
  id: number;
  check_id: string;
  album_id: string | null;
  severity: FindingSeverity;
  summary: string;
  detail: Record<string, unknown> | null;
  first_seen_at: string;
  last_seen_at: string;
  resolved_at: string | null;
  ignored_at: string | null;
  ignored_reason: string | null;
}

/** Returned by GET /api/checks — one entry per registered check. */
export interface CheckSummary {
  meta: CheckMeta;
  open_count: number;
  last_run: CheckRun | null;
}

/** Returned by GET /api/checks/{id} — drilldown for a single check. */
export interface CheckDetail {
  meta: CheckMeta;
  runs: CheckRun[];
  findings: Finding[];
}

export interface FixFindingResult {
  ok: boolean;
  message: string;
  job_id: string | null;
}

// ----- User-defined track tags (v9) -----
//
// The user's custom taxonomy ("Cocktail Hour", "Christmas",
// "Instrumental"…) — distinct from the FLAC GENRE field. Stored per
// track in the cache. Album-level chips show the intersection (tags
// every track on the album carries).

export interface AlbumTagsResponse {
  album_id: string;
  /** filename → list of tags, only filenames with ≥1 tag are present */
  track_tags: Record<string, string[]>;
  /** Tags that appear on every track of the album. */
  album_tags: string[];
}

export interface TagUsage {
  tag: string;
  count: number;
}

export interface TagListResponse {
  tags: TagUsage[];
}

// ----- Tag vocabulary (v10) -----
//
// User-curated approved list of tags that powers the picker UIs.
// `count` is the live usage count from track_tags so the Settings
// panel can show "Cocktail Hour · 47 tracks" inline.

export interface VocabularyEntry {
  tag: string;
  description: string;
  color: string | null;
  sort_order: number;
  created_at: string;
  count: number;
}

export interface OrphanTag {
  tag: string;
  count: number;
}

export interface VocabularyResponse {
  vocabulary: VocabularyEntry[];
  orphans: OrphanTag[];
  /** Groups of vocabulary spellings that now count as one tag (entries
   *  saved under older, ASCII-only case rules, e.g. "Été" / "été").
   *  Absent when there are none, and from backends that don't report it. */
  case_conflicts?: string[][];
  /** Present after a rename / merge / delete that touched track rows. */
  rename_result?: VocabularyMutationResult;
  merge_result?: VocabularyMutationResult;
  delete_result?: VocabularyMutationResult;
}

export interface VocabularyMutationResult {
  albums?: string[];
  /** Albums whose tag sidecar file wasn't rewritten (album Missing, folder
   *  gone or not writable). Absent when every write succeeded. */
  sidecars_not_written?: string[];
}

// ----- DB backup / snapshots -----

export interface DbCounts {
  albums: number;
  albums_with_mbid: number;
  track_fingerprints: number;
  track_tags: number;
  tag_vocabulary: number;
  ai_jobs: number;
  check_findings_open: number;
}

export interface DbInfo {
  path: string;
  exists: boolean;
  size_bytes: number;
  schema_version: number | null;
  current_schema: number;
  modified_at: string | null;
  counts: DbCounts;
}

export interface DbSnapshot {
  filename: string;
  size_bytes: number;
  modified_at: string;
  schema_version: number | null;
  valid: boolean;
  counts: Partial<DbCounts>;
}

export interface DbSnapshotListResponse {
  snapshots: DbSnapshot[];
}

export interface DbSnapshotCreateResponse {
  filename: string;
  size_bytes: number;
  modified_at: string;
  schema_version: number;
  pruned: string[];
}

export interface DbRestoreResponse {
  schema_version: number;
  size_bytes: number;
  counts: Partial<DbCounts>;
  restored_at: string;
  aside_paths: string[];
}

// ----- Filename-mismatch finding shape -----

export interface FilenameRenameEntry {
  /** Current filename on disk. */
  filename: string;
  /** What the template says it should be. */
  expected: string;
  tracknumber: string;
  title: string;
  artist: string;
  /** When non-null, another file in the cluster also rendered to
   *  the same expected name — the rename plan can't be applied
   *  without resolving the collision first. */
  collides_with: string | null;
}

export interface DirRename {
  old: string;
  new: string;
}

export interface FilenameMismatchDetail {
  dir_name: string;
  is_compilation: boolean;
  template: string;
  /** Album-directory template (only present when the dir name is
   *  also being checked). */
  dir_template?: string;
  headline: string;
  renames: FilenameRenameEntry[];
  /** Optional dir rename — present when the album folder name
   *  doesn't match the configured dir template. Apply it first,
   *  then file renames execute inside the renamed folder. */
  dir_rename: DirRename | null;
}

/** Response from GET /api/checks/filename-mismatch/findings/{id}/plan-preview.
 *  Same shape as `FilenameMismatchDetail` but freshly derived against
 *  current disk + DB state, plus state-flag fields so the Resolve dialog
 *  can render the right empty / error / missing-album branch. */
export interface FilenameMismatchPlanPreview extends FilenameMismatchDetail {
  album_id: string;
  /** True when there's nothing to rename — dialog should clear the
   *  stale finding and skip / close rather than render empty rows. */
  is_empty: boolean;
  /** True when the album directory disappeared from disk between
   *  finding-creation and now. */
  album_missing?: boolean;
  /** Present when plan re-derivation crashed server-side. */
  error?: string;
}

/** Response from GET /api/checks/duplicate-tracks/findings/{id}/cluster-preview.
 *  Re-derives the cluster live from `track_fingerprints` so the
 *  Resolve dialog never shows files that have already been trashed,
 *  renamed, or otherwise dropped from the cluster. */
export interface DuplicateClusterPreview {
  album_id: string;
  /** True when no duplicate cluster overlapping the finding remains —
   *  the dialog should close and let the finding auto-resolve. */
  is_empty: boolean;
  recording_mbid: string | null;
  filenames: string[];
  durations_s: number[];
  duplicate_count: number;
  recommended_keep: string | null;
  recommended_reason: string | null;
  /** Files the finding originally listed but that are no longer in the
   *  cluster (deleted, renamed, re-encoded). UI may surface a quiet
   *  "N file was already trashed" hint. */
  removed_filenames: string[];
}

/** Response from GET /api/checks/multi-disc-layout/findings/{id}/conversion-preview.
 *  What converting a multi-disc set to one folder would do, re-derived
 *  from current disk + DB state. Only a set in one folder converts. */
export type MultiDiscPreviewState = "one_folder" | "folders" | "incomplete" | "not_a_set";

export interface MultiDiscMember {
  album_id: string;
  dir_name: string | null;
  disc: number;
  missing: boolean;
}

export interface MultiDiscTrack {
  album_id: string;
  disc: number | null;
  filename: string;
  title: string;
  tracknumber: string;
  track: number | null;
  new_track: number | null;
  new_filename: string | null;
  has_disc_tags: boolean;
  /** An interrupted conversion already wrote this file's new numbering;
   *  `disc` and `track` are then the values its record kept. */
  tags_done?: boolean;
  /** How far an interrupted conversion got with this file: renamed and
   *  retagged in its own folder, or moved into the set's folder. */
  state?: MultiDiscTrackState;
  /** The folder the file is in now. */
  folder?: string;
}

export type MultiDiscTrackState = "untouched" | "renamed" | "tagged" | "moved";

export type MultiDiscCoverAction = "kept" | "moved" | "identical" | "different";

export interface MultiDiscFolder {
  album_id: string;
  dir_name: string;
  disc: number | null;
  destination: boolean;
  /** `done`: an interrupted conversion already did it. */
  covers: { name: string; action: MultiDiscCoverAction; done?: boolean }[];
  sidecar: boolean;
  sidecar_done?: boolean;
  other_files: string[];
  /** System clutter (.DS_Store and the like) that goes to Musaic's trash
   *  so the emptied folder can be removed. */
  junk?: { name: string; done: boolean }[];
  /** The folder is still on disk. */
  exists?: boolean;
}

/** One step of a conversion streaming its progress. */
export interface MultiDiscProgress {
  step: string;
  done: number;
  total: number;
}

export interface MultiDiscBlocker {
  code: string;
  message: string;
}

export interface MultiDiscConversionPreview {
  finding_id: number;
  album_id: string;
  state: MultiDiscPreviewState;
  /** No blocker: the plan could run as it stands. */
  convertible: boolean;
  /** Whether converting can be started from the dialog: a convertible set
   *  kept in one folder (sets in several folders cannot convert yet). */
  conversion_available: boolean;
  /** What the conversion must be sent back, so a plan that changed since
   *  the user looked is never carried out. */
  digest?: string;
  /** An interrupted conversion of this album, which converting finishes. */
  resume?: { record: string; started_at: string; attempts: number } | null;
  /** A set kept in several folders: how far the database side got. */
  gather?: { folded: boolean; folder_renamed: boolean; merges: number; orphan_rows: number } | null;
  /** Where the records of conversions are kept. */
  records_dir?: string | null;
  headline: string;
  /** Why no conversion is offered (an incomplete set, or no set). */
  reason: string | null;
  numbering?: "per_disc" | "continuous" | "irregular" | null;
  disc_total?: number | null;
  present_discs?: number[];
  absent_discs?: number[];
  members: MultiDiscMember[];
  is_compilation?: boolean;
  template?: string;
  dir_template?: string;
  tracks: MultiDiscTrack[];
  track_total?: number | null;
  target_folder?: { current: string | null; new: string | null } | null;
  removes_disc_tags?: boolean;
  folders: MultiDiscFolder[];
  blockers: MultiDiscBlocker[];
}

/** One file of a multi-disc conversion's result. */
export interface MultiDiscConvertFile {
  filename: string;
  new_filename: string;
  rename: "renamed" | "already" | "failed" | "not_reached";
  tags: "written" | "already" | "failed" | "not_reached";
  /** A set kept in several folders: the move into the set's folder. */
  move?: "moved" | "already" | "stays" | "failed" | "not_reached";
  /** The folder it was in before. */
  folder?: string;
  reason_code?: string;
  user_message?: string;
}

/** One folder of a set gathered from several folders, after the run. */
export interface MultiDiscConvertFolder {
  dir_name: string;
  destination: boolean;
  /** `removed` when it was emptied, `kept` with `left` when files stay. */
  outcome: "destination" | "removed" | "kept" | "not_reached";
  left: string[];
}

/** Response from POST /api/checks/multi-disc-layout/findings/{id}/convert,
 *  or a refusal (409) turned into one by the api helper (`refused`). */
export interface MultiDiscConvertResult {
  ok: boolean;
  /** Nothing was changed: the server refused before writing. */
  refused?: boolean;
  album_id?: string;
  /** A plain sentence for the user. */
  user_message: string;
  reason_code?: string;
  files?: MultiDiscConvertFile[];
  folder?: {
    old: string;
    new: string;
    outcome: "renamed" | "unchanged" | "failed" | "not_reached";
    reason_code?: string;
    user_message?: string;
  };
  /** Where the record of the conversion is kept. */
  record?: string;
  attempts?: number;
  blockers?: MultiDiscBlocker[];
  /** A set gathered from several folders. */
  folders?: MultiDiscConvertFolder[];
  merged?: string[];
}

/** A plan entry the rename left alone. */
export interface SkippedRename {
  old: string;
  new: string;
  reason: "source_missing" | "already_renamed";
}

export interface FilenameRenameResult {
  ok: boolean;
  renamed: number;
  errors: string[];
  /** The message also sums these up. Present only when non-empty. */
  skipped?: SkippedRename[];
  /** The technical text, for the log. Not for display. */
  message: string;
  /** Present when the rename was refused, failed, or needs a note: a stable
   *  code and the plain sentence to show the user. */
  reason_code?: string;
  user_message?: string;
  /** True when the album dir was renamed as part of this apply. */
  dir_renamed?: boolean;
  /** New album_id after the dir rename (sha1 of the new path).
   *  Only set when dir_renamed is true. */
  new_album_id?: string;
}

// ----- Apply (SSE) -----

/** Album-level fields an apply can remove from the files (`clear`). */
export type ClearableAlbumField =
  | "title" | "album_artist" | "artist" | "year" | "genre"
  | "album_artist_sort" | "artist_sort" | "album_sort";

/** Per-track fields an apply can remove (`track_clear`). TRACKNUMBER cannot
 *  be removed (the scanner orders tracks by it): set a number instead. */
export type ClearableTrackField = "title" | "artist" | "artist_sort";

export interface ApplyPayload {
  /** Values to write. An empty or whitespace-only value means "no change":
   *  a suggestion never removes a tag (use `clear`). */
  suggestions?: Partial<Record<"title" | "album_artist" | "artist" | "year" | "genre", string>>;
  /** Per-file overrides keyed by FLAC filename. Allowed inner keys:
   *  "title", "artist", "tracknumber". */
  track_suggestions?: Record<string, Partial<Record<"title" | "artist" | "tracknumber", string>>>;
  /** Fields to remove from every FLAC of the album — only for an explicit
   *  user request (a field the user blanked), never for a source's missing
   *  data. A field may not be both suggested and cleared (422). */
  clear?: ClearableAlbumField[];
  /** Per-file fields to remove, keyed by FLAC filename. */
  track_clear?: Record<string, ClearableTrackField[]>;
  cover_url?: string | null;
  dry_run?: boolean;
}

/** One tag's change in a file. `new: null` = the tag is removed. */
export interface TagDiff {
  old: string;
  new: string | null;
}

export interface FileChange {
  path: string;
  filename: string;
  diffs: Record<string, TagDiff>;
  art_changed: boolean;
  saved: boolean;
  error: string | null;
  /** What a save that rewrote the file could not keep of it (extended
   *  attributes, owner, a hard link). Present only when non-empty. */
  warnings?: string[];
}

/** A file whose TRACKNUMBER was written but which kept its old name. */
export interface RenameFailure {
  filename: string;
  new_filename: string;
  /** The technical text, for the log. Not for display. */
  error: string;
  reason_code?: string;
  /** The plain sentence to show the user. */
  user_message?: string;
}

export type ApplyEvent =
  | { type: "cover_downloading"; url: string }
  | { type: "cover_downloaded"; bytes: number; mime: string }
  | { type: "start"; album_path: string; dry_run: boolean; file_count: number; suggestions_applied: Record<string, string>; cleared?: string[]; has_cover_change: boolean }
  | { type: "file_started"; index: number; filename: string; path: string }
  | { type: "file_done"; change: FileChange }
  | { type: "done"; saved_count: number; art_changed_count: number; errors: number; suggestions_applied: Record<string, string>; cleared?: string[]; rename_failures?: RenameFailure[] }
  | { type: "updated_album"; album: Album }
  | { type: "removed_album"; id: string; path: string }
  | { type: "error"; detail: string };
