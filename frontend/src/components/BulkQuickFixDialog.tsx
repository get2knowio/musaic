import { useEffect, useRef, useState } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import { Music, X, Zap } from "lucide-react";
import { toast } from "sonner";

import {
  streamQuickFixAll,
  type QuickFixProgress,
  type QuickFixSummary,
} from "@/hooks/useChecks";
import { useAiStatus } from "@/hooks/useAiStatus";

/**
 * Modal that runs the MB-only bulk pass against every open finding for a
 * check, streaming per-finding outcomes as they happen. Cancel mid-run
 * aborts the stream (the in-flight finding may still complete server-side
 * but no more start). On done, refreshes the findings cache so the trimmed
 * list shows.
 */
export function BulkQuickFixDialog({
  open,
  onClose,
  checkId,
  checkLabel,
  bulkActionLabel,
  fixKind,
  totalFindings,
}: {
  open: boolean;
  onClose: () => void;
  checkId: string;
  checkLabel: string;
  /** Verb for this check's bulk action — e.g. "Try MusicBrainz on all",
   *  "Identify on MusicBrainz", "Fingerprint all". Used in the dialog
   *  title and body copy so each check speaks for itself. */
  bulkActionLabel: string;
  /** Drives the body copy: deterministic checks won't fall through to AI. */
  fixKind: "none" | "ai-job" | "deterministic";
  totalFindings: number;
}) {
  const [phase, setPhase] = useState<"confirm" | "running" | "done">("confirm");
  const [progress, setProgress] = useState<{
    total: number;
    processed: number;
    counts: { fixed: number; skipped: number; errors: number; not_applicable: number };
    last?: QuickFixProgress;
  }>({ total: totalFindings, processed: 0, counts: { fixed: 0, skipped: 0, errors: 0, not_applicable: 0 } });
  const [summary, setSummary] = useState<QuickFixSummary | null>(null);
  const cancelRef = useRef<(() => void) | null>(null);

  // Reset everything when the dialog opens. CRITICAL: only depend on `open`,
  // not `totalFindings` — that prop changes during the run as findings get
  // resolved, and re-running this effect mid-run would yank the user back to
  // the "confirm" phase while the stream is still going. (We seed the initial
  // total from the prop value at open-time but ignore subsequent updates;
  // the stream's own start/progress events drive the in-run total.)
  useEffect(() => {
    if (open) {
      setPhase("confirm");
      setProgress({ total: totalFindings, processed: 0, counts: { fixed: 0, skipped: 0, errors: 0, not_applicable: 0 } });
      setSummary(null);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  // If the dialog is closed mid-run, make sure to abort the stream.
  useEffect(() => {
    return () => cancelRef.current?.();
  }, []);

  const onStart = () => {
    // Idempotency: if a stream is already in flight (e.g. user double-
    // clicked, or React StrictMode re-fired an effect), don't spawn a
    // second one — the server would happily start another bulk loop and
    // we'd see the same album processed twice.
    if (cancelRef.current) return;
    setPhase("running");
    setProgress((p) => ({ ...p, processed: 0 }));
    cancelRef.current = streamQuickFixAll(checkId, {
      onStart: (total) => setProgress((p) => ({ ...p, total })),
      onProgress: (ev) => setProgress((p) => {
        const next = { ...p.counts };
        if (ev.outcome === "fixed") next.fixed++;
        else if (ev.outcome === "skipped") next.skipped++;
        else if (ev.outcome === "error") next.errors++;
        else if (ev.outcome === "not-applicable") next.not_applicable++;
        return { ...p, processed: p.processed + 1, counts: next, last: ev };
      }),
      onDone: (s) => {
        cancelRef.current = null;  // allow a future re-run from this dialog
        setSummary(s);
        setPhase("done");
        // Cache refresh rides the BulkBatchCompleted event the backend
        // publishes at end-of-loop — useLibraryEventsStream catches it
        // and invalidates the findings/checks caches.
      },
      onError: (detail) => {
        cancelRef.current = null;
        toast.error(`Quick-fix stream errored: ${detail}`);
        setPhase("done");
      },
    });
  };

  const onCancel = () => {
    cancelRef.current?.();
    onClose();
  };

  const fmt = `${progress.processed} of ${progress.total}`;
  const pct = progress.total > 0 ? Math.round((progress.processed / progress.total) * 100) : 0;

  return (
    <Dialog.Root open={open} onOpenChange={(o) => { if (!o) onCancel(); }}>
      <Dialog.Portal>
        <Dialog.Overlay className="fixed inset-0 z-40 bg-black/50 backdrop-blur-[1px]" />
        <Dialog.Content
          className="fixed left-1/2 top-1/2 z-50 -translate-x-1/2 -translate-y-1/2 w-[min(560px,95vw)] bg-panel border border-line rounded-md shadow-sheet outline-none flex flex-col"
          aria-describedby={undefined}
          onEscapeKeyDown={(e) => { if (phase === "running") e.preventDefault(); }}
          onPointerDownOutside={(e) => { if (phase === "running") e.preventDefault(); }}
          onInteractOutside={(e) => { if (phase === "running") e.preventDefault(); }}
        >
          <div className="flex items-center justify-between px-4 h-11 border-b border-line">
            <div className="flex items-center gap-2">
              <Music className="w-3.5 h-3.5 text-accent" />
              <Dialog.Title className="font-semibold text-md">
                {bulkActionLabel}
              </Dialog.Title>
            </div>
            {phase !== "running" && (
              <button
                onClick={onClose}
                className="w-8 h-8 grid place-items-center text-fg-3 hover:text-fg hover:bg-bg-3 rounded transition"
                aria-label="Close"
              >
                <X className="w-4 h-4" />
              </button>
            )}
          </div>

          <div className="px-4 py-4">
            {phase === "confirm" && (
              <ConfirmBody
                checkId={checkId}
                checkLabel={checkLabel}
                bulkActionLabel={bulkActionLabel}
                fixKind={fixKind}
                totalFindings={totalFindings}
              />
            )}
            {phase === "running" && (
              <RunningBody fmt={fmt} pct={pct} counts={progress.counts} last={progress.last} />
            )}
            {phase === "done" && summary && (
              <DoneBody summary={summary} fixKind={fixKind} />
            )}
          </div>

          <div className="flex items-center justify-end gap-2 px-4 py-3 border-t border-line bg-panel-2 rounded-b-md">
            {phase === "confirm" && (
              <>
                <button
                  onClick={onClose}
                  className="h-8 px-3 border border-line rounded text-sm hover:bg-bg-3 transition"
                >
                  Cancel
                </button>
                <button
                  onClick={onStart}
                  className="h-8 px-3 rounded bg-accent text-accent-fg text-sm font-medium hover:opacity-90 transition inline-flex items-center gap-1.5"
                >
                  <Zap className="w-3.5 h-3.5" />
                  Start
                </button>
              </>
            )}
            {phase === "running" && (
              <button
                onClick={onCancel}
                className="h-8 px-3 border border-line rounded text-sm hover:bg-bg-3 transition"
              >
                Stop
              </button>
            )}
            {phase === "done" && (
              <button
                onClick={onClose}
                className="h-8 px-3 rounded bg-accent text-accent-fg text-sm font-medium hover:opacity-90 transition"
              >
                Close
              </button>
            )}
          </div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}

function ConfirmBody({
  checkId,
  checkLabel,
  bulkActionLabel,
  fixKind,
  totalFindings,
}: {
  checkId: string;
  checkLabel: string;
  bulkActionLabel: string;
  fixKind: "none" | "ai-job" | "deterministic";
  totalFindings: number;
}) {
  // Per-check pacing copy. Most checks are MB-rate-limited (1 req/s per
  // album); the identify cascade can also run fpcalc — fpcalc + AcoustID
  // lookup is ~1 sec per *track*, so a 12-track album takes ~12s on its
  // own. The unidentified-album bulk hits the worst case when the
  // library has never been fingerprinted.
  const isIdentify = checkId === "unidentified-album";
  // ai-job fix_kind means leftovers from the bulk pass fall through to
  // AI when the user opens a per-finding Fix later — only when a model
  // backend can run (lib/aiGating.ts).
  const ai = useAiStatus();
  const fallsThroughToAi = fixKind === "ai-job" && ai.ready;

  const lowSec = isIdentify
    ? totalFindings * 8        // wild guess, ~8 tracks per album
    : totalFindings;           // ~1 req/s for MB
  const highSec = isIdentify
    ? totalFindings * 15
    : Math.ceil(totalFindings * 1.5);

  return (
    <>
      <p className="text-sm text-fg-2 mb-2">
        We'll run <b>{bulkActionLabel}</b> against each of <b>{totalFindings}</b>{" "}
        open finding{totalFindings === 1 ? "" : "s"} on <b>{checkLabel}</b>.
        {isIdentify ? (
          <>
            {" "}Each album is fingerprinted (when missing) and run through
            the resolver to find its MusicBrainz match. Albums that don't
            match stay open — try editing the directory name to the
            canonical "Artist - Album" form first.
          </>
        ) : fallsThroughToAi ? (
          <>
            {" "}Findings we can resolve via MusicBrainz will be applied
            directly; the rest stay open for you to Fix individually (which
            queues an AI job).
          </>
        ) : null}
      </p>
      <p className="text-xs text-fg-3 leading-relaxed">
        {isIdentify ? (
          <>
            Fingerprinting takes ~1 second per track when needed, so this
            run will take up to{" "}
            <b>{Math.ceil(lowSec / 60)}–{Math.ceil(highSec / 60)} minutes</b>{" "}
            for {totalFindings} album{totalFindings === 1 ? "" : "s"} (less
            if many are already fingerprinted).
          </>
        ) : (
          <>
            MusicBrainz allows ~1 request per second so this will take roughly{" "}
            <b>{lowSec}–{highSec} seconds</b>.
          </>
        )}
        {" "}You can stop at any time.{ai.ready && " No AI quota will be burned."}
      </p>
    </>
  );
}

function RunningBody({
  fmt, pct, counts, last,
}: {
  fmt: string;
  pct: number;
  counts: { fixed: number; skipped: number; errors: number; not_applicable: number };
  last?: QuickFixProgress;
}) {
  return (
    <>
      <div className="flex items-center justify-between mb-2">
        <span className="text-sm text-fg-2">Processing {fmt}</span>
        <span className="mono text-xs text-fg-4 tabular-nums">{pct}%</span>
      </div>
      <div className="h-1.5 bg-bg-3 rounded-full overflow-hidden mb-3">
        <div
          className="h-full bg-accent transition-all"
          style={{ width: `${pct}%` }}
        />
      </div>
      <div className="grid grid-cols-4 gap-2 text-xs mb-3">
        <Stat label="Fixed" value={counts.fixed} tone="ok" />
        <Stat label="Skipped" value={counts.skipped} tone="muted" />
        <Stat label="N/A" value={counts.not_applicable} tone="muted" />
        <Stat label="Errors" value={counts.errors} tone="err" />
      </div>
      {last && (
        <div className="text-xs text-fg-4 truncate">
          Latest: <span className={`mono ${
            last.outcome === "fixed" ? "text-ok" :
            last.outcome === "error" ? "text-err" : "text-fg-3"
          }`}>{last.outcome}</span>
          {" · "}{last.message}
        </div>
      )}
    </>
  );
}

function DoneBody({
  summary,
  fixKind,
}: {
  summary: QuickFixSummary;
  fixKind: "none" | "ai-job" | "deterministic";
}) {
  // `fixed` counts only findings that are gone; a fix that wrote part of
  // what its finding asks for is `still_open`.
  const stillOpen = summary.still_open ?? 0;
  const partial = summary.fixed > 0 && summary.skipped + summary.errors + stillOpen > 0;
  const ai = useAiStatus();
  const fallsThroughToAi = fixKind === "ai-job" && ai.ready;
  const stillOpenNote = stillOpen > 0 && (
    <>
      {" "}
      <b>{stillOpen}</b> {stillOpen === 1 ? "fix was" : "fixes were"} applied but{" "}
      {stillOpen === 1 ? "its finding is" : "their findings are"} still open.
    </>
  );
  return (
    <>
      <div className={`grid ${stillOpen > 0 ? "grid-cols-5" : "grid-cols-4"} gap-2 text-xs mb-3`}>
        <Stat label="Fixed" value={summary.fixed} tone="ok" />
        {stillOpen > 0 && <Stat label="Still open" value={stillOpen} tone="muted" />}
        <Stat label="Skipped" value={summary.skipped} tone="muted" />
        <Stat label="N/A" value={summary.not_applicable} tone="muted" />
        <Stat label="Errors" value={summary.errors} tone="err" />
      </div>
      <p className="text-sm text-fg-2">
        {summary.fixed === 0 ? (
          <>
            {stillOpen > 0
              ? "No findings were fully resolved."
              : fallsThroughToAi
                ? "No findings were resolvable directly. The rest can still be fixed individually with AI."
                : "No findings were resolvable. Check the per-finding messages for hints."}
            {stillOpenNote}
          </>
        ) : (
          <>
            Resolved <b>{summary.fixed}</b> of {summary.total} finding
            {summary.total === 1 ? "" : "s"}.
            {stillOpenNote}
            {partial && fallsThroughToAi && " The rest can be fixed individually with AI."}
            {partial && !fallsThroughToAi && " The rest stay open — see per-finding messages for why."}
          </>
        )}
      </p>
    </>
  );
}

function Stat({ label, value, tone }: { label: string; value: number; tone: "ok" | "err" | "muted" }) {
  const cls = tone === "ok" ? "text-ok" : tone === "err" ? "text-err" : "text-fg-3";
  return (
    <div className="border border-line bg-panel-2 rounded px-2 py-1.5">
      <div className="mono text-[10px] uppercase tracking-[0.06em] text-fg-4">{label}</div>
      <div className={`mono text-base font-semibold tabular-nums ${cls}`}>{value}</div>
    </div>
  );
}
