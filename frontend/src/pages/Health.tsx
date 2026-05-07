import { useMemo } from "react";
import { Link } from "react-router-dom";
import {
  Activity,
  AlertCircle,
  CheckCircle2,
  Image as ImageIcon,
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
 * Health page — index. Card per registered check; click into detail.
 *
 * Phase 1 ships with REGISTRY=[] on the backend, so this page renders an
 * empty state. Phase 2 brings the cards to life.
 */
export default function HealthPage() {
  usePageTitle("Health");
  const { data: checks = [], isLoading } = useChecks();

  // Group cards by category so similar checks cluster visually.
  const grouped = useMemo(() => {
    const map = new Map<CheckCategory, CheckSummary[]>();
    for (const c of checks) {
      const list = map.get(c.meta.category) ?? [];
      list.push(c);
      map.set(c.meta.category, list);
    }
    return Array.from(map.entries()).sort((a, b) => a[0].localeCompare(b[0]));
  }, [checks]);

  return (
    <div className="flex-1 overflow-y-auto">
      <Header totalChecks={checks.length} totalIssues={checks.reduce((n, c) => n + c.open_count, 0)} />
      {isLoading && checks.length === 0 ? (
        <div className="px-4 py-8 text-center text-fg-3 text-sm">Loading…</div>
      ) : checks.length === 0 ? (
        <Empty />
      ) : (
        <div className="px-4 py-4 space-y-6">
          {grouped.map(([category, items]) => (
            <section key={category}>
              <h2 className="mono text-[11px] uppercase tracking-[0.08em] text-fg-4 mb-2 px-1">
                {CATEGORY_LABEL[category] ?? category}
              </h2>
              <div className="grid gap-3" style={{ gridTemplateColumns: "repeat(auto-fill, minmax(360px, 1fr))" }}>
                {items.map((c) => <CheckCard key={c.meta.id} check={c} />)}
              </div>
            </section>
          ))}
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

function CheckCard({ check }: { check: CheckSummary }) {
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
      className="block border border-line bg-panel-2 rounded-md p-3 hover:border-accent hover:-translate-y-0.5 hover:shadow-card transition"
    >
      <div className="flex items-start gap-3">
        <Icon className="w-4 h-4 text-accent shrink-0 mt-0.5" />
        <div className="flex-1 min-w-0">
          <div className="flex items-center gap-2">
            <span className="font-medium truncate">{check.meta.label}</span>
            <span className="ml-auto inline-flex items-center gap-1.5">
              {check.open_count > 0 ? (
                <span className="inline-flex items-center gap-1 mono text-[11px] font-semibold text-warn px-1.5 py-0.5 rounded-full bg-warn-soft border border-warn/30 tabular-nums">
                  <AlertCircle className="w-3 h-3" />
                  {check.open_count}
                </span>
              ) : lastRun?.status === "completed" ? (
                <span className="inline-flex items-center gap-1 mono text-[11px] font-semibold text-ok">
                  <CheckCircle2 className="w-3 h-3" /> 0
                </span>
              ) : null}
            </span>
          </div>
          <p className="text-sm text-fg-3 leading-snug mt-1">{check.meta.description}</p>
          <div className="flex items-center justify-between mt-2 text-xs text-fg-4 mono">
            <span className="tabular-nums">
              {isRunning ? (
                <span className="inline-flex items-center gap-1.5 text-accent">
                  <Loader2 className="w-3 h-3 spin" /> Running…
                </span>
              ) : lastRun ? (
                <>Last run {fmtRelative(lastRun.completed_at || lastRun.started_at)}</>
              ) : (
                <span className="italic">Never run</span>
              )}
            </span>
            <button
              onClick={onRun}
              disabled={isRunning || runMutation.isPending}
              className="h-6 px-2 rounded border border-line text-fg-2 hover:bg-bg-3 hover:text-fg transition disabled:opacity-60 inline-flex items-center gap-1 normal-case tracking-normal"
            >
              <Sparkles className="w-3 h-3" />
              {runMutation.isPending ? "Starting…" : "Run now"}
            </button>
          </div>
        </div>
      </div>
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
