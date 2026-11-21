import { useState } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import { Link, useNavigate } from "react-router-dom";
import { ArrowUpRight, CheckCircle2, Clock, Loader2, Pause, Play, RotateCcw, Trash2, XCircle } from "lucide-react";
import { toast } from "sonner";

import {
  useBulkDeleteJobs,
  useDeleteJob,
  useQueueState,
  useResumeQueue,
  useRetryJob,
} from "@/hooks/useJobs";
import { fmtRelative } from "@/lib/relativeTime";
import type { Job, JobStatus } from "@/lib/types";

/** Pending delete action — drives the confirmation modal. */
export type PendingDelete =
  | { kind: "single"; job: Job }
  | { kind: "bulk"; statuses: JobStatus[]; label: string; count: number };


/**
 * Surface the auto-paused state at the top of the Jobs page so the user
 * isn't confused why nothing is dispatching after a failure. Stays mounted
 * across filter changes (rendered between header + filters in JobsPage).
 */
export function PausedBanner({ aiReady }: { aiReady: boolean }) {
  const { data: queueState } = useQueueState();
  const resume = useResumeQueue();
  if (!queueState?.paused) return null;
  const remaining = queueState.queued;
  return (
    <div className="mx-4 mt-3 mb-1 rounded-md border border-warn/40 bg-warn-soft px-3 py-2.5 flex items-center gap-3 text-sm">
      <Pause className="w-4 h-4 text-warn shrink-0" />
      <div className="flex-1 min-w-0">
        <div className="font-medium text-fg">{aiReady ? "AI queue paused after a failure" : "Queue paused after a failure"}</div>
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


export function JobRow({
  job,
  onRequestDelete,
  highlight = false,
}: {
  job: Job;
  onRequestDelete: (j: Job) => void;
  /** The row a deep link points at. */
  highlight?: boolean;
}) {
  const navigate = useNavigate();
  const retry = useRetryJob();
  const [transcriptOpen, setTranscriptOpen] = useState(false);

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
  // Captured Claude transcript leading up to the failure. Only present on
  // failed jobs whose worker call streamed at least one assistant message
  // before erroring out — empty for the rate-limit "exit code 1 from
  // first turn" case (nothing was said yet).
  const failureExcerpt =
    job.status === "failed" ? job.result?.failure_excerpt ?? null : null;

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
      data-job-id={job.id}
      className={
        "border-b border-line last:border-0 " +
        (job.status === "completed" ? "cursor-pointer hover:bg-bg-3 " : "") +
        (highlight ? "bg-accent-soft" : "")
      }
      onClick={onOpen}
    >
      <div
        className="grid gap-3 px-3 py-2 items-center text-sm"
        style={{ gridTemplateColumns: "100px minmax(0, 1.2fr) 110px 110px 72px" }}
      >
        <StatusPill status={job.status} actedOnAction={job.acted_on_action} />
        <div className="min-w-0">
          {/* Title row: album name + a small icon-link that jumps to
              the album detail sheet on the Library page. Works for
              ALL job statuses (the row's onClick already opens the
              album for completed jobs only; this link is the path
              for running / queued / failed rows too). stopPropagation
              so clicking the icon doesn't ALSO fire the row onClick. */}
          <div className="flex items-center gap-1.5 min-w-0">
            <span
              className="font-medium truncate"
              title={job.album_title || job.album_path}
            >
              {job.album_title || job.album_id}
            </span>
            <Link
              to={`/library/${encodeURIComponent(job.album_id)}`}
              onClick={(e) => e.stopPropagation()}
              className="shrink-0 inline-flex items-center justify-center w-5 h-5 rounded text-fg-4 hover:text-accent hover:bg-bg-3 transition"
              title="View album detail"
              aria-label="View album detail"
            >
              <ArrowUpRight className="w-3.5 h-3.5" />
            </Link>
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
          {failureExcerpt && (
            <button
              type="button"
              onClick={(e) => {
                e.stopPropagation();
                setTranscriptOpen((v) => !v);
              }}
              className="mt-1 mono text-[11px] text-fg-3 hover:text-accent transition inline-flex items-center gap-1"
              aria-expanded={transcriptOpen}
              // Now includes the full structured transcript: text
              // blocks, tool calls (→ name args), tool results
              // (← summary), and the final result message. Useful
              // for diagnosing WHERE a stuck/failed job got
              // wedged — e.g. "→ WebFetch musicbrainz.org/..." with
              // no following ← entry means MB never responded.
              title={transcriptOpen ? "Hide transcript" : "Show what Claude said + which tools it called"}
            >
              {transcriptOpen ? "▾" : "▸"} {transcriptOpen ? "Hide" : "Show"} Claude's transcript
            </button>
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
      {failureExcerpt && transcriptOpen && (
        <div
          className="px-3 pb-3 text-xs"
          onClick={(e) => e.stopPropagation()}
        >
          <pre className="mono whitespace-pre-wrap break-words bg-bg-2 border border-line rounded-md p-3 max-h-80 overflow-y-auto text-fg-2 leading-relaxed">
            {failureExcerpt}
          </pre>
        </div>
      )}
    </div>
  );
}

/**
 * Confirmation modal for any delete (single row or bulk header button).
 * Closes itself on confirm/cancel; surfaces server errors via toast.
 */
export function DeleteConfirmDialog({
  pending,
  onClose,
  aiReady,
}: {
  pending: PendingDelete | null;
  onClose: () => void;
  aiReady: boolean;
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
        ? `This will discard ${aiReady ? "Claude's suggestions" : "the suggestions"} for "${j.album_title || j.album_id}". This can't be undone.`
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

export function StatusPill({ status, actedOnAction }: { status: Job["status"]; actedOnAction: Job["acted_on_action"] }) {
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
        <CheckCircle2 className="w-3 h-3" /> To review
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

export function jobTypeLabel(t: Job["job_type"]): string {
  switch (t) {
    case "metadata":       return "Metadata";
    case "cover":          return "Cover art";
    case "metadata+cover": return "Metadata + cover";
  }
}
