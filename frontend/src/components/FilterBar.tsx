import { ChevronDown, X } from "lucide-react";
import { useSearchParams } from "react-router-dom";

interface FilterBarProps {
  totalCount: number;
  shownCount?: number;
  libraryDir: string | null;
  scannedAt: string | null;
  activeFilterLabel?: string;
}

/**
 * Strip above the tile grid: stats on the left, sort/filter chips on the right.
 *
 * Sort dropdown is a placeholder — sortable view comes when we add the list
 * view + TanStack Table.
 */
export default function FilterBar({ totalCount, shownCount, libraryDir, scannedAt, activeFilterLabel }: FilterBarProps) {
  const filtered = shownCount !== undefined && shownCount !== totalCount;
  const [params, setParams] = useSearchParams();

  // Explicit close affordance for the active filter chip — was previously
  // only reachable by re-clicking the matching sidebar chip (a power-user
  // toggle with no visual cue). The × on the pill is the obvious way out.
  const clearFilter = () => {
    const next = new URLSearchParams(params);
    next.delete("filter");
    setParams(next, { replace: true });
  };

  return (
    <div className="flex items-center gap-2.5 px-4 h-[var(--filterbar-h)] border-b border-line bg-panel-2 text-sm shrink-0">
      <span className="mono text-fg-3">
        <b className="text-fg font-medium tabular-nums">{totalCount}</b> albums
      </span>
      {activeFilterLabel && (
        <>
          <span className="text-fg-4 mono text-xs">·</span>
          <span className="inline-flex items-center gap-1 h-6 pl-2 pr-1 rounded-full bg-accent-soft text-accent border border-accent/30 text-xs">
            <span className="mono">filter:</span> {activeFilterLabel}
            <button
              type="button"
              onClick={clearFilter}
              className="w-4 h-4 grid place-items-center rounded-full hover:bg-accent/20 transition"
              title="Clear filter"
              aria-label="Clear filter"
            >
              <X className="w-3 h-3" />
            </button>
          </span>
        </>
      )}
      {filtered && (
        <>
          <span className="text-fg-4 mono text-xs">·</span>
          <span className="mono text-fg-3 tabular-nums">{shownCount} shown</span>
        </>
      )}
      {libraryDir && (
        <>
          <span className="text-fg-4 mono text-xs">·</span>
          <span className="mono text-fg-3 truncate max-w-md" title={libraryDir}>
            {abbreviateHome(libraryDir)}
          </span>
        </>
      )}
      {scannedAt && (
        <>
          <span className="text-fg-4 mono text-xs">·</span>
          <span className="mono text-fg-3" title={scannedAt}>
            scanned {fmtRelative(scannedAt)}
          </span>
        </>
      )}

      <div className="flex-1" />

      <button className="inline-flex items-center gap-1.5 h-7 px-2.5 border border-line rounded bg-panel text-fg-2 text-xs hover:bg-bg-3 transition" disabled>
        <span className="text-fg-4">Sort</span>
        <span>Artist ↑</span>
        <ChevronDown className="w-3.5 h-3.5 text-fg-3" />
      </button>
    </div>
  );
}

function abbreviateHome(path: string): string {
  // Cheap ~/ replacement; works for the common case.
  return path.replace(/^\/Users\/[^/]+/, "~");
}

function fmtRelative(iso: string): string {
  const t = Date.parse(iso);
  if (Number.isNaN(t)) return iso;
  const diffSec = (Date.now() - t) / 1000;
  if (diffSec < 60) return "just now";
  if (diffSec < 3600) return `${Math.floor(diffSec / 60)}m ago`;
  if (diffSec < 86400) return `${Math.floor(diffSec / 3600)}h ago`;
  return `${Math.floor(diffSec / 86400)}d ago`;
}
