import { AlertTriangle, RotateCcw } from "lucide-react";

import type { SourceStatus } from "@/hooks/useReview";

/**
 * One line per data source that failed: what is missing or out of date,
 * and a Retry for that source alone. Sources that are loading or answered
 * say nothing. The rest of the page keeps showing whatever did load.
 */
export function SourceNotice({ sources, className = "" }: { sources: SourceStatus[]; className?: string }) {
  const failed = sources.filter((s) => s.state === "unavailable" || s.state === "stale");
  if (failed.length === 0) return null;
  return (
    <div role="status" className={`space-y-1.5 ${className}`}>
      {failed.map((s) => (
        <div
          key={s.label}
          data-source={s.label}
          data-source-state={s.state}
          className="rounded-md border border-warn/40 bg-warn-soft px-3 py-2 flex items-center gap-3 text-sm"
        >
          <AlertTriangle className="w-4 h-4 text-warn shrink-0" />
          <span className="flex-1 min-w-0 text-fg-2">
            {s.state === "unavailable"
              ? `Couldn't load ${s.label}. What is shown may be incomplete.`
              : `Showing ${s.label} as last loaded; refreshing them failed.`}
          </span>
          <button
            type="button"
            onClick={s.retry}
            disabled={s.retrying}
            className="h-7 px-2.5 border border-line bg-panel rounded text-xs text-fg-2 hover:bg-bg-3 hover:text-fg transition disabled:opacity-60 inline-flex items-center gap-1 shrink-0"
          >
            <RotateCcw className="w-3 h-3" />
            {s.retrying ? "Retrying…" : `Retry ${s.label}`}
          </button>
        </div>
      ))}
    </div>
  );
}
