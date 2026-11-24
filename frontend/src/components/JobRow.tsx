import { useState } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import { Link, useNavigate } from "react-router-dom";
import { ArrowUpRight, CheckCircle2, Clock, Loader2, Trash2, XCircle } from "lucide-react";
import { toast } from "sonner";

import { useBulkDeleteJobs, useCancelResearchGroup, useDeleteJob } from "@/hooks/useJobs";
import { ResearchGroup } from "./ResearchGroup";
import { fmtRelative } from "@/lib/relativeTime";
import type { Job, JobStatus } from "@/lib/types";
import { isHistoryOnlyJob } from "@/lib/filters";

/** Pending remove or cancel action — drives the confirmation modal. */
export type PendingDelete =
  | { kind: "single"; job: Job }
  | { kind: "bulk"; statuses: JobStatus[]; label: string; count: number };

/** A card whose requests are still waiting, held or running: its action is
 *  Cancel (the research cancel, FR-036), never a delete. An old job's
 *  history-only record is history whatever status it was left in. */
function isUnresolved(job: Job): boolean {
  return (job.status === "queued" || job.status === "running") && !isHistoryOnlyJob(job);
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
  const [transcriptOpen, setTranscriptOpen] = useState(false);

  const onOpen = () => {
    if (job.status === "completed") {
      navigate(`/library/${encodeURIComponent(job.album_id)}`);
    }
  };

  // A waiting or running request is cancelled; a finished record is
  // removed from the history. Both go through the confirmation dialog.
  const unresolved = isUnresolved(job);
  // The transcript an older record kept of a failed attempt, if any.
  const failureExcerpt =
    job.status === "failed" ? job.result?.failure_excerpt ?? null : null;

  if (job.result?.delegated) return <ResearchGroup job={job} />;

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
              title={transcriptOpen ? "Hide transcript" : "Show what was said and which tools were called"}
            >
              {transcriptOpen ? "▾" : "▸"} {transcriptOpen ? "Hide" : "Show"} transcript
            </button>
          )}
        </div>
        <span className="mono text-xs text-fg-3">{jobTypeLabel(job.job_type)}</span>
        <span className="mono text-xs text-fg-4 tabular-nums" title={job.created_at}>
          {fmtRelative(job.completed_at || job.started_at || job.created_at)}
        </span>
        <div className="flex items-center justify-end gap-0.5">
          <button
            onClick={(e) => {
              e.stopPropagation();
              onRequestDelete(job);
            }}
            className="w-7 h-7 grid place-items-center rounded text-fg-4 hover:bg-bg-3 hover:text-err transition"
            title={unresolved ? "Cancel this research request" : "Remove from the history"}
            aria-label={unresolved ? "Cancel research request" : "Remove from history"}
          >
            {unresolved ? <XCircle className="w-3.5 h-3.5" /> : <Trash2 className="w-3.5 h-3.5" />}
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
 * Confirmation modal for a row's action or the bulk header button. A
 * waiting or running request is cancelled through the research cancel
 * (`POST /api/ai/jobs/{id}/cancel`, FR-036); only finished history is
 * removed. Closes itself on confirm/cancel; surfaces server errors (a 409
 * says nothing was left to cancel or remove) via toast. The `work` event
 * family refreshes the lists, so nothing is invalidated here (rule 5).
 */
export function DeleteConfirmDialog({
  pending,
  onClose,
}: {
  pending: PendingDelete | null;
  onClose: () => void;
}) {
  const deleteJob = useDeleteJob();
  const bulkDelete = useBulkDeleteJobs();
  const cancelGroup = useCancelResearchGroup();
  const open = pending !== null;
  const isPending = deleteJob.isPending || bulkDelete.isPending || cancelGroup.isPending;
  const cancelling = pending?.kind === "single" && isUnresolved(pending.job);

  const onConfirm = async () => {
    if (!pending) return;
    try {
      if (pending.kind === "single") {
        if (isUnresolved(pending.job)) {
          await cancelGroup.mutateAsync(pending.job.id);
        } else {
          await deleteJob.mutateAsync(pending.job.id);
        }
      } else {
        await bulkDelete.mutateAsync(pending.statuses);
      }
      onClose();
    } catch (err) {
      const e = err as Error & { status?: number };
      const fallback = cancelling ? "Couldn't cancel the research request" : "Could not remove it";
      if (e.status === 409) {
        toast.warning(e.message || fallback, { duration: 4000 });
      } else {
        toast.error(e.message || fallback, { duration: 4000 });
      }
      onClose();
    }
  };

  // Compose the prompt strings.
  let title = "";
  let body = "";
  let confirmLabel = "Remove";
  let busyLabel = "Removing…";
  if (pending?.kind === "single") {
    const j = pending.job;
    const name = j.album_title || j.album_id;
    const what = jobTypeLabel(j.job_type).toLowerCase();
    if (cancelling) {
      title = "Cancel this research request?";
      body = `Cancel the ${what} research for "${name}"? Anything waiting or in progress for it stops; a proposal already waiting for review stays.`;
      confirmLabel = "Cancel request";
      busyLabel = "Cancelling…";
    } else {
      title = "Remove from the history?";
      body = `Remove the ${STATUS_WORD[j.status]} ${what} research for "${name}" from the history? The album is not changed. This can't be undone.`;
    }
  } else if (pending?.kind === "bulk") {
    const records = `record${pending.count === 1 ? "" : "s"}`;
    title = `Remove ${pending.count} ${pending.label} ${records}?`;
    body = `This will permanently remove ${pending.count} ${pending.label} ${records} from the history. Albums are not changed. This can't be undone.`;
    confirmLabel = `Remove ${pending.count}`;
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
                {cancelling ? "Keep it" : "Cancel"}
              </button>
            </Dialog.Close>
            <button
              onClick={onConfirm}
              disabled={isPending}
              className="h-8 px-3 rounded bg-err text-white font-medium text-sm hover:opacity-90 transition disabled:opacity-60"
            >
              {isPending ? busyLabel : confirmLabel}
            </button>
          </div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}

const STATUS_WORD: Record<Job["status"], string> = {
  queued: "queued",
  running: "running",
  completed: "finished",
  failed: "failed",
  acted_on: "finished",
};

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
    // An older history record can name a type this build has no research
    // for (lyrics); it is shown as stored.
    default:               return String(t || "Research");
  }
}
