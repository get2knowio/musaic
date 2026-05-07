import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Disc3 } from "lucide-react";
import { Link, useSearchParams } from "react-router-dom";

import FilterBar from "@/components/FilterBar";
import ScanRibbon from "@/components/ScanRibbon";
import TileGrid from "@/components/TileGrid";
import AlbumList from "@/components/AlbumList";
import DetailSheet from "@/components/DetailSheet";
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
import type { Album } from "@/lib/types";

/**
 * Library page — initial scan + tile grid.
 *
 * Reads the current search query from the URL (`?q=…`) so the Titlebar's
 * search input can drive filtering without a shared context. Filtering is
 * client-side over the current album list.
 */
export default function LibraryPage() {
  usePageTitle("Library");
  const config = useLibraryConfig();
  const enabled = config.data?.exists === true;
  const { albums: scannedAlbums, scan, libraryDir } = useScanStream({ enabled });

  // Local overrides from apply flow — when the sheet emits an updated album,
  // we splice it into the scanned list so the tile reflects the change without
  // a full re-render. Keyed by album id.
  const [overrides, setOverrides] = useState<Map<string, Album>>(() => new Map());

  const albums = useMemo(() => {
    if (overrides.size === 0) return scannedAlbums;
    return scannedAlbums.map((a) => overrides.get(a.id) ?? a);
  }, [scannedAlbums, overrides]);

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

  // Albums needing attention (for the "Needs review" filter) — derived from
  // the live jobs cache.
  const { data: jobs = [] } = useJobs();
  const needsAttention = useMemo(() => buildNeedsAttentionSet(jobs), [jobs]);
  // album_id → active AI job (queued|running|completed-not-acted-on). Drives
  // the per-tile / per-row status overlay so the user can see at a glance
  // which albums already have a job queued and avoid double-queuing.
  const activeJobsMap = useActiveJobAlbumMap();

  // Apply filter chip first, then text search. Both can stack.
  const filtered = useMemo(() => {
    const afterFilter = applyFilter(albums, filterKey, needsAttention);
    return filterAlbums(afterFilter, query);
  }, [albums, query, filterKey, needsAttention]);

  // Skeletons during a fresh scan: how many slots we expect that haven't arrived.
  const skeletonCount =
    scan.status === "scanning" && scan.total > 0
      ? Math.max(0, scan.total - albums.length)
      : 0;

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
    return <NoLibraryConfigured />;
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
      <ScanRibbon scan={scan} />
      <ScrollPersistedArea storageKey={`mm.library.scroll.${view}`}>
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
      <DetailSheet albums={albums} onAlbumUpdated={onAlbumUpdated} />
    </>
  );
}

// ---------- empty / placeholder views ----------

function NoLibraryConfigured() {
  return (
    <div className="flex-1 grid place-items-center px-8 py-16 overflow-auto">
      <div className="max-w-lg text-center">
        <div className="w-20 h-20 rounded-full bg-bg-2 mx-auto mb-5 grid place-items-center text-fg-3">
          <Disc3 className="w-9 h-9" />
        </div>
        <h2 className="text-xl font-semibold mb-2">Configure your library</h2>
        <p className="text-md text-fg-3 leading-relaxed">
          Point Media Manager at a folder containing your album directories.
          Subfolders with FLAC files are treated as albums.
        </p>
        <Link
          to="/settings"
          className="inline-flex items-center justify-center gap-1.5 mt-5 h-9 px-4 rounded bg-accent text-accent-fg text-sm font-medium hover:opacity-90 transition"
        >
          Set library directory in Settings
        </Link>
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
}: {
  storageKey: string;
  children: React.ReactNode;
}) {
  const ref = useRef<HTMLDivElement>(null);

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
    <div ref={ref} className="flex-1 overflow-y-auto">
      {children}
    </div>
  );
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
