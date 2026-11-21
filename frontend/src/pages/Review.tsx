import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { useNavigationType, useSearchParams } from "react-router-dom";
import { Activity as ActivityIcon, CheckCircle2, Eye, EyeOff, Inbox, Trash2, X, Zap } from "lucide-react";
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
import {
  albumCount,
  countText,
  filterReviewItems,
  itemsText,
  jobKey,
  URGENCY_LABEL,
  useReview,
  type ReviewFilter,
  type ReviewItem,
} from "@/hooks/useReview";
import { useChecks, useIgnoredFindings } from "@/hooks/useChecks";
import { useBulkDeleteJobs, useDeleteJob, useJobs } from "@/hooks/useJobs";
import { usePageTitle } from "@/hooks/usePageTitle";
import { openActivityPanel, type ActivitySection } from "@/lib/activityPanel";
import { displayArtist } from "@/lib/albumFields";
import type { Album, CheckSummary, Finding, Job } from "@/lib/types";

/**
 * Review: one inbox of everything that asks something of the user.
 *
 * It opens to All open: every open finding whatever its severity
 * (informational ones included, nothing behind a tab or disclosure),
 * every proposed change and every failed job, most pressing first
 * (`sortReviewItems`), each row with a short urgency label. The header
 * counts the items shown and the albums they are about; the Sidebar badge
 * is the same count of All open, so a filter ("2 of 3 items") never
 * changes it.
 *
 * Left, the filters: All open, then each type (proposed changes, failed
 * jobs, and each check once) with its count; exactly one is current. An
 * album filter (`?album=<id>`, set from an album's panel) narrows any of
 * them and is shown as a chip in the header.
 *
 * Below them, not counted: ignored findings, and a way to the Activity
 * panel, which holds jobs in progress and job history (they are not review
 * items).
 *
 * The URL holds the state, so Back from an album's panel returns to it:
 * `?check=<id>`, `?kind=proposed|failed`, `?album=<id>`, `?show=ignored`,
 * `?ignored=1` (a check's ignored findings listed with it), `?all=1` (past
 * the first SECTION_ROW_LIMIT rows), and `?job=<id>` (a deep link: the job
 * highlighted where it lives; a job in progress or finished opens the
 * Activity panel at it). `?show=running|history` from older builds opens
 * the Activity panel too. The scroll position and the row last opened are
 * restored on Back. Old Needs attention, Health and Jobs links are
 * translated by `reviewRedirects.tsx`.
 *
 * Checks run on their own (the Activity control shows progress), so there
 * is nothing to run from here.
 */

/** Rows listed before "Show all N". */
export const SECTION_ROW_LIMIT = 50;

type View = { kind: "inbox"; filter: ReviewFilter } | { kind: "ignored" };

const SCROLL_KEY = "mm.review.scroll";
const ITEM_KEY = "mm.review.item";
const QUIET_BUTTON =
  "h-7 px-2.5 border border-line rounded text-xs text-fg-2 hover:bg-bg-3 transition inline-flex items-center gap-1.5";

function readSession(key: string): string | null {
  try {
    return sessionStorage.getItem(key);
  } catch {
    return null;
  }
}

function writeSession(key: string, value: string) {
  try {
    sessionStorage.setItem(key, value);
  } catch {
    // Without storage Back just starts at the top.
  }
}

function viewKey(v: View, album: string | null): string {
  const base =
    v.kind !== "inbox"
      ? v.kind
      : v.filter.kind === "check"
        ? `check.${v.filter.id}`
        : v.filter.kind;
  return album && v.kind === "inbox" ? `${base}@${album}` : base;
}

function sameView(a: View, b: View): boolean {
  return viewKey(a, null) === viewKey(b, null);
}

const FILTER_TITLE: Record<"all" | "proposed" | "failed", string> = {
  all: "All open",
  proposed: "Proposed changes",
  failed: "Failed jobs",
};

function albumName(a: Album | undefined, id: string): string {
  if (!a) return `album ${id.slice(0, 8)}`;
  const title = (a.title || a.dir_name || "").trim();
  const artist = a.is_compilation ? "Various Artists" : displayArtist(a);
  return artist && title ? `${title} — ${artist}` : title || a.id;
}

export default function ReviewPage() {
  usePageTitle("Review");
  const [params, setParams] = useSearchParams();

  const review = useReview();
  const { data: checks = [] } = useChecks();
  const { data: allJobs = [] } = useJobs();
  const ai = useAiStatus();
  const { data: albums = [] } = useAlbumsSnapshot();
  const albumsById = useMemo(() => new Map<string, Album>(albums.map((a) => [a.id, a])), [albums]);
  const checksById = useMemo(() => new Map(checks.map((c) => [c.meta.id, c])), [checks]);
  const checkLabel = (id: string) => checksById.get(id)?.meta.label ?? id;
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

  // ---- the view, from the URL ----
  const album = params.get("album");
  const linkedJobId = params.get("job");
  const linked = linkedJobId ? allJobs.find((j) => j.id === linkedJobId) : undefined;
  const show = params.get("show");
  const kindParam = params.get("kind");
  const checkParam = params.get("check");
  const view: View =
    show === "ignored"
      ? { kind: "ignored" }
      : checkParam
        ? { kind: "inbox", filter: { kind: "check", id: checkParam } }
        : kindParam === "proposed" || kindParam === "failed"
          ? { kind: "inbox", filter: { kind: kindParam } }
          : { kind: "inbox", filter: { kind: "all" } };
  const key = viewKey(view, album);

  // Jobs in progress and job history live in the Activity panel: an old
  // `show=running|history` link, or a link to a job that is not a review
  // item, opens it over All open.
  const activitySection: ActivitySection | null =
    show === "running"
      ? "jobs"
      : show === "history"
        ? "history"
        : linked && (linked.status === "queued" || linked.status === "running")
          ? "jobs"
          : linked?.status === "acted_on"
            ? "history"
            : null;
  useEffect(() => {
    if (!activitySection) return;
    openActivityPanel({ section: activitySection, job: linkedJobId });
    const p = new URLSearchParams(params);
    p.delete("show");
    p.delete("job");
    setParams(p, { replace: true });
  }, [activitySection, linkedJobId, params, setParams]);

  const select = (next: View) => {
    const p = new URLSearchParams();
    if (next.kind === "inbox") {
      if (next.filter.kind === "check") p.set("check", next.filter.id);
      else if (next.filter.kind !== "all") p.set("kind", next.filter.kind);
      // The album filter narrows whichever type is chosen.
      if (album) p.set("album", album);
    } else {
      p.set("show", next.kind);
    }
    setParams(p, { replace: true });
  };
  const setParam = (name: string, value: string | null) => {
    const p = new URLSearchParams(params);
    if (value === null) p.delete(name);
    else p.set(name, value);
    setParams(p, { replace: true });
  };

  // ---- the rail ----
  const rail = useMemo(() => {
    const perCheck = new Map<string, number>();
    let proposed = 0;
    let failed = 0;
    for (const i of review.items) {
      if (i.kind === "finding") perCheck.set(i.finding.check_id, (perCheck.get(i.finding.check_id) ?? 0) + 1);
      else if (i.job.status === "completed") proposed++;
      else failed++;
    }
    // Registry order (the checks' display order); unknown checks last.
    const order = new Map(checks.map((c, i) => [c.meta.id, i]));
    const checkIds = [...perCheck.keys()].sort((a, b) => (order.get(a) ?? 1e9) - (order.get(b) ?? 1e9));
    return { perCheck, proposed, failed, checkIds };
  }, [review.items, checks]);

  // ---- what is listed ----
  const visible = view.kind === "inbox" ? filterReviewItems(review.items, view.filter, album) : [];
  const filtered = view.kind === "inbox" && (view.filter.kind !== "all" || !!album);

  // Ignored findings are never counted. A check's own are listed with it
  // on request; all of them have a view of their own.
  const showIgnored = view.kind === "inbox" && view.filter.kind === "check" && params.get("ignored") === "1";
  const ignoredQ = useIgnoredFindings(showIgnored || view.kind === "ignored");

  // ---- Back: the scroll position and the row last opened ----
  const navType = useNavigationType();
  const scroller = useRef<HTMLDivElement>(null);
  const ready = !review.isLoading;
  const restored = useRef<string | null>(null);
  useLayoutEffect(() => {
    const el = scroller.current;
    if (!el || !ready || restored.current === key) return;
    restored.current = key;
    el.scrollTop = navType === "POP" ? parseInt(readSession(`${SCROLL_KEY}.${key}`) ?? "", 10) || 0 : 0;
  }, [ready, key, navType]);
  useEffect(() => {
    const el = scroller.current;
    if (!el) return;
    const onScroll = () => {
      if (restored.current === key) writeSession(`${SCROLL_KEY}.${key}`, String(el.scrollTop));
    };
    el.addEventListener("scroll", onScroll, { passive: true });
    return () => el.removeEventListener("scroll", onScroll);
  }, [key]);
  // Coming Back (the page mounts again), the row the user opened is marked.
  const [returned] = useState(() =>
    navType === "POP" ? { view: key, item: readSession(`${ITEM_KEY}.${key}`) } : null,
  );
  const rememberItem = (itemKey: string) => writeSession(`${ITEM_KEY}.${key}`, itemKey);

  const highlightKey = linkedJobId ? jobKey(linkedJobId) : returned?.view === key ? returned.item : null;

  // ... and, once listed, focused again: its album link (the way the user
  // left), else its first control, so the keyboard picks up where it was.
  const refocused = useRef(false);
  const returnedItem = returned?.view === key ? returned.item : null;
  useEffect(() => {
    if (refocused.current || !ready || !returnedItem) return;
    const row = scroller.current?.querySelector(`[data-review-item="${CSS.escape(returnedItem)}"]`);
    if (!row) return;
    refocused.current = true;
    const target =
      row.querySelector<HTMLElement>('a[href^="/library/"]') ?? row.querySelector<HTMLElement>("a[href], button");
    target?.focus({ preventScroll: true });
  }, [ready, returnedItem]);

  // A deep-linked job is brought into view once it is on the page.
  const linkedPresent = !!linked;
  useEffect(() => {
    if (!linkedJobId || !linkedPresent) return;
    scroller.current
      ?.querySelector(`[data-job-id="${CSS.escape(linkedJobId)}"]`)
      ?.scrollIntoView?.({ block: "nearest" });
  }, [linkedJobId, linkedPresent]);

  // ---- the header ----
  const lowerBound = review.countIsLowerBound;
  const total = itemsText(review.count, lowerBound);
  const albumsOf = (n: number) => `${n} ${n === 1 ? "album" : "albums"}`;
  const summary = review.isLoading
    ? "Loading…"
    : view.kind === "inbox" && filtered
      ? `${visible.length} of ${total} · ${albumsOf(albumCount(visible))}`
      : `${total} · ${albumsOf(albumCount(review.items))}`;

  // ---- rows ----
  const findingRow = (f: Finding, opts: { urgency?: string; withCheck: boolean; siblings: Finding[] }) => {
    const check = checksById.get(f.check_id);
    return (
      <FindingRow
        checkId={f.check_id}
        finding={f}
        fixKind={check?.meta.fix_kind ?? "none"}
        album={f.album_id ? albumsById.get(f.album_id) ?? null : null}
        siblings={opts.siblings}
        urgencyLabel={opts.urgency}
        checkLabel={opts.withCheck ? checkLabel(f.check_id) : undefined}
      />
    );
  };

  const itemList = (items: ReviewItem[]) => {
    const highlightAt = highlightKey ? items.findIndex((i) => i.key === highlightKey) : -1;
    const capped =
      params.get("all") !== "1" && items.length > SECTION_ROW_LIMIT && highlightAt < SECTION_ROW_LIMIT;
    const siblingsByCheck = new Map<string, Finding[]>();
    for (const i of items) {
      if (i.kind !== "finding") continue;
      const list = siblingsByCheck.get(i.finding.check_id) ?? [];
      list.push(i.finding);
      siblingsByCheck.set(i.finding.check_id, list);
    }
    const withCheck = !(view.kind === "inbox" && view.filter.kind === "check");
    return (
      <>
        <ul aria-label="Review items" className="rounded-md border border-line bg-panel-2 overflow-hidden divide-y divide-line">
          {(capped ? items.slice(0, SECTION_ROW_LIMIT) : items).map((i) => (
            <li
              key={i.key}
              data-review-item={i.key}
              data-current={i.key === highlightKey ? "true" : undefined}
              className={i.kind === "finding" && i.key === highlightKey ? "bg-accent-soft" : undefined}
              onClickCapture={() => rememberItem(i.key)}
            >
              {i.kind === "finding" ? (
                findingRow(i.finding, {
                  urgency: URGENCY_LABEL[i.urgency],
                  withCheck,
                  siblings: siblingsByCheck.get(i.finding.check_id) ?? [],
                })
              ) : (
                <JobRow job={i.job} onRequestDelete={onSingleDelete} highlight={i.key === highlightKey} />
              )}
            </li>
          ))}
        </ul>
        {capped && (
          <button type="button" onClick={() => setParam("all", "1")} className={`mt-2 ${QUIET_BUTTON}`}>
            Show all {items.length}
          </button>
        )}
      </>
    );
  };

  // ---- the selected view ----
  const inboxDetail = (filter: ReviewFilter) => {
    const check = filter.kind === "check" ? checksById.get(filter.id) : undefined;
    const title = filter.kind === "check" ? checkLabel(filter.id) : FILTER_TITLE[filter.kind];
    const ignored =
      filter.kind === "check" && showIgnored
        ? (ignoredQ.data ?? []).filter((f) => f.check_id === filter.id && (!album || f.album_id === album))
        : [];
    const failedJobs = filter.kind === "failed" ? visible.filter((i) => i.kind === "job") : [];
    const bulk = check?.meta.bulk_action_label && check.open_count > 0 ? check : null;
    const bulkScope = bulk
      ? `${bulk.meta.bulk_action_label} works on all ${bulk.open_count} open ${bulk.meta.label} ` +
        `${bulk.open_count === 1 ? "finding" : "findings"} in the library` +
        (visible.length !== bulk.open_count ? `, not only the ${visible.length} shown here.` : ".")
      : null;

    let empty: React.ReactNode = null;
    if (visible.length === 0) {
      if (filter.kind === "all" && !album && review.allClear) {
        empty = (
          <div className="px-8 py-14 text-center">
            <CheckCircle2 className="w-9 h-9 mx-auto mb-3 text-ok" />
            <h2 className="text-md font-medium mb-1.5">Nothing to review</h2>
            <p className="text-sm text-fg-3 max-w-md mx-auto">
              Checks run on their own in the background. Anything they find, any change proposed for an
              album and any job that fails will be listed here.
            </p>
          </div>
        );
      } else if (review.isLoading) {
        empty = <Quiet>Loading…</Quiet>;
      } else if (review.degraded) {
        empty = (
          <div className="px-8 py-14 text-center">
            <h2 className="text-md font-medium mb-1.5">Review data isn't available</h2>
            <p className="text-sm text-fg-3 max-w-md mx-auto">
              Nothing is listed because part of it could not be loaded, not because nothing needs review.
              Retry above.
            </p>
          </div>
        );
      } else {
        empty = (
          <Quiet>
            {filter.kind === "check"
              ? `Nothing open for this check${album ? " on this album" : ""}. It runs on its own; anything it finds will be listed here.`
              : album
                ? "Nothing open here for this album."
                : "Nothing here right now."}
          </Quiet>
        );
      }
    }

    return (
      <section id={filter.kind === "check" ? `check-${filter.id}` : undefined} aria-labelledby="review-detail-heading">
        <DetailHead
          title={title}
          count={visible.length}
          description={check?.meta.description}
          actions={
            <>
              {filter.kind === "check" && (
                <button
                  type="button"
                  aria-pressed={showIgnored}
                  onClick={() => setParam("ignored", showIgnored ? null : "1")}
                  className={QUIET_BUTTON}
                >
                  {showIgnored ? <EyeOff className="w-3 h-3" /> : <Eye className="w-3 h-3" />}
                  {showIgnored ? "Hide ignored" : "Show ignored"}
                </button>
              )}
              {bulk && (
                <button type="button" onClick={() => setBulkCheck(bulk)} title={bulkScope ?? undefined} className={QUIET_BUTTON}>
                  <Zap className="w-3 h-3" />
                  {bulk.meta.bulk_action_label} ({bulk.open_count})
                </button>
              )}
              {failedJobs.length > 0 && !album && (
                <button
                  type="button"
                  onClick={() =>
                    onBulkDelete({ kind: "bulk", statuses: ["failed"], label: "failed", count: failedJobs.length })
                  }
                  className={QUIET_BUTTON}
                >
                  <Trash2 className="w-3 h-3" />
                  Clear {failedJobs.length} failed
                </button>
              )}
            </>
          }
          note={bulkScope}
        />
        {empty ?? itemList(visible)}
        {showIgnored && (
          <div className="mt-4">
            <h3 className="text-xs text-fg-3 mb-1.5">Ignored ({ignoredQ.isLoading ? "…" : ignored.length})</h3>
            {ignoredQ.isLoading ? (
              <Quiet>Loading…</Quiet>
            ) : ignored.length === 0 ? (
              <Quiet>Nothing ignored for this check.</Quiet>
            ) : (
              <div className="rounded-md border border-line bg-panel-2 overflow-hidden divide-y divide-line">
                {ignored.map((f) => (
                  <div key={f.id}>{findingRow(f, { withCheck: false, siblings: ignored })}</div>
                ))}
              </div>
            )}
          </div>
        )}
      </section>
    );
  };

  const detail = () => {
    if (view.kind === "inbox") return inboxDetail(view.filter);

    // Ignored findings.
    const byCheck = new Map<string, Finding[]>();
    for (const f of ignoredQ.data ?? []) byCheck.set(f.check_id, [...(byCheck.get(f.check_id) ?? []), f]);
    return (
      <section aria-labelledby="review-detail-heading">
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
                <h3 className="text-xs text-fg-3 mb-1.5">{checkLabel(checkId)}</h3>
                <div className="rounded-md border border-line bg-panel-2 overflow-hidden divide-y divide-line">
                  {list.map((f) => (
                    <div key={f.id}>{findingRow(f, { withCheck: false, siblings: list })}</div>
                  ))}
                </div>
              </div>
            ))}
          </div>
        )}
      </section>
    );
  };

  const all: View = { kind: "inbox", filter: { kind: "all" } };

  return (
    <div className="flex-1 min-h-0 flex flex-col">
      <div className="flex flex-wrap items-center gap-x-3 gap-y-1 px-4 min-h-[var(--filterbar-h)] border-b border-line bg-panel-2 text-sm shrink-0">
        <Inbox className="w-3.5 h-3.5 text-fg-2" />
        <h1 className="font-medium">Review</h1>
        <span className="text-fg-3 tabular-nums" aria-live="polite" data-review-summary>
          {summary}
        </span>
        {album && view.kind === "inbox" && (
          <span className="inline-flex items-center gap-1 h-6 pl-2 pr-1 rounded-full border border-line text-xs text-fg-2 max-w-[22rem]">
            <span className="truncate">Album: {albumName(albumsById.get(album), album)}</span>
            <button
              type="button"
              onClick={() => setParam("album", null)}
              aria-label="Clear the album filter"
              className="w-4 h-4 grid place-items-center rounded-full hover:bg-bg-3"
            >
              <X className="w-3 h-3" />
            </button>
          </span>
        )}
      </div>

      <PausedBanner aiReady={ai.ready} />
      <SourceNotice sources={[review.sources.findings, review.sources.jobs]} className="mx-4 mt-3 mb-1" />

      <div className="flex-1 min-h-0 flex flex-col md:flex-row">
        <nav
          aria-label="Filters"
          className="shrink-0 max-h-[40%] md:max-h-none md:w-[248px] overflow-y-auto border-b md:border-b-0 md:border-r border-line bg-panel-2 px-2 py-3"
        >
          <RailRow
            label="All open"
            count={review.isLoading ? undefined : review.count}
            display={countText(review.count, lowerBound)}
            active={sameView(view, all)}
            onSelect={() => select(all)}
          />
          <RailGroup label="By type">
            {(rail.proposed > 0 || (view.kind === "inbox" && view.filter.kind === "proposed")) && (
              <RailRow
                label="Proposed changes"
                count={rail.proposed}
                active={view.kind === "inbox" && view.filter.kind === "proposed"}
                onSelect={() => select({ kind: "inbox", filter: { kind: "proposed" } })}
              />
            )}
            {(rail.failed > 0 || (view.kind === "inbox" && view.filter.kind === "failed")) && (
              <RailRow
                label="Failed jobs"
                count={rail.failed}
                active={view.kind === "inbox" && view.filter.kind === "failed"}
                onSelect={() => select({ kind: "inbox", filter: { kind: "failed" } })}
              />
            )}
            {[
              ...rail.checkIds,
              // A check opened by link with nothing open keeps its entry.
              ...(view.kind === "inbox" && view.filter.kind === "check" && !rail.perCheck.has(view.filter.id)
                ? [view.filter.id]
                : []),
            ].map((id) => (
              <RailRow
                key={id}
                label={checkLabel(id)}
                count={rail.perCheck.get(id) ?? 0}
                active={view.kind === "inbox" && view.filter.kind === "check" && view.filter.id === id}
                onSelect={() => select({ kind: "inbox", filter: { kind: "check", id } })}
              />
            ))}
          </RailGroup>
          <RailGroup label="Not counted">
            <RailRow
              label="Ignored findings"
              active={view.kind === "ignored"}
              onSelect={() => select({ kind: "ignored" })}
            />
            {/* Jobs in progress and their history are activity, not
                decisions: they live in the Activity panel. */}
            <button
              type="button"
              onClick={() => openActivityPanel({ section: "jobs" })}
              aria-haspopup="dialog"
              className={
                "w-full h-7 px-2 rounded flex items-center gap-2 text-left text-sm transition text-fg-2 hover:bg-bg-3 hover:text-fg " +
                "focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent"
              }
            >
              <span className="truncate flex-1">Jobs in progress and history</span>
              <ActivityIcon className="w-3 h-3 text-fg-4" aria-hidden />
            </button>
          </RailGroup>
        </nav>
        <div ref={scroller} tabIndex={-1} className="flex-1 min-h-0 min-w-0 overflow-y-auto outline-none px-5 py-4">
          {detail()}
        </div>
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

function RailGroup({ label, note, children }: { label: string; note?: string; children: React.ReactNode }) {
  return (
    <div className="mt-4">
      <div className="flex items-baseline gap-2 px-2 mb-1 text-xs text-fg-3">
        <span className="font-medium">{label}</span>
        {note && <span className="ml-auto text-fg-4">{note}</span>}
      </div>
      {children}
    </div>
  );
}

/** One filter. Exactly one is current. */
function RailRow({
  label,
  count,
  display,
  active,
  onSelect,
}: {
  label: string;
  /** Absent where the number is not known without asking (ignored findings). */
  count?: number;
  /** The count as shown, when it is not just the number ("3+"). */
  display?: string;
  active: boolean;
  onSelect: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onSelect}
      aria-current={active ? "true" : undefined}
      title={label}
      className={
        "w-full h-7 px-2 rounded flex items-center gap-2 text-left text-sm transition " +
        "focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent " +
        (active ? "bg-accent-soft text-accent font-medium" : "text-fg-2 hover:bg-bg-3 hover:text-fg")
      }
    >
      <span className="truncate flex-1">{label}</span>
      {count !== undefined && (
        <span className={"text-xs tabular-nums " + (active ? "" : "text-fg-4")}>{display ?? count}</span>
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
        <h2 id="review-detail-heading" className="text-lg font-medium leading-tight">
          {title}
        </h2>
        {count !== undefined && <span className="text-sm text-fg-3 tabular-nums">{count}</span>}
        <div className="flex-1" />
        <div className="flex items-center gap-1.5">{actions}</div>
      </div>
      {note && (
        <p data-bulk-scope className="mt-1 text-xs text-fg-3">
          {note}
        </p>
      )}
      {description && <p className="mt-2 text-sm text-fg-2 max-w-[68ch] leading-relaxed">{description}</p>}
    </header>
  );
}

function Quiet({ children }: { children: React.ReactNode }) {
  return <p className="rounded-md border border-dashed border-line px-3 py-6 text-sm text-fg-3">{children}</p>;
}
