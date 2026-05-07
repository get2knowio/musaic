/**
 * Per-finding resolve dialog for the mb-canonical-mismatch check.
 *
 * Shows the album, the MB-canonical artist credit, and a list of
 * field-level diffs (album_artist + per-track artists). The user
 * opts in field-by-field via checkboxes; Apply writes only the
 * accepted fields.
 *
 * Chain mode: walks through findings after each apply, same pattern
 * as the other resolve dialogs.
 */
import { useEffect, useMemo, useState } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { Disc3, FileAudio, Loader2, X } from "lucide-react";
import { toast } from "sonner";

import { applyMbCanonical } from "@/lib/api";
import {
  CHECKS_QUERY_KEY,
  CHECK_DETAIL_KEY,
  FINDINGS_QUERY_KEY,
} from "@/hooks/useChecks";
import type {
  Finding,
  MbCanonicalMismatchDetail,
  MbCanonicalAlbumArtistDiff,
  MbCanonicalTrackArtistDiff,
} from "@/lib/types";

interface Props {
  finding: Finding;
  open: boolean;
  onClose: () => void;
  findings?: Finding[];
}

export default function MbCanonicalResolveDialog({
  finding,
  open,
  onClose,
  findings,
}: Props) {
  const qc = useQueryClient();

  const [chainSnapshot, setChainSnapshot] = useState<Finding[]>([]);
  const [currentFindingId, setCurrentFindingId] = useState<number>(finding.id);

  useEffect(() => {
    if (!open) return;
    const filtered = (findings ?? [finding]).filter(
      (f) => f.check_id === finding.check_id && !f.resolved_at && !f.ignored_at,
    );
    setChainSnapshot(filtered.length > 0 ? filtered : [finding]);
    setCurrentFindingId(finding.id);
  }, [open, finding.id]);   // eslint-disable-line react-hooks/exhaustive-deps

  const currentFinding =
    chainSnapshot.find((f) => f.id === currentFindingId) ?? finding;
  const currentIndex = chainSnapshot.findIndex((f) => f.id === currentFindingId);
  const totalInChain = chainSnapshot.length;
  const hasNext = currentIndex >= 0 && currentIndex < totalInChain - 1;

  const detail = (currentFinding.detail || {}) as MbCanonicalMismatchDetail;
  const albumDiff: MbCanonicalAlbumArtistDiff | null = detail.album_artist_diff ?? null;
  const trackDiffs: MbCanonicalTrackArtistDiff[] = detail.track_artist_diffs ?? [];

  // Per-field opt-in state. Default all to checked when the dialog
  // opens for a new finding — most users will want everything;
  // unchecking individual lines is cheaper than re-checking all.
  const [acceptAlbum, setAcceptAlbum] = useState(true);
  const [acceptedFiles, setAcceptedFiles] = useState<Set<string>>(new Set());

  useEffect(() => {
    if (!open) return;
    setAcceptAlbum(albumDiff !== null);
    setAcceptedFiles(new Set(trackDiffs.map((t) => t.filename)));
  }, [open, currentFindingId]);   // eslint-disable-line react-hooks/exhaustive-deps

  const acceptedCount =
    (acceptAlbum && albumDiff ? 1 : 0) + acceptedFiles.size;

  const mutation = useMutation({
    mutationFn: () =>
      applyMbCanonical(currentFinding.id, {
        acceptAlbumArtist: acceptAlbum && albumDiff !== null,
        acceptTrackFilenames: Array.from(acceptedFiles),
      }),
    onSuccess: (r) => {
      toast.success(r.message);
      qc.invalidateQueries({ queryKey: FINDINGS_QUERY_KEY });
      qc.invalidateQueries({ queryKey: CHECKS_QUERY_KEY });
      qc.invalidateQueries({
        queryKey: CHECK_DETAIL_KEY("mb-canonical-mismatch"),
      });
      if (hasNext) {
        const next = chainSnapshot[currentIndex + 1];
        setCurrentFindingId(next.id);
      } else {
        if (totalInChain > 1) {
          toast.success(`Done — processed ${totalInChain} albums.`, {
            duration: 3000,
          });
        }
        onClose();
      }
    },
    onError: (e) => {
      toast.error((e as Error).message || "Apply failed");
    },
  });

  const onApply = () => {
    if (acceptedCount === 0) return;
    mutation.mutate();
  };

  const onSkipNext = () => {
    if (hasNext) {
      setCurrentFindingId(chainSnapshot[currentIndex + 1].id);
    } else {
      onClose();
    }
  };

  const toggleFile = (filename: string) => {
    setAcceptedFiles((prev) => {
      const next = new Set(prev);
      if (next.has(filename)) {
        next.delete(filename);
      } else {
        next.add(filename);
      }
      return next;
    });
  };

  const toggleAll = () => {
    if (acceptedFiles.size === trackDiffs.length) {
      setAcceptedFiles(new Set());
    } else {
      setAcceptedFiles(new Set(trackDiffs.map((t) => t.filename)));
    }
  };

  return (
    <Dialog.Root open={open} onOpenChange={(o) => { if (!o) onClose(); }}>
      <Dialog.Portal>
        <Dialog.Overlay className="fixed inset-0 z-40 bg-black/45 backdrop-blur-[1px]" />
        <Dialog.Content
          className="fixed left-1/2 top-1/2 z-50 -translate-x-1/2 -translate-y-1/2 w-[min(720px,95vw)] bg-panel border border-line rounded-md shadow-sheet outline-none flex flex-col"
          aria-describedby={undefined}
        >
          <div className="flex items-center justify-between px-4 h-11 border-b border-line">
            <div className="flex items-center gap-2 min-w-0">
              <Disc3 className="w-3.5 h-3.5 text-accent shrink-0" />
              <Dialog.Title className="font-semibold text-md truncate">
                Apply MusicBrainz canonical names
              </Dialog.Title>
              {totalInChain > 1 && currentIndex >= 0 && (
                <span className="mono text-[11px] text-fg-4 tabular-nums shrink-0">
                  · {currentIndex + 1} of {totalInChain}
                </span>
              )}
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

          <div className="px-4 py-3.5 space-y-3 overflow-y-auto max-h-[70vh]">
            <div>
              <div className="mono text-xs text-fg-3 truncate" title={detail.dir_name}>
                {detail.dir_name}
                {detail.is_compilation && (
                  <span className="ml-2 text-accent">[compilation]</span>
                )}
              </div>
              <div className="mono text-[11px] text-fg-4 mt-0.5 truncate">
                MB release-group: <code className="text-fg-3">{detail.mbid}</code>
                {detail.score && (
                  <span className="ml-2">match score {detail.score}</span>
                )}
              </div>
            </div>

            {/* Album-level diff */}
            {albumDiff && (
              <label className="flex items-start gap-3 border border-line rounded-md bg-panel-2 px-3 py-2.5 cursor-pointer hover:bg-bg-3 transition">
                <input
                  type="checkbox"
                  checked={acceptAlbum}
                  onChange={(e) => setAcceptAlbum(e.target.checked)}
                  className="mt-1 accent-accent shrink-0"
                />
                <div className="flex-1 min-w-0">
                  <div className="mono text-[10px] uppercase tracking-[0.08em] text-fg-4 mb-1">
                    album_artist
                  </div>
                  <div className="grid grid-cols-[1fr_auto_1fr] gap-2 items-center text-sm min-w-0">
                    <div className="mono text-[12px] text-warn line-through truncate" title={albumDiff.current}>
                      {albumDiff.current}
                    </div>
                    <div className="text-fg-4 mono text-xs">→</div>
                    <div className="mono text-[12px] text-ok truncate" title={albumDiff.canonical}>
                      {albumDiff.canonical}
                    </div>
                  </div>
                </div>
              </label>
            )}

            {/* Per-track diffs */}
            {trackDiffs.length > 0 && (
              <div className="border border-line rounded-md bg-panel-2 divide-y divide-line">
                <div className="flex items-center px-3 py-1.5 border-b border-line text-[11px] mono uppercase tracking-[0.06em] text-fg-4">
                  <span>{trackDiffs.length} per-track artist diff{trackDiffs.length === 1 ? "" : "s"}</span>
                  <button
                    onClick={toggleAll}
                    className="ml-auto text-fg-3 hover:text-fg transition mono text-[11px]"
                  >
                    {acceptedFiles.size === trackDiffs.length ? "Uncheck all" : "Check all"}
                  </button>
                </div>
                {trackDiffs.map((t) => (
                  <label
                    key={t.filename}
                    className="flex items-start gap-3 px-3 py-2 cursor-pointer hover:bg-bg-3 transition"
                  >
                    <input
                      type="checkbox"
                      checked={acceptedFiles.has(t.filename)}
                      onChange={() => toggleFile(t.filename)}
                      className="mt-1 accent-accent shrink-0"
                    />
                    <FileAudio className="w-4 h-4 text-fg-4 shrink-0 mt-0.5" />
                    <div className="flex-1 min-w-0 text-sm">
                      <div className="grid grid-cols-[1fr_auto_1fr] gap-2 items-center min-w-0">
                        <div className="mono text-[12px] text-warn line-through truncate" title={t.current}>
                          {t.current}
                        </div>
                        <div className="text-fg-4 mono text-xs">→</div>
                        <div className="mono text-[12px] text-ok truncate" title={t.canonical}>
                          {t.canonical}
                        </div>
                      </div>
                      <div className="mono text-[11px] text-fg-4 truncate mt-0.5" title={t.filename}>
                        {t.filename}
                        {t.mb_title && (
                          <span className="ml-2 text-fg-3">· MB: {t.mb_title}</span>
                        )}
                      </div>
                    </div>
                  </label>
                ))}
              </div>
            )}

            <p className="text-xs text-fg-4 leading-relaxed">
              Accepted fields are written to the FLAC tags via the
              standard apply pipeline. The check re-runs after — if
              you accepted everything, the finding clears immediately.
            </p>
          </div>

          <div className="flex items-center justify-end gap-2 px-4 py-3 border-t border-line bg-panel-2 rounded-b-md">
            <Dialog.Close asChild>
              <button
                className="h-8 px-3 border border-line rounded text-sm hover:bg-bg-3 transition"
                disabled={mutation.isPending}
              >
                Cancel
              </button>
            </Dialog.Close>
            {hasNext && (
              <button
                onClick={onSkipNext}
                disabled={mutation.isPending}
                className="h-8 px-3 border border-line rounded text-sm text-fg-2 hover:bg-bg-3 transition disabled:opacity-60"
              >
                Skip →
              </button>
            )}
            <button
              onClick={onApply}
              disabled={mutation.isPending || acceptedCount === 0}
              className="h-8 px-3 rounded bg-accent text-accent-fg text-sm font-medium hover:opacity-90 transition disabled:opacity-60 inline-flex items-center gap-1.5"
            >
              {mutation.isPending ? (
                <><Loader2 className="w-3.5 h-3.5 spin" /> Applying…</>
              ) : (
                <>Apply {acceptedCount} field{acceptedCount === 1 ? "" : "s"}{hasNext ? " & next" : ""}</>
              )}
            </button>
          </div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
