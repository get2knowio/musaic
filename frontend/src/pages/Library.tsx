import { useCallback, useEffect, useImperativeHandle, useMemo, useRef, useState } from "react";
import { Disc3, FolderX, RotateCcw, TriangleAlert } from "lucide-react";
import { useLocation, useNavigate, useParams, useSearchParams } from "react-router-dom";

import FilterBar from "@/components/FilterBar";
import AcoustidPrompt from "@/components/AcoustidPrompt";
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
import { useChecks } from "@/hooks/useChecks";
import { reviewViewName } from "@/lib/reviewLinks";
import {
  allAlbumsUrl,
  clearFilterUrl,
  clearSearchUrl,
  collectionName,
} from "@/lib/libraryLinks";
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
  const { albums: scannedAlbums, scan, libraryDir, snapshotSyncs } = useScanStream({ enabled });

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
  const inCollection = useMemo(
    () => applyFilter(albums, filterKey, needsAttention),
    [albums, filterKey, needsAttention],
  );
  const filtered = useMemo(
    () => sortAlbums(filterAlbums(inCollection, query), sortKey),
    [inCollection, query, sortKey],
  );

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

  // Close: back to the page the album was opened from (a Review list,
  // Settings, …), which says so ("Back to Track gaps"), else to the library
  // with the same filters (a direct album link). Focus goes to the album's
  // tile, or to <main> when leaving the page or the tile is filtered out.
  const openedFrom = albumId ? albumOpenedFrom(location.key) : null;
  const { data: checks = [] } = useChecks();
  const returnLabel = openedFrom
    ? `Back to ${originName(openedFrom, (id) => checks.find((c) => c.meta.id === id)?.meta.label ?? id)}`
    : undefined;
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

  // Three different answers, three different offers: the settings could
  // not be read (retry; nothing is known about the library), no folder is
  // set (setup), or the folder set is not there (recovery: reconnect the
  // drive and try again, or choose another folder). A failed refresh with
  // a last-known answer keeps using that answer.
  if (!config.data) {
    return (
      <LibraryConfigUnavailable
        detail={config.error instanceof Error ? config.error.message : null}
        retrying={config.isFetching}
        onRetry={() => void config.refetch()}
      />
    );
  }

  if (!config.data.library_dir) {
    return <NoLibraryConfigured />;
  }

  if (!config.data.exists) {
    return (
      <LibraryFolderUnreachable
        dir={config.data.library_dir}
        retrying={config.isFetching}
        onRetry={() => void config.refetch()}
      />
    );
  }

  // ---- Main view --------------------------------------------------------

  return (
    <>
      <FilterBar
        totalCount={albums.length}
        collectionCount={inCollection.length}
        shownCount={query ? filtered.length : undefined}
        libraryDir={libraryDir}
        scannedAt={scan.scannedAt}
        activeFilterLabel={filterKey ? collectionName(filterKey) : undefined}
      />
      <AcoustidPrompt scannedAt={scan.scannedAt} />
      <div ref={rowRef} className="relative flex-1 min-h-0 flex">
        <ScrollPersistedArea ref={gridRef} inert={panelCoversGrid} storageKey={`mm.library.scroll.${view}`}>
          {albums.length === 0 && scan.status === "done" ? (
            <EmptyLibrary />
          ) : filtered.length === 0 && (query || filterKey) ? (
            <EmptyFilter
              query={query}
              filterKey={filterKey}
              here={location.pathname + location.search}
            />
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
            returnLabel={returnLabel}
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
function NoLibraryConfigured() {
  return (
    <LibraryShell icon={<Disc3 className="w-9 h-9" />} title="Configure your library">
      <p className="text-md text-fg-3 leading-relaxed">
        Point Musaic at a folder containing your album directories.
        Subfolders with FLAC files are treated as albums.
      </p>
      <div className="flex flex-wrap items-center justify-center gap-2 mt-5">
        <LibraryPathForm initial="" inputClassName="w-80 max-w-full" />
      </div>
      <p className="text-xs text-fg-4 mt-2">{libraryPathHelp()}</p>
    </LibraryShell>
  );
}

/** A folder is configured but is not there now: most often a drive that
 *  is not connected. Nothing in the library is touched; once the folder is
 *  back, Try again shows it. */
function LibraryFolderUnreachable({
  dir,
  retrying,
  onRetry,
}: {
  dir: string;
  retrying: boolean;
  onRetry: () => void;
}) {
  return (
    <LibraryShell icon={<FolderX className="w-9 h-9" />} title="Library folder not found">
      <p className="text-md text-fg-3 leading-relaxed">
        Musaic can't reach the library folder:{" "}
        <code className="mono text-fg-2 break-all">{dir}</code>
      </p>
      <p className="text-sm text-fg-3 mt-2 leading-relaxed">
        If it is on an external or network drive, connect the drive and try again. Your albums, tags and
        findings are kept while the folder is away.
      </p>
      <div className="mt-5">
        <RetryButton retrying={retrying} onRetry={onRetry} label="Try again" />
      </div>
      <p className="text-sm text-fg-3 mt-6 mb-2">Or choose a different folder:</p>
      <div className="flex flex-wrap items-center justify-center gap-2">
        <LibraryPathForm initial={dir} inputClassName="w-80 max-w-full" />
      </div>
      <p className="text-xs text-fg-4 mt-2">{libraryPathHelp()}</p>
    </LibraryShell>
  );
}

/** The library settings could not be read: nothing is known about the
 *  library, so neither setup nor recovery is offered. */
function LibraryConfigUnavailable({
  detail,
  retrying,
  onRetry,
}: {
  detail: string | null;
  retrying: boolean;
  onRetry: () => void;
}) {
  return (
    <LibraryShell icon={<TriangleAlert className="w-9 h-9" />} title="Couldn't load the library settings">
      <p className="text-md text-fg-3 leading-relaxed">
        Musaic couldn't read which folder holds your library. Nothing has been changed.
      </p>
      {detail && <p className="text-xs text-fg-4 mt-2 mono break-all">{detail}</p>}
      <div className="mt-5">
        <RetryButton retrying={retrying} onRetry={onRetry} label="Retry" />
      </div>
    </LibraryShell>
  );
}

function RetryButton({ retrying, onRetry, label }: { retrying: boolean; onRetry: () => void; label: string }) {
  return (
    <button
      type="button"
      onClick={onRetry}
      disabled={retrying}
      className="h-8 px-3 rounded bg-accent text-accent-fg text-sm font-medium hover:opacity-90 transition disabled:opacity-60 inline-flex items-center gap-1.5"
    >
      <RotateCcw className="w-3.5 h-3.5" />
      {retrying ? "Checking…" : label}
    </button>
  );
}

function LibraryShell({
  icon,
  title,
  children,
}: {
  icon: React.ReactNode;
  title: string;
  children: React.ReactNode;
}) {
  return (
    <div className="flex-1 grid place-items-center px-8 py-16 overflow-auto">
      <div className="max-w-lg text-center">
        <div className="w-20 h-20 rounded-full bg-bg-2 mx-auto mb-5 grid place-items-center text-fg-3">
          {icon}
        </div>
        <h2 className="text-xl font-semibold mb-2">{title}</h2>
        {children}
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

/**
 * Nothing in this collection, or nothing matching the search: name what
 * narrowed the list and offer the way out of each (Clear search, Clear
 * filter), plus Show all albums, the full reset. Sort and view are kept.
 */
function EmptyFilter({ query, filterKey, here }: { query: string; filterKey: FilterKey | null; here: string }) {
  const navigate = useNavigate();
  const name = filterKey ? collectionName(filterKey) : null;
  const title =
    query && name
      ? `Nothing in ${name} matches "${query}"`
      : name
        ? `No albums in ${name}`
        : `No albums match "${query}"`;
  // The open album (if any) stays open while only the search or the filter
  // changes; Show all albums closes it too.
  const go = (url: string) => navigate(url, { replace: true });
  return (
    <div className="grid place-items-center h-full text-center px-8 py-12">
      <div className="max-w-md">
        <h3 className="text-md font-medium mb-1.5">{title}</h3>
        <div className="flex flex-wrap items-center justify-center gap-2 mt-4">
          {query && (
            <button type="button" className={EMPTY_ACTION} onClick={() => go(keepAlbum(here, clearSearchUrl(here)))}>
              Clear search
            </button>
          )}
          {query && filterKey && (
            <button type="button" className={EMPTY_ACTION} onClick={() => go(keepAlbum(here, clearFilterUrl(here)))}>
              Clear filter
            </button>
          )}
          <button type="button" className={EMPTY_ACTION} onClick={() => go(allAlbumsUrl(here))}>
            Show all albums
          </button>
        </div>
      </div>
    </div>
  );
}

const EMPTY_ACTION =
  "h-8 px-3 border border-line rounded text-sm text-fg-2 hover:bg-bg-3 transition focus:outline-none focus-visible:ring-2 focus-visible:ring-accent";

/** `next` (a /library URL) on the album route `here` is on, if any. */
function keepAlbum(here: string, next: string): string {
  const path = here.split("?")[0];
  return path === "/library" ? next : next.replace(/^\/library/, path);
}

/** The name of the page an album was opened from, for "Back to …". */
function originName(from: string, checkLabel: (id: string) => string): string {
  const q = from.indexOf("?");
  const pathname = q >= 0 ? from.slice(0, q) : from;
  const search = q >= 0 ? from.slice(q) : "";
  if (pathname.startsWith("/review")) return reviewViewName(search, checkLabel) ?? "Review";
  if (pathname.startsWith("/settings")) return "Settings";
  return "previous page";
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
