/**
 * Resolve dialog for the multi-disc-layout check: what converting the set
 * to one folder (tracks numbered straight through, no disc numbers in the
 * names, no disc tags) does, and a Convert button behind a confirmation,
 * for a set kept in one folder and for one kept in one folder per disc
 * (whose files are gathered into disc 1's folder and whose albums become
 * one).
 *
 * The plan comes fresh from the server on every open (the finding's
 * detail is a label snapshot from check time): members, the old → new
 * table, what happens to covers and other files, and anything that would
 * stop a conversion, in plain words. Convert sends back the preview's
 * digest; the server re-derives the plan and refuses if it changed. A set
 * in several folders converts over a stream that reports its progress. A
 * conversion that stops part-way keeps the dialog open with a reason per
 * file and a fresh preview of what is left, which says where every file is
 * and offers to finish (or to stop treating it as a conversion). Caches
 * refresh from the library events the conversion publishes (no
 * invalidation here).
 */
import { useState } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import { useMutation, useQuery } from "@tanstack/react-query";
import { AlertTriangle, FolderInput, Info, Layers, Loader2, X } from "lucide-react";
import { toast } from "sonner";

import {
  abandonMultiDiscConversion,
  convertMultiDisc,
  convertMultiDiscStreaming,
  fetchMultiDiscConversionPreview,
} from "@/lib/api";
import { confirmationText, progressText } from "@/lib/multiDisc";
import type {
  Finding,
  MultiDiscConversionPreview,
  MultiDiscConvertResult,
  MultiDiscCoverAction,
  MultiDiscFolder,
  MultiDiscMember,
  MultiDiscProgress,
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

const STATE_WORDS: Record<NonNullable<MultiDiscTrack["state"]>, string> = {
  untouched: "not started",
  renamed: "renamed",
  tagged: "renamed and renumbered",
  moved: "done",
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

/** The lines that say what went wrong, per file and for the folder. */
function problemLines(r: MultiDiscConvertResult): string[] {
  const lines: string[] = [];
  for (const f of r.files ?? []) {
    if (f.user_message && (f.rename === "failed" || f.tags === "failed" || f.move === "failed")) {
      lines.push(f.user_message);
    }
  }
  if (r.folder?.outcome === "failed" && r.folder.user_message) lines.push(r.folder.user_message);
  for (const b of r.blockers ?? []) lines.push(b.message);
  return lines;
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
  const [confirming, setConfirming] = useState(false);
  const [abandoning, setAbandoning] = useState(false);
  const [progress, setProgress] = useState<MultiDiscProgress | null>(null);
  const [result, setResult] = useState<MultiDiscConvertResult | null>(null);
  // The next open starts from the plan again, not from this one's answer.
  const close = () => {
    setConfirming(false);
    setAbandoning(false);
    setProgress(null);
    setResult(null);
    onClose();
  };

  const mutation = useMutation({
    mutationFn: () =>
      p?.state === "folders"
        ? convertMultiDiscStreaming(finding.id, p?.digest ?? "", setProgress)
        : convertMultiDisc(finding.id, p?.digest ?? ""),
    onSettled: () => setProgress(null),
    onSuccess: (r) => {
      setConfirming(false);
      if (r.ok) {
        toast.success(r.user_message || "Converted");
        close();
        return;
      }
      // Partly done or refused: stay open, say why, and show what is left
      // (the plan is the dialog's own, re-derived on every fetch).
      toast.warning(r.user_message || "The set was not fully converted");
      setResult(r);
      void preview.refetch();
    },
    onError: (e) => {
      setConfirming(false);
      toast.error((e as Error).message || "Couldn't convert the set");
    },
  });
  const abandon = useMutation({
    mutationFn: () => abandonMultiDiscConversion(finding.id),
    onSuccess: (r) => {
      toast.success(r.user_message || "Stopped");
      close();
    },
    onError: (e) => {
      setAbandoning(false);
      toast.error((e as Error).message || "Couldn't stop the conversion");
    },
  });
  const busy = mutation.isPending || abandon.isPending;
  const canConvert = !!p?.conversion_available && !!p.digest && !busy;

  return (
    <Dialog.Root open={open} onOpenChange={(o) => { if (!o) close(); }}>
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

            {result && !result.ok && (
              <div
                role="alert"
                data-testid="convert-result"
                className="flex items-start gap-2 text-xs text-fg-2 leading-relaxed bg-warn-soft border border-warn/30 rounded px-3 py-2"
              >
                <AlertTriangle className="w-3.5 h-3.5 text-warn shrink-0 mt-0.5" />
                <div className="min-w-0">
                  <div className="font-medium text-fg">
                    {result.refused ? "Nothing was converted" : "The set was not fully converted"}
                  </div>
                  <div className="mt-0.5 text-fg-3 break-words">{result.user_message}</div>
                  <ul className="mt-0.5 text-fg-3 space-y-0.5">
                    {problemLines(result).map((l, i) => (
                      <li key={i} className="break-words">{l}</li>
                    ))}
                  </ul>
                  {!result.refused && (
                    <div className="mt-0.5 text-fg-3">
                      What was done stays done. Fix the cause, then convert again to finish.
                    </div>
                  )}
                  {result.record && (
                    <div className="mt-0.5 text-fg-4 break-all">
                      Record of the conversion: <span className="mono">{result.record}</span>
                    </div>
                  )}
                </div>
              </div>
            )}

            {p?.resume && (
              <div className="flex items-start gap-2 text-xs text-fg-2 leading-relaxed bg-panel-2 border border-line rounded px-3 py-2">
                <Info className="w-3.5 h-3.5 text-fg-3 shrink-0 mt-0.5" />
                <div className="min-w-0">
                  <div className="font-medium text-fg">A conversion of this set did not finish</div>
                  <div className="mt-0.5 text-fg-3">
                    {p.state === "folders"
                      ? "Each file below says where it is now. Converting finishes the rest, from the record kept of the first attempt. You can also stop treating it as a conversion: every file then stays where it is."
                      : "Files marked “done” already have their new numbers. Converting finishes the rest, from the record kept of the first attempt."}
                  </div>
                  <div className="mt-0.5 text-fg-4 break-all">
                    Record: <span className="mono">{p.resume.record}</span>
                  </div>
                </div>
              </div>
            )}

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
                The disc tags (DISCNUMBER, DISCTOTAL) are removed in the same write that
                renumbers the tracks, so the tags and the file names agree. The audio is not
                touched, and tags you added in Musaic stay with the files.
              </p>
            )}

            {confirming && p && (
              <div
                role="alertdialog"
                aria-label="Confirm the conversion"
                className="border border-accent/40 rounded-md bg-accent/5 px-3 py-2.5 text-sm text-fg-2"
              >
                <div className="font-medium text-fg">Convert this set?</div>
                <p className="mt-1 text-xs text-fg-2 leading-relaxed">{confirmationText(p)}</p>
              </div>
            )}

            {abandoning && p?.resume && (
              <div
                role="alertdialog"
                aria-label="Confirm stopping the conversion"
                className="border border-warn/40 rounded-md bg-warn-soft px-3 py-2.5 text-sm text-fg-2"
              >
                <div className="font-medium text-fg">Stop treating this as a conversion?</div>
                <p className="mt-1 text-xs text-fg-2 leading-relaxed">
                  Every file stays where it is now, as listed above; nothing is moved back. The
                  folders are then albums like any other, and the record of what was done is kept
                  at {p.resume.record}.
                </p>
              </div>
            )}

            {mutation.isPending && progress && (
              <div
                role="status"
                className="flex items-center gap-2 text-xs text-fg-2 bg-panel-2 border border-line rounded px-3 py-2"
              >
                <Loader2 className="w-3.5 h-3.5 spin shrink-0" />
                <span>{progressText(progress)}</span>
              </div>
            )}
          </div>

          <div className="flex items-center justify-between gap-3 px-4 py-3 border-t border-line bg-panel-2 rounded-b-md">
            <p className="text-xs text-fg-3">
              {mutation.isPending
                ? "Converting. You can close this; the conversion goes on."
                : p?.conversion_available
                  ? "Nothing changes until you confirm."
                  : "Nothing can be converted as it stands; nothing is changed."}
            </p>
            <div className="flex items-center gap-2 shrink-0">
              <Dialog.Close asChild>
                <button
                  className="h-8 px-3 border border-line rounded text-sm hover:bg-bg-3 transition"
                  disabled={abandon.isPending}
                >
                  {confirming || abandoning ? "Cancel" : "Close"}
                </button>
              </Dialog.Close>
              {p?.resume && p.state === "folders" && !confirming && !abandoning && (
                <button
                  onClick={() => { setResult(null); setAbandoning(true); }}
                  disabled={busy}
                  className="h-8 px-3 border border-line rounded text-sm hover:bg-bg-3 transition disabled:opacity-60"
                >
                  Stop this conversion…
                </button>
              )}
              {abandoning && (
                <button
                  onClick={() => abandon.mutate()}
                  disabled={busy}
                  className="h-8 px-3 rounded bg-warn text-accent-fg text-sm font-medium hover:opacity-90 transition disabled:opacity-60"
                >
                  {abandon.isPending ? "Stopping…" : "Stop it"}
                </button>
              )}
              {p?.conversion_available && !confirming && !abandoning && (
                <button
                  onClick={() => { setResult(null); setConfirming(true); }}
                  disabled={!canConvert}
                  className="h-8 px-3 rounded bg-accent text-accent-fg text-sm font-medium hover:opacity-90 transition disabled:opacity-60"
                >
                  {p.resume ? "Finish converting…" : "Convert…"}
                </button>
              )}
              {confirming && (
                <button
                  onClick={() => mutation.mutate()}
                  disabled={!canConvert}
                  className="h-8 px-3 rounded bg-accent text-accent-fg text-sm font-medium hover:opacity-90 transition disabled:opacity-60 inline-flex items-center gap-1.5"
                >
                  {mutation.isPending ? (
                    <>
                      <Loader2 className="w-3.5 h-3.5 spin" />
                      Converting…
                    </>
                  ) : (
                    "Convert now"
                  )}
                </button>
              )}
            </div>
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
              {t.state && t.state !== "untouched" && (
                <span data-state={t.state} className="ml-1.5 text-accent">
                  ({STATE_WORDS[t.state]}{t.folder ? `, in ${t.folder}` : ""})
                </span>
              )}
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
          {f.exists === false && <div>Already removed: it was empty.</div>}
          {f.covers.length === 0 &&
            !f.sidecar &&
            f.other_files.length === 0 &&
            !f.junk?.length &&
            !f.strays?.length && (
            <div>No covers or other files.</div>
          )}
          {f.covers.map((c) => (
            <div key={c.name} data-cover-action={c.action}>
              <span className="mono">{c.name}</span> {COVER_WORDS[c.action]}.
              {c.done && <span className="ml-1 text-fg-4">(done)</span>}
            </div>
          ))}
          {!!f.junk?.length && (
            <div>
              System clutter goes to Musaic's trash:{" "}
              <span className="mono">{f.junk.map((j) => j.name).join(", ")}</span>
            </div>
          )}
          {f.strays?.map((s) => (
            <div key={s.name} data-stray-action={s.action}>
              <span className="mono">{s.name}</span>{" "}
              {s.action === "identical" ? (
                "is the same file as the one in the set's folder, so this copy goes to Musaic's trash."
              ) : s.new_name ? (
                <>
                  moves into the set's folder as <span className="mono">{s.new_name}</span>: a
                  different file named {s.name} is already there.
                </>
              ) : (
                "moves into the set's folder."
              )}
              {s.done && <span className="ml-1 text-fg-4">(done)</span>}
            </div>
          ))}
          {!f.destination && f.exists !== false && (
            <div>
              {f.other_files.length > 0
                ? "This folder is kept: not everything in it is moved."
                : "This folder is removed once it is empty."}
            </div>
          )}
          {f.sidecar && (
            <div>
              {f.destination
                ? "Its Musaic tags file is kept, with the other discs' tags added."
                : "Its Musaic tags are added to the set's folder."}
            </div>
          )}
          {f.other_files.length > 0 && (
            <div>
              {f.destination ? "Stays with the set: " : "Not moved (Musaic never moves a subfolder): "}
              <span className="mono">{f.other_files.join(", ")}</span>
            </div>
          )}
        </div>
      ))}
    </div>
  );
}
