import { Check, ChevronDown, X } from "lucide-react";
import * as DropdownMenu from "@radix-ui/react-dropdown-menu";
import { useSearchParams } from "react-router-dom";

import {
  DEFAULT_SORT,
  SORT_OPTIONS,
  parseSort,
  shortLabel,
  type SortKey,
} from "@/lib/albumSort";

interface FilterBarProps {
  /** Albums in the library. */
  totalCount: number;
  /** Albums in the current collection (defaults to the library's). */
  collectionCount?: number;
  /** Albums the search leaves of the collection, while searching. */
  shownCount?: number;
  libraryDir: string | null;
  scannedAt: string | null;
  /** The current collection's name; none is All albums. */
  activeFilterLabel?: string;
}

/**
 * Strip above the tile grid: the current collection and its count on the
 * left, the sort on the right.
 *
 * Sort selection is URL-driven (`?sort=<field>-<dir>`) so it survives reloads
 * and stays in sync with the rest of the navigation state (`view`, `filter`).
 */
export default function FilterBar({
  totalCount, collectionCount, shownCount, libraryDir, scannedAt, activeFilterLabel,
}: FilterBarProps) {
  const searched = shownCount !== undefined && shownCount !== (collectionCount ?? totalCount);
  const [params, setParams] = useSearchParams();
  const sortKey = parseSort(params.get("sort"));

  // Explicit close affordance for the active filter chip — was previously
  // only reachable by re-clicking the matching sidebar chip (a power-user
  // toggle with no visual cue). The × on the pill is the obvious way out.
  const clearFilter = () => {
    const next = new URLSearchParams(params);
    next.delete("filter");
    setParams(next, { replace: true });
  };

  const setSort = (next: SortKey) => {
    const p = new URLSearchParams(params);
    // Drop the param when the user reverts to the default; keeps URLs
    // clean for the common case.
    if (next === DEFAULT_SORT) p.delete("sort");
    else p.set("sort", next);
    setParams(p, { replace: true });
  };

  // The current collection and its count in albums: "All albums 120", or
  // "Missing artwork 12 of 120 albums" with a way back to All albums.
  const inCollection = collectionCount ?? totalCount;
  return (
    <div className="flex items-center gap-2.5 px-4 h-[var(--filterbar-h)] border-b border-line bg-panel-2 text-sm shrink-0 min-w-0">
      {activeFilterLabel ? (
        <span className="inline-flex items-center gap-1 h-6 pl-2 pr-1 rounded-full bg-accent-soft text-accent border border-accent/30 text-xs shrink-0">
          <span data-collection-name>{activeFilterLabel}</span>
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
      ) : (
        <span data-collection-name className="text-fg font-medium shrink-0">All albums</span>
      )}
      <span className="mono text-fg-3 shrink-0" data-collection-count>
        <b className="text-fg font-medium tabular-nums">{inCollection}</b>
        {activeFilterLabel ? ` of ${totalCount}` : ""}{" "}
        {(activeFilterLabel ? totalCount : inCollection) === 1 ? "album" : "albums"}
      </span>
      {searched && (
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

      <DropdownMenu.Root>
        <DropdownMenu.Trigger asChild>
          <button
            className="inline-flex items-center gap-1.5 h-7 px-2.5 border border-line rounded bg-panel text-fg-2 text-xs hover:bg-bg-3 transition focus:outline-none focus:ring-2 focus:ring-accent"
            aria-label={`Sort: ${shortLabel(sortKey)}`}
          >
            <span className="text-fg-4">Sort</span>
            <span>{shortLabel(sortKey)}</span>
            <ChevronDown className="w-3.5 h-3.5 text-fg-3" />
          </button>
        </DropdownMenu.Trigger>
        <DropdownMenu.Portal>
          <DropdownMenu.Content
            align="end"
            sideOffset={4}
            className="z-50 min-w-[12rem] rounded-md border border-line bg-panel shadow-popup p-1 text-sm"
          >
            {SORT_OPTIONS.map((opt) => {
              const active = opt.key === sortKey;
              return (
                <DropdownMenu.Item
                  key={opt.key}
                  onSelect={() => setSort(opt.key)}
                  className={`flex items-center gap-2 px-2 py-1.5 rounded-sm cursor-pointer outline-none data-[highlighted]:bg-bg-3 ${
                    active ? "text-accent" : "text-fg-2"
                  }`}
                >
                  <span className="w-3 h-3 grid place-items-center shrink-0">
                    {active && <Check className="w-3 h-3" />}
                  </span>
                  <span>{opt.label}</span>
                </DropdownMenu.Item>
              );
            })}
          </DropdownMenu.Content>
        </DropdownMenu.Portal>
      </DropdownMenu.Root>
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
