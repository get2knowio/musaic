import { useEffect, useRef, useState } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import { Link, useLocation } from "react-router-dom";
import { toast } from "sonner";
import {
  Activity as ActivityIcon,
  AlertTriangle,
  CheckCircle2,
  Clock,
  Loader2,
  PauseCircle,
  RotateCcw,
  Trash2,
  X,
  XCircle,
} from "lucide-react";

import { useActivity } from "@/hooks/useActivity";
import { checkScanStatus } from "@/hooks/useScanFollower";
import { useBulkDeleteJobs, useCancelResearchGroup, useQueueState } from "@/hooks/useJobs";
import { ResultsList, RunOutcome } from "@/components/BulkQuickFixDialog";
import { ResearchBatchEntry } from "@/components/ResearchBatch";
import { useResearchBatches } from "@/hooks/useResearchBatches";
import { StatusPill, jobTypeLabel } from "@/components/JobRow";
import { fmtRelative } from "@/lib/relativeTime";
import { isHistoryOnlyJob } from "@/lib/filters";
import {
  STATE_LABEL,
  progressText,
  type ActivityState,
  type Summary,
} from "@/lib/activityModel";
import {
  ACTIVITY_PANEL_ID,
  ACTIVITY_TOGGLE_ID,
  closeActivityPanel,
  useActivityPanel,
} from "@/lib/activityPanel";
import { clearFinishedBulkRuns, stopBulkRun, type BulkRun } from "@/lib/bulkRuns";
import { reviewUrl } from "@/lib/reviewLinks";
import { cancelRunningScan, isScanActive } from "@/lib/scanActivity";
import type { Job } from "@/lib/types";

/** Finished jobs listed before "Show all N". */
const HISTORY_LIMIT = 20;

const QUIET_BUTTON =
  "h-7 px-2.5 border border-line rounded text-xs text-fg-2 hover:bg-bg-3 transition inline-flex items-center gap-1.5 " +
  "focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent disabled:opacity-60 disabled:cursor-not-allowed";
const LINK = "text-accent hover:underline focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent rounded-sm";

/**
 * The Activity panel: every piece of background work in one place, over
 * whatever page is open (it never changes the route; closing it leaves the
 * user where they were). Opened by the toolbar control, by
 * `openActivityPanel()` (lib/activityPanel.ts) or by a `?activity=` link.
 *
 * Sections: the library scan, bulk fixes, background checks,
 * fingerprinting, research requests (in progress, then history). Each says its state
 * in words (running, waiting, stopped, failed, completed, or that its
 * progress is out of date or unavailable) and offers Cancel / Stop only
 * where the API can do it: the scan, a bulk run, a waiting or running
 * research request.
 *
 * Links (an album, Review) close the panel and go there; the album panel's
 * Close and Back return to the page the user was on.
 *
 * Keyboard: a modal dialog. Focus moves into it on open (to the section a
 * link asked for, else its heading), Tab stays inside, Escape closes, and
 * focus returns to the control that opened it.
 */
export default function ActivityPanel() {
  const panel = useActivityPanel();
  const navigating = useRef(false);
  const content = useRef<HTMLDivElement>(null);
  const returnTo = useRef<HTMLElement | null>(null);

  const onNavigate = () => {
    navigating.current = true;
    closeActivityPanel();
  };

  return (
    <Dialog.Root open={panel.open} onOpenChange={(o) => { if (!o) closeActivityPanel(); }}>
      <Dialog.Portal>
        <Dialog.Overlay className="fixed inset-0 top-[var(--titlebar-h)] z-30 bg-black/25" />
        <Dialog.Content
          id={ACTIVITY_PANEL_ID}
          ref={content}
          className="fixed right-0 top-[var(--titlebar-h)] bottom-0 z-40 w-[min(440px,100vw)] bg-panel border-l border-line shadow-sheet outline-none flex flex-col"
          onOpenAutoFocus={(e) => {
            e.preventDefault();
            navigating.current = false;
            // Where focus goes back to: whatever had it (the toolbar
            // control, the Review rail entry, a page field for a menu).
            const before = document.activeElement;
            returnTo.current = before instanceof HTMLElement && before !== document.body ? before : null;
            const root = content.current;
            const target =
              (panel.section && root?.querySelector<HTMLElement>(`[data-activity-section="${panel.section}"]`)) ||
              root?.querySelector<HTMLElement>("[data-activity-title]");
            target?.focus();
            target?.scrollIntoView?.({ block: "start" });
          }}
          onCloseAutoFocus={(e) => {
            // Radix would focus a Dialog.Trigger; the panel has none (it
            // opens from several places), so focus goes back by hand.
            e.preventDefault();
            // Going somewhere: that page places focus.
            if (!navigating.current) {
              const back = returnTo.current?.isConnected ? returnTo.current : document.getElementById(ACTIVITY_TOGGLE_ID);
              back?.focus();
            }
            navigating.current = false;
            returnTo.current = null;
          }}
          onPointerDownOutside={(e) => {
            // The toolbar control toggles the panel itself.
            const t = e.target as Element | null;
            if (t?.closest(`#${ACTIVITY_TOGGLE_ID}`)) e.preventDefault();
          }}
        >
          <div className="flex items-center gap-2 px-4 h-11 border-b border-line shrink-0">
            <ActivityIcon className="w-3.5 h-3.5 text-accent" aria-hidden />
            <Dialog.Title data-activity-title tabIndex={-1} className="font-semibold text-md outline-none">
              Activity
            </Dialog.Title>
            <div className="flex-1" />
            <Dialog.Close
              className="w-8 h-8 grid place-items-center text-fg-3 hover:text-fg hover:bg-bg-3 rounded transition focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent"
              aria-label="Close Activity"
            >
              <X className="w-4 h-4" />
            </Dialog.Close>
          </div>
          <Dialog.Description className="sr-only">
            Scans, background checks, fingerprinting, bulk fixes and research requests. Press Escape to close.
          </Dialog.Description>
          <div className="flex-1 min-h-0 overflow-y-auto px-4 py-3 space-y-3">
            <ActivityBody onNavigate={onNavigate} markedJob={panel.job} />
          </div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}

function ActivityBody({ onNavigate, markedJob }: { onNavigate: () => void; markedJob: string | null }) {
  const a = useActivity();
  return (
    <>
      <ScanCard summary={a.scan.summary} active={isScanActive(a.scan.data.phase)} phase={a.scan.data.phase} missing={a.scan.data.markedMissing} onNavigate={onNavigate} />
      {a.bulk.length > 0 && <BulkSection runs={a.bulk} onNavigate={onNavigate} />}
      <Card id="checks" title="Background checks" summary={a.sweep.summary}>
        {a.sweep.summary.state === "completed" && (
          <Link to={reviewUrl()} onClick={onNavigate} className={`text-xs ${LINK}`}>
            Open Review
          </Link>
        )}
      </Card>
      <Card id="fingerprint" title="Fingerprinting" summary={a.pool.summary} />
      <JobsSection jobs={a.jobs} onNavigate={onNavigate} markedJob={markedJob} />
      <BatchesSection onNavigate={onNavigate} />
    </>
  );
}

// ---- shared pieces ----

const STATE_TONE: Record<ActivityState, string> = {
  running: "text-accent border-accent/40 bg-accent-soft",
  waiting: "text-fg-2 border-line bg-bg-3",
  stopped: "text-warn border-warn/40 bg-warn-soft",
  failed: "text-err border-err/40 bg-err/10",
  completed: "text-ok border-ok/40 bg-ok/10",
  idle: "text-fg-3 border-line bg-panel-2",
  stale: "text-warn border-warn/40 bg-warn-soft",
  unavailable: "text-warn border-warn/40 bg-warn-soft",
};

function StateIcon({ state }: { state: ActivityState }) {
  const cls = "w-3 h-3";
  switch (state) {
    case "running":
      return <Loader2 className={`${cls} spin`} aria-hidden />;
    case "waiting":
      return <Clock className={cls} aria-hidden />;
    case "stopped":
      return <PauseCircle className={cls} aria-hidden />;
    case "failed":
      return <XCircle className={cls} aria-hidden />;
    case "completed":
      return <CheckCircle2 className={cls} aria-hidden />;
    case "stale":
    case "unavailable":
      return <AlertTriangle className={cls} aria-hidden />;
    case "idle":
      return null;
  }
}

export function StateBadge({ state }: { state: ActivityState }) {
  return (
    <span
      data-activity-state={state}
      className={`inline-flex items-center gap-1 h-5 px-1.5 rounded border mono text-[10.5px] uppercase tracking-[0.04em] whitespace-nowrap ${STATE_TONE[state]}`}
    >
      <StateIcon state={state} />
      {STATE_LABEL[state]}
    </span>
  );
}

function ProgressBar({ summary }: { summary: Summary }) {
  const p = summary.progress;
  if (!p) return null;
  const known = p.total !== null && p.total > 0;
  const pct = known ? Math.min(100, Math.round((p.done / (p.total as number)) * 100)) : 0;
  return (
    <div className="mt-2">
      <div className="flex items-center justify-between text-xs text-fg-3 mb-1">
        <span className="tabular-nums">{progressText(p)}</span>
        {known && <span className="mono tabular-nums">{pct}%</span>}
      </div>
      <div
        role="progressbar"
        aria-label={`${summary.verb} progress`}
        aria-valuemin={0}
        aria-valuemax={known ? (p.total as number) : undefined}
        aria-valuenow={known ? Math.min(p.done, p.total as number) : undefined}
        aria-valuetext={progressText(p) + (summary.state === "stale" ? " (out of date)" : "")}
        className="h-1 bg-bg-3 rounded border border-line-2 overflow-hidden"
      >
        <div
          className={`h-full transition-[width] duration-300 ${summary.state === "stale" ? "bg-warn" : "bg-accent"}`}
          style={{ width: known ? `${pct}%` : "0%" }}
        />
      </div>
    </div>
  );
}

function Card({
  id,
  title,
  summary,
  children,
  section,
}: {
  id: string;
  title: string;
  summary: Summary;
  children?: React.ReactNode;
  /** A part a link may ask to focus (`openActivityPanel({ section })`). */
  section?: string;
}) {
  const headingId = `activity-${id}-heading`;
  return (
    <section
      aria-labelledby={headingId}
      data-activity={id}
      data-activity-section={section}
      tabIndex={section ? -1 : undefined}
      className="rounded-md border border-line bg-panel-2 px-3 py-2.5 outline-none focus-visible:ring-2 focus-visible:ring-accent"
    >
      <div className="flex items-center gap-2">
        <h3 id={headingId} className="text-sm font-medium flex-1 min-w-0 truncate">
          {title}
        </h3>
        <StateBadge state={summary.state} />
      </div>
      <p className="mt-1 text-xs text-fg-3">{summary.detail}</p>
      <ProgressBar summary={summary} />
      {children && <div className="mt-2 flex flex-wrap items-center gap-2">{children}</div>}
    </section>
  );
}

/** A link to an album that returns to this page when the album is closed. */
function useAlbumHref() {
  const location = useLocation();
  const onLibrary = location.pathname === "/library" || location.pathname.startsWith("/library/");
  // On Library, keep its filters so closing the album lands on the same view.
  return (id: string) => `/library/${encodeURIComponent(id)}${onLibrary ? location.search : ""}`;
}

// ---- the scan ----

function ScanCard({
  summary,
  active,
  phase,
  missing,
  onNavigate,
}: {
  summary: Summary;
  active: boolean;
  phase: string;
  missing: number;
  onNavigate: () => void;
}) {
  const [asking, setAsking] = useState(false);
  const cancelling = phase === "cancelling";
  const onCancel = async () => {
    setAsking(true);
    try {
      const r = await cancelRunningScan();
      if (r === "too_late") {
        toast("The scan is already saving its result", { description: "It will finish in a moment." });
      }
    } catch (err) {
      toast.error((err as Error).message || "Couldn't cancel the scan");
    } finally {
      setAsking(false);
    }
  };
  return (
    <Card id="scan" title="Library scan" summary={summary}>
      {active && (
        <button
          type="button"
          className={QUIET_BUTTON}
          disabled={cancelling || asking || phase === "starting"}
          onClick={() => void onCancel()}
          title="Stop the scan. Nothing it has read is saved; the library keeps its previous scan."
        >
          {cancelling ? "Cancelling…" : "Cancel scan"}
        </button>
      )}
      {phase === "lost" && (
        <button type="button" className={QUIET_BUTTON} onClick={() => void checkScanStatus()}>
          <RotateCcw className="w-3 h-3" aria-hidden />
          Check again
        </button>
      )}
      {phase === "done" && missing > 0 && (
        <Link to="/library?filter=missing" onClick={onNavigate} className={`text-xs ${LINK}`}>
          Show {missing} missing {missing === 1 ? "album" : "albums"}
        </Link>
      )}
      {phase === "done" && (
        <Link to="/library" onClick={onNavigate} className={`text-xs ${LINK}`}>
          Open Library
        </Link>
      )}
    </Card>
  );
}

// ---- bulk fixes ----

function BulkSection({ runs, onNavigate }: { runs: { run: BulkRun; summary: Summary }[]; onNavigate: () => void }) {
  const finished = runs.filter((r) => r.run.phase !== "running").length;
  return (
    <div className="space-y-2" data-activity="bulk">
      <div className="flex items-center gap-2 pt-1">
        <h3 className="text-xs font-medium text-fg-3 flex-1">Bulk fixes</h3>
        {finished > 0 && (
          <button type="button" className={QUIET_BUTTON} onClick={clearFinishedBulkRuns}>
            Clear finished
          </button>
        )}
      </div>
      {runs.map(({ run, summary }) => (
        <BulkCard key={run.id} run={run} summary={summary} onNavigate={onNavigate} />
      ))}
    </div>
  );
}

function BulkCard({ run, summary, onNavigate }: { run: BulkRun; summary: Summary; onNavigate: () => void }) {
  const [open, setOpen] = useState(false);
  const resultsId = `activity-bulk-${run.id}-results`;
  return (
    <Card id={`bulk-${run.id}`} title={run.actionLabel} summary={summary}>
      {run.phase === "running" && (
        <button
          type="button"
          className={QUIET_BUTTON}
          onClick={() => stopBulkRun(run.id)}
          title="Start no more findings. What was already fixed stays fixed."
        >
          Stop
        </button>
      )}
      {run.phase !== "running" && run.phase !== "failed" && (
        <button
          type="button"
          className={QUIET_BUTTON}
          aria-expanded={open}
          aria-controls={resultsId}
          onClick={() => setOpen((o) => !o)}
        >
          {open ? "Hide results" : `Show results (${run.items.length})`}
        </button>
      )}
      <Link to={reviewUrl({ check: run.checkId })} onClick={onNavigate} className={`text-xs ${LINK}`}>
        Open {run.checkLabel} in Review
      </Link>
      {open && (
        <div id={resultsId} className="w-full">
          <RunOutcome run={run} />
          <ResultsList items={run.items} stillOpen={run.summary?.still_open ?? 0} onOpenAlbum={onNavigate} scroll={false} />
        </div>
      )}
    </Card>
  );
}

// ---- research requests ----

function JobsSection({
  jobs,
  onNavigate,
  markedJob,
}: {
  jobs: ReturnType<typeof useActivity>["jobs"];
  onNavigate: () => void;
  markedJob: string | null;
}) {
  const clear = useBulkDeleteJobs();
  // What Clear removes, counted by the server in requests (the jobs
  // stream's queue_state), not in job cards.
  const { data: queue } = useQueueState();
  const acted = queue?.clearable ?? 0;
  const [all, setAll] = useState(false);
  // A marked job deeper in the history is shown, not hidden behind "Show all".
  const markedAt = markedJob ? jobs.finished.findIndex((j) => j.id === markedJob) : -1;
  const shown = all || markedAt >= HISTORY_LIMIT ? jobs.finished : jobs.finished.slice(0, HISTORY_LIMIT);

  useEffect(() => {
    if (!markedJob) return;
    document.querySelector(`[data-activity-job="${CSS.escape(markedJob)}"]`)?.scrollIntoView?.({ block: "nearest" });
  }, [markedJob]);

  return (
    <>
      <Card id="jobs" section="jobs" title="Research requests" summary={jobs.summary}>
        {jobs.inProgress.length > 0 && (
          <ul aria-label="Research requests in progress" className="w-full rounded border border-line bg-panel divide-y divide-line">
            {jobs.inProgress.map((j) => (
              <JobLine key={j.id} job={j} marked={j.id === markedJob} onNavigate={onNavigate} />
            ))}
          </ul>
        )}
      </Card>
      {(jobs.finished.length > 0 || markedJob) && (
        <section
          aria-labelledby="activity-history-heading"
          data-activity="history"
          data-activity-section="history"
          tabIndex={-1}
          className="rounded-md border border-line bg-panel-2 px-3 py-2.5 outline-none focus-visible:ring-2 focus-visible:ring-accent"
        >
          <div className="flex items-center gap-2">
            <h3 id="activity-history-heading" className="text-sm font-medium flex-1">
              Research history
            </h3>
            {acted > 0 && (
              <button
                type="button"
                className={QUIET_BUTTON}
                disabled={clear.isPending}
                onClick={() =>
                  clear.mutate(["acted_on"], {
                    onError: (err) => toast.error((err as Error).message || "Could not clear the history"),
                  })
                }
              >
                <Trash2 className="w-3 h-3" aria-hidden />
                Clear {acted} applied or discarded
              </button>
            )}
          </div>
          <p className="mt-1 text-xs text-fg-3">
            Finished research requests. A proposed change or a failure waits in Review until you act on it.
          </p>
          {jobs.finished.length > 0 ? (
            <ul aria-label="Research history" className="mt-2 rounded border border-line bg-panel divide-y divide-line">
              {shown.map((j) => (
                <JobLine key={j.id} job={j} marked={j.id === markedJob} onNavigate={onNavigate} />
              ))}
            </ul>
          ) : (
            <p className="mt-2 text-xs text-fg-3">That research request is no longer listed.</p>
          )}
          {shown.length < jobs.finished.length && (
            <button type="button" className={`mt-2 ${QUIET_BUTTON}`} onClick={() => setAll(true)}>
              Show all {jobs.finished.length}
            </button>
          )}
        </section>
      )}
    </>
  );
}

// ---- research batches ----

/** Each Fix all batch once, with its size, counts and Cancel; its items are
 * in Review. Nothing when there is none. */
function BatchesSection({ onNavigate }: { onNavigate: () => void }) {
  const { data } = useResearchBatches();
  const batches = Array.isArray(data) ? data : [];
  if (batches.length === 0) return null;
  return (
    <section
      aria-labelledby="activity-batches-heading"
      data-activity="batches"
      data-activity-section="batches"
      tabIndex={-1}
      className="rounded-md border border-line bg-panel-2 px-3 py-2.5 outline-none focus-visible:ring-2 focus-visible:ring-accent"
    >
      <h3 id="activity-batches-heading" className="text-sm font-medium">
        Research from Fix all
      </h3>
      <ul aria-label="Research from Fix all" className="mt-2 rounded border border-line bg-panel divide-y divide-line">
        {batches.map((b) => (
          <li key={b.batchId} className="px-2 py-1.5">
            <ResearchBatchEntry batch={b}>
              <Link to={reviewUrl()} onClick={onNavigate} className={`text-xs ${LINK}`}>
                See the requests in Review
              </Link>
            </ResearchBatchEntry>
          </li>
        ))}
      </ul>
    </section>
  );
}

function JobLine({ job, marked, onNavigate }: { job: Job; marked: boolean; onNavigate: () => void }) {
  const albumHref = useAlbumHref();
  const cancel = useCancelResearchGroup();
  const when = job.completed_at ?? job.started_at ?? job.created_at;
  // An old job's history-only record has no review and nothing to cancel.
  const history = isHistoryOnlyJob(job);
  return (
    <li
      data-activity-job={job.id}
      data-current={marked ? "true" : undefined}
      className={`px-2 py-1.5 text-xs ${marked ? "bg-accent-soft" : ""}`}
    >
      <div className="flex items-center gap-2 min-w-0">
        <StatusPill status={job.status} actedOnAction={job.acted_on_action} />
        <Link
          to={albumHref(job.album_id)}
          onClick={onNavigate}
          className="flex-1 min-w-0 truncate text-fg hover:text-accent hover:underline"
          title={`Open ${job.album_title || "the album"}`}
        >
          {job.album_title || job.album_id}
        </Link>
        <span className="text-fg-4 shrink-0">{fmtRelative(when)}</span>
      </div>
      <div className="mt-0.5 flex items-center gap-2 text-fg-3">
        <span>{jobTypeLabel(job.job_type)}</span>
        {job.status === "completed" && !history && (
          <Link to={reviewUrl({ kind: "proposed", job: job.id })} onClick={onNavigate} className={LINK}>
            Review the proposal
          </Link>
        )}
        {job.status === "failed" && !history && (
          <Link to={reviewUrl({ kind: "failed", job: job.id })} onClick={onNavigate} className={LINK}>
            See in Review
          </Link>
        )}
        {(job.status === "queued" || job.status === "running") && !history && (
          // Cancels every waiting, held or running request of the card
          // (FR-036); a 409 means nothing was left to cancel.
          <button
            type="button"
            className={`ml-auto ${QUIET_BUTTON}`}
            disabled={cancel.isPending}
            onClick={() =>
              cancel.mutate(job.id, {
                onError: (err) => toast.error((err as Error).message || "Couldn't cancel the research request"),
              })
            }
            aria-label={`Cancel the ${jobTypeLabel(job.job_type).toLowerCase()} research request for ${job.album_title || "this album"}`}
          >
            Cancel
          </button>
        )}
      </div>
      {job.status === "failed" && job.error && (
        <div className="mt-0.5 text-err truncate" title={job.error}>
          {job.error}
        </div>
      )}
    </li>
  );
}
