/**
 * useRememberedLibraryUrl — preserves the user's library "view state" across
 * navigation to other workspace areas (AI Jobs, Settings, etc.).
 *
 * The library view state is fully encoded in the URL (filter/search/view
 * search params, plus :albumId for the open detail sheet). This hook:
 *
 *   1. Watches location changes and saves the current /library URL into
 *      sessionStorage whenever the user is on a library route.
 *   2. Exposes the most-recently-saved URL via `useRememberedLibraryUrl()`,
 *      so the Sidebar's "Library" link can restore the user to the same view
 *      they had before they navigated away.
 *
 * Scroll position is handled separately by ScrollPersistedArea in Library.tsx
 * (also via sessionStorage), so it survives the round-trip too.
 *
 * If the user has the album detail sheet open when they navigate away, we
 * still remember the /library/:id path — but the sheet only re-opens when
 * the URL is a /library/:id, so this works automatically.
 */
import { useEffect, useState } from "react";
import { useLocation } from "react-router-dom";

const STORAGE_KEY = "mm.library.lastUrl";
const DEFAULT_URL = "/library";

/**
 * Mount this once at app top-level. It silently maintains the saved URL
 * as the user navigates around. Reading the saved URL is a separate hook
 * (`useRememberedLibraryUrl`) so the Sidebar doesn't need to be re-rendered
 * on every location change.
 */
export function useTrackLibraryUrl(): void {
  const location = useLocation();
  useEffect(() => {
    if (!location.pathname.startsWith("/library")) return;
    const url = location.pathname + location.search;
    try {
      sessionStorage.setItem(STORAGE_KEY, url);
    } catch {
      // sessionStorage may be unavailable (incognito quotas, etc.) — fall back
      // silently; worst case the user just doesn't get the restore behavior.
    }
  }, [location.pathname, location.search]);
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
    if (location.pathname.startsWith("/library")) {
      setUrl(location.pathname + location.search);
    } else {
      // Re-read in case another tab / hook updated it.
      setUrl(readSaved());
    }
  }, [location.pathname, location.search]);

  return url;
}

function readSaved(): string {
  try {
    return sessionStorage.getItem(STORAGE_KEY) || DEFAULT_URL;
  } catch {
    return DEFAULT_URL;
  }
}
