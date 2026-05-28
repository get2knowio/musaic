import { Activity, Disc3, History, ListMusic, Music2, Settings as SettingsIcon, Sparkles } from "lucide-react";
import { useMemo } from "react";
import { NavLink, useLocation, useNavigate, useSearchParams } from "react-router-dom";

import { useJobs, useAttentionJobCount, useInFlightJobCounts } from "@/hooks/useJobs";
import { useChecks } from "@/hooks/useChecks";
import { useAlbumsSnapshot } from "@/hooks/useAlbumsSnapshot";
import { useRememberedLibraryUrl } from "@/hooks/useRememberedLibraryUrl";
import { FILTERS, buildNeedsAttentionSet, isValidFilterKey, type FilterKey } from "@/lib/filters";

interface NavItem {
  to?: string;
  label: string;
  icon: typeof Disc3;
  count?: number;
  later?: boolean;
  badge?: { text: string; tone: "accent" | "warn" | "err" };
  /** Show a pulsing dot to indicate background activity (e.g. AI job running). */
  activity?: { label: string };
}

const FOLLOWUP_WORKSPACE_ITEMS: NavItem[] = [
  { label: "Playlists", icon: ListMusic, later: true },
  { label: "Plex sync", icon: Music2, later: true },
  { label: "Scan history", icon: History, later: true },
];

export default function Sidebar() {
  const location = useLocation();
  const isLibrary = location.pathname.startsWith("/library");
  const isJobs = location.pathname.startsWith("/jobs");
  const isHealth = location.pathname.startsWith("/health");

  const navigate = useNavigate();
  const [params, setParams] = useSearchParams();
  const activeFilter: FilterKey | null = isValidFilterKey(params.get("filter"))
    ? (params.get("filter") as FilterKey)
    : null;

  // The Library nav item should bring the user back to the same filter /
  // search / open album they had when they last left the library. The URL
  // is the source of truth for view state; we just remember the most recent
  // /library URL in sessionStorage and target it from the nav link.
  const rememberedLibraryUrl = useRememberedLibraryUrl();

  // Toggle a filter chip. Clicking the currently-active filter clears it;
  // clicking from /jobs or /settings navigates BACK to the remembered library
  // URL with the filter applied — but strip any open-album segment so a
  // filter click doesn't surprise the user by re-opening a detail sheet.
  // Other view state (search query, view mode) IS preserved.
  const onFilterClick = (key: FilterKey | null) => {
    if (!isLibrary) {
      const remembered = new URL(rememberedLibraryUrl, window.location.origin);
      const next = new URLSearchParams(remembered.search);
      if (key === null) next.delete("filter");
      else next.set("filter", key);
      const qs = next.toString();
      // Drop the /:albumId segment if present — filter clicks shouldn't
      // re-open a sheet the user already closed earlier.
      navigate(`/library${qs ? `?${qs}` : ""}`);
      return;
    }
    const next = new URLSearchParams(params);
    if (key === null || key === activeFilter) {
      next.delete("filter");
    } else {
      next.set("filter", key);
    }
    setParams(next, { replace: true });
  };

  // Live counts for filter chips
  const { data: albums = [] } = useAlbumsSnapshot();
  const { data: jobs = [] } = useJobs();
  const needsAttention = useMemo(() => buildNeedsAttentionSet(jobs), [jobs]);

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

  const attentionCount = useAttentionJobCount();
  const { running: runningCount, queued: queuedCount } = useInFlightJobCounts();
  const activityLabel =
    runningCount > 0
      ? queuedCount > 0
        ? `1 running, ${queuedCount} queued`
        : "1 running"
      : queuedCount > 0
      ? `${queuedCount} queued`
      : null;
  const aiJobsItem: NavItem = {
    to: "/jobs",
    label: "AI Jobs",
    icon: Sparkles,
    badge: attentionCount > 0
      ? { text: String(attentionCount), tone: "accent" }
      : undefined,
    activity: runningCount > 0 && activityLabel
      ? { label: activityLabel }
      : undefined,
  };

  // Health item: shows total open-finding count across all checks as a badge.
  // Hidden when the registry is empty (Phase 1 has zero checks installed).
  const { data: checkSummaries = [] } = useChecks();
  const totalOpenFindings = checkSummaries.reduce((n, c) => n + c.open_count, 0);
  const healthItem: NavItem = {
    to: "/health",
    label: "Health",
    icon: Activity,
    badge: totalOpenFindings > 0
      ? { text: String(totalOpenFindings), tone: "warn" }
      : undefined,
  };

  // Tooltip the "Needs review" filter chip with both perspectives — albums
  // with completed jobs (sidebar's count) and the actual jobs in completed/
  // failed status (Jobs page's count). Same name, different granularities;
  // surfacing both prevents confusion when the numbers disagree.
  const needsReviewJobCount = jobs.filter(
    (j) => j.status === "completed" || j.status === "failed",
  ).length;

  const filterTooltip = (key: FilterKey): string | undefined => {
    if (key === "needs-review") {
      const albums = counts["needs-review"];
      return `${albums} album${albums === 1 ? "" : "s"} · ${needsReviewJobCount} job${needsReviewJobCount === 1 ? "" : "s"} awaiting review`;
    }
    return undefined;
  };

  return (
    <nav className="border-r border-line bg-panel-2 px-2.5 py-3 flex flex-col gap-1 text-sm overflow-y-auto">
      <SectionLabel>Workspace</SectionLabel>
      {/* Library item targets the remembered URL so the user lands back on
          their last filter / search / open album when returning from /jobs. */}
      <WorkspaceItem
        item={{ to: rememberedLibraryUrl, label: "Library", icon: Disc3 }}
        active={isLibrary && activeFilter === null}
      />
      <WorkspaceItem item={aiJobsItem} active={isJobs} />
      <WorkspaceItem item={healthItem} active={isHealth} />

      <SectionLabel>Filters</SectionLabel>
      <FilterChipRow
        label="All albums"
        dotColor="var(--accent)"
        count={counts.all}
        active={isLibrary && activeFilter === null}
        onClick={() => onFilterClick(null)}
      />
      {FILTERS.map((f) => (
        <FilterChipRow
          key={f.key}
          label={f.label}
          dotColor={f.dotColor}
          count={counts[f.key]}
          title={filterTooltip(f.key)}
          active={isLibrary && activeFilter === f.key}
          onClick={() => onFilterClick(f.key)}
        />
      ))}

      <div className="flex-1" />

      <NavLink
        to="/settings"
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
        <span className="ml-auto mono text-xs text-fg-4">,</span>
      </NavLink>

      {/* Roadmap items — quietly listed below Settings so they're discoverable
          without crowding the primary nav. Rendered as plain text rather than
          interactive items so users don't try to click them. */}
      <ComingSoonFooter items={FOLLOWUP_WORKSPACE_ITEMS} />
    </nav>
  );
}

function ComingSoonFooter({ items }: { items: NavItem[] }) {
  return (
    <div className="mt-3 pt-2 border-t border-line">
      <div className="mono text-[10px] uppercase tracking-[0.08em] text-fg-4 px-2.5 pb-1">
        Coming soon
      </div>
      {items.map((item) => {
        const Icon = item.icon;
        return (
          <div
            key={item.label}
            className="flex items-center gap-2.5 h-6 px-2.5 text-xs text-fg-4"
            title={`${item.label} — coming in a future milestone`}
          >
            <Icon className="w-3 h-3 shrink-0 opacity-70" />
            <span className="truncate">{item.label}</span>
          </div>
        );
      })}
    </div>
  );
}

function SectionLabel({ children }: { children: React.ReactNode }) {
  return (
    <div className="mono text-[11px] uppercase tracking-[0.08em] text-fg-4 px-2.5 pt-3 pb-1.5">
      {children}
    </div>
  );
}

function WorkspaceItem({ item, active }: { item: NavItem; active: boolean }) {
  const Icon = item.icon;
  const content = (
    <div className="flex items-center gap-2.5 w-full">
      <Icon className="w-3.5 h-3.5 shrink-0" />
      <span className="truncate">{item.label}</span>
      {/* Activity dot: pulses next to the label when there's background work
          (e.g. an AI job is running). Sits BEFORE the badge so a job that
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
          className={
            (item.activity ? "" : "ml-auto ") +
            "mono text-[10.5px] tabular-nums px-1.5 py-0.5 rounded-full font-semibold " +
            (item.badge.tone === "accent"
              ? "bg-accent text-accent-fg"
              : item.badge.tone === "warn"
              ? "bg-warn-soft text-warn"
              : "bg-err-soft text-err")
          }
        >
          {item.badge.text}
        </span>
      )}
      {item.activity && !item.badge && <span className="ml-auto" />}
      {item.count !== undefined && !item.badge && (
        <span className="ml-auto mono text-xs text-fg-4 tabular-nums">{item.count}</span>
      )}
      {item.later && (
        <span className="ml-auto mono text-[10.5px] text-fg-4 uppercase tracking-[0.06em]">v1.1</span>
      )}
    </div>
  );

  const baseClasses =
    "flex items-center h-7 px-2.5 rounded relative transition";
  const activeClasses = active
    ? "bg-accent-soft text-accent font-medium before:content-[''] before:absolute before:-left-2.5 before:top-1 before:bottom-1 before:w-0.5 before:bg-accent before:rounded"
    : item.later
    ? "opacity-55 text-fg-3 cursor-default"
    : "text-fg-2 hover:bg-bg-3 hover:text-fg cursor-pointer";

  if (item.to && !item.later) {
    return (
      <NavLink to={item.to} className={`${baseClasses} ${activeClasses}`}>
        {content}
      </NavLink>
    );
  }
  return <div className={`${baseClasses} ${activeClasses}`}>{content}</div>;
}

function FilterChipRow({
  label, dotColor, count, active, onClick, title,
}: {
  label: string;
  dotColor: string;
  count?: number;
  active: boolean;
  onClick: () => void;
  title?: string;
}) {
  return (
    <button
      onClick={onClick}
      title={title}
      className={
        "flex items-center gap-2.5 h-7 px-2.5 rounded-sm text-sm transition " +
        (active
          ? "bg-accent-soft text-accent font-medium"
          : "text-fg-3 hover:bg-bg-3 hover:text-fg")
      }
    >
      <span className="w-2 h-2 rounded-full shrink-0" style={{ background: dotColor }} />
      <span className="truncate">{label}</span>
      {count !== undefined && (
        <span className="ml-auto mono text-xs text-fg-4 tabular-nums">{count}</span>
      )}
    </button>
  );
}
