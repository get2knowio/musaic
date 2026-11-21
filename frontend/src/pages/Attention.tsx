import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { useNavigationType, useSearchParams } from "react-router-dom";
import { AlertCircle, CheckCircle2, Eye, EyeOff, Trash2, Zap } from "lucide-react";
import { toast } from "sonner";

import { BulkQuickFixDialog } from "@/components/BulkQuickFixDialog";
import { FindingRow } from "@/components/FindingRow";
import { SourceNotice } from "@/components/SourceNotice";
import {
  DeleteConfirmDialog,
  JobRow,
  PausedBanner,
  type PendingDelete,
} from "@/components/JobRow";
import { useAlbumsSnapshot } from "@/hooks/useAlbumsSnapshot";
import { useAiStatus } from "@/hooks/useAiStatus";
import { needsDecisionFinding, useAttention } from "@/hooks/useAttention";
import { useChecks, useIgnoredFindings } from "@/hooks/useChecks";
import { useBulkDeleteJobs, useDeleteJob, useJobs } from "@/hooks/useJobs";
import { usePageTitle } from "@/hooks/usePageTitle";
import type { Album, CheckSummary, Finding, Job } from "@/lib/types";

/**
 * Needs attention: everything the checks and the job queue have to say, on
 * one page with nothing behind a tab.
 *
 * Left, an index of every list that has something in it, each with its
 * count, in three groups: "Needs a decision" (open warn/error findings per
 * check, proposed and failed jobs: the Sidebar's first number),
 * "Suggestions" (informational findings per check: the Sidebar's second
 * number) and "Activity" (jobs in progress, finished jobs as history,
 * ignored findings). Right, the selected list, with what the check looks
 * for said above its findings.
 *
 * The selection is in the URL, so Back from an album's panel returns to
 * it: `?check=<id>` (all of that check's open findings, decisions first),
 * `?jobs=proposed|failed|running|finished`, `?list=ignored`, and `?job=<id>`
 * (a deep link: the group holding that job, with the job highlighted).
 * `?view=suggestions` and `?view=other`, the names of a former tab, open
 * the first list of suggestions. With no selection the first list in the
 * index is shown. `?ignored=1` adds a check's ignored findings to its list.
 * The scroll position is restored on Back too.
 *
 * Checks run on their own (the titlebar shows progress), so there is
 * nothing to run from here.
 */

/** Findings listed for a check before "Show all N". */
export const SECTION_ROW_LIMIT = 50;

export type JobGroup = "proposed" | "failed" | "running" | "finished";

type Selection =
  | { kind: "check"; id: string }
  | { kind: "jobs"; group: JobGroup }
  | { kind: "ignored" };

type Tier = "decision" | "suggestion" | "activity";

interface IndexEntry {
  key: string;
  tier: Tier;
  label: string;
  /** Absent where the number is not known without asking (ignored findings). */
  count?: number;
  selection: Selection;
}

const JOB_GROUP_LABEL: Record<JobGroup, string> = {
  proposed: "Proposed changes to review",
  failed: "Failed jobs",
  running: "In progress",
  finished: "Finished jobs",
};

const TIER_LABEL: Record<Tier, string> = {
  decision: "Needs a decision",
  suggestion: "Suggestions",
  activity: "Activity",
};

const SCROLL_KEY = "mm.attention.scroll";
const JOB_COLS = "100px minmax(0, 1.2fr) 110px 110px 72px";
const QUIET_BUTTON =
  "h-7 px-2.5 border border-line rounded text-xs text-fg-2 hover:bg-bg-3 transition inline-flex items-center gap-1.5";

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

const sameSelection = (a: Selection | null, b: Selection): boolean =>
  !!a &&
  a.kind === b.kind &&
  (a.kind === "check" ? a.id === (b as typeof a).id : a.kind === "jobs" ? a.group === (b as typeof a).group : true);

const selectionKey = (s: Selection | null): string =>
  !s ? "" : s.kind === "check" ? `check.${s.id}` : s.kind === "jobs" ? `jobs.${s.group}` : "ignored";

function jobGroupOf(j: Job): JobGroup {
  if (j.status === "completed") return "proposed";
  if (j.status === "failed") return "failed";
  if (j.status === "acted_on") return "finished";
  return "running";
}

export default function AttentionPage() {
  usePageTitle("Needs attention");
  const [params, setParams] = useSearchParams();
  const [expanded, setExpanded] = useState<ReadonlySet<string>>(new Set());

  const attention = useAttention();
  const { data: checks = [] } = useChecks();
  const { data: allJobs = [] } = useJobs();
  const ai = useAiStatus();
  const { data: albums = [] } = useAlbumsSnapshot();
  const albumsById = useMemo(() => new Map<string, Album>(albums.map((a) => [a.id, a])), [albums]);
  const checksById = useMemo(() => new Map(checks.map((c) => [c.meta.id, c])), [checks]);
  const [bulkCheck, setBulkCheck] = useState<CheckSummary | null>(null);

  // ---- job deletes ----
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

  // ---- the index ----
  const jobsByGroup: Record<JobGroup, Job[]> = useMemo(
    () => ({
      proposed: attention.proposed,
      failed: attention.failed,
      running: attention.inProgress,
      finished: attention.finishedJobs,
    }),
    [attention.proposed, attention.failed, attention.inProgress, attention.finishedJobs],
  );

  const index = useMemo(() => {
    // Registry order (the checks' display order); unknown checks last.
    const order = new Map(checks.map((c, i) => [c.meta.id, i]));
    const perCheck = (list: Finding[], tier: Tier): IndexEntry[] => {
      const counts = new Map<string, number>();
      for (const f of list) counts.set(f.check_id, (counts.get(f.check_id) ?? 0) + 1);
      return [...counts.entries()]
        .sort((a, b) => (order.get(a[0]) ?? 1e9) - (order.get(b[0]) ?? 1e9))
        .map(([id, count]) => ({
          key: `${tier}.${id}`,
          tier,
          label: checksById.get(id)?.meta.label ?? id,
          count,
          selection: { kind: "check", id },
        }));
    };
    const jobEntry = (group: JobGroup, tier: Tier): IndexEntry[] =>
      jobsByGroup[group].length > 0
        ? [
            {
              key: `jobs.${group}`,
              tier,
              label: JOB_GROUP_LABEL[group],
              count: jobsByGroup[group].length,
              selection: { kind: "jobs", group },
            },
          ]
        : [];
    return [
      ...jobEntry("proposed", "decision"),
      ...jobEntry("failed", "decision"),
      ...perCheck(attention.findings, "decision"),
      ...perCheck(attention.infoFindings, "suggestion"),
      ...jobEntry("running", "activity"),
      ...jobEntry("finished", "activity"),
    ];
  }, [checks, checksById, jobsByGroup, attention.findings, attention.infoFindings]);

  // ---- the selection, from the URL ----
  const checkParam = params.get("check");
  const jobsParam = params.get("jobs");
  const linkedJobId = params.get("job");
  const viewParam = params.get("view");
  const linked = linkedJobId ? allJobs.find((j) => j.id === linkedJobId) : undefined;
  const selection: Selection | null = checkParam
    ? { kind: "check", id: checkParam }
    : params.get("list") === "ignored"
      ? { kind: "ignored" }
      : jobsParam && jobsParam in JOB_GROUP_LABEL
        ? { kind: "jobs", group: jobsParam as JobGroup }
        : linked
          ? { kind: "jobs", group: jobGroupOf(linked) }
          : ((viewParam === "suggestions" || viewParam === "other"
              ? index.find((e) => e.tier === "suggestion")
              : undefined) ?? index[0])?.selection ?? null;

  const select = (next: Selection) => {
    const p = new URLSearchParams();
    if (next.kind === "check") p.set("check", next.id);
    else if (next.kind === "jobs") p.set("jobs", next.group);
    else p.set("list", "ignored");
    setParams(p, { replace: true });
  };

  // Ignored findings are never counted. A check's own are listed with it
  // on request; all of them have a list of their own.
  const showIgnored = selection?.kind === "check" && params.get("ignored") === "1";
  const ignoredQ = useIgnoredFindings(showIgnored || selection?.kind === "ignored");
  const toggleIgnored = () => {
    const p = new URLSearchParams(params);
    if (showIgnored) p.delete("ignored");
    else p.set("ignored", "1");
    setParams(p, { replace: true });
  };

  // ---- scroll: restored when coming Back, not on a fresh visit ----
  const navType = useNavigationType();
  const scroller = useRef<HTMLDivElement>(null);
  const scrollKey = selectionKey(selection);
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

  const suggestionCount = attention.infoFindings.length;
  const plus = attention.countIsLowerBound ? "+" : "";
  const tierTotal: Record<Tier, string | null> = {
    decision: `${attention.count}${plus}`,
    suggestion: `${suggestionCount}${plus}`,
    activity: null,
  };
  const nothingAtAll = index.length === 0 && !selection;

  const jobTable = (jobs: Job[]) => (
    <div className="rounded-md border border-line bg-panel-2 overflow-hidden">
      <div
        className="grid gap-3 px-3 py-2 border-b border-line text-fg-4 text-xs"
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

  const findingList = (checkId: string, list: Finding[], capKey: string | null) => {
    const check = checksById.get(checkId);
    const capped = capKey !== null && !expanded.has(capKey) && list.length > SECTION_ROW_LIMIT;
    return (
      <>
        <div className="rounded-md border border-line bg-panel-2 overflow-hidden divide-y divide-line">
          {(capped ? list.slice(0, SECTION_ROW_LIMIT) : list).map((f) => (
            <FindingRow
              key={f.id}
              checkId={checkId}
              finding={f}
              fixKind={check?.meta.fix_kind ?? "none"}
              album={f.album_id ? albumsById.get(f.album_id) ?? null : null}
              siblings={list}
            />
          ))}
        </div>
        {capped && (
          <button
            type="button"
            onClick={() => setExpanded((prev) => new Set(prev).add(capKey))}
            className={`mt-2 ${QUIET_BUTTON}`}
          >
            Show all {list.length}
          </button>
        )}
      </>
    );
  };

  const detail = () => {
    if (!selection) return null;

    if (selection.kind === "check") {
      const check = checksById.get(selection.id);
      const open = [...attention.findings, ...attention.infoFindings].filter(
        (f) => f.check_id === selection.id,
      );
      const ignored = showIgnored ? (ignoredQ.data ?? []).filter((f) => f.check_id === selection.id) : [];
      const list = [...open, ...ignored];
      const decisions = open.filter(needsDecisionFinding).length;
      return (
        <section id={`check-${selection.id}`} aria-labelledby="attention-detail-heading">
          <DetailHead
            title={check?.meta.label ?? selection.id}
            count={open.length}
            note={
              decisions > 0 && decisions < open.length
                ? `${decisions} need a decision, ${open.length - decisions} are suggestions`
                : decisions === 0 && open.length > 0
                  ? "Suggestions: nothing here is wrong, but each can be tidied"
                  : null
            }
            description={check?.meta.description}
            actions={
              <>
                <button type="button" aria-pressed={showIgnored} onClick={toggleIgnored} className={QUIET_BUTTON}>
                  {showIgnored ? <EyeOff className="w-3 h-3" /> : <Eye className="w-3 h-3" />}
                  {showIgnored ? "Hide ignored" : "Show ignored"}
                </button>
                {check?.meta.bulk_action_label && check.open_count > 0 && (
                  <button type="button" onClick={() => setBulkCheck(check)} className={QUIET_BUTTON}>
                    <Zap className="w-3 h-3" />
                    {check.meta.bulk_action_label} ({check.open_count})
                  </button>
                )}
              </>
            }
          />
          {list.length > 0 ? (
            findingList(selection.id, list, selection.id)
          ) : (
            <Quiet>
              {showIgnored && ignoredQ.isLoading
                ? "Loading…"
                : "Nothing open for this check. It runs on its own; anything it finds will be listed here."}
            </Quiet>
          )}
        </section>
      );
    }

    if (selection.kind === "ignored") {
      const byCheck = new Map<string, Finding[]>();
      for (const f of ignoredQ.data ?? []) byCheck.set(f.check_id, [...(byCheck.get(f.check_id) ?? []), f]);
      return (
        <section aria-labelledby="attention-detail-heading">
          <DetailHead
            title="Ignored findings"
            count={ignoredQ.data?.length}
            description="Findings you chose to ignore. They are not counted anywhere and stay ignored until you restore them."
          />
          {ignoredQ.isLoading ? (
            <Quiet>Loading…</Quiet>
          ) : byCheck.size === 0 ? (
            <Quiet>Nothing is ignored.</Quiet>
          ) : (
            <div className="space-y-4">
              {[...byCheck.entries()].map(([checkId, list]) => (
                <div key={checkId}>
                  <h3 className="text-xs text-fg-3 mb-1.5">{checksById.get(checkId)?.meta.label ?? checkId}</h3>
                  {findingList(checkId, list, null)}
                </div>
              ))}
            </div>
          )}
        </section>
      );
    }

    const jobs = jobsByGroup[selection.group];
    const clear =
      selection.group === "failed"
        ? ({ statuses: ["failed"], label: "failed" } as const)
        : selection.group === "finished"
          ? ({ statuses: ["acted_on"], label: "acted-on" } as const)
          : null;
    return (
      <section aria-labelledby="attention-detail-heading">
        <DetailHead
          title={JOB_GROUP_LABEL[selection.group]}
          count={jobs.length}
          description={
            selection.group === "proposed"
              ? "Changes a job worked out and has not applied. Open one to review it, then apply or discard it."
              : selection.group === "failed"
                ? "Jobs that stopped with an error. Retry one from its row, or clear them."
                : selection.group === "running"
                  ? "Jobs that are queued or running now."
                  : "Jobs whose result you applied or discarded, kept as history."
          }
          actions={
            clear && jobs.length > 0 ? (
              <button
                type="button"
                onClick={() => onBulkDelete({ kind: "bulk", statuses: [...clear.statuses], label: clear.label, count: jobs.length })}
                className={QUIET_BUTTON}
              >
                <Trash2 className="w-3 h-3" />
                Clear {jobs.length} {clear.label}
              </button>
            ) : null
          }
        />
        {jobs.length > 0 ? jobTable(jobs) : <Quiet>None right now.</Quiet>}
      </section>
    );
  };

  return (
    <div className="flex-1 min-h-0 flex flex-col">
      <div className="flex flex-wrap items-center gap-x-3 gap-y-1 px-4 min-h-[var(--filterbar-h)] border-b border-line bg-panel-2 text-sm shrink-0">
        <AlertCircle className="w-3.5 h-3.5 text-accent" />
        <h1 className="font-medium">Needs attention</h1>
        <span className="text-fg-3" aria-live="polite">
          <b
            className={`font-medium tabular-nums ${
              attention.count > 0 ? "text-warn" : attention.allClear ? "text-ok" : "text-fg-2"
            }`}
          >
            {attention.count}
            {plus}
          </b>{" "}
          to decide,{" "}
          <b className="font-medium tabular-nums text-fg-2">
            {suggestionCount}
            {plus}
          </b>{" "}
          {suggestionCount === 1 ? "suggestion" : "suggestions"}
        </span>
      </div>

      <PausedBanner aiReady={ai.ready} />
      <SourceNotice
        sources={[attention.sources.findings, attention.sources.jobs]}
        className="mx-4 mt-3 mb-1"
      />

      {attention.isLoading && nothingAtAll ? (
        <div className="px-4 py-8 text-center text-fg-3 text-sm">Loading…</div>
      ) : nothingAtAll && !attention.complete ? (
        // A source failed or has not answered: an empty page here is not
        // an all-clear.
        <div className="px-8 py-16 text-center">
          <h2 className="text-md font-medium mb-1.5">
            {attention.degraded ? "Review data isn't available" : "Loading…"}
          </h2>
          {attention.degraded && (
            <p className="text-sm text-fg-3 max-w-md mx-auto">
              Nothing is listed because part of it could not be loaded, not because nothing needs attention.
              Retry above.
            </p>
          )}
        </div>
      ) : nothingAtAll ? (
        <div className="px-8 py-16 text-center">
          <CheckCircle2 className="w-9 h-9 mx-auto mb-3 text-ok" />
          <h2 className="text-md font-medium mb-1.5">Nothing needs attention</h2>
          <p className="text-sm text-fg-3 max-w-md mx-auto">
            Checks run on their own in the background. Anything they find, and any job that needs a look,
            will be listed here.
          </p>
          <button type="button" onClick={() => select({ kind: "ignored" })} className={`mt-4 ${QUIET_BUTTON}`}>
            Ignored findings
          </button>
        </div>
      ) : (
        <div className="flex-1 min-h-0 flex flex-col md:flex-row">
          <nav
            aria-label="Lists"
            className="shrink-0 max-h-[40%] md:max-h-none md:w-[248px] overflow-y-auto border-b md:border-b-0 md:border-r border-line bg-panel-2 px-2 py-3"
          >
            {(["decision", "suggestion", "activity"] as const).map((tier) => {
              const entries = index.filter((e) => e.tier === tier);
              return (
                <div key={tier} className="mb-4 last:mb-0">
                  <div data-tier={tier} className="flex items-baseline gap-2 px-2 mb-1 text-xs text-fg-3">
                    <span className="font-medium">{TIER_LABEL[tier]}</span>
                    {tierTotal[tier] !== null && (
                      <span className="ml-auto tabular-nums">{tierTotal[tier]}</span>
                    )}
                  </div>
                  {entries.length === 0 && tier !== "activity" && (
                    <div className="px-2 py-1 text-xs text-fg-4">
                      {tier === "decision" ? "Nothing to decide" : "No suggestions"}
                    </div>
                  )}
                  {entries.map((e) => (
                    <IndexRow
                      key={e.key}
                      entry={e}
                      active={sameSelection(selection, e.selection)}
                      onSelect={() => select(e.selection)}
                    />
                  ))}
                  {tier === "activity" && (
                    <IndexRow
                      entry={{ key: "ignored", tier, label: "Ignored findings", selection: { kind: "ignored" } }}
                      active={selection?.kind === "ignored"}
                      onSelect={() => select({ kind: "ignored" })}
                    />
                  )}
                </div>
              );
            })}
          </nav>
          <div ref={scroller} tabIndex={-1} className="flex-1 min-h-0 min-w-0 overflow-y-auto outline-none px-5 py-4">
            {detail()}
          </div>
        </div>
      )}

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

/** One line of the index. The mark says which kind of list it is: filled
 *  for what needs a decision, a ring for suggestions, nothing for activity. */
function IndexRow({ entry, active, onSelect }: { entry: IndexEntry; active: boolean; onSelect: () => void }) {
  return (
    <button
      type="button"
      onClick={onSelect}
      aria-current={active ? "true" : undefined}
      title={entry.label}
      className={
        "w-full h-7 px-2 rounded flex items-center gap-2 text-left text-sm transition " +
        "focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent " +
        (active ? "bg-accent-soft text-accent font-medium" : "text-fg-2 hover:bg-bg-3 hover:text-fg")
      }
    >
      <span
        aria-hidden
        className={
          "w-1.5 h-1.5 rounded-full shrink-0 " +
          (entry.tier === "decision" ? "bg-warn" : entry.tier === "suggestion" ? "border border-fg-4" : "")
        }
      />
      <span className="truncate flex-1">{entry.label}</span>
      {entry.count !== undefined && (
        <span className={"text-xs tabular-nums " + (active ? "" : "text-fg-4")}>{entry.count}</span>
      )}
    </button>
  );
}

function DetailHead({
  title,
  count,
  note,
  description,
  actions,
}: {
  title: string;
  count?: number;
  note?: string | null;
  description?: string;
  actions?: React.ReactNode;
}) {
  return (
    <header className="mb-4">
      <div className="flex flex-wrap items-center gap-x-3 gap-y-2">
        <h2 id="attention-detail-heading" className="text-lg font-medium leading-tight">
          {title}
        </h2>
        {count !== undefined && <span className="text-sm text-fg-3 tabular-nums">{count}</span>}
        <div className="flex-1" />
        <div className="flex items-center gap-1.5">{actions}</div>
      </div>
      {note && <p className="mt-1 text-xs text-fg-3">{note}</p>}
      {description && <p className="mt-2 text-sm text-fg-2 max-w-[68ch] leading-relaxed">{description}</p>}
    </header>
  );
}

function Quiet({ children }: { children: React.ReactNode }) {
  return <p className="rounded-md border border-dashed border-line px-3 py-6 text-sm text-fg-3">{children}</p>;
}
