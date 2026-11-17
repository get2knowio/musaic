/**
 * User-controlled column count for the Library tile grid.
 *
 * Kept by the server (the desktop window's address changes on every launch,
 * so browser storage alone forgets it); localStorage under `mm.tile.columns`
 * is only a fast cache so the grid starts at the last size before the server
 * answers (see useUiPrefs). A null value means "auto" — let the responsive
 * `auto-fill, minmax(--tile-min, 1fr)` grid pick the count from viewport
 * width (the pre-control behavior).
 *
 * Subscribers (TileGrid + Titlebar control) get instant updates via
 * useSyncExternalStore — no React Query, no event listeners, no
 * route-param plumbing.
 */
import { useSyncExternalStore } from "react";

import { putUiPrefs } from "@/lib/api";

const STORAGE_KEY = "mm.tile.columns";
export const COLUMN_MIN = 3;
export const COLUMN_MAX = 7;

const listeners = new Set<() => void>();

function readFromStorage(): number | null {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (raw === null || raw === "") return null;
    const n = parseInt(raw, 10);
    if (Number.isNaN(n)) return null;
    return Math.min(COLUMN_MAX, Math.max(COLUMN_MIN, n));
  } catch {
    return null;
  }
}

let snapshot: number | null = readFromStorage();

function emit() {
  for (const fn of listeners) fn();
}

function subscribe(fn: () => void): () => void {
  listeners.add(fn);
  return () => {
    listeners.delete(fn);
  };
}

/** The server's answer wins: show it and refresh the cache (no write back). */
export function applyServerTileColumns(value: number | null): void {
  const next =
    value === null ? null : Math.min(COLUMN_MAX, Math.max(COLUMN_MIN, value));
  if (next === snapshot) return;
  snapshot = next;
  cache(next);
  emit();
}

export function setTileColumns(value: number | null): void {
  // Clamp to allowed range; null means "back to auto".
  const next =
    value === null ? null : Math.min(COLUMN_MAX, Math.max(COLUMN_MIN, value));
  if (next === snapshot) return;
  snapshot = next;
  cache(next);
  emit();
  // A failed write leaves the choice in place for this session.
  putUiPrefs({ tile_columns: next }).catch(() => {});
}

function cache(next: number | null): void {
  try {
    if (next === null) localStorage.removeItem(STORAGE_KEY);
    else localStorage.setItem(STORAGE_KEY, String(next));
  } catch {
    /* ignore quota / privacy-mode failures — in-memory snapshot still works */
  }
}

export function useTileColumns(): number | null {
  return useSyncExternalStore(
    subscribe,
    () => snapshot,
    () => snapshot,
  );
}
