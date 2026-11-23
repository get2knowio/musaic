import { useState } from "react";
import { useMutation, useQuery } from "@tanstack/react-query";
import { Link } from "react-router-dom";
import {
  decideResearch,
  previewResearch,
  researchItems,
  undoAgentChange,
  type ResearchItem,
  type ResearchPreview,
} from "@/lib/api";
import type { Job, JobType } from "@/lib/types";

const button =
  "rounded border border-line px-2 py-1 text-sm hover:bg-bg-3 focus-visible:outline-accent disabled:opacity-50";
const states: Record<string, string> = {
  queued: "Waiting for an assistant",
  claimed: "Claimed",
  running: "In progress",
  awaiting_review: "Ready to review",
  applied: "Applied",
  discarded: "Discarded",
  failed: "Needs attention",
  cancelled: "Cancelled",
};
const kinds: Record<string, string> = {
  "metadata.identify": "Identify the release",
  "metadata.correct": "Research metadata",
  "cover.research": "Find cover art",
};

export function RequestResearch({
  onRequest,
  pending,
}: {
  onRequest: (kind: JobType, hint: string) => Promise<void>;
  pending: boolean;
}) {
  const [kind, setKind] = useState<JobType>("metadata");
  const [hint, setHint] = useState("");
  return (
    <details className="mx-4 my-3 rounded border border-line p-3 text-sm">
      <summary className="cursor-pointer">Request assistant research</summary>
      <form
        className="mt-2 grid gap-2"
        onSubmit={(e) => {
          e.preventDefault();
          void onRequest(kind, hint);
        }}
      >
        <label>
          Research{" "}
          <select
            className="bg-panel"
            value={kind}
            onChange={(e) => setKind(e.target.value as JobType)}
          >
            <option value="metadata">Album metadata</option>
            <option value="cover">Cover art</option>
            <option value="metadata+cover">Metadata and cover art</option>
          </select>
        </label>
        <label>
          What should the assistant look for?{" "}
          <input
            className="w-full rounded border border-line bg-panel p-1"
            maxLength={1000}
            value={hint}
            onChange={(e) => setHint(e.target.value)}
          />
        </label>
        <span className="text-xs text-fg-3">
          A paired assistant can pick up the request while Musaic is open.
        </span>
        <button className={button} disabled={pending}>
          {pending ? "Requesting…" : "Request research"}
        </button>
      </form>
    </details>
  );
}

function Child({ item }: { item: ResearchItem }) {
  const [preview, setPreview] = useState<ResearchPreview | null>(null);
  const [message, setMessage] = useState("");
  const [confirmUndo, setConfirmUndo] = useState(false);
  const undo = useMutation({
    mutationFn: undoAgentChange,
    onSuccess: (result) => {
      setConfirmUndo(false);
      setMessage(
        result.complete
          ? "Previous values restored."
          : (result.message ?? "Some changes could not be restored."),
      );
    },
  });
  const fetchPreview = useMutation({
    mutationFn: () => previewResearch(item),
    onSuccess: setPreview,
  });
  const decide = useMutation({
    mutationFn: (action: "apply" | "discard" | "retry" | "cancel") =>
      decideResearch(
        item,
        action,
        action === "apply" ? (preview ?? undefined) : undefined,
      ),
    onError: () => setPreview(null),
    onSuccess: (_, action) => {
      setPreview(null);
      setMessage(
        action === "apply"
          ? "Applied."
          : action === "retry"
            ? "Queued again."
            : action === "cancel"
              ? "Cancelled."
              : "Discarded.",
      );
    },
  });
  const result = item.result;
  const error = undo.error ?? decide.error ?? fetchPreview.error;
  const busy = fetchPreview.isPending || decide.isPending || undo.isPending;
  return (
    <section className="grid gap-2 border-t border-line py-2">
      <p>
        <strong>{kinds[item.kind] ?? item.kind}</strong> ·{" "}
        {states[item.state] ?? item.state}
      </p>
      {item.note && (
        <p>
          {item.note === "legacy_result_unconverted"
            ? "Historical result. Its original evidence could not be verified."
            : item.note === "review_interrupted_check_backup_and_refresh"
              ? "Musaic stopped during this edit. Check the backup and request a fresh preview for any remaining changes."
              : item.note}
        </p>
      )}
      {item.state === "failed" && item.applyOutcome && (
        <details>
          <summary>Confirmed file results</summary>
          <ul>
            {item.applyOutcome.files.map((file, index) => (
              <li key={index}>{file.fileName} · {file.status} {file.message}</li>
            ))}
            {item.applyOutcome.cover && <li>Cover · {item.applyOutcome.cover.status}</li>}
            {item.applyOutcome.renamed && <li>Files renamed</li>}
          </ul>
        </details>
      )}
      {item.historicalResult && (
        <details>
          <summary>Original result</summary>
          <pre className="whitespace-pre-wrap break-all">
            {item.historicalResult}
          </pre>
        </details>
      )}
      {result && (
        <>
          {result.recommendation.rationale && (
            <p>{result.recommendation.rationale}</p>
          )}
          {result.recommendation.url && (
            <p className="break-all">
              Proposed cover: {result.recommendation.url}
            </p>
          )}
          {result.recommendation.rightsUncertainty && (
            <p>Usage rights: {result.recommendation.rightsUncertainty}</p>
          )}
          {result.recommendation.identifiers?.map((x) => (
            <p key={x.type + x.id}>
              MusicBrainz {x.type.replace("_", " ")}: {x.id}
            </p>
          ))}
          {result.recommendation.explanation && (
            <p>{result.recommendation.explanation}</p>
          )}
          {result.recommendation.question && (
            <p>{result.recommendation.question}</p>
          )}
          {result.evidence.length > 0 && (
            <details>
              <summary>Evidence ({result.evidence.length})</summary>
              <ul className="pl-4 list-disc">
                {result.evidence.map((e, i) => (
                  <li key={i}>
                    {e.claim}{" "}
                    <span className="break-all text-fg-3">
                      {e.source} · {e.retrievedAt}
                    </span>{" "}
                    {e.limitations.join(" ")}
                  </li>
                ))}
              </ul>
            </details>
          )}
          <p>Assistant confidence: {Math.round(result.confidence * 100)}%</p>
          {result.uncertainties.length > 0 && (
            <p>Uncertainties: {result.uncertainties.join(" ")}</p>
          )}
          {item.validation === "pending" && (
            <p>
              Verification is pending. Preview again when Musaic is connected.
            </p>
          )}
        </>
      )}
      {preview && (
        <div aria-label="Proposed changes">
          <ul>
            {preview.diff.tags.map((t, i) => (
              <li key={i}>
                {t.scope} · {t.field}: {t.before ?? "(empty)"} →{" "}
                {t.after ?? "(empty)"}
              </li>
            ))}
          </ul>
          {preview.diff.files.map((file) => (
            <p key={file.from}>Rename file: {file.from} → {file.to}</p>
          ))}
          {preview.diff.folder && (
            <p>Rename folder: {preview.diff.folder.from} → {preview.diff.folder.to}</p>
          )}
          {preview.diff.cover && <p>{preview.diff.cover}</p>}
          {preview.coverThumbnail && (
            <img
              src={preview.coverThumbnail}
              width={128}
              alt="Proposed album cover"
            />
          )}
        </div>
      )}
      <div className="flex flex-wrap gap-2">
        {item.state === "awaiting_review" && (
          <>
            {result?.outcome === "proposal" &&
              (preview ? (
                <button
                  className={button}
                  disabled={busy}
                  onClick={() => decide.mutate("apply")}
                >
                  Apply reviewed changes
                </button>
              ) : (
                <button
                  className={button}
                  disabled={busy}
                  onClick={() => fetchPreview.mutate()}
                >
                  Preview changes
                </button>
              ))}
            <button
              className={button}
              disabled={busy}
              onClick={() => decide.mutate("discard")}
            >
              Discard result
            </button>
          </>
        )}
        {["queued", "claimed", "running"].includes(item.state) && (
          <button
            className={button}
            disabled={busy}
            onClick={() => decide.mutate("cancel")}
          >
            Cancel research
          </button>
        )}
        {item.changeId &&
          (confirmUndo ? (
            <>
              <p>Restore the previous values for this change?</p>
              <button
                className={button}
                disabled={busy}
                onClick={() => item.changeId && undo.mutate(item.changeId)}
              >
                Restore previous values
              </button>
              <button className={button} onClick={() => setConfirmUndo(false)}>
                Keep changes
              </button>
            </>
          ) : (
            <button className={button} onClick={() => setConfirmUndo(true)}>
              Undo change
            </button>
          ))}
        {(item.state === "failed" || item.state === "cancelled") && (
          <button
            className={button}
            disabled={busy}
            onClick={() => decide.mutate("retry")}
          >
            Retry research
          </button>
        )}
      </div>
      {error && <p role="alert">{error.message}</p>}
      {message && <p role="status">{message}</p>}
    </section>
  );
}

export function ResearchGroup({ job }: { job: Job }) {
  const q = useQuery({
    queryKey: ["research", job.id],
    queryFn: () => researchItems(job.id),
    staleTime: Infinity,
  });
  return (
    <div
      data-job-id={job.id}
      className="m-3 rounded border border-line p-3 text-sm"
    >
      <Link
        className="font-semibold underline"
        to={`/library/${encodeURIComponent(job.album_id)}`}
      >
        {job.album_title || "Album research"}
      </Link>
      {q.isPending && <p>Loading research…</p>}
      {q.error && <p role="alert">{q.error.message}</p>}
      {q.data?.map((item) => (
        <Child key={item.id + ":" + item.digest} item={item} />
      ))}
      {q.data?.some((item) =>
        ["queued", "claimed", "running"].includes(item.state),
      ) && (
        <p className="text-xs text-fg-3">
          Keep Musaic open for your assistant to claim or update this research.
        </p>
      )}
    </div>
  );
}
