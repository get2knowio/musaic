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
 * imperative `rescan()` / `cancel()` controls.
 *
 * A scan ends in exactly one of `done` (saved), `cancelled` (the user
 * stopped it) or `aborted` (the backend's sanity check refused to save: too
 * many albums vanished at once), or `error`. Only `done` reports the library
 * as freshly scanned; after `cancelled` / `aborted` nothing was saved, so the
 * list is reloaded from the cached snapshot (albums the stream showed but
 * the database never stored drop out) and a toast says why.
 *
 * One stream open per library_dir at a time; switching libraries closes
 * the previous one.
 */

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { toast } from "sonner";

import { cancelScan, listAlbums, rescanLibrary } from "@/lib/api";
import { openScanStream } from "@/lib/sse";
import { registerRescan, setScanning } from "@/lib/rescanController";
import { useAlbumsSnapshot } from "@/hooks/useAlbumsSnapshot";
import type { Album } from "@/lib/types";

export interface ScanState {
  status:
    | "idle"
    | "loading-cache"
    | "scanning"
    | "cancelling"
    | "done"
    | "cancelled"
    | "aborted"
    | "error";
  /** The scan stream is open: a scan runs (or is about to), so Cancel
   *  has something to stop. */
  canCancel: boolean;
  total: number;
  scanned: number;
  cachedCount: number;
  freshCount: number;
  markedMissingCount: number;
  message: string | null;       // last status / error
  scannedAt: string | null;     // ISO timestamp from "done"
}

const INITIAL: ScanState = {
  status: "idle",
  canCancel: false,
  total: 0,
  scanned: 0,
  cachedCount: 0,
  freshCount: 0,
  markedMissingCount: 0,
  message: null,
  scannedAt: null,
};

export interface UseScanStreamResult {
  albums: Album[];
  scan: ScanState;
  libraryDir: string | null;
  rescan: () => Promise<void>;
  /** Ask the running scan to stop (`POST /api/scan/cancel`). */
  cancel: () => Promise<void>;
  /** Counts the times the list was replaced by a newer ["albums-snapshot"]
   *  fetch (see the sync effect); Library drops its apply overrides then. */
  snapshotSyncs: number;
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
 * A new library directory was saved: the next time the
 * Library page is enabled it opens the scan stream again, so the new
 * directory is scanned at once instead of only read from the cache.
 */
export function requestScanOnNextLoad(): void {
  _initialScanDone = false;
}

/** The scan is over and the list holds what the database holds. */
export function isSettled(status: ScanState["status"]): boolean {
  return status === "done" || status === "cancelled" || status === "aborted";
}

/** A scan is in progress (the ribbon shows, Rescan is disabled). */
export function isScanBusy(status: ScanState["status"]): boolean {
  return status === "scanning" || status === "loading-cache" || status === "cancelling";
}

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
  // When each row in the list was last put there by the cached load or the
  // scan stream, so a snapshot can tell which rows it is newer than.
  const rowStamps = useRef<Map<string, number>>(new Map());

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
        const now = Date.now();
        rowStamps.current = new Map();
        for (const a of cached.albums) {
          map.set(a.id, a);
          rowStamps.current.set(a.id, now);
        }
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

      // The scan ended without saving: show what the database holds.
      const reloadFromCache = async () => {
        try {
          const cached = await listAlbums();
          if (cancelled) return;
          const map = new Map<string, Album>();
          const now = Date.now();
          rowStamps.current = new Map();
          for (const a of cached.albums) {
            map.set(a.id, a);
            rowStamps.current.set(a.id, now);
          }
          setAlbumsById(map);
          setScan((s) => ({ ...s, scannedAt: cached.scanned_at || null }));
        } catch {
          /* keep the streamed list; the next snapshot sync replaces it */
        }
      };

      // 2. Open the SSE stream to refresh + extend.
      const url = `/api/scan/stream${force ? "?force=true" : ""}`;
      setScan((s) => ({ ...s, canCancel: true }));
      const handle = openScanStream(url, (ev) => {
        if (cancelled) return;
        switch (ev.type) {
          case "start":
            setScan((s) => ({
              ...s,
              // A cancel accepted during discovery precedes `start`.
              status: s.status === "cancelling" ? "cancelling" : "scanning",
              total: ev.total,
              cachedCount: ev.cached_count,
              scanned: 0,
              freshCount: 0,
              markedMissingCount: 0,
              message: ev.force ? "Rescanning…" : "Scanning library…",
            }));
            break;

          case "album":
            rowStamps.current.set(ev.album.id, Date.now());
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

          case "marked_missing":
            // Don't delete from the local map — the row + cached
            // metadata persists, just with `missing_since` set. The
            // grid tile picks up the greyed-out treatment on the
            // next snapshot refresh (useLibraryEventsStream's
            // AlbumMarkedMissing handler invalidates ['albums-
            // snapshot'], which is what feeds this hook on reload).
            setScan((s) => ({ ...s, markedMissingCount: s.markedMissingCount + 1 }));
            break;

          case "progress":
            setScan((s) => ({ ...s, scanned: ev.scanned, total: ev.total }));
            break;

          case "cancelling":
            setScan((s) => ({ ...s, status: "cancelling", message: "Cancelling…" }));
            break;

          case "cancelled":
            setScan((s) => ({ ...s, status: "cancelled", canCancel: false, message: ev.detail }));
            toast("Scan cancelled", {
              description: "Nothing was saved; the library keeps its previous scan.",
            });
            void reloadFromCache();
            break;

          case "aborted":
            setScan((s) => ({ ...s, status: "aborted", canCancel: false, message: ev.detail }));
            toast.warning("Scan stopped without saving", { description: ev.detail });
            void reloadFromCache();
            break;

          case "done":
            setScan((s) => ({
              ...s,
              status: "done",
              canCancel: false,
              scannedAt: ev.scanned_at,
              message:
                s.freshCount === 0 && s.markedMissingCount === 0
                  ? "Library up to date"
                  : `${s.freshCount} new/updated${s.markedMissingCount ? `, ${s.markedMissingCount} missing` : ""}`,
            }));
            break;

          case "error":
            setScan((s) => ({ ...s, status: "error", canCancel: false, message: ev.detail }));
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
    // The flag is set when the scan ends (below), not here: a dev-mode
    // double mount closes the first stream at once, and the second mount
    // must still open the scan instead of reading the cache.
    return start(false);
  }, [enabled, start, refreshFromCache]);

  // The first scan of the session reached an end: later mounts only reload
  // the cached snapshot. A mount that goes away mid-scan leaves the flag
  // unset, so the next one reattaches to the running scan's stream.
  useEffect(() => {
    if (isSettled(scan.status) || scan.status === "error") _initialScanDone = true;
  }, [scan.status]);

  const rescan = useCallback(async () => {
    await rescanLibrary();
    setAlbumsById(new Map());
    rowStamps.current = new Map();
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

  const cancel = useCallback(async () => {
    try {
      const r = await cancelScan();
      if (r.outcome === "requested" || r.outcome === "already_requested") {
        // The stream's `cancelling` event says the same; this is just sooner.
        setScan((s) => (isScanBusy(s.status) ? { ...s, status: "cancelling", message: "Cancelling…" } : s));
      } else if (r.outcome === "too_late") {
        toast("The scan is already saving its result", {
          description: "It will finish in a moment.",
        });
      }
    } catch (err) {
      toast.error((err as Error).message || "Couldn't cancel the scan");
    }
  }, []);

  useEffect(() => {
    setScanning(isScanBusy(scan.status));
  }, [scan.status]);

  // Follow the bus while no scan runs (public issue: an uploaded cover
  // did not appear until Library was reopened). useLibraryEventsStream
  // invalidates ["albums-snapshot"] on every album change; this list is
  // local state, so without this it kept the old rows. Only a snapshot
  // fetched AFTER this list last settled is taken: an older cached one
  // (from before the scan that just finished) must not replace newer rows.
  const snapshot = useAlbumsSnapshot();
  const settledAt = useRef(0);
  const [snapshotSyncs, setSnapshotSyncs] = useState(0);
  useEffect(() => {
    if (isSettled(scan.status)) settledAt.current = Date.now();
  }, [scan.status, scan.scannedAt]);
  useEffect(() => {
    if (!enabled || !isSettled(scan.status)) return;
    if (!snapshot.data || snapshot.isFetching) return;
    if (snapshot.dataUpdatedAt <= settledAt.current) return;
    settledAt.current = snapshot.dataUpdatedAt;
    const map = new Map<string, Album>();
    for (const a of snapshot.data) map.set(a.id, a);
    setAlbumsById(map);
    setSnapshotSyncs((n) => n + 1);
  }, [enabled, scan.status, snapshot.data, snapshot.dataUpdatedAt, snapshot.isFetching]);

  // While a scan runs the list is not replaced: it holds rows the scan read
  // that the database does not have yet. But an album changed meanwhile (a
  // cover uploaded, tags applied) is saved to the database at once, so a row
  // whose snapshot was fetched after the list last got that row is taken
  // from the snapshot. Rows only the stream knows stay; none are added.
  const mergedAt = useRef(0);
  useEffect(() => {
    if (!enabled || !isScanBusy(scan.status)) return;
    if (!snapshot.data || snapshot.isFetching) return;
    if (snapshot.dataUpdatedAt <= mergedAt.current) return;
    mergedAt.current = snapshot.dataUpdatedAt;
    const at = snapshot.dataUpdatedAt;
    const newer = snapshot.data.filter((a) => at > (rowStamps.current.get(a.id) ?? Infinity));
    if (newer.length === 0) return;
    setAlbumsById((prev) => {
      const next = new Map(prev);
      for (const a of newer) {
        if (next.has(a.id)) next.set(a.id, a);
        rowStamps.current.set(a.id, at);
      }
      return next;
    });
  }, [enabled, scan.status, snapshot.data, snapshot.dataUpdatedAt, snapshot.isFetching]);

  return { albums, scan, libraryDir, rescan, cancel, snapshotSyncs };
}
