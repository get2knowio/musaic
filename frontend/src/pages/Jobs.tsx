import { useMemo, useState } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import { useNavigate } from "react-router-dom";
import { CheckCircle2, Clock, Loader2, Pause, Play, RotateCcw, Sparkles, Trash2, XCircle } from "lucide-react";
import { toast } from "sonner";

import {
  useBulkDeleteJobs,
  useDeleteJob,
  useJobs,
  useQueueState,
  useResumeQueue,
  useRetryJob,
} from "@/hooks/useJobs";
import { usePageTitle } from "@/hooks/usePageTitle";
import type { Job, JobStatus } from "@/lib/types";

/** Pending delete action — drives the confirmation modal. */
type PendingDelete =
  | { kind: "single"; job: Job }
  | { kind: "bulk"; statuses: JobStatus[]; label: string; count: number };

const STATUS_FILTERS: { label: string; statuses: JobStatus[] | null }[] = [
  { label: "All",            statuses: null },
  { label: "Needs review",   statuses: ["completed", "failed"] },
  { label: "In progress",    statuses: ["queued", "running"] },
  { label: "Acted on",       statuses: ["acted_on"] },
];

// Sort by submit date, newest first. With the serial worker this naturally
// reads top-down as "queued (newest first) → running → past (newest first)" —
// the running job is always older than anything queued behind it, and past
// jobs are older still. Matches user's mental model without needing a
// dedicated status-grouping override.
function compareJobs(a: Job, b: Job): number {
  return a.created_at < b.created_at ? 1 : -1;
}

export default function JobsPage() {
  usePageTitle("AI Jobs");
  const { data: jobs = [], isLoading } = useJobs();
  const [filterIdx, setFilterIdx] = useState(0);
  const filter = STATUS_FILTERS[filterIdx];
  // Single piece of "pending delete" state — drives the confirm modal for
  // deletes that DO need a confirmation (completed-but-not-acted-on, failed,
  // queued). Already-acted-on jobs are history and skip the modal entirely
  // (it would just be friction — they've already been resolved).
  const [pendingDelete, setPendingDelete] = useState<PendingDelete | null>(null);
  const deleteJob = useDeleteJob();
  const bulkDelete = useBulkDeleteJobs();

  const onSingleDeleteRequest = (job: Job) => {
    // History rows (acted_on) can be deleted without ceremony.
    if (job.status === "acted_on") {
      deleteJob.mutate(job.id, {
        onError: (err) => {
          const e = err as Error & { status?: number };
          toast.error(e.message || "Could not delete job", { duration: 4000 });
        },
      });
      return;
    }
    setPendingDelete({ kind: "single", job });
  };

  const onBulkDeleteRequest = (p: PendingDelete & { kind: "bulk" }) => {
    // "Clear N acted-on" — already-resolved history, no confirm needed.
    if (p.statuses.length === 1 && p.statuses[0] === "acted_on") {
      bulkDelete.mutate(p.statuses, {
        onError: (err) => {
          const e = err as Error & { status?: number };
          toast.error(e.message || "Could not bulk delete", { duration: 4000 });
        },
      });
      return;
    }
    setPendingDelete(p);
  };

  const filtered = useMemo(() => {
    const rows = filter.statuses ? jobs.filter((j) => filter.statuses!.includes(j.status)) : jobs;
    return [...rows].sort(compareJobs);
  }, [jobs, filter]);

  // Buckets for header counters
  const counts = useMemo(() => ({
    completed: jobs.filter((j) => j.status === "completed").length,
    failed:    jobs.filter((j) => j.status === "failed").length,
    running:   jobs.filter((j) => j.status === "running").length,
    queued:    jobs.filter((j) => j.status === "queued").length,
    acted_on:  jobs.filter((j) => j.status === "acted_on").length,
  }), [jobs]);

  return (
    <div className="flex-1 overflow-y-auto">
      <Header counts={counts} onRequestBulkDelete={onBulkDeleteRequest} />
      <PausedBanner />
      <Filters filterIdx={filterIdx} setFilterIdx={setFilterIdx} />
      {isLoading && jobs.length === 0 ? (
        <div className="px-4 py-8 text-center text-fg-3 text-sm">Loading…</div>
      ) : filtered.length === 0 ? (
        <Empty filterLabel={filter.label} totalJobs={jobs.length} />
      ) : (
        <div className="px-4 py-3">
          <div className="rounded-md border border-line bg-panel-2 overflow-hidden">
            <div
              className="grid gap-3 px-3 py-2 border-b border-line text-fg-4 mono text-xs uppercase tracking-[0.06em]"
              style={{ gridTemplateColumns: "100px minmax(0, 1.2fr) 110px 110px 72px" }}
            >
              <span>Status</span>
              <span>Album</span>
              <span>Job</span>
              <span>When</span>
              <span />
            </div>
            {filtered.map((j) => (
              <JobRow
                key={j.id}
                job={j}
                onRequestDelete={onSingleDeleteRequest}
              />
            ))}
          </div>
        </div>
      )}
      <DeleteConfirmDialog pending={pendingDelete} onClose={() => setPendingDelete(null)} />
    </div>
  );
}

function Header({
  counts,
  onRequestBulkDelete,
}: {
  counts: Record<string, number>;
  onRequestBulkDelete: (p: PendingDelete) => void;
}) {
  return (
    <div className="flex items-center gap-3 px-4 h-[var(--filterbar-h)] border-b border-line bg-panel-2 text-sm shrink-0">
      <Sparkles className="w-3.5 h-3.5 text-accent" />
      <span className="font-medium">AI Jobs</span>
      <span className="text-fg-4 mono text-xs">·</span>
      <span className="mono text-fg-3">
        <b className="text-fg font-medium tabular-nums">{counts.completed}</b> completed
      </span>
      <span className="text-fg-4 mono text-xs">·</span>
      <span className="mono text-fg-3">
        <b className="text-fg font-medium tabular-nums">{counts.failed}</b> failed
      </span>
      <span className="text-fg-4 mono text-xs">·</span>
      <span className="mono text-fg-3">
        <b className="text-fg font-medium tabular-nums">{counts.queued + counts.running}</b> in flight
      </span>

      <div className="flex-1" />

      {counts.acted_on > 0 && (
        <button
          onClick={() => onRequestBulkDelete({
            kind: "bulk",
            statuses: ["acted_on"],
            label: "acted-on",
            count: counts.acted_on,
          })}
          className="h-7 px-2.5 border border-line rounded text-xs text-fg-2 hover:bg-bg-3 transition inline-flex items-center gap-1.5"
        >
          <Trash2 className="w-3 h-3" />
          Clear {counts.acted_on} acted-on
        </button>
      )}
      {counts.failed > 0 && (
        <button
          onClick={() => onRequestBulkDelete({
            kind: "bulk",
            statuses: ["failed"],
            label: "failed",
            count: counts.failed,
          })}
          className="h-7 px-2.5 border border-line rounded text-xs text-fg-2 hover:bg-bg-3 transition inline-flex items-center gap-1.5"
        >
          <Trash2 className="w-3 h-3" />
          Clear {counts.failed} failed
        </button>
      )}
    </div>
  );
}

/**
 * Surface the auto-paused state at the top of the Jobs page so the user
 * isn't confused why nothing is dispatching after a failure. Stays mounted
 * across filter changes (rendered between header + filters in JobsPage).
 */
function PausedBanner() {
  const { data: queueState } = useQueueState();
  const resume = useResumeQueue();
  if (!queueState?.paused) return null;
  const remaining = queueState.queued;
  return (
    <div className="mx-4 mt-3 mb-1 rounded-md border border-warn/40 bg-warn-soft px-3 py-2.5 flex items-center gap-3 text-sm">
      <Pause className="w-4 h-4 text-warn shrink-0" />
      <div className="flex-1 min-w-0">
        <div className="font-medium text-fg">AI queue paused after a failure</div>
        <div className="text-xs text-fg-3 mt-0.5">
          {remaining > 0
            ? `${remaining} job${remaining === 1 ? "" : "s"} queued behind it. Retry the failed job(s) or resume to keep dispatching.`
            : "Resume to dispatch any future jobs you queue."}
        </div>
      </div>
      <button
        onClick={() => resume.mutate(undefined, {
          onSuccess: () => toast.success("Queue resumed"),
          onError: () => toast.error("Couldn't resume queue"),
        })}
        disabled={resume.isPending}
        className="h-8 px-3 rounded bg-accent text-accent-fg text-sm font-medium hover:opacity-90 transition disabled:opacity-60 inline-flex items-center gap-1.5 shrink-0"
      >
        <Play className="w-3.5 h-3.5" />
        {resume.isPending ? "Resuming…" : "Resume"}
      </button>
    </div>
  );
}

function Filters({ filterIdx, setFilterIdx }: { filterIdx: number; setFilterIdx: (i: number) => void }) {
  return (
    <div className="flex items-center gap-1.5 px-4 py-2 border-b border-line bg-bg">
      {STATUS_FILTERS.map((f, i) => (
        <button
          key={f.label}
          onClick={() => setFilterIdx(i)}
          className={
            "h-7 px-2.5 rounded text-xs transition " +
            (filterIdx === i
              ? "bg-accent-soft text-accent border border-accent/30"
              : "text-fg-3 hover:bg-bg-3 border border-transparent")
          }
        >
          {f.label}
        </button>
      ))}
    </div>
  );
}

function Empty({ filterLabel, totalJobs }: { filterLabel: string; totalJobs: number }) {
  return (
    <div className="px-8 py-16 text-center">
      <Sparkles className="w-9 h-9 mx-auto mb-3 text-fg-4" />
      {totalJobs === 0 ? (
        <>
          <h3 className="text-md font-medium mb-1.5">No AI jobs yet</h3>
          <p className="text-sm text-fg-3 max-w-md mx-auto">
            Open an album and click <b>Suggest fixes</b> or <b>Find cover art</b> to queue up your first job.
            They'll run in the background — bebop around the library while they work.
          </p>
        </>
      ) : (
        <>
          <h3 className="text-md font-medium mb-1.5">No jobs match "{filterLabel}"</h3>
          <p className="text-sm text-fg-3">Try a different filter.</p>
        </>
      )}
    </div>
  );
}

function JobRow({ job, onRequestDelete }: { job: Job; onRequestDelete: (j: Job) => void }) {
  const navigate = useNavigate();
  const retry = useRetryJob();

  const onOpen = () => {
    if (job.status === "completed") {
      navigate(`/library/${encodeURIComponent(job.album_id)}`);
    }
  };

  // Running jobs are not deletable — the worker is mid-call and we have no
  // way to interrupt it. Disable the trash button rather than letting the
  // user click and get a server-side 409.
  const isRunning = job.status === "running";
  const canRetry = job.status === "failed";

  const onRetry = (e: React.MouseEvent) => {
    e.stopPropagation();
    retry.mutate(job.id, {
      onSuccess: () => toast.success(`Retrying ${jobTypeLabel(job.job_type).toLowerCase()}…`, { duration: 2500 }),
      onError: (err) => {
        const e = err as Error & { status?: number };
        toast.error(e.message || "Couldn't retry job");
      },
    });
  };

  return (
    <div
      className={
        "grid gap-3 px-3 py-2 border-b border-line last:border-0 items-center text-sm " +
        (job.status === "completed" ? "cursor-pointer hover:bg-bg-3" : "")
      }
      style={{ gridTemplateColumns: "100px minmax(0, 1.2fr) 110px 110px 72px" }}
      onClick={onOpen}
    >
      <StatusPill status={job.status} actedOnAction={job.acted_on_action} />
      <div className="min-w-0">
        <div className="font-medium truncate" title={job.album_title || job.album_path}>
          {job.album_title || job.album_id}
        </div>
        {job.error && job.status === "failed" && (
          <div className="text-xs text-err truncate mt-0.5" title={job.error}>
            {job.error}
          </div>
        )}
        {job.hint && (
          <div className="text-xs text-fg-4 italic truncate mt-0.5" title={job.hint}>
            "{job.hint}"
          </div>
        )}
      </div>
      <span className="mono text-xs text-fg-3">{jobTypeLabel(job.job_type)}</span>
      <span className="mono text-xs text-fg-4 tabular-nums" title={job.created_at}>
        {fmtRelative(job.completed_at || job.started_at || job.created_at)}
      </span>
      <div className="flex items-center justify-end gap-0.5">
        {canRetry && (
          <button
            onClick={onRetry}
            disabled={retry.isPending}
            className="w-7 h-7 grid place-items-center rounded text-fg-4 hover:bg-bg-3 hover:text-accent transition disabled:opacity-50"
            title="Retry this job"
            aria-label="Retry job"
          >
            <RotateCcw className="w-3.5 h-3.5" />
          </button>
        )}
        <button
          onClick={(e) => {
            e.stopPropagation();
            if (isRunning) return;
            onRequestDelete(job);
          }}
          disabled={isRunning}
          className="w-7 h-7 grid place-items-center rounded text-fg-4 hover:bg-bg-3 hover:text-err transition disabled:opacity-30 disabled:cursor-not-allowed"
          title={isRunning ? "Can't delete a job while it's running" : "Delete job"}
          aria-label={isRunning ? "Can't delete a running job" : "Delete job"}
        >
          <Trash2 className="w-3.5 h-3.5" />
        </button>
      </div>
    </div>
  );
}

/**
 * Confirmation modal for any delete (single row or bulk header button).
 * Closes itself on confirm/cancel; surfaces server errors via toast.
 */
function DeleteConfirmDialog({
  pending,
  onClose,
}: {
  pending: PendingDelete | null;
  onClose: () => void;
}) {
  const deleteJob = useDeleteJob();
  const bulkDelete = useBulkDeleteJobs();
  const open = pending !== null;
  const isPending = deleteJob.isPending || bulkDelete.isPending;

  const onConfirm = async () => {
    if (!pending) return;
    try {
      if (pending.kind === "single") {
        await deleteJob.mutateAsync(pending.job.id);
      } else {
        await bulkDelete.mutateAsync(pending.statuses);
      }
      onClose();
    } catch (err) {
      const e = err as Error & { status?: number };
      if (e.status === 409) {
        toast.warning(e.message || "Can't delete a running job", { duration: 4000 });
      } else {
        toast.error(e.message || "Could not delete", { duration: 4000 });
      }
      onClose();
    }
  };

  // Compose the prompt strings.
  let title = "Delete this job?";
  let body = "";
  let confirmLabel = "Delete";
  if (pending?.kind === "single") {
    const j = pending.job;
    title = "Delete this job?";
    body =
      j.status === "completed"
        ? `This will discard Claude's suggestions for "${j.album_title || j.album_id}". This can't be undone.`
        : j.status === "queued"
        ? `Remove the queued ${jobTypeLabel(j.job_type).toLowerCase()} job for "${j.album_title || j.album_id}" before it runs?`
        : `Remove the ${j.status} ${jobTypeLabel(j.job_type).toLowerCase()} job for "${j.album_title || j.album_id}" from the list?`;
  } else if (pending?.kind === "bulk") {
    title = `Delete ${pending.count} ${pending.label} job${pending.count === 1 ? "" : "s"}?`;
    body = `This will permanently remove ${pending.count} ${pending.label} job${pending.count === 1 ? "" : "s"} from the list. This can't be undone.`;
    confirmLabel = `Delete ${pending.count}`;
  }

  return (
    <Dialog.Root open={open} onOpenChange={(o) => { if (!o) onClose(); }}>
      <Dialog.Portal>
        <Dialog.Overlay className="fixed inset-0 z-40 bg-black/50 backdrop-blur-[1px]" />
        <Dialog.Content
          className="fixed left-1/2 top-1/2 z-50 -translate-x-1/2 -translate-y-1/2 w-[420px] max-w-[92vw] bg-panel border border-line rounded-md shadow-sheet outline-none"
          aria-describedby={undefined}
        >
          <div className="px-4 pt-4 pb-3">
            <Dialog.Title className="text-md font-semibold mb-1.5">{title}</Dialog.Title>
            <Dialog.Description className="text-sm text-fg-3 leading-relaxed">
              {body}
            </Dialog.Description>
          </div>
          <div className="flex items-center justify-end gap-2 px-4 py-3 border-t border-line bg-panel-2 rounded-b-md">
            <Dialog.Close asChild>
              <button
                className="h-8 px-3 border border-line rounded text-sm hover:bg-bg-3 transition disabled:opacity-60"
                disabled={isPending}
              >
                Cancel
              </button>
            </Dialog.Close>
            <button
              onClick={onConfirm}
              disabled={isPending}
              className="h-8 px-3 rounded bg-err text-white font-medium text-sm hover:opacity-90 transition disabled:opacity-60"
            >
              {isPending ? "Deleting…" : confirmLabel}
            </button>
          </div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}

function StatusPill({ status, actedOnAction }: { status: Job["status"]; actedOnAction: Job["acted_on_action"] }) {
  if (status === "queued") {
    return (
      <span className="inline-flex items-center gap-1.5 mono text-[11px] text-fg-3">
        <Clock className="w-3 h-3" /> Queued
      </span>
    );
  }
  if (status === "running") {
    return (
      <span className="inline-flex items-center gap-1.5 mono text-[11px] text-accent">
        <Loader2 className="w-3 h-3 spin" /> Running
      </span>
    );
  }
  if (status === "completed") {
    return (
      <span className="inline-flex items-center gap-1.5 mono text-[11px] text-ok">
        <CheckCircle2 className="w-3 h-3" /> Ready
      </span>
    );
  }
  if (status === "failed") {
    return (
      <span className="inline-flex items-center gap-1.5 mono text-[11px] text-err">
        <XCircle className="w-3 h-3" /> Failed
      </span>
    );
  }
  // acted_on
  return (
    <span className="inline-flex items-center gap-1.5 mono text-[11px] text-fg-4">
      <CheckCircle2 className="w-3 h-3" /> {actedOnAction === "applied" ? "Applied" : "Discarded"}
    </span>
  );
}

function jobTypeLabel(t: Job["job_type"]): string {
  switch (t) {
    case "metadata":       return "Metadata";
    case "cover":          return "Cover art";
    case "metadata+cover": return "Metadata + cover";
  }
}

function fmtRelative(iso: string): string {
  if (!iso) return "—";
  const t = Date.parse(iso);
  if (Number.isNaN(t)) return iso;
  const diffSec = (Date.now() - t) / 1000;
  if (diffSec < 60) return "just now";
  if (diffSec < 3600) return `${Math.floor(diffSec / 60)}m ago`;
  if (diffSec < 86400) return `${Math.floor(diffSec / 3600)}h ago`;
  return `${Math.floor(diffSec / 86400)}d ago`;
}
