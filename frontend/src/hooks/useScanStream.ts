/**
 * useScanStream — orchestrates the library load:
 *
 *   1. Fetch the persistent snapshot from /api/albums (instant — no scan).
 *   2. Open /api/scan/stream and merge events into the album list as they
 *      arrive (cached entries echo, fresh ones come through, removed ones
 *      drop out, done event closes the stream).
 *   3. Tracks progress for the scan ribbon UI.
 *
 * Returns a stable map of albums (sorted), the current scan state, and
 * imperative `rescan()` / `loadCachedOnly()` controls.
 *
 * One stream open per library_dir at a time; switching libraries closes
 * the previous one.
 */

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { listAlbums, rescanLibrary } from "@/lib/api";
import { openScanStream } from "@/lib/sse";
import { registerRescan, setScanning } from "@/lib/rescanController";
import type { Album } from "@/lib/types";

export interface ScanState {
  status: "idle" | "loading-cache" | "scanning" | "done" | "error";
  total: number;
  scanned: number;
  cachedCount: number;
  freshCount: number;
  removedCount: number;
  message: string | null;       // last status / error
  scannedAt: string | null;     // ISO timestamp from "done"
}

const INITIAL: ScanState = {
  status: "idle",
  total: 0,
  scanned: 0,
  cachedCount: 0,
  freshCount: 0,
  removedCount: 0,
  message: null,
  scannedAt: null,
};

export interface UseScanStreamResult {
  albums: Album[];
  scan: ScanState;
  libraryDir: string | null;
  rescan: () => Promise<void>;
}

/**
 * Module-level guard so the SSE scan only opens ONCE per browser session.
 * Subsequent LibraryPage mounts (route changes, hot reload, sheet close) just
 * refresh the cached snapshot via /api/albums — no re-scan.
 *
 * The user can still force a fresh scan via the Rescan button (which calls
 * `rescan()` and ignores this flag).
 */
let _initialScanDone = false;

/**
 * Sort key for the "by Artist" library order. Compilations always cluster
 * under "Various Artists" regardless of what their album_artist tag says.
 * Empty/unknown sinks to the bottom under "~unknown".
 *
 * Prefers the Vorbis sort variant (`album_artist_sort`) over the display
 * tag, so "The Beatles" sorts under "B" via "Beatles, The" and Ray
 * Conniff variants ("Ray Conniff & His Orchestra", etc.) all cluster
 * together under "Conniff, Ray" — same convention Plex / Roon / Foobar
 * use. Falls back through display fields when no sort tag is set.
 */
function sortArtistKey(a: Album): string {
  if (a.is_compilation) return "various artists";
  const v = (
    a.album_artist_sort
    || a.artist_sort
    || a.album_artist
    || a.artist
    || ""
  ).trim().toLowerCase();
  return v || "~unknown";
}

export function useScanStream(opts: { enabled: boolean }): UseScanStreamResult {
  const { enabled } = opts;
  const [albumsById, setAlbumsById] = useState<Map<string, Album>>(() => new Map());
  const [libraryDir, setLibraryDir] = useState<string | null>(null);
  const [scan, setScan] = useState<ScanState>(INITIAL);
  const streamRef = useRef<{ close: () => void } | null>(null);

  // Sorted view derived once per render.
  // Sort key MUST mirror the artist string the tile/row renders. Otherwise
  // compilations sort by whatever happens to be in their album_artist tag
  // (often the series name like "Time Life Music" or a blank) and end up
  // sprinkled through the alphabet while displaying "Various Artists".
  // We make compilations cluster at "V" together, matching what the user sees.
  const albums = useMemo(() => {
    const out = Array.from(albumsById.values());
    out.sort((a, b) => {
      const aa = sortArtistKey(a);
      const ba = sortArtistKey(b);
      if (aa !== ba) return aa < ba ? -1 : 1;
      const ay = a.year || "";
      const by_ = b.year || "";
      if (ay !== by_) return ay < by_ ? -1 : 1;
      const at = (a.title || "").toLowerCase();
      const bt = (b.title || "").toLowerCase();
      return at < bt ? -1 : at > bt ? 1 : 0;
    });
    return out;
  }, [albumsById]);

  const start = useCallback((force: boolean) => {
    streamRef.current?.close();
    setScan({ ...INITIAL, status: "loading-cache" });

    let cancelled = false;

    (async () => {
      // 1. Pull cached snapshot first so the UI populates instantly.
      try {
        const cached = await listAlbums();
        if (cancelled) return;
        const map = new Map<string, Album>();
        for (const a of cached.albums) map.set(a.id, a);
        setAlbumsById(map);
        setLibraryDir(cached.library_dir);
        setScan((s) => ({
          ...s,
          scannedAt: cached.scanned_at || null,
          message: `Loaded ${cached.count} cached albums`,
        }));
      } catch (err) {
        if (cancelled) return;
        const e = err as Error & { status?: number };
        if (e.status === 409) {
          // No library configured yet — caller should show empty state.
          setLibraryDir(null);
          setScan({ ...INITIAL, status: "idle", message: null });
          return;
        }
        setScan({ ...INITIAL, status: "error", message: e.message });
        return;
      }

      // 2. Open the SSE stream to refresh + extend.
      const url = `/api/scan/stream${force ? "?force=true" : ""}`;
      const handle = openScanStream(url, (ev) => {
        if (cancelled) return;
        switch (ev.type) {
          case "start":
            setScan((s) => ({
              ...s,
              status: "scanning",
              total: ev.total,
              cachedCount: ev.cached_count,
              scanned: 0,
              freshCount: 0,
              removedCount: 0,
              message: ev.force ? "Rescanning…" : "Scanning library…",
            }));
            break;

          case "album":
            setAlbumsById((prev) => {
              const next = new Map(prev);
              next.set(ev.album.id, ev.album);
              return next;
            });
            setScan((s) => ({
              ...s,
              scanned: s.scanned + 1,
              freshCount: ev.album._status === "fresh" ? s.freshCount + 1 : s.freshCount,
            }));
            break;

          case "removed":
            setAlbumsById((prev) => {
              if (!prev.has(ev.id)) return prev;
              const next = new Map(prev);
              next.delete(ev.id);
              return next;
            });
            setScan((s) => ({ ...s, removedCount: s.removedCount + 1 }));
            break;

          case "progress":
            setScan((s) => ({ ...s, scanned: ev.scanned, total: ev.total }));
            break;

          case "done":
            setScan((s) => ({
              ...s,
              status: "done",
              scannedAt: ev.scanned_at,
              message:
                s.freshCount === 0 && s.removedCount === 0
                  ? "Library up to date"
                  : `${s.freshCount} new/updated${s.removedCount ? `, ${s.removedCount} removed` : ""}`,
            }));
            break;

          case "error":
            setScan((s) => ({ ...s, status: "error", message: ev.detail }));
            break;
        }
      });
      streamRef.current = handle;
    })();

    return () => {
      cancelled = true;
      streamRef.current?.close();
      streamRef.current = null;
    };
  }, []);

  // Just pull the cached snapshot from /api/albums — no SSE, no scan.
  // Used on every LibraryPage mount AFTER the initial session scan is done,
  // so navigating Library ⇄ AI Jobs ⇄ Settings doesn't kick off a re-scan.
  const refreshFromCache = useCallback(() => {
    let cancelled = false;
    setScan({ ...INITIAL, status: "loading-cache" });
    (async () => {
      try {
        const cached = await listAlbums();
        if (cancelled) return;
        const map = new Map<string, Album>();
        for (const a of cached.albums) map.set(a.id, a);
        setAlbumsById(map);
        setLibraryDir(cached.library_dir);
        // Mark as "done" so the UI knows the data is settled (no scan in flight).
        setScan({
          ...INITIAL,
          status: "done",
          scannedAt: cached.scanned_at || null,
          message: null,
        });
      } catch (err) {
        if (cancelled) return;
        const e = err as Error & { status?: number };
        if (e.status === 409) {
          setLibraryDir(null);
          setScan({ ...INITIAL, status: "idle", message: null });
          return;
        }
        setScan({ ...INITIAL, status: "error", message: e.message });
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  // Run the load once when enabled flips true (i.e. config exists). Subsequent
  // mounts within the same session skip the SSE and just refresh from cache.
  useEffect(() => {
    if (!enabled) return;
    if (_initialScanDone) {
      return refreshFromCache();
    }
    _initialScanDone = true;
    return start(false);
  }, [enabled, start, refreshFromCache]);

  const rescan = useCallback(async () => {
    await rescanLibrary();
    setAlbumsById(new Map());
    _initialScanDone = true;  // ensure flag stays set after manual rescan
    start(true);
  }, [start]);

  // Expose this hook's rescan to the Titlebar's button via a module-level
  // registry — keeps the SSE-driven album state local while still letting
  // out-of-tree chrome trigger a refresh. Also publishes the "is scanning"
  // flag so the button can show a spinner when used from outside this hook.
  useEffect(() => {
    registerRescan(rescan);
    return () => registerRescan(null);
  }, [rescan]);

  useEffect(() => {
    setScanning(scan.status === "scanning" || scan.status === "loading-cache");
  }, [scan.status]);

  return { albums, scan, libraryDir, rescan };
}
