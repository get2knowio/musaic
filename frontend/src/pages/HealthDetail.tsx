import { useEffect, useMemo, useRef, useState } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import { Link, useNavigate, useParams } from "react-router-dom";
import {
  AlertCircle,
  ArrowLeft,
  ArrowUpRight,
  CheckCircle2,
  Eye,
  EyeOff,
  Info,
  Loader2,
  Music,
  Sparkles,
  X,
  XCircle,
  Zap,
} from "lucide-react";
import { toast } from "sonner";
import {
  streamQuickFixAll,
  useCheck,
  useRunCheck,
  useUnignoreFinding,
  type QuickFixProgress,
  type QuickFixSummary,
} from "@/hooks/useChecks";
import { useAlbumsSnapshot } from "@/hooks/useAlbumsSnapshot";
import { usePageTitle } from "@/hooks/usePageTitle";
import { useAiStatus } from "@/hooks/useAiStatus";
import FindingFixButton from "@/components/FindingFixButton";
import IgnoreFindingControl from "@/components/IgnoreFindingControl";
import { displayArtist } from "@/lib/albumFields";
import type { Album, CheckRun, Finding } from "@/lib/types";

/**
 * Drilldown for a single check. Open findings on the left (the bulk of the
 * page), recent runs in a small sidebar on the right.
 */
export default function HealthDetailPage() {
  const { checkId } = useParams<{ checkId: string }>();
  const navigate = useNavigate();
  const { data: detail, isLoading, error } = useCheck(checkId);
  usePageTitle(detail?.meta.label ? `${detail.meta.label} · Health` : "Health");
  // Source the findings from the check-detail response itself rather
  // than from the global useFindings cache. The global one is hard-
  // capped at 10k rows and filters client-side; on big libraries that
  // can hide a check's entire result set behind another check's
  // newer-timestamped findings. The check-detail endpoint already
  // returns ALL open findings for this one check, scoped server-side.
  const findings = detail?.findings ?? [];
  const runMutation = useRunCheck();
  // ALL hooks must run on every render (Rules of Hooks). Keep useState
  // above the early returns even though `bulkOpen` isn't used until later.
  const [bulkOpen, setBulkOpen] = useState(false);
  // "no AI quota" only when a model can run.
  const ai = useAiStatus();

  if (isLoading && !detail) {
    return <div className="flex-1 grid place-items-center text-fg-3 text-sm">Loading…</div>;
  }
  if (error || !detail) {
    return (
      <div className="flex-1 grid place-items-center text-fg-3 text-sm">
        <div className="text-center">
          <p className="text-md font-medium mb-1.5">Check not found</p>
          <button onClick={() => navigate("/health")} className="text-accent text-sm hover:underline">
            ← Back to Health
          </button>
        </div>
      </div>
    );
  }

  const onRun = () => {
    runMutation.mutate(
      { checkId: detail.meta.id, scope: "all" },
      { onError: (e) => toast.error((e as Error).message || "Couldn't start check") },
    );
  };

  const lastRun = detail.runs[0];
  const isRunning = lastRun?.status === "running";
  const openCount = findings.filter((f) => !f.ignored_at).length;

  return (
    <div className="flex-1 overflow-y-auto">
      {/* Header */}
      <div className="flex items-center gap-3 px-4 h-[var(--filterbar-h)] border-b border-line bg-panel-2 text-sm shrink-0">
        <Link to="/health" className="inline-flex items-center gap-1 text-fg-3 hover:text-fg transition">
          <ArrowLeft className="w-3.5 h-3.5" /> Health
        </Link>
        <span className="text-fg-4 mono text-xs">/</span>
        <span className="font-medium truncate">{detail.meta.label}</span>
        <span className="ml-auto" />
        {/* Per-check bulk action — only rendered when the check declares
            a bulk_action_label. Different checks need different verbs
            ("Try MusicBrainz on all", "Identify on MusicBrainz",
            "Fingerprint all"). Hidden when the check has no bulk path
            (e.g. duplicate-tracks where every fix is per-file). */}
        {detail.meta.bulk_action_label && openCount > 0 && (
          <button
            onClick={() => setBulkOpen(true)}
            className="h-7 px-2.5 border border-line rounded text-xs text-fg-2 hover:bg-bg-3 transition inline-flex items-center gap-1.5"
            title={
              ai.ready
                ? `Run "${detail.meta.bulk_action_label}" against every open finding — no AI quota burned`
                : `Run "${detail.meta.bulk_action_label}" against every open finding`
            }
          >
            <Music className="w-3 h-3" />
            {detail.meta.bulk_action_label} ({openCount})
          </button>
        )}
        <button
          onClick={onRun}
          disabled={isRunning || runMutation.isPending}
          className="h-7 px-2.5 border border-line rounded text-xs text-fg-2 hover:bg-bg-3 transition disabled:opacity-60 inline-flex items-center gap-1.5"
        >
          {isRunning || runMutation.isPending ? (
            <><Loader2 className="w-3 h-3 spin" /> Running…</>
          ) : (
            <><Sparkles className="w-3 h-3" /> Run now</>
          )}
        </button>
      </div>

      <div className="grid grid-cols-[1fr_280px] gap-4 px-4 py-4 min-h-0">
        <div className="min-w-0 space-y-3">
          <FindingsList
            checkId={detail.meta.id}
            findings={findings}
            fixKind={detail.meta.fix_kind}
            description={detail.meta.description}
          />
        </div>
        <RunsSidebar runs={detail.runs} />
      </div>

      <BulkQuickFixDialog
        open={bulkOpen}
        onClose={() => setBulkOpen(false)}
        checkId={detail.meta.id}
        checkLabel={detail.meta.label}
        bulkActionLabel={detail.meta.bulk_action_label ?? "Run bulk fix"}
        fixKind={detail.meta.fix_kind}
        totalFindings={openCount}
      />
    </div>
  );
}

/**
 * Modal that runs the MB-only bulk pass against every open finding for a
 * check, streaming per-finding outcomes as they happen. Cancel mid-run
 * aborts the stream (the in-flight finding may still complete server-side
 * but no more start). On done, refreshes the findings cache so the trimmed
 * list shows.
 */
function BulkQuickFixDialog({
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

function FindingsList({
  checkId,
  findings,
  fixKind,
  description,
}: {
  checkId: string;
  findings: Finding[];
  fixKind: "none" | "ai-job" | "deterministic";
  /** What this check looks for. Surfaced in the empty state so a user
   *  who has zero findings still has an explanation of what they're
   *  looking at. Hidden from the populated list to keep the page tight. */
  description: string;
}) {
  const [showIgnored, setShowIgnored] = useState(false);
  const visible = showIgnored ? findings : findings.filter((f) => !f.ignored_at);

  // Look up albums by id once for the whole list — each row uses the same
  // Map and avoids re-filtering the (possibly large) album array per row.
  const { data: albums = [] } = useAlbumsSnapshot();
  const albumsById = useMemo(() => {
    const m = new Map<string, Album>();
    for (const a of albums) m.set(a.id, a);
    return m;
  }, [albums]);

  if (visible.length === 0) {
    return (
      <div className="border border-line rounded-md bg-panel-2 px-4 py-8 text-center">
        <CheckCircle2 className="w-8 h-8 mx-auto mb-2 text-ok" />
        <p className="text-md font-medium">No open findings</p>
        <p className="text-sm text-fg-3 mt-1 max-w-prose mx-auto leading-relaxed">{description}</p>
      </div>
    );
  }
  return (
    <div className="rounded-md border border-line bg-panel-2 overflow-hidden">
      <div className="flex items-center gap-3 px-3 py-2 border-b border-line text-xs text-fg-4">
        <span className="mono uppercase tracking-[0.06em]">{visible.length} finding{visible.length === 1 ? "" : "s"}</span>
        <span className="ml-auto" />
        <button
          onClick={() => setShowIgnored((v) => !v)}
          className="inline-flex items-center gap-1 text-fg-3 hover:text-fg transition"
        >
          {showIgnored ? <EyeOff className="w-3 h-3" /> : <Eye className="w-3 h-3" />}
          {showIgnored ? "Hide ignored" : "Show ignored"}
        </button>
      </div>
      <div className="divide-y divide-line">
        {visible.map((f) => (
          <FindingRow
            key={f.id}
            checkId={checkId}
            finding={f}
            fixKind={fixKind}
            album={f.album_id ? albumsById.get(f.album_id) ?? null : null}
            siblings={visible}
          />
        ))}
      </div>
    </div>
  );
}

function FindingRow({
  checkId,
  finding,
  fixKind,
  album,
  siblings,
}: {
  checkId: string;
  finding: Finding;
  fixKind: "none" | "ai-job" | "deterministic";
  /** Looked up from the album cache by the parent. Null = library-level
   *  finding OR the album row was deleted out from under it. */
  album: Album | null;
  /** All findings currently visible in the parent's list. Threaded
   *  through to FindingFixButton so cluster-resolve dialogs can
   *  chain through them after each apply. */
  siblings: Finding[];
}) {
  const unignoreMutation = useUnignoreFinding();

  const onUnignore = () => {
    unignoreMutation.mutate(
      { checkId, findingId: finding.id },
      { onError: (e) => toast.error((e as Error).message || "Couldn't restore") },
    );
  };

  const SevIcon = finding.severity === "error" ? XCircle : finding.severity === "info" ? Info : AlertCircle;
  const sevClass = finding.severity === "error" ? "text-err" : finding.severity === "info" ? "text-fg-3" : "text-warn";

  // Display name for the row's primary line.
  //
  // Most checks point at one specific album, so showing
  // "Title — Artist" is the natural headline. But some checks
  // describe a CLUSTER (e.g. inconsistent-artist-sort): the
  // album_id is just a deterministic representative pick for stable
  // upserting, and headlining one of the cluster's albums obscures
  // what the finding is actually about. Those checks include a
  // `headline` field in detail that we prefer over the album line.
  // Falls back to "unknown album · <id stub>" for stale findings
  // whose album disappeared since the last scan.
  const detailHeadline =
    finding.detail && typeof finding.detail.headline === "string"
      ? finding.detail.headline
      : null;
  const albumLine = detailHeadline
    ? detailHeadline
    : album
    ? formatAlbumLine(album)
    : finding.album_id
    ? `unknown album · ${finding.album_id.slice(0, 8)}`
    : "—";

  return (
    <div className={`px-3 py-2 flex items-start gap-3 ${finding.ignored_at ? "opacity-55" : ""}`}>
      <SevIcon className={`w-4 h-4 shrink-0 mt-0.5 ${sevClass}`} />
      <div className="flex-1 min-w-0">
        {/* Title row + jump-to-album icon. Same pattern as the Jobs
            page: the title itself isn't a link (keeping the visual
            consistent with non-clickable text), an inline
            ArrowUpRight icon jumps to /library/{album_id}. The route
            mounts LibraryPage with the album detail sheet open. */}
        <div className="text-sm flex items-center gap-1.5 min-w-0">
          <span
            className={
              "font-medium truncate "
              + (finding.album_id ? "" : "text-fg-3")
            }
            title={album?.path || finding.album_id || undefined}
          >
            {albumLine}
          </span>
          {finding.album_id && (
            <Link
              to={`/library/${encodeURIComponent(finding.album_id)}`}
              className="shrink-0 inline-flex items-center justify-center w-5 h-5 rounded text-fg-4 hover:text-accent hover:bg-bg-3 transition"
              title="View album detail"
              aria-label="View album detail"
            >
              <ArrowUpRight className="w-3.5 h-3.5" />
            </Link>
          )}
        </div>
        <div className="mono text-[11px] text-fg-4 mt-0.5 flex items-center gap-2 flex-wrap">
          <span className="text-fg-3 normal-case tracking-normal">
            {finding.summary}
          </span>
          <span>·</span>
          <span>seen {fmtRelative(finding.last_seen_at)}</span>
          {finding.ignored_reason && (
            <>
              <span>·</span>
              <span className="italic truncate" title={finding.ignored_reason}>
                ignored: {finding.ignored_reason}
              </span>
            </>
          )}
        </div>
      </div>
      <div className="flex items-center gap-1 shrink-0">
        <FindingFixButton finding={finding} fixKind={fixKind} siblings={siblings} />
        {finding.ignored_at ? (
          <button
            onClick={onUnignore}
            disabled={unignoreMutation.isPending}
            className="h-7 px-2 rounded border border-line text-xs text-fg-2 hover:bg-bg-3 hover:text-fg transition disabled:opacity-60"
          >
            Restore
          </button>
        ) : (
          <IgnoreFindingControl checkId={checkId} findingId={finding.id} />
        )}
      </div>
    </div>
  );
}

function RunsSidebar({ runs }: { runs: CheckRun[] }) {
  return (
    <div className="space-y-1.5">
      <h3 className="mono text-[11px] uppercase tracking-[0.08em] text-fg-4 px-1">Recent runs</h3>
      <div className="border border-line rounded-md bg-panel-2 divide-y divide-line">
        {runs.length === 0 ? (
          <div className="px-3 py-3 text-xs text-fg-4 italic">No runs yet</div>
        ) : (
          runs.map((r) => <RunRow key={r.id} run={r} />)
        )}
      </div>
    </div>
  );
}

function RunRow({ run }: { run: CheckRun }) {
  let StatusIcon = CheckCircle2;
  let statusClass = "text-ok";
  if (run.status === "running") { StatusIcon = Loader2; statusClass = "text-accent"; }
  else if (run.status === "failed") { StatusIcon = XCircle; statusClass = "text-err"; }
  else if (run.status === "cancelled") { StatusIcon = XCircle; statusClass = "text-fg-4"; }
  return (
    <div className="px-3 py-2 text-xs flex items-center gap-2">
      <StatusIcon className={`w-3 h-3 shrink-0 ${statusClass} ${run.status === "running" ? "spin" : ""}`} />
      <div className="flex-1 min-w-0">
        <div className="mono tabular-nums">
          {fmtRelative(run.completed_at || run.started_at)}
          {run.duration_ms != null && (
            <span className="text-fg-4"> · {run.duration_ms}ms</span>
          )}
        </div>
        <div className="text-fg-4 truncate">
          {run.scope === "all" ? "full" : run.scope === "incremental" ? "incremental" : "subset"} ·
          {" "}<span className={run.issue_count > 0 ? "text-warn font-semibold" : ""}>{run.issue_count}</span> findings
        </div>
        {run.error && (
          <div className="text-err truncate mt-0.5" title={run.error}>{run.error}</div>
        )}
      </div>
    </div>
  );
}

function fmtRelative(iso: string | null): string {
  if (!iso) return "—";
  const t = Date.parse(iso);
  if (Number.isNaN(t)) return iso;
  const diffSec = (Date.now() - t) / 1000;
  if (diffSec < 60) return "just now";
  if (diffSec < 3600) return `${Math.floor(diffSec / 60)}m ago`;
  if (diffSec < 86400) return `${Math.floor(diffSec / 3600)}h ago`;
  return `${Math.floor(diffSec / 86400)}d ago`;
}

/** Display label for an album in a finding row. Mirrors the artist logic in
 *  Tile / AlbumList: "Various Artists" for compilations, otherwise
 *  album_artist || artist. Falls back to dir_name when both are blank. */
function formatAlbumLine(a: Album): string {
  const artist = a.is_compilation
    ? "Various Artists"
    : displayArtist(a);
  const title = (a.title || a.dir_name || "").trim();
  if (artist && title) return `${title} — ${artist}`;
  return title || a.dir_name || a.id;
}
