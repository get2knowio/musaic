import { useState } from "react";
import { Link } from "react-router-dom";
import { toast } from "sonner";

import { useChecks } from "@/hooks/useChecks";
import { useDelegatedWork, useRetryDelegatedWork } from "@/hooks/useDelegatedWork";
import { useCancelResearchBatch, useResearchBatch } from "@/hooks/useResearchBatches";
import type { BatchSummary, DelegatedWorkItem } from "@/lib/api";
import { errorText } from "@/lib/errors";
import { fmtRelative } from "@/lib/relativeTime";
import {
  BATCH_ITEM_LIMIT,
  BATCH_STATE_LABEL,
  KIND_LABEL,
  batchCountsText,
  batchTitle,
  cancellableCount,
} from "@/lib/researchBatches";

const BUTTON =
  "h-7 px-2.5 border border-line rounded text-xs hover:bg-bg-3 transition disabled:opacity-60 focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent";
const LINK =
  "text-accent hover:underline focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent rounded-sm";

function useCheckLabel(checkId: string): string {
  const { data: checks } = useChecks();
  const found = Array.isArray(checks) ? checks.find((c) => c.meta.id === checkId) : undefined;
  return found?.meta.label ?? checkId;
}

/**
 * One Fix all batch as a single entry: its origin, size and counts by state,
 * "from an unfinished run" when the run stopped part-way, a Cancel for the
 * items still waiting or held (awaiting-review and finished ones stay), and,
 * with `expandable`, a way into its items. Review and Activity both use it.
 */
export function ResearchBatchEntry({
  batch,
  expandable = false,
  children,
}: {
  batch: BatchSummary;
  /** Offer "Show requests", which lists the batch's items. */
  expandable?: boolean;
  /** Extra controls (a link to Review from Activity). */
  children?: React.ReactNode;
}) {
  const checkLabel = useCheckLabel(batch.checkId);
  const title = batchTitle(batch, checkLabel);
  const counts = batchCountsText(batch);
  const cancellable = cancellableCount(batch);
  const cancel = useCancelResearchBatch();
  const [confirming, setConfirming] = useState(false);
  const [open, setOpen] = useState(false);
  const itemsId = `research-batch-${batch.batchId}-items`;

  const onCancel = () =>
    cancel.mutate(batch.batchId, {
      onSuccess: (r) => {
        setConfirming(false);
        const n = typeof r?.cancelled === "number" ? r.cancelled : cancellable;
        toast.success(`Cancelled ${n} research ${n === 1 ? "request" : "requests"}.`, { duration: 2500 });
      },
      onError: (err) => toast.error(errorText(err, "Could not cancel the batch"), { duration: 4000 }),
    });

  return (
    <section aria-label={title} data-batch-id={batch.batchId} className="text-sm space-y-1">
      <div className="font-medium">{title}</div>
      <div className="text-xs text-fg-3">
        Started {fmtRelative(batch.startedAt)}
        {batch.unfinished && (
          <>
            <span className="mx-1.5">·</span>
            <span className="text-warn">from an unfinished run</span>
          </>
        )}
      </div>
      {counts && <div className="text-xs text-fg-2 tabular-nums">{counts}</div>}
      <div className="flex flex-wrap items-center gap-2 pt-0.5">
        {expandable && (
          <button
            type="button"
            className={BUTTON}
            aria-expanded={open}
            aria-controls={itemsId}
            onClick={() => setOpen((o) => !o)}
          >
            {open ? "Hide requests" : `Show requests (${batch.size})`}
          </button>
        )}
        {cancellable > 0 && !confirming && (
          <button
            type="button"
            className={BUTTON}
            onClick={() => setConfirming(true)}
            aria-label={`Cancel the ${cancellable} waiting or held ${cancellable === 1 ? "request" : "requests"} of this batch`}
          >
            Cancel…
          </button>
        )}
        {cancellable > 0 && confirming && (
          <span role="group" aria-label="Confirm cancel" className="inline-flex flex-wrap items-center gap-2 text-xs text-fg-2">
            Cancel {cancellable} {cancellable === 1 ? "request" : "requests"}? Results waiting for you stay.
            <button type="button" className={BUTTON} disabled={cancel.isPending} onClick={onCancel}>
              {cancel.isPending ? "Cancelling…" : "Cancel requests"}
            </button>
            <button type="button" className={BUTTON} disabled={cancel.isPending} onClick={() => setConfirming(false)}>
              Keep
            </button>
          </span>
        )}
        {children}
      </div>
      {expandable && open && <BatchItems id={itemsId} batchId={batch.batchId} />}
    </section>
  );
}

function BatchItems({ id, batchId }: { id: string; batchId: string }) {
  const detail = useResearchBatch(batchId, true);
  const [all, setAll] = useState(false);
  // A failed or cancelled item keeps its Retry (spec 005 FR-040); batched
  // items are not listed one by one anywhere else, so it is offered here.
  // Only for an item of the current library (the work list holds those):
  // the retry decision refuses work of an earlier library epoch.
  const work = useDelegatedWork();
  const retry = useRetryDelegatedWork();
  const current = new Map((Array.isArray(work.data) ? work.data : []).map((w) => [w.workId, w]));
  const onRetry = (item: DelegatedWorkItem) =>
    retry.mutate(item, {
      onSuccess: () => toast.success("Queued again.", { duration: 2500 }),
      onError: (err) => toast.error(errorText(err, "Could not retry"), { duration: 4000 }),
    });
  if (detail.isPending) {
    return (
      <p id={id} className="text-xs text-fg-3">
        Loading…
      </p>
    );
  }
  const items = Array.isArray(detail.data?.items) ? detail.data.items : [];
  if (detail.isError || items.length === 0) {
    return (
      <p id={id} className="text-xs text-fg-3">
        {detail.isError ? "The requests of this batch could not be read." : "This batch has no requests left."}
      </p>
    );
  }
  const shown = all ? items : items.slice(0, BATCH_ITEM_LIMIT);
  return (
    <div id={id}>
      <ul aria-label="Requests in this batch" className="mt-1 rounded border border-line bg-panel divide-y divide-line">
        {shown.map((i) => {
          const retryable = (i.state === "failed" || i.state === "cancelled") ? current.get(i.workId) : undefined;
          return (
            <li key={i.workId} data-work-id={i.workId} className="px-2 py-1.5 text-xs flex flex-wrap items-center gap-x-3 gap-y-0.5">
              <Link to={`/library/${encodeURIComponent(i.albumId)}`} className={`font-medium ${LINK}`}>
                {i.albumLabel ?? i.albumId}
              </Link>
              <span className="text-fg-3">{KIND_LABEL[i.kind] ?? i.kind}</span>
              <span className="text-fg-2 ml-auto">{BATCH_STATE_LABEL[i.state] ?? i.state}</span>
              {retryable && (
                <button
                  type="button"
                  className={BUTTON}
                  disabled={retry.isPending}
                  onClick={() => onRetry(retryable)}
                  aria-label={`Retry ${i.albumLabel ?? i.albumId}`}
                >
                  Retry
                </button>
              )}
            </li>
          );
        })}
      </ul>
      {!all && items.length > BATCH_ITEM_LIMIT && (
        <button type="button" className={`mt-2 ${BUTTON}`} onClick={() => setAll(true)}>
          Show all {items.length}
        </button>
      )}
    </div>
  );
}
