import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { useNavigationType, useSearchParams } from "react-router-dom";
import { AlertCircle, CheckCircle2, Trash2, X, Zap } from "lucide-react";
import { toast } from "sonner";

import { BulkQuickFixDialog } from "@/components/BulkQuickFixDialog";
import { FindingRow } from "@/components/FindingRow";
import {
  DeleteConfirmDialog,
  JobRow,
  PausedBanner,
  type PendingDelete,
} from "@/components/JobRow";
import { useAlbumsSnapshot } from "@/hooks/useAlbumsSnapshot";
import { useAiStatus } from "@/hooks/useAiStatus";
import { useAttention } from "@/hooks/useAttention";
import { useChecks } from "@/hooks/useChecks";
import { useBulkDeleteJobs, useDeleteJob, useJobs } from "@/hooks/useJobs";
import { usePageTitle } from "@/hooks/usePageTitle";
import type { Album, CheckSummary, Finding, Job } from "@/lib/types";

/**
 * Needs attention: the one place for what wants a decision from the user.
 *
 * Default view ("Needs a decision", the Sidebar's count): open findings of
 * severity warn or error, grouped by check, and jobs that are proposed
 * (awaiting review) or failed. The "Other" view lists what does not count:
 * informational findings and finished jobs. Queued or running jobs show as
 * a strip while there are any.
 *
 * View state is in the URL, so Back from an album's panel returns to the
 * same view: `?view=other`, `?check=<id>` (one check only) and
 * `?job=<id>` (a deep link: that job is shown and highlighted whatever
 * the view; a check filter lists all of that check's open findings). The scroll position is restored on Back too.
 *
 * Checks run on their own (the titlebar shows progress), so there is
 * nothing to run from here.
 */
export type AttentionView = "decision" | "other";

const SCROLL_KEY = "mm.attention.scroll";
const JOB_COLS = "100px minmax(0, 1.2fr) 110px 110px 72px";

function readScroll(key: string): number {
  try {
    const raw = sessionStorage.getItem(`${SCROLL_KEY}.${key}`);
    return raw ? parseInt(raw, 10) || 0 : 0;
  } catch {
    return 0;
  }
}

function writeScroll(key: string, top: number) {
  try {
    sessionStorage.setItem(`${SCROLL_KEY}.${key}`, String(top));
  } catch {
    // Without storage Back just starts at the top.
  }
}

export default function AttentionPage() {
  usePageTitle("Needs attention");
  const [params, setParams] = useSearchParams();
  const view: AttentionView = params.get("view") === "other" ? "other" : "decision";
  const checkFilter = params.get("check");
  const linkedJobId = params.get("job");

  const attention = useAttention();
  const { data: checks = [] } = useChecks();
  const { data: allJobs = [] } = useJobs();
  const ai = useAiStatus();
  const { data: albums = [] } = useAlbumsSnapshot();
  const albumsById = useMemo(() => new Map<string, Album>(albums.map((a) => [a.id, a])), [albums]);

  const checksById = useMemo(() => new Map(checks.map((c) => [c.meta.id, c])), [checks]);
  const [bulkCheck, setBulkCheck] = useState<CheckSummary | null>(null);

  // ---- job deletes (as the old Jobs page did) ----
  const [pendingDelete, setPendingDelete] = useState<PendingDelete | null>(null);
  const deleteJob = useDeleteJob();
  const bulkDelete = useBulkDeleteJobs();
  const onSingleDelete = (job: Job) => {
    if (job.status === "acted_on") {
      deleteJob.mutate(job.id, {
        onError: (err) => toast.error((err as Error).message || "Could not delete job", { duration: 4000 }),
      });
      return;
    }
    setPendingDelete({ kind: "single", job });
  };
  const onBulkDelete = (p: Extract<PendingDelete, { kind: "bulk" }>) => {
    if (p.statuses.length === 1 && p.statuses[0] === "acted_on") {
      bulkDelete.mutate(p.statuses, {
        onError: (err) => toast.error((err as Error).message || "Could not bulk delete", { duration: 4000 }),
      });
      return;
    }
    setPendingDelete(p);
  };

  // ---- what to show ----
  // A check filter (a deep link, or "Bulk fix" from the album panel) shows
  // all of that check's open findings whatever their severity.
  const byCheck = useMemo(() => {
    const findings = checkFilter
      ? [...attention.findings, ...attention.infoFindings]
      : view === "decision"
        ? attention.findings
        : attention.infoFindings;
    const m = new Map<string, Finding[]>();
    for (const f of findings) {
      if (checkFilter && f.check_id !== checkFilter) continue;
      const list = m.get(f.check_id) ?? [];
      list.push(f);
      m.set(f.check_id, list);
    }
    // Registry order (the checks' display order); unknown checks last.
    const order = new Map(checks.map((c, i) => [c.meta.id, i]));
    return [...m.entries()].sort(
      (a, b) => (order.get(a[0]) ?? 1e9) - (order.get(b[0]) ?? 1e9),
    );
  }, [attention.findings, attention.infoFindings, view, checks, checkFilter]);

  const showJobs = !checkFilter;
  const proposed = view === "decision" && showJobs ? attention.proposed : [];
  const failed = view === "decision" && showJobs ? attention.failed : [];
  const finished = view === "other" && showJobs ? attention.finishedJobs : [];
  const inProgress = view === "decision" && showJobs ? attention.inProgress : [];
  const shownJobIds = new Set([...proposed, ...failed, ...finished, ...inProgress].map((j) => j.id));
  const linked = linkedJobId ? allJobs.find((j) => j.id === linkedJobId) : undefined;
  const linkedExtra = linked && !shownJobIds.has(linked.id) ? linked : undefined;

  const shown =
    byCheck.length + proposed.length + failed.length + finished.length + inProgress.length +
    (linkedExtra ? 1 : 0);
  const otherCount = attention.infoFindings.length + attention.finishedJobs.length;

  const setView = (next: AttentionView) => {
    const p = new URLSearchParams(params);
    if (next === "other") p.set("view", "other");
    else p.delete("view");
    setParams(p, { replace: true });
  };
  const clearCheck = () => {
    const p = new URLSearchParams(params);
    p.delete("check");
    setParams(p, { replace: true });
  };

  // ---- scroll: restored when coming Back, not on a fresh visit ----
  const navType = useNavigationType();
  const scroller = useRef<HTMLDivElement>(null);
  const scrollKey = `${view}.${checkFilter ?? ""}`;
  const ready = !attention.isLoading;
  const restored = useRef<string | null>(null);
  useLayoutEffect(() => {
    const el = scroller.current;
    if (!el || !ready || restored.current === scrollKey) return;
    restored.current = scrollKey;
    el.scrollTop = navType === "POP" ? readScroll(scrollKey) : 0;
  }, [ready, scrollKey, navType]);
  useEffect(() => {
    const el = scroller.current;
    if (!el) return;
    const onScroll = () => {
      if (restored.current === scrollKey) writeScroll(scrollKey, el.scrollTop);
    };
    el.addEventListener("scroll", onScroll, { passive: true });
    return () => el.removeEventListener("scroll", onScroll);
  }, [scrollKey]);

  // A deep-linked job is brought into view once it is on the page.
  const linkedPresent = !!linked;
  useEffect(() => {
    if (!linkedJobId || !linkedPresent) return;
    scroller.current
      ?.querySelector(`[data-job-id="${CSS.escape(linkedJobId)}"]`)
      ?.scrollIntoView?.({ block: "nearest" });
  }, [linkedJobId, linkedPresent]);

  const jobTable = (jobs: Job[]) => (
    <div className="rounded-md border border-line bg-panel-2 overflow-hidden">
      <div
        className="grid gap-3 px-3 py-2 border-b border-line text-fg-4 mono text-xs uppercase tracking-[0.06em]"
        style={{ gridTemplateColumns: JOB_COLS }}
      >
        <span>Status</span>
        <span>Album</span>
        <span>Job</span>
        <span>When</span>
        <span />
      </div>
      {jobs.map((j) => (
        <JobRow key={j.id} job={j} onRequestDelete={onSingleDelete} highlight={j.id === linkedJobId} />
      ))}
    </div>
  );

  return (
    <div className="flex-1 min-h-0 flex flex-col">
      <div className="flex flex-wrap items-center gap-x-3 gap-y-1 px-4 min-h-[var(--filterbar-h)] border-b border-line bg-panel-2 text-sm shrink-0">
        <AlertCircle className="w-3.5 h-3.5 text-accent" />
        <h1 className="font-medium">Needs attention</h1>
        <span className="mono text-fg-3" aria-live="polite">
          <b className={`font-medium tabular-nums ${attention.count > 0 ? "text-warn" : "text-ok"}`}>
            {attention.count}
            {attention.countIsLowerBound ? "+" : ""}
          </b>{" "}
          {attention.count === 1 ? "item" : "items"}
        </span>
        <div className="flex-1" />
        <div role="group" aria-label="Show" className="flex items-center gap-1.5 py-1">
          {(
            [
              ["decision", "Needs a decision"],
              ["other", `Other (${otherCount})`],
            ] as const
          ).map(([key, label]) => (
            <button
              key={key}
              type="button"
              aria-pressed={view === key}
              onClick={() => setView(key)}
              className={
                "h-7 px-2.5 rounded text-xs transition " +
                (view === key
                  ? "bg-accent-soft text-accent border border-accent/30"
                  : "text-fg-3 hover:bg-bg-3 border border-transparent")
              }
            >
              {label}
            </button>
          ))}
        </div>
      </div>

      <div ref={scroller} tabIndex={-1} className="flex-1 min-h-0 overflow-y-auto outline-none">
        <PausedBanner aiReady={ai.ready} />
        {checkFilter && (
          <div className="mx-4 mt-3 flex items-center gap-2 text-xs text-fg-3">
            <span>Showing only {checksById.get(checkFilter)?.meta.label ?? checkFilter}</span>
            <button
              type="button"
              onClick={clearCheck}
              className="inline-flex items-center gap-1 h-6 px-2 rounded border border-line hover:bg-bg-3 transition"
            >
              <X className="w-3 h-3" /> Show everything
            </button>
          </div>
        )}

        {attention.isLoading && shown === 0 ? (
          <div className="px-4 py-8 text-center text-fg-3 text-sm">Loading…</div>
        ) : shown === 0 ? (
          <div className="px-8 py-16 text-center">
            <CheckCircle2 className="w-9 h-9 mx-auto mb-3 text-ok" />
            <h2 className="text-md font-medium mb-1.5">
              {view === "decision" ? "Nothing needs attention" : "Nothing else to show"}
            </h2>
            <p className="text-sm text-fg-3 max-w-md mx-auto">
              {view === "decision"
                ? "Checks run on their own in the background; anything that needs a decision will show up here."
                : "Informational findings and finished jobs appear here."}
            </p>
          </div>
        ) : (
          <div className="px-4 py-4 space-y-5">
            {linkedExtra && (
              <Section title="Linked job" count={1}>
                {jobTable([linkedExtra])}
              </Section>
            )}
            {inProgress.length > 0 && (
              <Section title="In progress" count={inProgress.length}>
                {jobTable(inProgress)}
              </Section>
            )}
            {proposed.length > 0 && (
              <Section title="Proposed changes to review" count={proposed.length}>
                {jobTable(proposed)}
              </Section>
            )}
            {failed.length > 0 && (
              <Section
                title="Failed jobs"
                count={failed.length}
                action={
                  <button
                    type="button"
                    onClick={() =>
                      onBulkDelete({ kind: "bulk", statuses: ["failed"], label: "failed", count: failed.length })
                    }
                    className="h-7 px-2.5 border border-line rounded text-xs text-fg-2 hover:bg-bg-3 transition inline-flex items-center gap-1.5"
                  >
                    <Trash2 className="w-3 h-3" />
                    Clear {failed.length} failed
                  </button>
                }
              >
                {jobTable(failed)}
              </Section>
            )}
            {byCheck.map(([checkId, list]) => {
              const check = checksById.get(checkId);
              const fixKind = check?.meta.fix_kind ?? "none";
              return (
                <Section
                  key={checkId}
                  id={`check-${checkId}`}
                  title={check?.meta.label ?? checkId}
                  count={list.length}
                  action={
                    view === "decision" && check?.meta.bulk_action_label && check.open_count > 0 ? (
                      <button
                        type="button"
                        onClick={() => setBulkCheck(check)}
                        className="h-7 px-2.5 border border-line rounded text-xs text-fg-2 hover:bg-bg-3 transition inline-flex items-center gap-1.5"
                      >
                        <Zap className="w-3 h-3" />
                        {check.meta.bulk_action_label} ({check.open_count})
                      </button>
                    ) : null
                  }
                >
                  <div className="rounded-md border border-line bg-panel-2 overflow-hidden divide-y divide-line">
                    {list.map((f) => (
                      <FindingRow
                        key={f.id}
                        checkId={checkId}
                        finding={f}
                        fixKind={fixKind}
                        album={f.album_id ? albumsById.get(f.album_id) ?? null : null}
                        siblings={list}
                      />
                    ))}
                  </div>
                </Section>
              );
            })}
            {finished.length > 0 && (
              <Section
                title="Finished jobs"
                count={finished.length}
                action={
                  <button
                    type="button"
                    onClick={() =>
                      onBulkDelete({ kind: "bulk", statuses: ["acted_on"], label: "acted-on", count: finished.length })
                    }
                    className="h-7 px-2.5 border border-line rounded text-xs text-fg-2 hover:bg-bg-3 transition inline-flex items-center gap-1.5"
                  >
                    <Trash2 className="w-3 h-3" />
                    Clear {finished.length} acted-on
                  </button>
                }
              >
                {jobTable(finished)}
              </Section>
            )}
          </div>
        )}
      </div>

      <DeleteConfirmDialog pending={pendingDelete} onClose={() => setPendingDelete(null)} aiReady={ai.ready} />
      {bulkCheck && (
        <BulkQuickFixDialog
          open
          onClose={() => setBulkCheck(null)}
          checkId={bulkCheck.meta.id}
          checkLabel={bulkCheck.meta.label}
          bulkActionLabel={bulkCheck.meta.bulk_action_label ?? "Run bulk fix"}
          fixKind={bulkCheck.meta.fix_kind}
          totalFindings={bulkCheck.open_count}
        />
      )}
    </div>
  );
}

function Section({
  id,
  title,
  count,
  action,
  children,
}: {
  id?: string;
  title: string;
  count: number;
  action?: React.ReactNode;
  children: React.ReactNode;
}) {
  const headingId = `${id ?? title.replace(/\W+/g, "-")}-heading`;
  return (
    <section id={id} aria-labelledby={headingId}>
      <div className="flex items-center gap-2 mb-2 min-h-7">
        <h2 id={headingId} className="text-sm font-medium">
          {title}
        </h2>
        <span className="mono text-xs text-fg-4 tabular-nums">{count}</span>
        <div className="flex-1" />
        {action}
      </div>
      {children}
    </section>
  );
}
