import { toast } from "sonner";

import { useDelegatedWork, useRetryDelegatedWork } from "@/hooks/useDelegatedWork";
import type { DelegatedWorkItem, DelegatedWorkState } from "@/lib/api";
import { ago } from "@/lib/agentCopy";
import { errorText } from "@/lib/errors";
import { Link } from "react-router-dom";

const BUTTON =
  "h-7 px-2.5 border border-line rounded text-xs hover:bg-bg-3 transition disabled:opacity-60 focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent";

/** The listed states, in the order they are shown, with their plain names.
 * A running item is "Investigating". */
const GROUPS: { state: DelegatedWorkState; title: string }[] = [
  { state: "running", title: "Investigating" },
  { state: "claimed", title: "Claimed" },
  { state: "queued", title: "Waiting" },
  { state: "awaiting_review", title: "Result waiting for you" },
  { state: "failed", title: "Failed" },
  { state: "cancelled", title: "Cancelled" },
];

const KIND_LABEL: Record<string, string> = {
  "metadata.identify": "Identify album",
  "metadata.correct": "Correct metadata",
  "cover.research": "Find cover",
};

/**
 * The research Musaic has asked an assistant to do, by state: who holds it,
 * when they last showed life and when the claim lapses, "Waiting for an
 * agent" when nobody is listening, and Retry for failed or cancelled work.
 * Shows nothing when there is none (or when the list cannot be read).
 */
export function DelegatedWork() {
  const work = useDelegatedWork();
  const retry = useRetryDelegatedWork();
  const items = Array.isArray(work.data) ? work.data : [];
  if (items.length === 0) return null;

  const onRetry = (item: DelegatedWorkItem) =>
    retry.mutate(item, {
      onSuccess: () => toast.success("Queued again.", { duration: 2500 }),
      onError: (err) => toast.error(errorText(err, "Could not retry"), { duration: 4000 }),
    });

  return (
    <section aria-labelledby="delegated-work-heading" className="mt-8">
      <h2 id="delegated-work-heading" className="text-md font-medium mb-1">
        Delegated work
      </h2>
      <p className="text-xs text-fg-3 mb-3">
        Research an assistant is doing or has been asked to do. Nothing changes in your library until you
        approve a result.
      </p>
      <div className="space-y-4">
        {GROUPS.map(({ state, title }) => {
          const rows = items.filter((i) => i.state === state);
          if (rows.length === 0) return null;
          return (
            <div key={state}>
              <h3 className="text-xs text-fg-3 mb-1.5">
                {title} <span className="tabular-nums">({rows.length})</span>
              </h3>
              <ul aria-label={title} className="rounded-md border border-line bg-panel-2 divide-y divide-line">
                {rows.map((i) => (
                  <Row key={i.workId} item={i} busy={retry.isPending} onRetry={() => onRetry(i)} />
                ))}
              </ul>
            </div>
          );
        })}
      </div>
    </section>
  );
}

function Row({ item, busy, onRetry }: { item: DelegatedWorkItem; busy: boolean; onRetry: () => void }) {
  const held = item.state === "claimed" || item.state === "running";
  return (
    <li className="px-3 py-2 text-sm space-y-0.5" data-work-id={item.workId}>
      <div className="flex flex-wrap items-center gap-x-3 gap-y-1">
        <Link
          to={`/library/${item.albumId}`}
          className="font-medium text-accent hover:underline focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent rounded-sm"
        >
          {item.albumLabel ?? item.albumId}
        </Link>
        <span className="text-xs text-fg-3">{KIND_LABEL[item.kind] ?? item.kind}</span>
        <div className="flex-1" />
        {(item.state === "failed" || item.state === "cancelled") && (
          <button type="button" onClick={onRetry} disabled={busy} className={BUTTON} aria-label={`Retry ${item.albumLabel ?? item.albumId}`}>
            Retry
          </button>
        )}
      </div>
      {held && (
        <div className="text-xs text-fg-3">
          {item.holder ? `Held by ${item.holder}` : "Held"}
          <span className="mx-1.5">·</span>Last heard from {ago(item.lastHeartbeatAt)}
          <span className="mx-1.5">·</span>Claim lapses {item.leaseExpiresAt ? new Date(item.leaseExpiresAt).toLocaleTimeString() : "unknown"}
        </div>
      )}
      {item.waitingForAgent && (
        <div className="text-xs text-warn">
          Waiting for an agent. No connected assistant is listening; ask yours to process pending Musaic work.
        </div>
      )}
      {item.state === "failed" && (
        <div className="text-xs text-fg-3">
          Attempt {item.attempts} of {item.maxAttempts}
          {item.lastError ? `: ${item.lastError}` : ""}
        </div>
      )}
    </li>
  );
}
