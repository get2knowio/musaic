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
  Loader2,
  Music,
  Sparkles,
} from "lucide-react";
import { toast } from "sonner";

import { useChecks, useRunCheck } from "@/hooks/useChecks";
import { usePageTitle } from "@/hooks/usePageTitle";
import type { CheckCategory, CheckSummary } from "@/lib/types";

/**
 * Health page — index. Dense table-style list, one row per registered
 * check. Click a row → detail page.
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
      <Header
        totalChecks={checks.length}
        totalIssues={checks.reduce((n, c) => n + c.open_count, 0)}
      />
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

function Header({ totalChecks, totalIssues }: { totalChecks: number; totalIssues: number }) {
  return (
    <div className="flex items-center gap-3 px-4 h-[var(--filterbar-h)] border-b border-line bg-panel-2 text-sm shrink-0">
      <Activity className="w-3.5 h-3.5 text-accent" />
      <span className="font-medium">Health checks</span>
      <span className="text-fg-4 mono text-xs">·</span>
      <span className="mono text-fg-3">
        <b className="text-fg font-medium tabular-nums">{totalChecks}</b> registered
      </span>
      {totalChecks > 0 && (
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

// Grid template: icon · title · category · open count · last run · run button.
// `minmax(0, 1fr)` on the title column lets it absorb spare width without
// forcing truncation on shorter labels.
const COLS = "20px minmax(0, 1fr) 140px 80px 110px 96px";

function CheckTable({ rows }: { rows: CheckSummary[] }) {
  return (
    <div className="border border-line rounded-md overflow-hidden bg-panel-2">
      <div
        className="grid gap-3 px-3 py-2 border-b border-line text-fg-4 mono text-[10.5px] uppercase tracking-[0.06em]"
        style={{ gridTemplateColumns: COLS }}
      >
        <span />
        <span>Check</span>
        <span>Category</span>
        <span className="text-right">Open</span>
        <span>Last run</span>
        <span />
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
  const isRunning = lastRun?.status === "running";
  const runMutation = useRunCheck();

  const onRun = (e: React.MouseEvent) => {
    e.preventDefault();
    e.stopPropagation();
    runMutation.mutate(
      { checkId: check.meta.id, scope: "all" },
      {
        onError: (err) => {
          const e2 = err as Error & { status?: number };
          toast.error(e2.message || "Couldn't start check");
        },
      },
    );
  };

  return (
    <Link
      to={`/health/${encodeURIComponent(check.meta.id)}`}
      className="grid gap-3 px-3 py-2 items-center text-sm hover:bg-bg-2 transition group"
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
      <span className="text-fg-4 text-xs mono truncate" title={CATEGORY_LABEL[check.meta.category]}>
        {CATEGORY_LABEL[check.meta.category] ?? check.meta.category}
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
          <span className="text-fg-4 text-xs mono">—</span>
        )}
      </span>
      <span className="text-xs text-fg-4 mono tabular-nums truncate">
        {isRunning ? (
          <span className="inline-flex items-center gap-1.5 text-accent">
            <Loader2 className="w-3 h-3 spin" /> Running…
          </span>
        ) : lastRun ? (
          fmtRelative(lastRun.completed_at || lastRun.started_at)
        ) : (
          <span className="italic">Never run</span>
        )}
      </span>
      {/* Run-now is hover-revealed on the row to keep the resting visual
          uncluttered; focus-within keeps it accessible to keyboard users. */}
      <span className="justify-self-end opacity-0 group-hover:opacity-100 group-focus-within:opacity-100 transition-opacity">
        <button
          onClick={onRun}
          disabled={isRunning || runMutation.isPending}
          className="h-6 px-2 rounded border border-line text-fg-2 hover:bg-bg-3 hover:text-fg transition disabled:opacity-60 inline-flex items-center gap-1 normal-case tracking-normal text-xs mono"
        >
          <Sparkles className="w-3 h-3" />
          {runMutation.isPending ? "Starting…" : "Run now"}
        </button>
      </span>
    </Link>
  );
}

const CATEGORY_LABEL: Record<CheckCategory, string> = {
  metadata:   "Metadata",
  cover:      "Cover art",
  audio:      "Audio integrity",
  files:      "Files & paths",
  duplicates: "Duplicates",
  external:   "External sync",
};

const ICON_FOR_CATEGORY: Record<CheckCategory, typeof Activity> = {
  metadata:   FileWarning,
  cover:      ImageIcon,
  audio:      Music,
  files:      Files,
  duplicates: Files,
  external:   Activity,
};

function fmtRelative(iso: string | null): string {
  if (!iso) return "—";
  const t = Date.parse(iso);
  if (Number.isNaN(t)) return iso;
  const diffSec = (Date.now() - t) / 1000;
  if (diffSec < 60) return "just now";
  if (diffSec < 3600) return `${Math.floor(diffSec / 60)}m ago`;
  if (diffSec < 86400) return `${Math.floor(diffSec / 3600)}h ago`;
  return `${Math.floor(diffSec / 86400)}d ago`;
}
