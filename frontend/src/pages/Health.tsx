import { useMemo } from "react";
import { Link } from "react-router-dom";
import {
  Activity,
  AlertCircle,
  CheckCircle2,
  Image as ImageIcon,
  Info,
  FileWarning,
  Files,
  Music,
} from "lucide-react";

import { useChecks } from "@/hooks/useChecks";
import { usePageTitle } from "@/hooks/usePageTitle";
import type { CheckCategory, CheckSummary } from "@/lib/types";

/**
 * Health page — index. Dense table-style list, one row per check with its
 * open-finding count. Click a row → detail page. Checks run on their own in
 * the background (the titlebar shows progress), so there is nothing to run
 * from here.
 *
 * The previous card-grid layout left visible dead space when a category
 * had only 1-2 checks (cover art, duplicates, files), and the right-rail
 * count chip + run button kept squeezing the title until it truncated.
 * The list flattens that into a single sortable column set.
 */
export default function HealthPage() {
  usePageTitle("Health");
  const { data: checks = [], isLoading } = useChecks();

  // Sort by category (alpha) then label so similar checks still cluster
  // visually even without category headers.
  const rows = useMemo(() => {
    return [...checks].sort((a, b) => {
      const catCmp = a.meta.category.localeCompare(b.meta.category);
      if (catCmp !== 0) return catCmp;
      return a.meta.label.localeCompare(b.meta.label);
    });
  }, [checks]);

  return (
    <div className="flex-1 overflow-y-auto">
      <Header checks={checks} />
      {isLoading && checks.length === 0 ? (
        <div className="px-4 py-8 text-center text-fg-3 text-sm">Loading…</div>
      ) : checks.length === 0 ? (
        <Empty />
      ) : (
        <div className="px-4 py-4">
          <CheckTable rows={rows} />
        </div>
      )}
    </div>
  );
}

function Header({ checks }: { checks: CheckSummary[] }) {
  const totalIssues = checks.reduce((n, c) => n + c.open_count, 0);
  return (
    <div className="flex items-center gap-3 px-4 h-[var(--filterbar-h)] border-b border-line bg-panel-2 text-sm shrink-0">
      <Activity className="w-3.5 h-3.5 text-accent" />
      <span className="font-medium">Health checks</span>
      {checks.length > 0 && (
        <>
          <span className="text-fg-4 mono text-xs">·</span>
          <span className="mono text-fg-3">
            <b className={`font-medium tabular-nums ${totalIssues > 0 ? "text-warn" : "text-ok"}`}>
              {totalIssues}
            </b>{" "}
            open finding{totalIssues === 1 ? "" : "s"}
          </span>
        </>
      )}
    </div>
  );
}

function Empty() {
  return (
    <div className="px-8 py-16 text-center">
      <Activity className="w-9 h-9 mx-auto mb-3 text-fg-4" />
      <h3 className="text-md font-medium mb-1.5">No checks installed yet</h3>
      <p className="text-sm text-fg-3 max-w-md mx-auto">
        Maintenance checks land in upcoming versions. They'll surface library issues
        like missing tags, no cover art, track-number gaps, FLAC integrity, and more.
      </p>
    </div>
  );
}

/* -------------------- Table -------------------- */

// Grid template: icon · title · open count. `minmax(0, 1fr)` on the title
// column lets it absorb spare width without forcing truncation on shorter
// labels.
const COLS = "20px minmax(0, 1fr) 80px";

function CheckTable({ rows }: { rows: CheckSummary[] }) {
  return (
    <div className="border border-line rounded-md overflow-hidden bg-panel-2">
      <div
        className="grid gap-3 px-3 py-2 border-b border-line text-fg-4 mono text-[10.5px] uppercase tracking-[0.06em]"
        style={{ gridTemplateColumns: COLS }}
      >
        <span />
        <span>Check</span>
        <span className="text-right">Open</span>
      </div>
      <div className="divide-y divide-line">
        {rows.map((c) => <CheckRow key={c.meta.id} check={c} />)}
      </div>
    </div>
  );
}

function CheckRow({ check }: { check: CheckSummary }) {
  const Icon = ICON_FOR_CATEGORY[check.meta.category] ?? Activity;
  const lastRun = check.last_run;
  return (
    <Link
      to={`/health/${encodeURIComponent(check.meta.id)}`}
      className="grid gap-3 px-3 py-2 items-center text-sm hover:bg-bg-2 transition"
      style={{ gridTemplateColumns: COLS }}
    >
      <Icon className="w-3.5 h-3.5 text-accent" />
      <span className="flex items-center gap-1.5 min-w-0">
        <span className="font-medium truncate" title={check.meta.label}>
          {check.meta.label}
        </span>
        <span
          tabIndex={0}
          role="img"
          aria-label={check.meta.description}
          title={check.meta.description}
          onClick={(e) => { e.preventDefault(); e.stopPropagation(); }}
          className="text-fg-4 hover:text-fg-2 shrink-0 cursor-help"
        >
          <Info className="w-3 h-3" />
        </span>
      </span>
      <span className="text-right">
        {check.open_count > 0 ? (
          <span className="inline-flex items-center gap-1 mono text-[11px] font-semibold text-warn px-1.5 py-0.5 rounded-full bg-warn-soft border border-warn/30 tabular-nums">
            <AlertCircle className="w-3 h-3" />
            {check.open_count}
          </span>
        ) : lastRun?.status === "completed" ? (
          <span className="inline-flex items-center gap-1 mono text-[11px] font-semibold text-ok">
            <CheckCircle2 className="w-3 h-3" /> 0
          </span>
        ) : (
          <span className="text-fg-4 text-xs mono" title={lastRun ? undefined : "Not checked yet"}>—</span>
        )}
      </span>
    </Link>
  );
}

const ICON_FOR_CATEGORY: Record<CheckCategory, typeof Activity> = {
  metadata:   FileWarning,
  cover:      ImageIcon,
  audio:      Music,
  files:      Files,
  duplicates: Files,
  external:   Activity,
};
