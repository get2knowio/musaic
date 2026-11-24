import { useMemo, useState } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import { Link } from "react-router-dom";
import { Music, X, Zap } from "lucide-react";

import { type QuickFixSummary } from "@/hooks/useChecks";
import { useAgentPairings } from "@/hooks/useAgents";
import { useAlbumsSnapshot } from "@/hooks/useAlbumsSnapshot";
import { useFindings } from "@/hooks/useChecks";
import { displayArtist } from "@/lib/albumFields";
import { type BulkOutcome } from "@/lib/bulkFixReasons";
import {
  processed,
  startBulkRun,
  stopBulkRun,
  unconfirmed,
  useBulkRun,
  type BulkCounts,
  type BulkResultItem,
  type BulkRun,
} from "@/lib/bulkRuns";
import type { Album } from "@/lib/types";

/** Rows listed per outcome group before "Show all N" (as on the Review page). */
export const RESULT_ROW_LIMIT = 50;

function albumLabel(album: Album | undefined): string {
  if (!album) return "Unknown album";
  const artist = displayArtist(album);
  const title = (album.title || "").trim() || album.dir_name;
  return artist ? `${artist} – ${title}` : title;
}

/**
 * Modal that runs the MB-only bulk pass against every open finding for a
 * check, streaming per-finding outcomes as they happen.
 *
 * The run itself lives in `lib/bulkRuns.ts`, not here: closing the dialog
 * leaves it running (the Activity panel follows it), and Stop ends it
 * keeping every outcome reported so far; the dialog then shows that partial
 * summary until the user closes it, and Activity keeps it after (#186).
 * The findings caches refresh on the BulkBatchCompleted event.
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
  /** `ai-job` marks the four research-capable checks, which offer to send
   *  what the resolver can't settle to the assistant. */
  fixKind: "none" | "ai-job" | "deterministic";
  totalFindings: number;
}) {
  const [runId, setRunId] = useState<string | null>(null);
  const run = useBulkRun(runId);

  // The four checks whose Fix can go to research offer sending what the
  // resolver can't settle to the assistant. The choice starts on when an
  // assistant has been added (a pairing not revoked), off otherwise, until
  // the user changes it.
  const researchCapable = fixKind === "ai-job";
  const pairings = useAgentPairings();
  const hasAssistant = Array.isArray(pairings.data) && pairings.data.some((p) => p.revokedAt === null);
  const [researchChoice, setResearchChoice] = useState<boolean | null>(null);
  const research = researchCapable && (researchChoice ?? hasAssistant);

  // Album names come from what the page knew when the run started: findings
  // and albums change as the run writes, and the list should name each album
  // as the user saw it.
  const { data: findings = [] } = useFindings();
  const { data: albums = [] } = useAlbumsSnapshot();
  const albumOf = useMemo(() => {
    const byAlbum = new Map(albums.map((a) => [a.id, a]));
    const albumIdOf = new Map(findings.map((f) => [f.id, f.album_id]));
    return (findingId: number) => {
      const id = albumIdOf.get(findingId) ?? null;
      return { id, label: albumLabel(id ? byAlbum.get(id) : undefined) };
    };
  }, [albums, findings]);

  // A new opening starts at the confirmation (whatever `totalFindings` does
  // meanwhile: it changes while a run resolves findings).
  const [wasOpen, setWasOpen] = useState(open);
  if (open !== wasOpen) {
    setWasOpen(open);
    if (open) {
      setRunId(null);
      setResearchChoice(null);
    }
  }

  const onStart = () => {
    // One run per press: a double click must not start a second loop.
    if (run?.phase === "running") return;
    // Take the lookup as it is now: later renders see what the run changed.
    const snapshot = albumOf;
    setRunId(
      startBulkRun({
        checkId,
        checkLabel,
        actionLabel: bulkActionLabel,
        fixKind,
        research,
        total: totalFindings,
        albumOf: snapshot,
      }),
    );
  };

  const phase: "confirm" | BulkRun["phase"] = run ? run.phase : "confirm";
  const running = phase === "running";

  return (
    <Dialog.Root open={open} onOpenChange={(o) => { if (!o) onClose(); }}>
      <Dialog.Portal>
        <Dialog.Overlay className="fixed inset-0 z-40 bg-black/50 backdrop-blur-[1px]" />
        <Dialog.Content
          className="fixed left-1/2 top-1/2 z-50 -translate-x-1/2 -translate-y-1/2 w-[min(560px,95vw)] bg-panel border border-line rounded-md shadow-sheet outline-none flex flex-col"
          aria-describedby={undefined}
          // A stray click outside must not hide a running run; Escape and
          // Close do, and say it carries on.
          onPointerDownOutside={(e) => { if (running) e.preventDefault(); }}
          onInteractOutside={(e) => { if (running) e.preventDefault(); }}
        >
          <div className="flex items-center justify-between px-4 h-11 border-b border-line">
            <div className="flex items-center gap-2">
              <Music className="w-3.5 h-3.5 text-accent" />
              <Dialog.Title className="font-semibold text-md">
                {bulkActionLabel}
              </Dialog.Title>
            </div>
            <button
              onClick={onClose}
              className="w-8 h-8 grid place-items-center text-fg-3 hover:text-fg hover:bg-bg-3 rounded transition"
              aria-label={running ? "Close (the run continues)" : "Close"}
            >
              <X className="w-4 h-4" />
            </button>
          </div>

          <div className="px-4 py-4">
            {phase === "confirm" && (
              <ConfirmBody
                checkId={checkId}
                checkLabel={checkLabel}
                bulkActionLabel={bulkActionLabel}
                totalFindings={totalFindings}
              />
            )}
            {phase === "confirm" && researchCapable && (
              <label className="mt-3 flex items-start gap-2 text-sm text-fg-2 cursor-pointer">
                <input
                  type="checkbox"
                  className="mt-0.5 accent-[var(--accent)]"
                  checked={research}
                  onChange={(e) => setResearchChoice(e.target.checked)}
                  aria-describedby="bulk-research-scope"
                />
                <span>
                  Send what the resolver can't settle to my assistant
                  <span id="bulk-research-scope" className="block text-xs text-fg-3">
                    {totalFindings} {totalFindings === 1 ? "finding" : "findings"} in scope
                  </span>
                </span>
              </label>
            )}
            {run && running && (
              <>
                <RunningBody run={run} />
                <p className="mt-3 text-xs text-fg-3">
                  Closing this window does not stop the run. Its progress and results stay in Activity.
                </p>
              </>
            )}
            {run && !running && <RunOutcome run={run} />}
            {run && !running && <ResultsList items={run.items} stillOpen={run.summary?.still_open ?? 0} />}
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
            {run && running && (
              <>
                <button
                  onClick={() => stopBulkRun(run.id)}
                  className="h-8 px-3 border border-line rounded text-sm hover:bg-bg-3 transition"
                  title="Start no more findings. What was already fixed stays fixed."
                >
                  Stop
                </button>
                <button
                  onClick={onClose}
                  className="h-8 px-3 border border-line rounded text-sm hover:bg-bg-3 transition"
                >
                  Close
                </button>
              </>
            )}
            {run && !running && (
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

/**
 * How a run ended, for the dialog and Activity: the counts, then one or two
 * sentences. A stopped or interrupted run separates what is confirmed from
 * what is not known, and says that completed writes stand.
 */
export function RunOutcome({ run }: { run: BulkRun }) {
  if (run.phase === "completed" && run.summary) {
    return <DoneBody summary={run.summary} />;
  }
  if (run.phase === "failed") {
    return (
      <p className="text-sm text-fg-2">
        The run could not start{run.error ? `: ${run.error}` : "."} Nothing was changed.
      </p>
    );
  }
  const done = processed(run);
  const open = unconfirmed(run);
  return (
    <>
      <Counts counts={run.counts} research={run.research} />
      <p className="text-sm text-fg-2" data-bulk-partial>
        {run.phase === "stopped" ? (
          <>Stopped after {done} of {run.total}. </>
        ) : (
          <>The connection to the run was lost after {done} of {run.total}. </>
        )}
        {done > 0 ? "The outcomes listed below are confirmed, and the fixes made stay made. " : ""}
        {open > 0 &&
          (run.phase === "stopped"
            ? `The other ${open} ${open === 1 ? "has" : "have"} no confirmed outcome: the one in progress when you stopped may still have finished, and the rest were not started.`
            : `The other ${open} ${open === 1 ? "has" : "have"} no confirmed outcome: the run may have carried on without this window. Review shows what is still open.`)}
      </p>
    </>
  );
}

function Counts({ counts, research = false }: { counts: BulkCounts; research?: boolean }) {
  return (
    <div className={`grid ${research ? "grid-cols-5" : "grid-cols-4"} gap-2 text-xs mb-3`}>
      <Stat label="Fixed" value={counts.fixed} tone="ok" />
      {research && <Stat label="Research" value={counts.sent_for_research + counts.already_requested} tone="muted" />}
      <Stat label="Skipped" value={counts.skipped} tone="muted" />
      <Stat label="N/A" value={counts.not_applicable} tone="muted" />
      <Stat label="Errors" value={counts.errors} tone="err" />
    </div>
  );
}

function ConfirmBody({
  checkId,
  checkLabel,
  bulkActionLabel,
  totalFindings,
}: {
  checkId: string;
  checkLabel: string;
  bulkActionLabel: string;
  totalFindings: number;
}) {
  // Per-check pacing copy. Most checks are MB-rate-limited (1 req/s per
  // album); the identify cascade can also run fpcalc — fpcalc + AcoustID
  // lookup is ~1 sec per *track*, so a 12-track album takes ~12s on its
  // own. The unidentified-album bulk hits the worst case when the
  // library has never been fingerprinted.
  const isIdentify = checkId === "unidentified-album";
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
        {" "}You can stop at any time.
      </p>
    </>
  );
}

function RunningBody({ run }: { run: BulkRun }) {
  const done = processed(run);
  const pct = run.total > 0 ? Math.round((done / run.total) * 100) : 0;
  const last = run.items.at(-1);
  return (
    <>
      <div className="flex items-center justify-between mb-2">
        <span className="text-sm text-fg-2">Processing {done} of {run.total}</span>
        <span className="mono text-xs text-fg-4 tabular-nums">{pct}%</span>
      </div>
      <div className="h-1.5 bg-bg-3 rounded-full overflow-hidden mb-3">
        <div
          className="h-full bg-accent transition-all"
          style={{ width: `${pct}%` }}
        />
      </div>
      <Counts counts={run.counts} research={run.research} />
      {last && (
        <div className="text-xs text-fg-4 truncate">
          Latest: <span className={`mono ${
            last.outcome === "fixed" ? "text-ok" :
            last.outcome === "error" ? "text-err" : "text-fg-3"
          }`}>{OUTCOME_WORD[last.outcome]}</span>
          {" · "}{last.album}
        </div>
      )}
    </>
  );
}

function DoneBody({ summary }: { summary: QuickFixSummary }) {
  // `fixed` counts only findings that are gone; a fix that wrote part of
  // what its finding asks for is `still_open`.
  const stillOpen = summary.still_open ?? 0;
  const partial = summary.fixed > 0 && summary.skipped + summary.errors + stillOpen > 0;
  if (summary.sent_for_research !== undefined) return <ResearchDoneBody summary={summary} />;
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
              : "No findings were resolvable. The reasons are listed below."}
            {stillOpenNote}
          </>
        ) : (
          <>
            Resolved <b>{summary.fixed}</b> of {summary.total} finding
            {summary.total === 1 ? "" : "s"}.
            {stillOpenNote}
            {partial && " The rest stay open; the reasons are listed below."}
          </>
        )}
      </p>
    </>
  );
}

/** A run with research on: what was settled, sent and already requested. */
function ResearchDoneBody({ summary }: { summary: QuickFixSummary }) {
  const sent = summary.sent_for_research ?? 0;
  const already = summary.already_requested ?? 0;
  // Missing albums (skipped before the resolver) count as not applicable,
  // and a fix that wrote part of what its finding asks for as still open:
  // neither is settled, sent or skipped, so each is named when there is one.
  const notApplicable = summary.not_applicable ?? 0;
  const stillOpen = summary.still_open ?? 0;
  return (
    <>
      <div className="grid grid-cols-5 gap-2 text-xs mb-3">
        <Stat label="Settled" value={summary.fixed} tone="ok" />
        <Stat label="Research" value={sent} tone="muted" />
        <Stat label="Requested" value={already} tone="muted" />
        <Stat label="Skipped" value={summary.skipped} tone="muted" />
        <Stat label="Errors" value={summary.errors} tone="err" />
      </div>
      <p className="text-sm text-fg-2" data-bulk-research-summary>
        Settled {summary.fixed}. Sent {sent} for research{sent > 0 ? " (one batch)" : ""}. {already} already
        requested. Skipped {summary.skipped}, failed {summary.errors}.
        {notApplicable > 0 && ` ${notApplicable} not applicable (missing from disk or no quick fix).`}
        {stillOpen > 0 && ` ${stillOpen} partly written, still open.`}
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

const OUTCOME_WORD: Record<BulkOutcome, string> = {
  fixed: "fixed",
  skipped: "skipped",
  error: "failed",
  "not-applicable": "not applicable",
  "sent-for-research": "sent for research",
  "already-requested": "already requested",
};

// Problems first; fixed findings last and collapsed.
const GROUPS: ReadonlyArray<{ outcome: BulkOutcome; title: string; tone: string }> = [
  { outcome: "error", title: "Failed", tone: "text-err" },
  { outcome: "skipped", title: "Skipped", tone: "text-fg-2" },
  { outcome: "not-applicable", title: "Not applicable", tone: "text-fg-2" },
  { outcome: "sent-for-research", title: "Sent for research", tone: "text-fg-2" },
  { outcome: "already-requested", title: "Already requested", tone: "text-fg-2" },
  { outcome: "fixed", title: "Fixed", tone: "text-ok" },
];

/**
 * Every finding a run handled, grouped by outcome, problems first. With
 * `onOpenAlbum` (Activity), each album is a link to its panel.
 */
export function ResultsList({
  items,
  stillOpen,
  onOpenAlbum,
  scroll = true,
}: {
  items: BulkResultItem[];
  stillOpen: number;
  onOpenAlbum?: () => void;
  /** Scroll inside a capped height (the dialog); off where the page scrolls. */
  scroll?: boolean;
}) {
  if (items.length === 0) return null;
  return (
    <div className={`mt-3 space-y-3 ${scroll ? "max-h-[40vh] overflow-y-auto" : ""}`} data-testid="bulk-results">
      {GROUPS.map((g) => {
        const rows = items.filter((i) => i.outcome === g.outcome);
        return rows.length === 0 ? null : (
          <ResultGroup
            key={g.outcome}
            title={g.title}
            tone={g.tone}
            rows={rows}
            onOpenAlbum={onOpenAlbum}
            collapsed={g.outcome === "fixed"}
            note={
              g.outcome === "fixed" && stillOpen > 0
                ? `${stillOpen} of these did not fully resolve their finding and ${stillOpen === 1 ? "is" : "are"} still open.`
                : undefined
            }
          />
        );
      })}
    </div>
  );
}

function ResultGroup({
  title,
  tone,
  rows,
  collapsed,
  note,
  onOpenAlbum,
}: {
  title: string;
  tone: string;
  rows: BulkResultItem[];
  collapsed: boolean;
  note?: string;
  onOpenAlbum?: () => void;
}) {
  const [open, setOpen] = useState(!collapsed);
  const [all, setAll] = useState(false);
  const shown = all ? rows : rows.slice(0, RESULT_ROW_LIMIT);
  return (
    <section aria-label={title}>
      <button
        type="button"
        onClick={() => setOpen((o) => !o)}
        aria-expanded={open}
        className={`mono text-[11px] uppercase tracking-[0.06em] ${tone} hover:underline`}
      >
        {title} ({rows.length}) {open ? "\u2212" : "+"}
      </button>
      {note && <p className="text-xs text-fg-3 mt-0.5">{note}</p>}
      {open && (
        <>
          <ul className="mt-1 rounded border border-line bg-panel-2 divide-y divide-line">
            {shown.map((r) => (
              <li key={r.findingId} className="px-2 py-1.5 text-xs">
                {onOpenAlbum && r.albumId ? (
                  <Link
                    to={`/library/${encodeURIComponent(r.albumId)}`}
                    onClick={onOpenAlbum}
                    className="block text-fg truncate hover:text-accent hover:underline"
                    title={`Open ${r.album}`}
                  >
                    {r.album}
                  </Link>
                ) : (
                  <div className="text-fg truncate" title={r.album}>{r.album}</div>
                )}
                {r.reason && <div className="text-fg-3">{r.reason}</div>}
              </li>
            ))}
          </ul>
          {!all && rows.length > RESULT_ROW_LIMIT && (
            <button
              type="button"
              onClick={() => setAll(true)}
              className="mt-2 h-7 px-2.5 border border-line rounded text-xs text-fg-2 hover:bg-bg-3 transition"
            >
              Show all {rows.length}
            </button>
          )}
        </>
      )}
    </section>
  );
}
