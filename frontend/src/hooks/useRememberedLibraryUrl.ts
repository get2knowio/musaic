/**
 * useRememberedLibraryUrl — preserves the user's library "view state" across
 * navigation to other workspace areas (AI Jobs, Settings, etc.).
 *
 * The library view state is fully encoded in the URL (filter/search/view
 * search params, plus :albumId for the open album panel). This module:
 *
 *   1. Watches location changes and saves the current /library URL into
 *      sessionStorage whenever the user is on a library route.
 *   2. Exposes the most-recently-saved URL via `useRememberedLibraryUrl()`,
 *      so the Sidebar's "Library" link can restore the user to the same view
 *      they had before they navigated away.
 *   3. Notes when an album was opened from another page (Health, Jobs, a
 *      toast), so closing the album panel can go back there instead of to
 *      the library (`albumOpenedFrom`).
 *
 * Scroll position is handled separately by ScrollPersistedArea in Library.tsx
 * (also via sessionStorage), so it survives the round-trip too.
 */
import { useEffect, useRef, useState } from "react";
import { useLocation, useNavigationType } from "react-router-dom";

const STORAGE_KEY = "mm.library.lastUrl";
const OPENED_FROM_KEY = "mm.library.openedFrom";
const OPENED_FROM_LIMIT = 50;
const DEFAULT_URL = "/library";

function isLibraryPath(pathname: string): boolean {
  return pathname === "/library" || pathname.startsWith("/library/");
}

function isAlbumPath(pathname: string): boolean {
  return /^\/library\/[^/]+\/?$/.test(pathname);
}

/**
 * Mount this once at app top-level. It silently maintains the saved URL
 * as the user navigates around. Reading the saved URL is a separate hook
 * (`useRememberedLibraryUrl`) so the Sidebar doesn't need to be re-rendered
 * on every location change.
 */
export function useTrackLibraryUrl(): void {
  const location = useLocation();
  const navigationType = useNavigationType();
  const previous = useRef<{ pathname: string; key: string } | null>(null);

  useEffect(() => {
    const before = previous.current;
    previous.current = { pathname: location.pathname, key: location.key };
    if (!isLibraryPath(location.pathname)) return;
    const url = location.pathname + location.search;

    // History entries are identified by `location.key`, which survives a
    // reload, so the note does too. Only new entries are classified: a
    // back/forward visit keeps whatever its entry was given when created.
    if (isAlbumPath(location.pathname) && before && before.key !== location.key) {
      if (navigationType === "PUSH" && !isLibraryPath(before.pathname)) {
        // The Sidebar's Library link restoring the remembered view is a
        // return to the library, not an album opened from elsewhere.
        if (url !== readSaved()) recordOpenedFrom(location.key, before.pathname);
      } else if (navigationType === "REPLACE" && isAlbumPath(before.pathname)) {
        // Swapping albums replaces the entry; keep where it came from.
        const from = albumOpenedFrom(before.key);
        if (from) recordOpenedFrom(location.key, from);
      }
    }

    try {
      sessionStorage.setItem(STORAGE_KEY, url);
    } catch {
      // sessionStorage may be unavailable (incognito quotas, etc.) — fall back
      // silently; worst case the user just doesn't get the restore behavior.
    }
  }, [location.key, location.pathname, location.search, navigationType]);
}

/**
 * Returns the most-recently-visited library URL (path + search params), or
 * "/library" if nothing has been saved yet. Reads sessionStorage on mount
 * and refreshes whenever the user navigates AWAY from /library (so the
 * sidebar reflects the latest snapshot to return to).
 */
export function useRememberedLibraryUrl(): string {
  const location = useLocation();
  const [url, setUrl] = useState<string>(() => readSaved());

  // When the user is currently ON the library, the "remembered URL" is just
  // the current URL — keep it in sync so clicking Library doesn't bounce
  // them somewhere stale. When they navigate AWAY, freeze the last value.
  useEffect(() => {
    if (isLibraryPath(location.pathname)) {
      setUrl(location.pathname + location.search);
    } else {
      // Re-read in case another tab / hook updated it.
      setUrl(readSaved());
    }
  }, [location.pathname, location.search]);

  return url;
}

/**
 * The pathname of the page an album route's history entry was opened from,
 * when that was not the library; null otherwise.
 */
export function albumOpenedFrom(locationKey: string): string | null {
  return readOpenedFromMap()[locationKey] ?? null;
}

function recordOpenedFrom(locationKey: string, pathname: string): void {
  const map = readOpenedFromMap();
  delete map[locationKey];
  map[locationKey] = pathname;
  const keys = Object.keys(map);
  for (const k of keys.slice(0, Math.max(0, keys.length - OPENED_FROM_LIMIT))) delete map[k];
  try {
    sessionStorage.setItem(OPENED_FROM_KEY, JSON.stringify(map));
  } catch {
    // Without storage, closing falls back to returning to the library.
  }
}

function readOpenedFromMap(): Record<string, string> {
  try {
    const raw = sessionStorage.getItem(OPENED_FROM_KEY);
    const parsed: unknown = raw ? JSON.parse(raw) : null;
    if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
      return parsed as Record<string, string>;
    }
  } catch {
    // Unavailable or corrupt: treat as empty.
  }
  return {};
}

function readSaved(): string {
  try {
    return sessionStorage.getItem(STORAGE_KEY) || DEFAULT_URL;
  } catch {
    return DEFAULT_URL;
  }
}
