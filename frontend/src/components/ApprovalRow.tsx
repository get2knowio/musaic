import { useEffect, useState } from "react";
import { Check, X } from "lucide-react";

import { useApproveAgentApproval, useDenyAgentApproval } from "@/hooks/useAgents";
import { ApiError, type AgentApproval } from "@/lib/api";
import { approvalSentence, diffSummary, timeLeftText } from "@/lib/agentCopy";

function errorWords(err: unknown): string {
  if (err instanceof ApiError && err.status === 409) {
    return "This request is no longer waiting: it was already decided or it has expired.";
  }
  if (err instanceof ApiError && err.status === 404) return "This request no longer exists.";
  return err instanceof Error && err.message ? err.message : "Could not record your answer.";
}

/**
 * One assistant request waiting for a decision in the Review inbox. Showing
 * it has no side effect; only Approve or Deny sends anything. The change
 * summary is a label: the plan that runs is derived again by the backend.
 */
export function ApprovalRow({ approval }: { approval: AgentApproval }) {
  const approve = useApproveAgentApproval();
  const deny = useDenyAgentApproval();
  // The time left changes on its own; a coarse tick keeps the text current.
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const t = setInterval(() => setNow(Date.now()), 30_000);
    return () => clearInterval(t);
  }, []);

  const busy = approve.isPending || deny.isPending;
  const error = approve.error ?? deny.error;
  const summary = diffSummary(approval.diff);
  const left = timeLeftText(approval.expiresAt, now);
  const sentence = approvalSentence(approval);

  return (
    <div className="px-3 py-2.5 flex flex-wrap items-center gap-x-3 gap-y-2" data-approval-id={approval.id}>
      <div className="flex-1 min-w-[14rem]">
        <div className="flex items-center gap-2 text-sm">
          <span className="mono text-[10px] uppercase tracking-wide text-fg-4 shrink-0">Needs approval</span>
          <span className="text-fg">{sentence}</span>
        </div>
        <div className="mt-0.5 text-xs text-fg-3">
          {summary && <span>{summary}</span>}
          {summary && left && <span className="mx-1.5">·</span>}
          {left && <span>{left}</span>}
        </div>
        {error && (
          <p role="alert" className="mt-1 text-xs text-err">
            {errorWords(error)}
          </p>
        )}
      </div>
      <div className="flex items-center gap-1.5">
        <button
          type="button"
          disabled={busy}
          onClick={() => approve.mutate(approval.id)}
          aria-label={`Approve: ${sentence}`}
          className="h-7 px-2.5 rounded bg-accent text-accent-fg text-xs font-medium hover:opacity-90 transition disabled:opacity-60 inline-flex items-center gap-1.5 focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent"
        >
          <Check className="w-3 h-3" aria-hidden />
          Approve
        </button>
        <button
          type="button"
          disabled={busy}
          onClick={() => deny.mutate(approval.id)}
          aria-label={`Deny: ${sentence}`}
          className="h-7 px-2.5 border border-line rounded text-xs text-fg-2 hover:bg-bg-3 transition disabled:opacity-60 inline-flex items-center gap-1.5 focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent"
        >
          <X className="w-3 h-3" aria-hidden />
          Deny
        </button>
      </div>
    </div>
  );
}
