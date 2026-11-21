import { Check, ChevronDown, Grid, List, Search, X } from "lucide-react";
import * as DropdownMenu from "@radix-ui/react-dropdown-menu";
import { useEffect, useRef } from "react";
import { useSearchParams } from "react-router-dom";

import { useCommands } from "@/hooks/useCommands";
import {
  COLUMN_MAX,
  COLUMN_MIN,
  setTileColumns,
  useTileColumns,
} from "@/hooks/useTileColumns";
import { SORT_OPTIONS, shortLabel } from "@/lib/albumSort";
import { SORT_COMMANDS, currentSort, currentView } from "@/lib/commands";
import { takeSearchFocus, useSearchFocusRequest } from "@/lib/uiRequests";

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
 * left; the Library's own controls together on the right: search, sort,
 * Tiles/List and (tiles only, wide windows) the tile size.
 *
 * The browsing state is URL-driven (`?q=`, `?sort=<field>-<dir>`,
 * `?view=list`) so it survives reloads and stays in sync with the rest of
 * the navigation state. Sort and Tiles/List run the registry's commands
 * (lib/commands.ts), the same ones as View → Sort By and View → Tiles /
 * List in the macOS menu bar.
 *
 * Narrow windows: the collection text truncates and the folder and scan
 * time hide below `xl`; the search box shrinks; the tile size hides below
 * `lg`.
 */
export default function FilterBar({
  totalCount, collectionCount, shownCount, libraryDir, scannedAt, activeFilterLabel,
}: FilterBarProps) {
  const searched = shownCount !== undefined && shownCount !== (collectionCount ?? totalCount);
  const [params, setParams] = useSearchParams();
  const commands = useCommands();
  const sortKey = currentSort(commands.state);
  const view = currentView(commands.state);

  // Explicit close affordance for the active filter chip — was previously
  // only reachable by re-clicking the matching sidebar chip (a power-user
  // toggle with no visual cue). The × on the pill is the obvious way out.
  const clearFilter = () => {
    const next = new URLSearchParams(params);
    next.delete("filter");
    setParams(next, { replace: true });
  };

  // The current collection and its count in albums: "All albums 120", or
  // "Missing artwork 12 of 120 albums" with a way back to All albums.
  const inCollection = collectionCount ?? totalCount;
  return (
    <div className="flex items-center gap-2.5 px-4 h-[var(--filterbar-h)] border-b border-line bg-panel-2 text-sm shrink-0 min-w-0">
      <div className="flex items-center gap-2.5 min-w-0 overflow-hidden">
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
        <span className="hidden xl:contents">
          <span className="text-fg-4 mono text-xs">·</span>
          <span className="mono text-fg-3 truncate max-w-md" title={libraryDir}>
            {abbreviateHome(libraryDir)}
          </span>
        </span>
      )}
      {scannedAt && (
        <span className="hidden xl:contents">
          <span className="text-fg-4 mono text-xs">·</span>
          <span className="mono text-fg-3 whitespace-nowrap" title={scannedAt}>
            scanned {fmtRelative(scannedAt)}
          </span>
        </span>
      )}
      </div>

      <div className="flex-1" />

      <SearchField />

      <DropdownMenu.Root>
        <DropdownMenu.Trigger asChild>
          <button
            className="inline-flex shrink-0 items-center gap-1.5 h-7 px-2.5 border border-line rounded bg-panel text-fg-2 text-xs hover:bg-bg-3 transition focus:outline-none focus:ring-2 focus:ring-accent"
            aria-label={`Sort: ${shortLabel(sortKey)}`}
          >
            <span className="text-fg-4 hidden lg:inline">Sort</span>
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
                  onSelect={() => void commands.run(SORT_COMMANDS[opt.key])}
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

      {/* Tiles / List */}
      <div role="group" aria-label="Display" className="inline-flex shrink-0 border border-line rounded h-7 overflow-hidden bg-panel">
        <button
          type="button"
          className={`px-2 text-sm flex items-center ${view === "tiles" ? "bg-bg-3 text-fg" : "text-fg-3"}`}
          onClick={() => void commands.run("view.tiles")}
          aria-pressed={view === "tiles"}
          title="Tile view"
        >
          <Grid className="w-3.5 h-3.5" />
        </button>
        <button
          type="button"
          className={`px-2 text-sm flex items-center border-l border-line ${view === "list" ? "bg-bg-3 text-fg" : "text-fg-3"}`}
          onClick={() => void commands.run("view.list")}
          aria-pressed={view === "list"}
          title="List view"
        >
          <List className="w-3.5 h-3.5" />
        </button>
      </div>

      {/* Tile size — only meaningful in the tile grid; list view ignores
          the column count, so it hides rather than disables. */}
      {view === "tiles" && <TileColumnsSlider />}
    </div>
  );
}

/**
 * The Library search. Writes `?q=` for Library to filter; `/` (anywhere on
 * the page) and Find in Library (Cmd/Ctrl-F, also from Review) focus it.
 */
function SearchField() {
  const [params, setParams] = useSearchParams();
  const query = params.get("q") ?? "";
  const ref = useRef<HTMLInputElement>(null);
  const focusRequest = useSearchFocusRequest();

  useEffect(() => {
    if (!focusRequest) return;
    takeSearchFocus();
    ref.current?.focus();
    ref.current?.select();
  }, [focusRequest]);

  const onChange = (value: string) => {
    const next = new URLSearchParams(params);
    if (value) next.set("q", value);
    else next.delete("q");
    setParams(next, { replace: true });
  };

  return (
    <div className="flex items-center gap-2 bg-panel border border-line rounded px-2 h-7 w-64 min-w-28 shrink focus-within:border-accent focus-within:shadow-[0_0_0_2px_var(--accent-soft)] transition">
      <Search className="w-3.5 h-3.5 text-fg-3 shrink-0" />
      <input
        ref={ref}
        type="search"
        value={query}
        onChange={(e) => onChange(e.target.value)}
        placeholder="Filter albums and artists…"
        aria-label="Filter albums and artists"
        aria-keyshortcuts="/"
        className="bg-transparent border-0 outline-0 flex-1 min-w-0 text-sm placeholder:text-fg-4"
      />
      <kbd className="mono text-xs text-fg-3 border border-line bg-bg-2 px-1.5 py-0.5 rounded-sm shrink-0">/</kbd>
    </div>
  );
}

/**
 * 5-stop slider for choosing how many tile columns the Library grid
 * shows. Default (no preference set) uses the responsive auto-fill
 * layout — slider exposes 3..7 fixed-column overrides; a visible
 * "×" next to the number puts it back to auto.
 */
function TileColumnsSlider() {
  const columns = useTileColumns();
  // The slider visually represents 3-7. When `columns` is null (auto),
  // anchor the thumb at 5 (the typical auto-fill outcome on a wide
  // viewport) so the control isn't sitting empty.
  const value = columns ?? 5;

  return (
    <div className="hidden lg:inline-flex shrink-0 items-center gap-2 h-7 px-2 border border-line rounded bg-panel">
      <span className="mono text-[10.5px] uppercase tracking-[0.06em] text-fg-4">
        Size
      </span>
      <input
        type="range"
        min={COLUMN_MIN}
        max={COLUMN_MAX}
        step={1}
        value={value}
        onChange={(e) => setTileColumns(parseInt(e.target.value, 10))}
        className="w-20 accent-accent cursor-pointer"
        aria-label={`Tiles per row (currently ${columns === null ? "auto" : columns})`}
        title={columns === null ? "Auto — drag to set fixed column count" : `${columns} columns per row`}
      />
      {columns === null ? (
        <span className="mono text-[11px] text-fg-3 w-9 text-center">Auto</span>
      ) : (
        <button
          type="button"
          onClick={() => setTileColumns(null)}
          className="mono text-[11px] tabular-nums text-fg-3 hover:text-fg w-9 text-center cursor-pointer"
          title="Back to automatic tile size"
          aria-label={`${columns} per row. Back to automatic tile size`}
        >
          {columns} ×
        </button>
      )}
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
