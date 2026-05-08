/**
 * User-controlled column count for the Library tile grid.
 *
 * Stored in localStorage under `mm.tile.columns`. A null value means
 * "auto" — let the responsive `auto-fill, minmax(--tile-min, 1fr)`
 * grid pick the count from viewport width (the pre-control behavior).
 *
 * Subscribers (TileGrid + Titlebar control) get instant updates via
 * useSyncExternalStore — no React Query, no event listeners, no
 * route-param plumbing.
 */
import { useSyncExternalStore } from "react";

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

export function setTileColumns(value: number | null): void {
  // Clamp to allowed range; null means "back to auto".
  const next =
    value === null ? null : Math.min(COLUMN_MAX, Math.max(COLUMN_MIN, value));
  if (next === snapshot) return;
  snapshot = next;
  try {
    if (next === null) localStorage.removeItem(STORAGE_KEY);
    else localStorage.setItem(STORAGE_KEY, String(next));
  } catch {
    /* ignore quota / privacy-mode failures — in-memory snapshot still works */
  }
  emit();
}

export function useTileColumns(): number | null {
  return useSyncExternalStore(
    subscribe,
    () => snapshot,
    () => snapshot,
  );
}
