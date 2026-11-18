/**
 * Resolve dialog for the multi-disc-layout check: what converting the set
 * to one folder (tracks numbered straight through, no disc numbers in the
 * names, no disc tags) would do. Read-only: there is no Convert button,
 * as for every check without a fix; the dialog says so.
 *
 * The plan comes fresh from the server on every open (the finding's
 * detail is a label snapshot from check time): members, the old → new
 * table, what happens to covers and other files, and anything that would
 * stop a conversion, in plain words.
 */
import * as Dialog from "@radix-ui/react-dialog";
import { useQuery } from "@tanstack/react-query";
import { AlertTriangle, FolderInput, Info, Layers, Loader2, X } from "lucide-react";

import { fetchMultiDiscConversionPreview } from "@/lib/api";
import type {
  Finding,
  MultiDiscConversionPreview,
  MultiDiscCoverAction,
  MultiDiscFolder,
  MultiDiscMember,
  MultiDiscTrack,
} from "@/lib/types";

interface Props {
  finding: Finding;
  open: boolean;
  onClose: () => void;
}

const plural = (n: number, word: string) => `${n} ${word}${n === 1 ? "" : "s"}`;

const COVER_WORDS: Record<MultiDiscCoverAction, string> = {
  kept: "stays",
  moved: "moves into the set's folder",
  identical: "is the same picture as the one kept, so it is not needed",
  different: "is a different picture: the set's folder keeps its own and this one goes to Musaic's trash",
};

/** One sentence: what a conversion of this set would do. */
function describe(p: MultiDiscConversionPreview): string | null {
  const n = p.track_total ?? p.tracks.length;
  if (p.state === "one_folder") {
    return p.numbering === "continuous"
      ? `The ${plural(n, "track")} are already numbered straight through; converting would remove the disc tags.`
      : `Converting would number the ${plural(n, "track")} straight through all ${plural(p.present_discs?.length ?? 0, "disc")}, rename the files without a disc number and remove the disc tags.`;
  }
  if (p.state === "folders") {
    return `Converting would gather the ${plural(p.members.length, "disc folder")} into one folder, number the ${plural(n, "track")} straight through, rename the files without a disc number and remove the disc tags.`;
  }
  return null;
}

export default function MultiDiscResolveDialog({ finding, open, onClose }: Props) {
  const preview = useQuery<MultiDiscConversionPreview>({
    queryKey: ["multi-disc-conversion-preview", finding.id],
    queryFn: () => fetchMultiDiscConversionPreview(finding.id),
    enabled: open,
    // The plan reflects the disk as it is now: refetch on every open and
    // keep nothing once the dialog closes.
    staleTime: 0,
    gcTime: 0,
    retry: 1,
  });
  const p = preview.data;
  const headline =
    p?.headline || (finding.detail?.headline as string | undefined) || finding.summary;
  const outcome = p ? describe(p) : null;
  const converts = p?.state === "one_folder" || p?.state === "folders";

  return (
    <Dialog.Root open={open} onOpenChange={(o) => { if (!o) onClose(); }}>
      <Dialog.Portal>
        <Dialog.Overlay className="fixed inset-0 z-40 bg-black/45 backdrop-blur-[1px]" />
        <Dialog.Content
          className="fixed left-1/2 top-1/2 z-50 -translate-x-1/2 -translate-y-1/2 w-[min(760px,95vw)] bg-panel border border-line rounded-md shadow-sheet outline-none flex flex-col"
          aria-describedby={undefined}
        >
          <div className="flex items-center justify-between px-4 h-11 border-b border-line">
            <div className="flex items-center gap-2 min-w-0">
              <Layers className="w-3.5 h-3.5 text-accent shrink-0" />
              <Dialog.Title className="font-semibold text-md truncate">
                Put this multi-disc set in one folder
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

          <div className="px-4 py-3.5 space-y-3 overflow-y-auto max-h-[70vh]">
            <div>
              <div className="mono text-xs text-fg-3 truncate" title={headline}>
                {headline}
                {p?.is_compilation && <span className="ml-2 text-accent">[compilation]</span>}
              </div>
              {p?.template && (
                <div className="mono text-[11px] text-fg-4 mt-0.5 truncate">
                  File names: <code className="text-fg-3">{p.template}</code>
                  {p.dir_template && (
                    <>
                      <span className="mx-1.5">·</span>
                      Folder name: <code className="text-fg-3">{p.dir_template}</code>
                    </>
                  )}
                </div>
              )}
            </div>

            {preview.isLoading && (
              <div className="flex items-center gap-2 text-xs text-fg-3 bg-panel-2 border border-line rounded px-3 py-2">
                <Loader2 className="w-3.5 h-3.5 spin shrink-0" />
                <span>Checking the set's folders and files…</span>
              </div>
            )}

            {preview.isError && (
              <div
                role="alert"
                className="flex items-start gap-2 text-xs text-fg-2 leading-relaxed bg-warn-soft border border-warn/30 rounded px-3 py-2"
              >
                <AlertTriangle className="w-3.5 h-3.5 text-warn shrink-0 mt-0.5" />
                <div>
                  <div className="font-medium text-fg">Couldn't check the set's files</div>
                  <div className="mt-0.5 text-fg-3">
                    {(preview.error as Error)?.message || "Try again in a moment."}
                  </div>
                </div>
              </div>
            )}

            {outcome && <p className="text-sm text-fg-2">{outcome}</p>}

            {p && p.reason && (
              <div className="flex items-start gap-2 text-xs text-fg-2 leading-relaxed bg-panel-2 border border-line rounded px-3 py-2">
                <Info className="w-3.5 h-3.5 text-fg-3 shrink-0 mt-0.5" />
                <div>
                  <div className="font-medium text-fg">
                    {p.state === "incomplete" ? "Not every disc is here" : "Nothing to convert"}
                  </div>
                  <div className="mt-0.5 text-fg-3">{p.reason}</div>
                </div>
              </div>
            )}

            {p && p.blockers.length > 0 && (
              <div
                role="alert"
                className="flex items-start gap-2 text-xs text-fg-2 leading-relaxed bg-warn-soft border border-warn/30 rounded px-3 py-2"
              >
                <AlertTriangle className="w-3.5 h-3.5 text-warn shrink-0 mt-0.5" />
                <div className="min-w-0">
                  <div className="font-medium text-fg">
                    {p.blockers.length === 1
                      ? "One thing would stop a conversion"
                      : `${p.blockers.length} things would stop a conversion`}
                  </div>
                  <ul className="mt-0.5 text-fg-3 space-y-0.5">
                    {p.blockers.map((b, i) => (
                      <li key={`${b.code}-${i}`} data-code={b.code} className="break-words">
                        {b.message}
                      </li>
                    ))}
                  </ul>
                </div>
              </div>
            )}

            {p && p.members.length > 0 && <Members members={p.members} oneFolder={p.state === "one_folder"} />}

            {converts && p?.target_folder && (
              <TargetFolder current={p.target_folder.current} next={p.target_folder.new} />
            )}

            {converts && p && p.tracks.length > 0 && (
              <Tracks tracks={p.tracks} trackTotal={p.track_total ?? p.tracks.length} />
            )}

            {converts && p && p.folders.length > 0 && <Folders folders={p.folders} />}

            {converts && (
              <p className="text-xs text-fg-4 leading-relaxed">
                The disc tags (DISCNUMBER, DISCTOTAL) would be removed in the same write that
                renumbers the tracks, so the tags and the file names agree. The audio is not
                touched.
              </p>
            )}
          </div>

          <div className="flex items-center justify-between gap-3 px-4 py-3 border-t border-line bg-panel-2 rounded-b-md">
            <p className="text-xs text-fg-3">
              Converting is not available yet. This shows what it would do; nothing is changed.
            </p>
            <Dialog.Close asChild>
              <button className="h-8 px-3 border border-line rounded text-sm hover:bg-bg-3 transition shrink-0">
                Close
              </button>
            </Dialog.Close>
          </div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}

function SectionHead({ children }: { children: React.ReactNode }) {
  return (
    <div className="flex items-center px-3 py-1.5 border-b border-line text-[11px] mono uppercase tracking-[0.06em] text-fg-4">
      <span>{children}</span>
    </div>
  );
}

function Members({ members, oneFolder }: { members: MultiDiscMember[]; oneFolder: boolean }) {
  return (
    <div className="border border-line rounded-md bg-panel-2 divide-y divide-line">
      <SectionHead>{oneFolder ? "Discs in this folder" : "Disc folders"}</SectionHead>
      {members.map((m) => (
        <div
          key={`${m.album_id}-${m.disc}`}
          className="px-3 py-1.5 text-sm grid grid-cols-[64px_1fr] gap-3 items-center min-w-0"
        >
          <span className="mono text-[11px] text-fg-4 tabular-nums">Disc {m.disc}</span>
          <span className="mono text-[12px] text-fg-2 truncate" title={m.dir_name ?? m.album_id}>
            {m.dir_name ?? m.album_id}
            {m.missing && <span className="ml-2 text-warn">(missing)</span>}
          </span>
        </div>
      ))}
    </div>
  );
}

function TargetFolder({ current, next }: { current: string | null; next: string | null }) {
  if (!next) return null;
  const renamed = current !== next;
  return (
    <div className="border border-accent/40 rounded-md bg-accent/5 px-3 py-2.5">
      <div className="flex items-center gap-1.5 mb-1.5 text-[11px] mono uppercase tracking-[0.06em] text-accent">
        <FolderInput className="w-3 h-3" />
        <span>The set's folder</span>
      </div>
      {renamed ? (
        <div className="grid grid-cols-[1fr_auto_1fr] gap-2 items-center text-sm min-w-0">
          <div className="mono text-[12px] text-warn line-through truncate" title={current ?? ""}>
            {current}
          </div>
          <div className="text-fg-4 mono text-xs">→</div>
          <div className="mono text-[12px] text-ok truncate" title={next}>
            {next}
          </div>
        </div>
      ) : (
        <div className="mono text-[12px] text-fg-2 truncate" title={next}>
          {next} <span className="text-fg-4">(name stays)</span>
        </div>
      )}
    </div>
  );
}

function Tracks({ tracks, trackTotal }: { tracks: MultiDiscTrack[]; trackTotal: number }) {
  return (
    <div className="border border-line rounded-md bg-panel-2 divide-y divide-line">
      <SectionHead>
        {plural(tracks.length, "file")} · new track total {trackTotal}
      </SectionHead>
      {tracks.map((t) => (
        <div
          key={`${t.album_id}/${t.filename}`}
          className="px-3 py-2 text-sm min-w-0 grid grid-cols-[1fr_auto_1fr] gap-3 items-start"
        >
          <div className="min-w-0">
            <div className="mono text-[11px] text-fg-3 truncate" title={t.filename}>
              {t.filename}
            </div>
            <div className="text-[11px] text-fg-4 mt-0.5 truncate">
              {t.disc != null ? `Disc ${t.disc}` : "No disc"}
              {t.track != null ? `, track ${t.track}` : ", no track number"}
            </div>
          </div>
          <div className="text-fg-4 mono text-xs shrink-0 mt-1">→</div>
          <div className="min-w-0">
            <div
              className={`mono text-[11px] truncate ${t.new_filename ? "text-ok" : "text-fg-4"}`}
              title={t.new_filename ?? ""}
            >
              {t.new_filename ?? "no new name"}
            </div>
            <div className="text-[11px] text-fg-4 mt-0.5 truncate">
              {t.new_track != null ? `Track ${t.new_track}` : "—"}
            </div>
          </div>
        </div>
      ))}
    </div>
  );
}

function Folders({ folders }: { folders: MultiDiscFolder[] }) {
  return (
    <div className="border border-line rounded-md bg-panel-2 divide-y divide-line">
      <SectionHead>Covers and other files</SectionHead>
      {folders.map((f) => (
        <div key={f.album_id} className="px-3 py-2 text-xs text-fg-3 space-y-0.5 min-w-0">
          <div className="mono text-[12px] text-fg-2 truncate" title={f.dir_name}>
            {f.dir_name}
            {f.destination && <span className="ml-2 text-accent">(becomes the set's folder)</span>}
          </div>
          {f.covers.length === 0 && !f.sidecar && f.other_files.length === 0 && (
            <div>No covers or other files.</div>
          )}
          {f.covers.map((c) => (
            <div key={c.name} data-cover-action={c.action}>
              <span className="mono">{c.name}</span> {COVER_WORDS[c.action]}.
            </div>
          ))}
          {f.sidecar && (
            <div>
              {f.destination
                ? "Its Musaic tags file is kept, with the other discs' tags added."
                : "Its Musaic tags are added to the set's folder."}
            </div>
          )}
          {f.other_files.length > 0 && (
            <div>
              {f.destination ? "Stays with the set: " : "Stays in this folder: "}
              <span className="mono">{f.other_files.join(", ")}</span>
            </div>
          )}
        </div>
      ))}
    </div>
  );
}
