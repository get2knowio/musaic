import { Disc3, Inbox, Settings as SettingsIcon } from "lucide-react";
import { useMemo } from "react";
import { Link, NavLink, useLocation, useSearchParams } from "react-router-dom";

import { useDelegatedWork } from "@/hooks/useDelegatedWork";
import { useJobs } from "@/hooks/useJobs";
import { reviewBadge, useReview } from "@/hooks/useReview";
import { useAlbumsSnapshot } from "@/hooks/useAlbumsSnapshot";
import { useRememberedLibraryUrl } from "@/hooks/useRememberedLibraryUrl";
import {
  FILTERS,
  FILTERS_BY_KEY,
  jobsToReview,
  isValidFilterKey,
  type FilterKey,
} from "@/lib/filters";
import { collectionName, collectionUrl, type LibraryCollection } from "@/lib/libraryLinks";

interface NavItem {
  to?: string;
  label: string;
  icon: typeof Disc3;
  count?: number;
  badge?: { text: string; tone: "accent" | "neutral" | "warn" | "err"; title?: string; uncertain?: boolean };
  /** Show a pulsing dot to indicate background activity (e.g. a research request running). */
  activity?: { label: string };
}

export default function Sidebar() {
  const location = useLocation();
  const isLibrary = location.pathname.startsWith("/library");
  const isReview = location.pathname.startsWith("/review");

  const [params] = useSearchParams();
  const activeFilter: FilterKey | null = isValidFilterKey(params.get("filter"))
    ? (params.get("filter") as FilterKey)
    : null;
  const current: LibraryCollection | null = isLibrary ? activeFilter ?? "all" : null;

  // Library resumes the last browsing state (collection, search, open
  // album, sort, view): the URL is the source of truth for view state, and
  // the most recent /library URL is remembered in sessionStorage.
  const rememberedLibraryUrl = useRememberedLibraryUrl();
  // The collections nested under it start from that state too (search,
  // sort and view kept, the inspector closed); All albums is the reset.
  const base = isLibrary ? location.pathname + location.search : rememberedLibraryUrl;

  // Live counts for the collections
  const { data: albums = [] } = useAlbumsSnapshot();
  const { data: jobs = [] } = useJobs();
  // Requests of a Fix all batch have no job card: they come from the
  // delegated work list (kept fresh by the `work` event family).
  const { data: work } = useDelegatedWork();
  // The jobs waiting for the user: the filter's albums and its tooltip's
  // job count both come from this list, as does the jobs part of Review's.
  const toReview = useMemo(() => jobsToReview(jobs, Array.isArray(work) ? work : []), [jobs, work]);
  const needsAttention = useMemo(() => new Set(toReview.map((e) => e.albumId)), [toReview]);

  const counts = useMemo(() => {
    const out: Record<FilterKey | "all", number> = {
      "all": albums.length,
      "needs-review": 0,
      "missing-art": 0,
      "compilations": 0,
      "missing": 0,
    };
    for (const f of FILTERS) {
      out[f.key] = albums.filter((a) => f.matches(a, needsAttention)).length;
    }
    return out;
  }, [albums, needsAttention]);

  const review = useReview();
  // One neutral number: the distinct open items Review lists by default
  // (open findings of every severity, proposed changes, failed jobs), the
  // same count its header shows. Hidden at a verified zero; a source that
  // failed makes it a lower bound ("2+", or "?" with nothing known), never
  // a confident zero. Jobs in progress are activity, not decisions: the
  // titlebar's Activity control shows them, not this item.
  const badge = reviewBadge(review);
  const reviewItem: NavItem = {
    to: "/review",
    label: "Review",
    icon: Inbox,
    badge: badge ? { ...badge, tone: "neutral" } : undefined,
  };

  // Tooltip the "Jobs to review" filter chip with both perspectives — the
  // albums it lists and the jobs waiting for review (job cards, and one per
  // album for batched requests). Same name, different granularities;
  // surfacing both prevents confusion when the numbers disagree.
  const needsReviewJobCount = toReview.length;

  const collectionTooltip = (key: LibraryCollection): string => {
    const n = counts[key];
    const albumsText = `${n} album${n === 1 ? "" : "s"}`;
    if (key === "needs-review") {
      return `${albumsText} · ${needsReviewJobCount} job${needsReviewJobCount === 1 ? "" : "s"} awaiting review`;
    }
    return albumsText;
  };

  return (
    <nav className="border-r border-line bg-panel-2 px-2.5 py-3 flex flex-col gap-1 text-sm overflow-y-auto">
      <SectionLabel>Workspace</SectionLabel>
      {/* Library targets the remembered URL so the user lands back on their
          last collection / search / open album when returning from /review. */}
      <WorkspaceItem
        item={{ to: rememberedLibraryUrl, label: "Library", icon: Disc3 }}
        active={false}
        section={isLibrary}
      />
      <ul aria-label="Library collections" className="flex flex-col gap-0.5 pl-3.5">
        {(["all", ...FILTERS.map((f) => f.key)] as LibraryCollection[])
          .filter(
            // With no job waiting the jobs collection would be a permanent 0
            // (always, in a build with no model), so it is shown only when
            // it lists something.
            (c) => c !== "needs-review" || counts[c] > 0 || current === c,
          )
          .map((c) => (
            <li key={c}>
              <CollectionLink
                to={collectionUrl(c, base)}
                replace={isLibrary}
                label={collectionName(c)}
                dotColor={c === "all" ? "var(--accent)" : FILTERS_BY_KEY[c].dotColor}
                count={counts[c]}
                title={collectionTooltip(c)}
                current={current === c}
              />
            </li>
          ))}
      </ul>
      <WorkspaceItem item={reviewItem} active={isReview} />

      <div className="flex-1" />

      <NavLink
        to="/settings"
        aria-keyshortcuts=","
        className={({ isActive }) =>
          [
            "flex items-center gap-2.5 h-7 px-2.5 rounded text-fg-2 transition",
            isActive
              ? "bg-accent-soft text-accent font-medium"
              : "hover:bg-bg-3 hover:text-fg",
          ].join(" ")
        }
      >
        <SettingsIcon className="w-3.5 h-3.5" />
        <span>Settings</span>
        <kbd className="ml-auto mono text-xs text-fg-4">,</kbd>
      </NavLink>
    </nav>
  );
}

function SectionLabel({ children }: { children: React.ReactNode }) {
  return (
    <div className="mono text-[11px] uppercase tracking-[0.08em] text-fg-4 px-2.5 pt-3 pb-1.5">
      {children}
    </div>
  );
}

function WorkspaceItem({
  item,
  active,
  section = false,
}: {
  item: NavItem;
  active: boolean;
  /** The current page is in this item's section (its collections show which). */
  section?: boolean;
}) {
  const Icon = item.icon;
  const content = (
    <div className="flex items-center gap-2.5 w-full">
      <Icon className="w-3.5 h-3.5 shrink-0" />
      <span className="truncate">{item.label}</span>
      {/* Activity dot: pulses next to the label when there's background work
          (e.g. a research request is running). Sits BEFORE the badge so a job that
          completed-and-needs-review can still show its count badge alongside. */}
      {item.activity && (
        <span
          className="relative inline-flex items-center justify-center w-2.5 h-2.5 shrink-0"
          title={item.activity.label}
          aria-label={item.activity.label}
        >
          <span
            className="absolute inset-0 rounded-full bg-accent animate-[pulse_1.6s_ease-in-out_infinite] opacity-60"
            aria-hidden
          />
          <span className="relative w-1.5 h-1.5 rounded-full bg-accent" aria-hidden />
        </span>
      )}
      {item.badge && (
        <span
          data-badge
          title={item.badge.title}
          className={
            (item.activity ? "" : "ml-auto ") +
            "mono text-[10.5px] tabular-nums px-1.5 py-0.5 rounded-full font-semibold " +
            (item.badge.uncertain ? "border border-dashed border-current " : "") +
            (item.badge.tone === "accent"
              ? "bg-accent text-accent-fg"
              : item.badge.tone === "neutral"
              ? "bg-bg-3 text-fg-2"
              : item.badge.tone === "warn"
              ? "bg-warn-soft text-warn"
              : "bg-err-soft text-err")
          }
        >
          {item.badge.text}
          {item.badge.title && <span className="sr-only"> {item.badge.title.replace(/^\S+ /, "")}</span>}
        </span>
      )}
      {item.activity && !item.badge && <span className="ml-auto" />}
      {item.count !== undefined && !item.badge && (
        <span className="ml-auto mono text-xs text-fg-4 tabular-nums">{item.count}</span>
      )}
    </div>
  );

  const baseClasses =
    "flex items-center h-7 px-2.5 rounded relative transition";
  const activeClasses = active
    ? "bg-accent-soft text-accent font-medium before:content-[''] before:absolute before:-left-2.5 before:top-1 before:bottom-1 before:w-0.5 before:bg-accent before:rounded"
    : section
    ? "text-fg font-medium hover:bg-bg-3 cursor-pointer"
    : "text-fg-2 hover:bg-bg-3 hover:text-fg cursor-pointer";

  if (item.to) {
    return (
      // A plain Link: `active` is decided here, and on the Library the
      // current collection, not Library itself, is the current page.
      <Link to={item.to} aria-current={active ? "page" : undefined} className={`${baseClasses} ${activeClasses}`}>
        {content}
      </Link>
    );
  }
  return <div className={`${baseClasses} ${activeClasses}`}>{content}</div>;
}

/** One collection nested under Library: its name and its count in albums. */
function CollectionLink({
  to, replace, label, dotColor, count, current, title,
}: {
  to: string;
  replace: boolean;
  label: string;
  dotColor: string;
  count: number;
  current: boolean;
  title?: string;
}) {
  return (
    <Link
      to={to}
      replace={replace}
      title={title}
      aria-current={current ? "page" : undefined}
      className={
        "flex items-center gap-2.5 h-7 px-2.5 rounded-sm text-sm transition " +
        (current
          ? "bg-accent-soft text-accent font-medium"
          : "text-fg-3 hover:bg-bg-3 hover:text-fg")
      }
    >
      <span className="w-2 h-2 rounded-full shrink-0" style={{ background: dotColor }} aria-hidden />
      <span className="truncate">{label}</span>
      <span className="ml-auto mono text-xs text-fg-4 tabular-nums">
        {count}
        <span className="sr-only"> {count === 1 ? "album" : "albums"}</span>
      </span>
    </Link>
  );
}
