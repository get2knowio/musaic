/**
 * Per-finding resolve dialog for the filename-mismatch check.
 *
 * Shows the album, the template that's being applied, and a table
 * of current → expected filenames for every mismatched track. The
 * user confirms (Apply) and the backend renames everything on disk.
 * The copy describes the outcome for the user's files, not the
 * mechanism (#196).
 *
 * Chain mode: when `findings` is supplied, the dialog walks through
 * the list after each Apply instead of closing — same pattern as
 * ArtistSortResolveDialog. Ideal for the inevitable first-run
 * sweep where hundreds of albums need renaming.
 */
import { useEffect, useMemo, useState } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import { useMutation, useQuery } from "@tanstack/react-query";
import { AlertTriangle, FileEdit, FolderInput, Loader2, X } from "lucide-react";
import { toast } from "sonner";

import { applyFilenameRename, fetchFilenameRenamePreview } from "@/lib/api";
import type {
  Finding,
  FilenameMismatchDetail,
  FilenameMismatchPlanPreview,
  FilenameRenameEntry,
  FilenameRenameResult,
} from "@/lib/types";

/** What the dialog says after a rename that did not go cleanly. */
interface RenameNotice {
  title: string;
  lines: string[];
}

const plural = (n: number, word: string) => `${n} ${word}${n === 1 ? "" : "s"}`;

/** Null when the rename did exactly what was asked. Otherwise the words
 *  for the user: what happened, to which files, and what they can do. */
function describeRenameResult(r: FilenameRenameResult): RenameNotice | null {
  const missing = (r.skipped ?? []).filter((e) => e.reason === "source_missing");
  if (r.ok && missing.length === 0) return null;
  if (r.ok) {
    return {
      title: `Renamed ${plural(r.renamed, "file")}, but ${plural(missing.length, "file")} ${missing.length === 1 ? "wasn't" : "weren't"} in the folder`,
      lines: [
        ...missing.map((e) => `${e.old} wasn't in the folder, so it was left alone.`),
        "It may have been moved or renamed outside Musaic. Rescan the library, then check again.",
      ],
    };
  }
  const lines = [r.message || "Something went wrong while renaming."];
  if (r.dir_renamed) {
    return {
      title: "The album folder was renamed, but the files inside were not",
      lines: [...lines, "Close this and run the check again to see what is left."],
    };
  }
  lines.push(
    r.renamed === 0
      ? "No file was changed. You can fix the cause and try again, or skip this album."
      : `${plural(r.renamed, "file")} had already been renamed. Run the check again to see what is left.`,
  );
  return { title: "Couldn't rename the files", lines };
}

interface Props {
  finding: Finding;
  open: boolean;
  onClose: () => void;
  /** Optional chain — see ArtistSortResolveDialog for the full
   *  rationale. When set, applies advance through the list rather
   *  than closing the dialog. */
  findings?: Finding[];
}

export default function FilenameMismatchResolveDialog({
  finding,
  open,
  onClose,
  findings,
}: Props) {
  // Chain snapshot — frozen at open so post-apply cache
  // invalidation doesn't shift positions under us.
  const [chainSnapshot, setChainSnapshot] = useState<Finding[]>([]);
  const [currentFindingId, setCurrentFindingId] = useState<number>(finding.id);

  useEffect(() => {
    if (!open) return;
    const filtered = (findings ?? [finding]).filter(
      (f) =>
        f.check_id === finding.check_id
        && !f.resolved_at
        && !f.ignored_at,
    );
    setChainSnapshot(filtered.length > 0 ? filtered : [finding]);
    setCurrentFindingId(finding.id);
    setNotice(null);
  }, [open, finding.id]);   // eslint-disable-line react-hooks/exhaustive-deps

  const currentFinding =
    chainSnapshot.find((f) => f.id === currentFindingId) ?? finding;
  const currentIndex = chainSnapshot.findIndex(
    (f) => f.id === currentFindingId,
  );
  const totalInChain = chainSnapshot.length;
  const hasNext = currentIndex >= 0 && currentIndex < totalInChain - 1;

  // Always pull the plan fresh from the server on open: the finding's
  // `detail.renames` is a snapshot from check-run time and may have
  // gone stale (sibling Resolve already ran, user edited tags manually,
  // files renamed by hand). The preview re-derives against current
  // disk + DB + settings so what the user sees is always what would
  // actually happen.
  const preview = useQuery<FilenameMismatchPlanPreview>({
    queryKey: ["filename-mismatch-plan-preview", currentFinding.id],
    queryFn: () => fetchFilenameRenamePreview(currentFinding.id),
    enabled: open,
    // The plan reflects current disk state — don't cache across renders
    // beyond the dialog's lifetime, and refetch when we re-open.
    staleTime: 0,
    gcTime: 0,
    retry: 1,
  });

  // Fall back to the finding's stored detail when the preview is still
  // loading or errored out, so the dialog never renders empty during a
  // network blip. The fallback is intentionally lossy (it may show
  // stale rows) but it's strictly better than a blank dialog.
  const fallbackDetail = (currentFinding.detail || {}) as unknown as FilenameMismatchDetail;
  const source: FilenameMismatchDetail = preview.data ?? fallbackDetail;
  const previewIsEmpty = preview.data?.is_empty === true;
  const albumMissing = preview.data?.album_missing === true;
  const previewError = preview.data?.error;

  const renames: FilenameRenameEntry[] = source.renames ?? [];
  const dirRename = source.dir_rename ?? null;
  const dirName = source.dir_name || currentFinding.album_id || "";
  const template = source.template || "";
  const dirTemplate = source.dir_template || "";
  const isCompilation = !!source.is_compilation;

  // Filter out renames that have a name-collision flag — those would
  // overwrite each other and need user attention before applying.
  const collisions = useMemo(
    () => renames.filter((r) => r.collides_with),
    [renames],
  );
  const safe = useMemo(
    () => renames.filter((r) => !r.collides_with),
    [renames],
  );

  const [notice, setNotice] = useState<RenameNotice | null>(null);

  const mutation = useMutation({
    mutationFn: () =>
      applyFilenameRename(currentFinding.id, {
        plan: safe.map((r) => ({ old: r.filename, new: r.expected })),
        dirRename: dirRename
          ? { old: dirRename.old, new: dirRename.new }
          : undefined,
      }),
    onSuccess: (r) => {
      if (r.ok) {
        toast.success(r.message);
      } else {
        toast.warning(r.message || "Some files couldn't be renamed");
      }
      // A rename that was refused, partly done, or skipped files is not
      // something to scroll past in a toast: stay here and say it.
      const problem = describeRenameResult(r);
      if (problem) {
        setNotice(problem);
        return;
      }
      // Cache invalidation is handled by the App-level
      // useLibraryEventsStream subscriber — execute_renames publishes
      // AlbumRenamed (with old==new for file-only renames), the bus
      // runs a per-album filename-mismatch rerun, and the resulting
      // findings/checks deltas flow through the SSE channels.
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
      toast.error((e as Error).message || "Couldn't rename the files");
    },
  });

  const outcome =
    previewIsEmpty || albumMissing ? null : _describeOutcome(safe.length, !!dirRename);

  const onApply = () => {
    if (safe.length === 0 && !dirRename) return;
    mutation.mutate();
  };

  const onSkipNext = () => {
    setNotice(null);
    if (hasNext) {
      const next = chainSnapshot[currentIndex + 1];
      setCurrentFindingId(next.id);
    } else {
      onClose();
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
              <FileEdit className="w-3.5 h-3.5 text-accent shrink-0" />
              <Dialog.Title className="font-semibold text-md truncate">
                Rename files to match your templates
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
              <div className="mono text-xs text-fg-3 truncate" title={dirName}>
                {dirName}
                {isCompilation && (
                  <span className="ml-2 text-accent">[compilation]</span>
                )}
              </div>
              <div className="mono text-[11px] text-fg-4 mt-0.5 truncate">
                File names: <code className="text-fg-3">{template}</code>
                {dirTemplate && (
                  <>
                    <span className="mx-1.5">·</span>
                    Folder name: <code className="text-fg-3">{dirTemplate}</code>
                  </>
                )}
              </div>
            </div>

            {outcome && (
              <p className="text-sm text-fg-2">{outcome}</p>
            )}

            {preview.isLoading && !preview.data && (
              <div className="flex items-center gap-2 text-xs text-fg-3 bg-panel-2 border border-line rounded px-3 py-2">
                <Loader2 className="w-3.5 h-3.5 spin shrink-0" />
                <span>Checking the album's files…</span>
              </div>
            )}

            {notice && (
              <div
                role="alert"
                className="flex items-start gap-2 text-xs text-fg-2 leading-relaxed bg-warn-soft border border-warn/30 rounded px-3 py-2"
              >
                <AlertTriangle className="w-3.5 h-3.5 text-warn shrink-0 mt-0.5" />
                <div className="min-w-0">
                  <div className="font-medium text-fg">{notice.title}</div>
                  {notice.lines.map((l, i) => (
                    <div key={i} className="mt-0.5 text-fg-3 break-words">{l}</div>
                  ))}
                </div>
              </div>
            )}

            {previewError && (
              <div className="flex items-start gap-2 text-xs text-fg-2 leading-relaxed bg-warn-soft border border-warn/30 rounded px-3 py-2">
                <AlertTriangle className="w-3.5 h-3.5 text-warn shrink-0 mt-0.5" />
                <div>
                  <div className="font-medium text-fg">Couldn't check the files again</div>
                  <div className="mt-0.5 text-fg-3">{previewError}. The list
                  below is from when the check last ran and may be out of
                  date.</div>
                </div>
              </div>
            )}

            {albumMissing && (
              <div className="flex items-start gap-2 text-xs text-fg-2 leading-relaxed bg-warn-soft border border-warn/30 rounded px-3 py-2">
                <AlertTriangle className="w-3.5 h-3.5 text-warn shrink-0 mt-0.5" />
                <div>
                  <div className="font-medium text-fg">Album folder not found</div>
                  <div className="mt-0.5 text-fg-3">
                    This album's folder isn't on disk right now, so there is
                    nothing to rename.
                  </div>
                </div>
              </div>
            )}

            {previewIsEmpty && !albumMissing && (
              <div className="flex items-start gap-2 text-xs text-fg-2 leading-relaxed bg-ok/10 border border-ok/30 rounded px-3 py-2">
                <FileEdit className="w-3.5 h-3.5 text-ok shrink-0 mt-0.5" />
                <div>
                  <div className="font-medium text-fg">Already named correctly</div>
                  <div className="mt-0.5 text-fg-3">
                    The files already match your templates, so there is
                    nothing to rename. This finding clears the next time the
                    check runs.
                  </div>
                </div>
              </div>
            )}

            {dirRename && (
              <div className="border border-accent/40 rounded-md bg-accent/5 px-3 py-2.5">
                <div className="flex items-center gap-1.5 mb-1.5 text-[11px] mono uppercase tracking-[0.06em] text-accent">
                  <FolderInput className="w-3 h-3" />
                  <span>Album folder</span>
                </div>
                <div className="grid grid-cols-[1fr_auto_1fr] gap-2 items-center text-sm min-w-0">
                  <div className="mono text-[12px] text-warn line-through truncate" title={dirRename.old}>
                    {dirRename.old}
                  </div>
                  <div className="text-fg-4 mono text-xs">→</div>
                  <div className="mono text-[12px] text-ok truncate" title={dirRename.new}>
                    {dirRename.new}
                  </div>
                </div>
                <div className="mt-1.5 text-[11px] text-fg-3 leading-relaxed">
                  The folder is renamed first, then the files inside it.
                </div>
              </div>
            )}

            {collisions.length > 0 && (
              <div className="flex items-start gap-2 text-xs text-fg-2 leading-relaxed bg-warn-soft border border-warn/30 rounded px-3 py-2">
                <AlertTriangle className="w-3.5 h-3.5 text-warn shrink-0 mt-0.5" />
                <div>
                  <div className="font-medium text-fg">Some files would get the same name</div>
                  <div className="mt-0.5 text-fg-3">
                    {collisions.length === 1
                      ? "1 file would get the same name as another file, so it will be left as it is."
                      : `${collisions.length} files would get the same name as another file, so they will be left as they are.`}
                    {" "}This usually means two tracks share a track number; fix
                    their tags and run the check again.
                  </div>
                </div>
              </div>
            )}

            {renames.length > 0 && (
              <div className="border border-line rounded-md bg-panel-2 divide-y divide-line">
                <div className="flex items-center px-3 py-1.5 border-b border-line text-[11px] mono uppercase tracking-[0.06em] text-fg-4">
                  <span>{renames.length} file{renames.length === 1 ? "" : "s"} to rename</span>
                </div>
                {renames.map((r) => (
                  <RenameRow key={r.filename} entry={r} />
                ))}
              </div>
            )}

            <p className="text-xs text-fg-4 leading-relaxed">
              Only the names change. The audio, the tags inside each file and
              any tags you added in Musaic stay with the files.
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
                title="Don't rename this one — go to the next finding"
              >
                Skip →
              </button>
            )}
            <button
              onClick={onApply}
              disabled={
                mutation.isPending
                || preview.isLoading
                || previewIsEmpty
                || albumMissing
                || (safe.length === 0 && !dirRename)
              }
              className="h-8 px-3 rounded bg-accent text-accent-fg text-sm font-medium hover:opacity-90 transition disabled:opacity-60 inline-flex items-center gap-1.5"
            >
              {mutation.isPending ? (
                <><Loader2 className="w-3.5 h-3.5 spin" /> Renaming…</>
              ) : preview.isLoading ? (
                <><Loader2 className="w-3.5 h-3.5 spin" /> Loading…</>
              ) : (
                <>{_buildApplyLabel(safe.length, !!dirRename, hasNext)}</>
              )}
            </button>
          </div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}

function _buildApplyLabel(
  fileCount: number,
  hasDirRename: boolean,
  hasNext: boolean,
): string {
  const parts: string[] = [];
  if (hasDirRename) parts.push("folder");
  if (fileCount > 0) parts.push(`${fileCount} file${fileCount === 1 ? "" : "s"}`);
  if (parts.length === 0) return hasNext ? "Skip" : "Nothing to rename";
  return `Rename ${parts.join(" and ")}${hasNext ? " & next" : ""}`;
}

/** One sentence saying what Apply will do to the user's files. Null when
 *  there is nothing to do (the empty / missing banners say so). */
function _describeOutcome(fileCount: number, hasDirRename: boolean): string | null {
  const files = `${fileCount} file${fileCount === 1 ? "" : "s"}`;
  if (fileCount > 0 && hasDirRename) {
    return `Rename the album folder and ${files} in it to match your templates.`;
  }
  if (fileCount > 0) return `Rename ${files} in this album to match your filename template.`;
  if (hasDirRename) return "Rename the album folder to match your folder template.";
  return null;
}


function RenameRow({ entry }: { entry: FilenameRenameEntry }) {
  const stale = !!entry.collides_with;
  return (
    <div className="px-3 py-2 text-sm min-w-0 grid grid-cols-[1fr_auto_1fr] gap-3 items-start">
      <div className="min-w-0">
        <div className="mono text-[11px] text-warn line-through truncate" title={entry.filename}>
          {entry.filename}
        </div>
        <div className="text-[11px] text-fg-4 mt-0.5 truncate" title={entry.title}>
          {entry.tracknumber ? `${entry.tracknumber}. ` : ""}
          {entry.title}
          {entry.artist ? ` — ${entry.artist}` : ""}
        </div>
      </div>
      <div className="text-fg-4 mono text-xs shrink-0 mt-1">→</div>
      <div className="min-w-0">
        <div
          className={`mono text-[11px] truncate ${stale ? "text-fg-4 line-through" : "text-ok"}`}
          title={entry.expected}
        >
          {entry.expected}
        </div>
        {stale && entry.collides_with && (
          <div className="text-[11px] text-warn mt-0.5 truncate" title={entry.collides_with}>
            would collide with {entry.collides_with}
          </div>
        )}
      </div>
    </div>
  );
}
