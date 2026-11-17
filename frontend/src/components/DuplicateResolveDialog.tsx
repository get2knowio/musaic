/**
 * Per-finding resolve dialog for the duplicate-tracks check.
 *
 * Lists every duplicate file in the cluster (filename + duration) and lets
 * the user move copies to the trash one at a time. Each one:
 *   - posts to /api/checks/duplicate-tracks/findings/{fid}/delete-file
 *   - server moves the FLAC into the app's trash folder, drops the
 *     fingerprint row, re-runs checks
 *   - SSE pushes the cache update; useFindings refetches
 *   - the finding's `detail.filenames` list shrinks; the dialog re-renders
 *   - once we're down to 1 file the cluster is no longer a duplicate, the
 *     check resolves the finding, and the FindingFixButton (which renders
 *     the dialog's parent) unmounts us. The dialog closes naturally.
 *
 * Each move needs a confirmation (a nested Radix Dialog). The copy says
 * what happens to the file, not how (#196); the trash folder lives in
 * the config dir, so it is named in words, not as a path.
 */
import { useMemo, useState } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { AlertTriangle, Database, Files, Loader2, Music, ShieldCheck, Trash2, X } from "lucide-react";

/** How the copy names the trash: the folder sits in the config dir,
 *  which a user can move, and it is not the system Trash. */
const TRASH_NAME = "Musaic's trash folder";
import { toast } from "sonner";

import RevealInFileManager from "@/components/RevealInFileManager";
import { fetchDuplicateClusterPreview } from "@/lib/api";
import {
  useDeleteDuplicateFile,
  useDuplicateFindingCanonical,
} from "@/hooks/useChecks";
import type { DuplicateClusterPreview, Finding } from "@/lib/types";

interface Props {
  finding: Finding;
  open: boolean;
  onClose: () => void;
}

interface DuplicateDetail {
  recording_mbid?: string;
  filenames?: string[];
  durations_s?: number[];
  duplicate_count?: number;
  /** Filename the backend recommends keeping (others have Windows-
   *  incompatible characters). Null when all candidates are equally
   *  portable. */
  recommended_keep?: string | null;
  /** Inline-copy explanation of why this file is recommended. Null when
   *  recommended_keep is null. */
  recommended_reason?: string | null;
}

const PREVIEW_KEY = (fid: number) => ["duplicate-cluster-preview", fid] as const;

export default function DuplicateResolveDialog({ finding, open, onClose }: Props) {
  const qc = useQueryClient();
  // Always re-derive the cluster on open against current
  // track_fingerprints — the finding's detail.filenames is a snapshot
  // from check-run time and may have grown stale (sibling delete
  // landed, file re-encoded, name changed). The preview refetches
  // after each successful delete so the dialog state stays in lockstep
  // with disk.
  const preview = useQuery<DuplicateClusterPreview>({
    queryKey: PREVIEW_KEY(finding.id),
    queryFn: () => fetchDuplicateClusterPreview(finding.id),
    enabled: open,
    staleTime: 0,
    gcTime: 0,
    retry: 1,
  });

  // Stored detail is the fallback during the initial fetch and on
  // network errors. Intentionally lossy — it may show rows the user
  // already trashed — but better than rendering empty.
  const storedDetail = (finding.detail || {}) as DuplicateDetail;
  const filenames = preview.data?.filenames ?? storedDetail.filenames ?? [];
  const durations = preview.data?.durations_s ?? storedDetail.durations_s ?? [];
  const recordingMbid =
    preview.data?.recording_mbid ?? storedDetail.recording_mbid;
  const recommendedKeep =
    preview.data?.recommended_keep ?? storedDetail.recommended_keep ?? null;
  const recommendedReason =
    preview.data?.recommended_reason
    ?? storedDetail.recommended_reason
    ?? null;
  const previewIsEmpty = preview.data?.is_empty === true;
  const removedFilenames = preview.data?.removed_filenames ?? [];

  // Lazy MB lookup for the cluster's recording_mbid. Only fires when the
  // dialog is open AND we have an MBID to look up. Cached server-side so
  // repeat opens cost nothing.
  const canonicalQuery = useDuplicateFindingCanonical(finding.id, open);
  const canonical = canonicalQuery.data;

  // Compute which filename's tags match the canonical title best.
  // The "matches MB" badge goes on that one — that's the file the user
  // should keep when resolving an audio-identical-but-different-tag
  // cluster (which is what fingerprint similarity catches but the user
  // can't disambiguate from filenames alone).
  const mbMatchFilename = useMemo<string | null>(() => {
    if (!canonical?.title || filenames.length === 0) return null;
    const target = _normalizeForMatch(canonical.title);
    let best: string | null = null;
    let bestRatio = 0;
    for (const fn of filenames) {
      const r = _matchRatio(target, _normalizeForMatch(_stripPrefix(fn)));
      if (r > bestRatio) {
        bestRatio = r;
        best = fn;
      }
    }
    // Require a meaningful match — if every filename scores poorly
    // we'd rather not lie. Threshold lets "Theme From Harbor Lights"
    // (ratio ~1.0) win cleanly while rejecting "All I Want Tonight" (~0.0).
    return bestRatio >= 0.6 ? best : null;
  }, [canonical?.title, filenames]);

  const deleteMutation = useDeleteDuplicateFile();
  const [pendingDelete, setPendingDelete] = useState<string | null>(null);

  const onConfirmDelete = () => {
    if (!pendingDelete) return;
    const filename = pendingDelete;
    deleteMutation.mutate(
      { findingId: finding.id, filename },
      {
        onSuccess: () => {
          toast.success(`Moved ${filename} to trash`, {
            description: `You can move it back from ${TRASH_NAME}.`,
          });
          setPendingDelete(null);
          // Refetch the cluster so the file list shrinks immediately;
          // otherwise the row sticks around until the finding refetch
          // round-trips, and the user can briefly re-target the same
          // file. The cluster-preview is the authoritative source.
          qc.invalidateQueries({ queryKey: PREVIEW_KEY(finding.id) });
        },
        onError: (e) => {
          toast.error((e as Error).message || "Couldn't move file to trash");
          setPendingDelete(null);
        },
      },
    );
  };

  return (
    <Dialog.Root open={open} onOpenChange={(o) => { if (!o) onClose(); }}>
      <Dialog.Portal>
        <Dialog.Overlay className="fixed inset-0 z-40 bg-black/45 backdrop-blur-[1px]" />
        <Dialog.Content
          className="fixed left-1/2 top-1/2 z-50 -translate-x-1/2 -translate-y-1/2 w-[min(640px,95vw)] bg-panel border border-line rounded-md shadow-sheet outline-none flex flex-col"
          aria-describedby={undefined}
        >
          <div className="flex items-center justify-between px-4 h-11 border-b border-line">
            <div className="flex items-center gap-2">
              <Files className="w-3.5 h-3.5 text-accent" />
              <Dialog.Title className="font-semibold text-md">
                Remove duplicate copies
              </Dialog.Title>
            </div>
            <Dialog.Close asChild>
              <button
                className="w-8 h-8 grid place-items-center text-fg-3 hover:text-fg hover:bg-bg-3 rounded transition"
                aria-label="Close"
              >
                <X className="w-4 h-4" />
              </button>
            </Dialog.Close>
          </div>

          <div className="px-4 py-3.5">
            <p className="text-sm text-fg-2 mb-1">
              {filenames.length} files in this album contain the same recording.
              Move the copies you don't want to the trash; the last copy always
              stays.
            </p>
            {recordingMbid && (
              <p className="mono text-xs text-fg-4 mb-3">
                MusicBrainz recording: {recordingMbid}
              </p>
            )}

            {preview.isLoading && !preview.data && (
              <div className="mb-3 flex items-center gap-2 text-xs text-fg-3 bg-panel border border-line rounded px-3 py-2">
                <Loader2 className="w-3.5 h-3.5 spin shrink-0" />
                <span>Checking which copies are still on disk…</span>
              </div>
            )}

            {removedFilenames.length > 0 && !previewIsEmpty && (
              <div className="mb-3 flex items-start gap-2 text-xs text-fg-3 leading-relaxed bg-panel border border-line rounded px-3 py-2">
                <AlertTriangle className="w-3.5 h-3.5 text-fg-4 shrink-0 mt-0.5" />
                <div>
                  {removedFilenames.length === 1
                    ? "1 file the check found has since been moved or renamed"
                    : `${removedFilenames.length} files the check found have since been moved or renamed`}
                  ; showing the copies still on disk.
                </div>
              </div>
            )}

            {/* MusicBrainz canonical info — what MB says this recording
                actually is. Lazy-fetched on dialog open. The matching
                filename (if any) gets a "Matches MB" badge below. */}
            <CanonicalBanner
              loading={canonicalQuery.isLoading}
              canonical={canonical}
              matched={mbMatchFilename != null}
            />

            {recommendedKeep && recommendedReason && (
              <div className="mb-3 flex items-start gap-2 text-xs text-fg-2 leading-relaxed bg-ok-soft border border-ok/30 rounded px-3 py-2">
                <ShieldCheck className="w-3.5 h-3.5 text-ok shrink-0 mt-0.5" />
                <span>{recommendedReason}</span>
              </div>
            )}

            <div className="border border-line rounded-md bg-panel-2 divide-y divide-line">
              {previewIsEmpty ? (
                <div className="px-3 py-4 text-sm text-fg-3 text-center italic">
                  No duplicates are left. This finding clears the next time
                  the check runs.
                </div>
              ) : filenames.length === 0 ? (
                <div className="px-3 py-4 text-sm text-fg-3 text-center italic">
                  No duplicates are left. This finding clears the next time
                  the check runs.
                </div>
              ) : (
                filenames.map((fname, i) => (
                  <FileRow
                    key={fname}
                    filename={fname}
                    duration={durations[i]}
                    isRecommended={fname === recommendedKeep}
                    isMbMatch={fname === mbMatchFilename}
                    onDelete={() => setPendingDelete(fname)}
                    disabled={deleteMutation.isPending}
                    isLast={filenames.length <= 1}
                  />
                ))
              )}
            </div>

            <p className="text-xs text-fg-4 mt-3 leading-relaxed">
              Nothing is deleted: files you move to the trash go to
              {" "}{TRASH_NAME}. To get one back, move it from there into the
              album folder; empty the folder yourself once you're sure.
              {/* Desktop window only. */}
              <RevealInFileManager target={{ kind: "trash" }} className="ml-1.5 align-baseline" />
            </p>
          </div>

          <div className="flex items-center justify-end gap-2 px-4 py-3 border-t border-line bg-panel-2 rounded-b-md">
            <Dialog.Close asChild>
              <button className="h-8 px-3 border border-line rounded text-sm hover:bg-bg-3 transition">
                Done
              </button>
            </Dialog.Close>
          </div>
        </Dialog.Content>
      </Dialog.Portal>

      {/* Inline confirm dialog (nested Radix Dialog). Keeps the parent open
          so the user lands back on the file list after confirming. */}
      <Dialog.Root open={pendingDelete !== null} onOpenChange={(o) => { if (!o) setPendingDelete(null); }}>
        <Dialog.Portal>
          <Dialog.Overlay className="fixed inset-0 z-[60] bg-black/55 backdrop-blur-[1px]" />
          <Dialog.Content
            className="fixed left-1/2 top-1/2 z-[61] -translate-x-1/2 -translate-y-1/2 w-[min(420px,92vw)] bg-panel border border-line rounded-md shadow-sheet outline-none"
            aria-describedby={undefined}
          >
            <div className="px-4 pt-4 pb-3">
              <Dialog.Title className="text-md font-semibold mb-1.5 flex items-center gap-2">
                <AlertTriangle className="w-4 h-4 text-warn" />
                Move this file to trash?
              </Dialog.Title>
              <Dialog.Description className="text-sm text-fg-3 leading-relaxed">
                <span className="mono text-fg-2 break-all">{pendingDelete}</span>
                <br />
                will be moved to {TRASH_NAME}. You can move it back from there
                if you change your mind.
              </Dialog.Description>
            </div>
            <div className="flex items-center justify-end gap-2 px-4 py-3 border-t border-line bg-panel-2 rounded-b-md">
              <button
                onClick={() => setPendingDelete(null)}
                disabled={deleteMutation.isPending}
                className="h-8 px-3 border border-line rounded text-sm hover:bg-bg-3 transition disabled:opacity-60"
              >
                Cancel
              </button>
              <button
                onClick={onConfirmDelete}
                disabled={deleteMutation.isPending}
                className="h-8 px-3 rounded bg-err text-white text-sm font-medium hover:opacity-90 transition disabled:opacity-60 inline-flex items-center gap-1.5"
              >
                {deleteMutation.isPending ? (
                  <><Loader2 className="w-3.5 h-3.5 spin" /> Moving…</>
                ) : (
                  <><Trash2 className="w-3.5 h-3.5" /> Move to trash</>
                )}
              </button>
            </div>
          </Dialog.Content>
        </Dialog.Portal>
      </Dialog.Root>
    </Dialog.Root>
  );
}

function FileRow({
  filename,
  duration,
  isRecommended,
  isMbMatch,
  onDelete,
  disabled,
  isLast,
}: {
  filename: string;
  duration?: number;
  /** True when this is the file the backend's portability tie-breaker
   *  picked. Renders an inline "Recommended to keep" badge so the user
   *  can see at a glance which copy is the more portable filename. */
  isRecommended: boolean;
  /** True when this filename's title segment matches the MB canonical
   *  title for the cluster's recording_mbid. Strongest possible "keep
   *  this one" signal — its tags actually agree with what MB says the
   *  audio is. */
  isMbMatch: boolean;
  onDelete: () => void;
  disabled: boolean;
  isLast: boolean;
}) {
  return (
    <div className="px-3 py-2 flex items-center gap-3 text-sm">
      <Music className="w-4 h-4 text-fg-4 shrink-0" />
      <div className="flex-1 min-w-0">
        <div className="flex items-center gap-2 flex-wrap">
          <div className="mono text-xs text-fg break-all" title={filename}>{filename}</div>
          {isMbMatch && (
            <span
              className="inline-flex items-center gap-1 mono text-[10px] uppercase tracking-[0.06em] font-semibold text-ok bg-ok-soft border border-ok/30 px-1.5 py-0.5 rounded-full shrink-0"
              title="Filename's title matches what MusicBrainz says the audio is — this is the correctly-tagged copy"
            >
              <Database className="w-3 h-3" />
              Matches MB
            </span>
          )}
          {isRecommended && (
            <span
              className="inline-flex items-center gap-1 mono text-[10px] uppercase tracking-[0.06em] font-semibold text-ok bg-ok-soft border border-ok/30 px-1.5 py-0.5 rounded-full shrink-0"
              title="More portable filename — keeping this one is recommended for Windows / NAS compatibility"
            >
              <ShieldCheck className="w-3 h-3" />
              Keep this
            </span>
          )}
        </div>
        {typeof duration === "number" && duration > 0 && (
          <div className="mono text-[11px] text-fg-4 mt-0.5">
            {fmtDuration(duration)}
          </div>
        )}
      </div>
      <button
        onClick={onDelete}
        disabled={disabled || isLast}
        className="h-7 px-2 rounded border border-line text-xs text-fg-2 hover:bg-bg-3 hover:text-err transition disabled:opacity-50 disabled:cursor-not-allowed inline-flex items-center gap-1"
        title={isLast ? "The last copy always stays" : `Move this copy to ${TRASH_NAME}`}
      >
        <Trash2 className="w-3 h-3" />
        Move to trash
      </button>
    </div>
  );
}

function fmtDuration(seconds: number): string {
  const m = Math.floor(seconds / 60);
  const s = Math.floor(seconds % 60);
  return `${m}:${s.toString().padStart(2, "0")}`;
}

/** "MB says: <title> — <artist>" line above the file list. Loading/empty
 *  states are handled inline so we don't pop the layout while the lookup
 *  is in flight. When `matched` is false (no filename clearly matches the
 *  canonical title), the banner gets a subtle warn tone — that's a hint
 *  that the AcoustID cluster might be wrong, since neither file's tags
 *  agree with what MB thinks the audio is. */
function CanonicalBanner({
  loading,
  canonical,
  matched,
}: {
  loading: boolean;
  canonical: import("@/lib/api").CanonicalRecording | undefined;
  matched: boolean;
}) {
  if (loading) {
    return (
      <div className="mb-3 flex items-center gap-2 text-xs text-fg-3 bg-panel border border-line rounded px-3 py-2">
        <Loader2 className="w-3.5 h-3.5 spin text-fg-4" />
        <span>Looking up the recording on MusicBrainz…</span>
      </div>
    );
  }
  if (!canonical || !canonical.title) {
    // A cluster with no MBID is an ordinary case: say nothing. Every other
    // reason (MB has no such recording, offline, rate limited, ...) is shown
    // as the server's sentence, so the user knows what happened and what to do.
    if (!canonical?.reason || canonical.reason_code === "no_mbid") return null;
    return (
      <div
        role="status"
        data-reason-code={canonical.reason_code ?? undefined}
        className="mb-3 flex items-start gap-2 text-xs leading-relaxed text-fg-3 bg-panel border border-line rounded px-3 py-2"
      >
        <Database className="w-3.5 h-3.5 shrink-0 mt-0.5 text-fg-4" />
        <span>{canonical.reason}</span>
      </div>
    );
  }
  const tone = matched
    ? "border-ok/30 bg-ok-soft text-fg-2"
    : "border-warn/30 bg-warn-soft text-fg-2";
  return (
    <div className={`mb-3 flex items-start gap-2 text-xs leading-relaxed border rounded px-3 py-2 ${tone}`}>
      <Database className={`w-3.5 h-3.5 shrink-0 mt-0.5 ${matched ? "text-ok" : "text-warn"}`} />
      <div className="min-w-0">
        <div>
          MusicBrainz says this audio is{" "}
          <b>"{canonical.title}"</b>
          {canonical.artist && (
            <> by <b>{canonical.artist}</b></>
          )}.
        </div>
        {!matched && (
          <div className="text-fg-3 mt-1">
            None of the file names match that title. The audio match may
            be wrong, or the files are mislabelled.
          </div>
        )}
      </div>
    </div>
  );
}

// ---------- Helpers for "which filename matches MB?" ----------

/** Strip a leading "NN - " or "NN. " track-number prefix and the .flac
 *  extension, returning the rest of the filename for comparison. Mirrors
 *  the backend `_readable_label` helper (kept in sync deliberately). */
function _stripPrefix(filename: string): string {
  let name = filename;
  if (name.toLowerCase().endsWith(".flac")) name = name.slice(0, -5);
  let m = name.match(/^\s*\d+\s*-\s*(.*)$/);
  if (m) name = m[1];
  m = name.match(/^\s*\d+\s*\.\s*(.*)$/);
  if (m) name = m[1];
  return name.trim();
}

/** Lowercase + strip non-alphanumerics for fuzzy comparison. Filenames
 *  often embed extra metadata ("Artist - Title", "Title (Remastered)"),
 *  and we want a substring of the canonical title to count as a match. */
function _normalizeForMatch(s: string): string {
  return s.toLowerCase().replace(/[^a-z0-9]+/g, " ").trim();
}

/** How well does a normalized canonical title appear in a normalized
 *  filename? Substring → 1.0. Otherwise an approximate ratio from
 *  shared-token overlap. Cheap and good enough — we only call this in
 *  a per-cluster useMemo. */
function _matchRatio(needle: string, hay: string): number {
  if (!needle || !hay) return 0;
  if (hay.includes(needle)) return 1.0;
  const needleTokens = new Set(needle.split(" ").filter((t) => t.length >= 3));
  if (needleTokens.size === 0) return 0;
  const hayTokens = new Set(hay.split(" "));
  let hits = 0;
  for (const t of needleTokens) if (hayTokens.has(t)) hits++;
  return hits / needleTokens.size;
}
