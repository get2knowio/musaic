/**
 * Requests a command makes of a part of the page that may not be on screen
 * yet: focus the Library search field (Find in Library from Review), open
 * a section of the album inspector (Album → Edit My Tags…), show the
 * Keyboard Shortcuts dialog. Each is a tiny store; the component that owns
 * the target takes the request when it mounts, or at once when it already
 * is mounted.
 */
import { useSyncExternalStore } from "react";

function store<T>(initial: T) {
  let value = initial;
  const listeners = new Set<() => void>();
  return {
    get: () => value,
    set(next: T) {
      if (Object.is(next, value)) return;
      value = next;
      for (const l of listeners) l();
    },
    subscribe(l: () => void) {
      listeners.add(l);
      return () => {
        listeners.delete(l);
      };
    },
  };
}

// ---- Library search -------------------------------------------------------

const searchFocus = store(0);

/** Ask the Library search field to take focus (now, or when it mounts). */
export function requestSearchFocus(): void {
  searchFocus.set(searchFocus.get() + 1);
}

/** A focus request the search field has not taken yet (0: none). */
export function useSearchFocusRequest(): number {
  return useSyncExternalStore(searchFocus.subscribe, searchFocus.get, searchFocus.get);
}

/** The field took the request. */
export function takeSearchFocus(): void {
  searchFocus.set(0);
}

// ---- Album inspector sections ---------------------------------------------

/** Inspector sections a command can open (Section ids). */
export type AlbumSectionId = "my-tags" | "audio-analysis";

const albumSection = store<AlbumSectionId | null>(null);

export function requestAlbumSection(id: AlbumSectionId): void {
  // A repeated request for the same section still scrolls to it.
  albumSection.set(null);
  albumSection.set(id);
}

/** The pending section request, read once (a section mounting after it). */
export function peekAlbumSection(): AlbumSectionId | null {
  return albumSection.get();
}

export function subscribeAlbumSection(l: () => void): () => void {
  return albumSection.subscribe(l);
}

export function useAlbumSectionRequest(): AlbumSectionId | null {
  return useSyncExternalStore(albumSection.subscribe, albumSection.get, albumSection.get);
}

export function takeAlbumSection(): void {
  albumSection.set(null);
}

// ---- Keyboard Shortcuts dialog --------------------------------------------

const shortcuts = store(false);

export function openShortcutsDialog(): void {
  shortcuts.set(true);
}

export function closeShortcutsDialog(): void {
  shortcuts.set(false);
}

export function useShortcutsDialogOpen(): boolean {
  return useSyncExternalStore(shortcuts.subscribe, shortcuts.get, shortcuts.get);
}

/** Test hook. */
export function resetUiRequests(): void {
  searchFocus.set(0);
  albumSection.set(null);
  shortcuts.set(false);
}
