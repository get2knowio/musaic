import { useCallback, useEffect, useImperativeHandle, useMemo, useRef, useState } from "react";
import { Disc3 } from "lucide-react";
import { useLocation, useNavigate, useParams, useSearchParams } from "react-router-dom";

import FilterBar from "@/components/FilterBar";
import AcoustidPrompt from "@/components/AcoustidPrompt";
import ScanRibbon from "@/components/ScanRibbon";
import TileGrid from "@/components/TileGrid";
import AlbumList from "@/components/AlbumList";
import AlbumPanel from "@/components/DetailSheet";
import {
  findAlbumTile,
  focusAlbumTileOrMain,
  focusMain,
} from "@/components/DetailSheet/panelFocus";
import { panelFillsRow } from "@/components/DetailSheet/layout";
import { albumOpenedFrom } from "@/hooks/useRememberedLibraryUrl";
import LibraryPathForm from "@/components/LibraryPathForm";
import { libraryPathHelp } from "@/lib/desktop";
import { useLibraryConfig } from "@/hooks/useLibraryConfig";
import { usePageTitle } from "@/hooks/usePageTitle";
import { useScanStream } from "@/hooks/useScanStream";
import { useJobs, useActiveJobAlbumMap } from "@/hooks/useJobs";
import {
  applyFilter,
  buildNeedsAttentionSet,
  isValidFilterKey,
  FILTERS_BY_KEY,
  type FilterKey,
} from "@/lib/filters";
import { parseSort, sortAlbums } from "@/lib/albumSort";
import type { Album } from "@/lib/types";

/** The grid keeps at least this much width beside the album panel. */
const GRID_MIN_WIDTH = 320;

/**
 * Library page — initial scan + tile grid, and the album panel docked beside
 * the grid when the URL is /library/:albumId.
 *
 * Reads the current search query from the URL (`?q=…`) so the Titlebar's
 * search input can drive filtering without a shared context. Filtering is
 * client-side over the current album list.
 */
export default function LibraryPage() {
  usePageTitle("Library");
  const config = useLibraryConfig();
  const enabled = config.data?.exists === true;
  const { albums: scannedAlbums, scan, libraryDir, cancel, snapshotSyncs } = useScanStream({ enabled });

  // Local overrides from apply flow — when the sheet emits an updated album,
  // we splice it into the scanned list so the tile reflects the change without
  // a full re-render. Keyed by album id.
  const [overrides, setOverrides] = useState<Map<string, Album>>(() => new Map());

  const albums = useMemo(() => {
    if (overrides.size === 0) return scannedAlbums;
    return scannedAlbums.map((a) => overrides.get(a.id) ?? a);
  }, [scannedAlbums, overrides]);

  // A newer server snapshot replaced the list: it already holds whatever
  // an override carried, and an override kept longer would mask later
  // changes (apply, then a cover upload).
  useEffect(() => {
    if (snapshotSyncs > 0) setOverrides((prev) => (prev.size === 0 ? prev : new Map()));
  }, [snapshotSyncs]);

  const onAlbumUpdated = useCallback((updated: Album) => {
    setOverrides((prev) => {
      const next = new Map(prev);
      next.set(updated.id, updated);
      return next;
    });
  }, []);

  const [params] = useSearchParams();
  const query = (params.get("q") ?? "").trim().toLowerCase();
  const filterKey: FilterKey | null = isValidFilterKey(params.get("filter"))
    ? (params.get("filter") as FilterKey)
    : null;
  const view: "tiles" | "list" = params.get("view") === "list" ? "list" : "tiles";

  // Albums needing attention (for the "Jobs to review" filter) — derived from
  // the live jobs cache.
  const { data: jobs = [] } = useJobs();
  const needsAttention = useMemo(() => buildNeedsAttentionSet(jobs), [jobs]);
  // album_id → active AI job (queued|running|completed-not-acted-on). Drives
  // the per-tile / per-row status overlay so the user can see at a glance
  // which albums already have a job queued and avoid double-queuing.
  const activeJobsMap = useActiveJobAlbumMap();

  // Apply filter chip first, then text search, then sort. Sort runs
  // last so it operates on the smallest list.
  const sortKey = parseSort(params.get("sort"));
  const filtered = useMemo(() => {
    const afterFilter = applyFilter(albums, filterKey, needsAttention);
    const afterSearch = filterAlbums(afterFilter, query);
    return sortAlbums(afterSearch, sortKey);
  }, [albums, query, filterKey, needsAttention, sortKey]);

  // Skeletons during a fresh scan: how many slots we expect that haven't arrived.
  const skeletonCount =
    scan.status === "scanning" && scan.total > 0
      ? Math.max(0, scan.total - albums.length)
      : 0;

  // ---- Album panel ------------------------------------------------------

  const { albumId } = useParams<{ albumId: string }>();
  const navigate = useNavigate();
  const location = useLocation();
  const openAlbum = albumId ? albums.find((a) => a.id === albumId) : undefined;
  // Effects key on the id, not the object: the album list is rebuilt on
  // every scan / snapshot update.
  const openId = openAlbum?.id;
  const panelRef = useRef<HTMLElement>(null);
  const gridRef = useRef<HTMLDivElement>(null);
  const [rowRef, rowWidth] = useElementWidth<HTMLDivElement>();
  // Too narrow for grid and panel side by side: the panel covers the row
  // and the grid stays mounted (scroll position, state) but inert.
  const panelCoversGrid = !!openId && panelFillsRow(rowWidth, GRID_MIN_WIDTH);

  // Close: back to the page the album was opened from (Health, Jobs, …),
  // else to the library with the same filters. Focus goes to the album's
  // tile, or to <main> when leaving the page or the tile is filtered out.
  const closePanel = useCallback(() => {
    if (!albumId) return;
    if (albumOpenedFrom(location.key)) {
      focusMain();
      navigate(-1);
    } else {
      // The grid may be inert under a full-width panel; inert elements
      // cannot take focus, so lift it now rather than after the re-render.
      gridRef.current?.removeAttribute("inert");
      focusAlbumTileOrMain(albumId);
      navigate(`/library${location.search}`);
    }
  }, [albumId, location.key, location.search, navigate]);

  // The panel went away some other way (browser Back) while it held focus:
  // the focus fell to <body>; put it on the album's tile instead.
  const lastOpenId = useRef<string | undefined>(undefined);
  useEffect(() => {
    const previous = lastOpenId.current;
    lastOpenId.current = openId;
    if (previous && !openId && (!document.activeElement || document.activeElement === document.body)) {
      focusAlbumTileOrMain(previous);
    }
  }, [openId]);

  // Keep the open album's tile in view: on deep-link opens, when the grid
  // reflows narrower as the panel appears, and when the grid comes back from
  // under a full-width panel. Not while covered: the grid keeps its scroll.
  const openTileListed = !!openId && filtered.some((a) => a.id === openId);
  useEffect(() => {
    if (!openId || !openTileListed || panelCoversGrid) return;
    const raf = requestAnimationFrame(() => {
      findAlbumTile(openId, gridRef.current ?? document)?.scrollIntoView?.({ block: "nearest" });
    });
    return () => cancelAnimationFrame(raf);
  }, [openId, openTileListed, panelCoversGrid]);

  // F6 moves focus between the grid and the panel (nothing to move to while
  // the panel covers the grid).
  useEffect(() => {
    if (!openId || panelCoversGrid) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "F6") return;
      const panel = panelRef.current;
      const grid = gridRef.current;
      if (!panel || !grid) return;
      const active = document.activeElement;
      // A modal dialog owns focus while it is open.
      if (active?.closest('[role="dialog"], [role="alertdialog"]')) return;
      e.preventDefault();
      if (active && panel.contains(active)) {
        const target =
          findAlbumTile(openId, grid) ??
          grid.querySelector<HTMLElement>("[data-album-id]") ??
          grid;
        target.focus();
      } else {
        panel.querySelector<HTMLElement>("[data-panel-heading]")?.focus();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [openId, panelCoversGrid]);

  // ---- Empty / loading shells -------------------------------------------

  if (config.isLoading) {
    return (
      <>
        <FilterBar totalCount={0} libraryDir={null} scannedAt={null} />
        <div className="flex-1 grid place-items-center text-fg-3 text-sm">
          Loading…
        </div>
      </>
    );
  }

  if (!config.data?.exists) {
    return <NoLibraryConfigured current={config.data?.library_dir ?? null} />;
  }

  // ---- Main view --------------------------------------------------------

  return (
    <>
      <FilterBar
        totalCount={albums.length}
        shownCount={query || filterKey ? filtered.length : undefined}
        libraryDir={libraryDir}
        scannedAt={scan.scannedAt}
        activeFilterLabel={filterKey ? FILTERS_BY_KEY[filterKey].label : undefined}
      />
      <ScanRibbon scan={scan} onCancel={() => void cancel()} />
      <AcoustidPrompt scannedAt={scan.scannedAt} />
      <div ref={rowRef} className="relative flex-1 min-h-0 flex">
        <ScrollPersistedArea ref={gridRef} inert={panelCoversGrid} storageKey={`mm.library.scroll.${view}`}>
          {albums.length === 0 && scan.status === "done" ? (
            <EmptyLibrary />
          ) : filtered.length === 0 && (query || filterKey) ? (
            <EmptyFilter query={query} filterKey={filterKey} />
          ) : view === "list" ? (
            <AlbumList
              albums={filtered}
              skeletonCount={query || filterKey ? 0 : skeletonCount}
              activeJobs={activeJobsMap}
            />
          ) : (
            <TileGrid
              albums={filtered}
              skeletonCount={query || filterKey ? 0 : skeletonCount}
              activeJobs={activeJobsMap}
            />
          )}
        </ScrollPersistedArea>
        {openAlbum && (
          <AlbumPanel
            ref={panelRef}
            album={openAlbum}
            onClose={closePanel}
            onAlbumUpdated={onAlbumUpdated}
            availableWidth={rowWidth}
            listMinWidth={GRID_MIN_WIDTH}
          />
        )}
      </div>
    </>
  );
}

// ---------- empty / placeholder views ----------

/**
 * The path entry sits here (native folder picker in the
 * desktop window, text field in a browser) instead of a link to Settings.
 * A saved, existing directory flips the config query, this page renders
 * the grid, and its scan stream opens straight away (useScanStream).
 */
function NoLibraryConfigured({ current }: { current: string | null }) {
  return (
    <div className="flex-1 grid place-items-center px-8 py-16 overflow-auto">
      <div className="max-w-lg text-center">
        <div className="w-20 h-20 rounded-full bg-bg-2 mx-auto mb-5 grid place-items-center text-fg-3">
          <Disc3 className="w-9 h-9" />
        </div>
        <h2 className="text-xl font-semibold mb-2">Configure your library</h2>
        <p className="text-md text-fg-3 leading-relaxed">
          Point Musaic at a folder containing your album directories.
          Subfolders with FLAC files are treated as albums.
        </p>
        {current && (
          <p className="text-sm text-fg-3 mt-3">
            The configured directory doesn't exist:{" "}
            <code className="mono text-fg-2 break-all">{current}</code>
          </p>
        )}
        <div className="flex flex-wrap items-center justify-center gap-2 mt-5">
          <LibraryPathForm initial={current ?? ""} inputClassName="w-80 max-w-full" />
        </div>
        <p className="text-xs text-fg-4 mt-2">{libraryPathHelp()}</p>
      </div>
    </div>
  );
}

function EmptyLibrary() {
  return (
    <div className="grid place-items-center h-full text-center px-8 py-16">
      <div className="max-w-md">
        <h3 className="text-lg font-semibold mb-2">No FLAC files found</h3>
        <p className="text-sm text-fg-3">
          The configured library directory exists but contains no folders with
          FLAC files. Check the path in Settings.
        </p>
      </div>
    </div>
  );
}

function EmptyFilter({ query, filterKey }: { query: string; filterKey: FilterKey | null }) {
  let title = "";
  if (query && filterKey) {
    title = `No ${FILTERS_BY_KEY[filterKey].label.toLowerCase()} match "${query}"`;
  } else if (filterKey) {
    title = `No albums match "${FILTERS_BY_KEY[filterKey].label}"`;
  } else {
    title = `No albums match "${query}"`;
  }
  return (
    <div className="grid place-items-center h-full text-center px-8 py-12">
      <div className="max-w-md">
        <h3 className="text-md font-medium mb-1.5">{title}</h3>
        <p className="text-sm text-fg-3">
          {filterKey
            ? "Try a different filter from the sidebar, or clear it via 'All albums'."
            : "Try a different search term, or clear the search to see everything."}
        </p>
      </div>
    </div>
  );
}

// ---------- scroll persistence ----------

/**
 * Wraps a scrollable region and saves/restores its scrollTop in
 * sessionStorage. Belt-and-suspenders: the route restructure should keep
 * LibraryPage mounted so DOM scroll is naturally preserved, but if React
 * Router ever decides to remount (e.g. dev-time hot reload), this brings
 * the user back to where they were. Also survives an explicit page reload.
 */
function ScrollPersistedArea({
  storageKey,
  children,
  ref: outerRef,
  inert = false,
}: {
  storageKey: string;
  children: React.ReactNode;
  ref?: React.Ref<HTMLDivElement>;
  /** Covered by a full-width album panel: out of the tab order and the
   *  accessibility tree, but still laid out so its scroll position holds. */
  inert?: boolean;
}) {
  const ref = useRef<HTMLDivElement>(null);
  useImperativeHandle(outerRef, () => ref.current as HTMLDivElement);

  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    // Restore saved position on mount (rAF so children have laid out first).
    const saved = sessionStorage.getItem(storageKey);
    if (saved) {
      requestAnimationFrame(() => {
        el.scrollTop = parseInt(saved, 10) || 0;
      });
    }
    let raf: number | null = null;
    const onScroll = () => {
      if (raf !== null) cancelAnimationFrame(raf);
      raf = requestAnimationFrame(() => {
        sessionStorage.setItem(storageKey, String(el.scrollTop));
      });
    };
    el.addEventListener("scroll", onScroll, { passive: true });
    return () => {
      el.removeEventListener("scroll", onScroll);
      if (raf !== null) cancelAnimationFrame(raf);
    };
  }, [storageKey]);

  return (
    // tabIndex -1: F6 can land here when no tile is rendered.
    <div ref={ref} tabIndex={-1} inert={inert} className="flex-1 min-w-0 overflow-y-auto outline-none">
      {children}
    </div>
  );
}

// ---------- layout ----------

/** A callback ref and the element's current width (null until measured). */
function useElementWidth<T extends HTMLElement>(): [(el: T | null) => void, number | null] {
  const [width, setWidth] = useState<number | null>(null);
  const observer = useRef<ResizeObserver | null>(null);
  const ref = useCallback((el: T | null) => {
    observer.current?.disconnect();
    observer.current = null;
    if (!el || typeof ResizeObserver === "undefined") return;
    setWidth(el.getBoundingClientRect().width);
    observer.current = new ResizeObserver((entries) => {
      const w = entries[0]?.contentRect.width;
      if (w !== undefined) setWidth(w);
    });
    observer.current.observe(el);
  }, []);
  return [ref, width];
}

// ---------- filtering ----------

function filterAlbums(albums: Album[], query: string): Album[] {
  if (!query) return albums;
  return albums.filter((a) => {
    const haystack = [
      a.title,
      a.artist,
      a.album_artist,
      a.dir_artist,
      a.dir_album,
      a.year,
      a.genre,
      a.dir_name,
      a.is_compilation ? "various artists compilation" : "",
    ]
      .filter(Boolean)
      .join(" ")
      .toLowerCase();
    return haystack.includes(query);
  });
}
