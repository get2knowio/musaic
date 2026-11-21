/**
 * The light/dark choice. The server keeps it (the desktop window's address
 * changes on every launch, so browser storage alone forgets it);
 * localStorage is only a fast cache so the page starts in the right theme
 * before the server answers (see useUiPrefs). Dark is the default
 * (index.html starts with the `dark` class). Storage can be unavailable or
 * throw (private windows), so every access is guarded and the page simply
 * falls back to the default.
 */
import { useSyncExternalStore } from "react";

import { putUiPrefs } from "@/lib/api";

const STORAGE_KEY = "mm.theme";

export type Theme = "light" | "dark";

export function readStoredTheme(): Theme | null {
  try {
    const v = localStorage.getItem(STORAGE_KEY);
    return v === "light" || v === "dark" ? v : null;
  } catch {
    return null;
  }
}

const listeners = new Set<() => void>();

export function applyTheme(theme: Theme): void {
  document.documentElement.classList.toggle("dark", theme === "dark");
  for (const l of listeners) l();
}

/** The theme on screen now. */
export function currentTheme(): Theme {
  return document.documentElement.classList.contains("dark") ? "dark" : "light";
}

/** The theme on screen, re-rendering when it changes (View → Appearance,
 *  the server's stored choice arriving). */
export function useTheme(): Theme {
  return useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => {
        listeners.delete(l);
      };
    },
    currentTheme,
    currentTheme,
  );
}

/** Called once before the first render so the stored choice shows at once. */
export function applyStoredTheme(): void {
  const stored = readStoredTheme();
  if (stored) applyTheme(stored);
}

function cacheTheme(theme: Theme | null): void {
  try {
    if (theme === null) localStorage.removeItem(STORAGE_KEY);
    else localStorage.setItem(STORAGE_KEY, theme);
  } catch {
    /* the choice still applies for this session */
  }
}

/** The server's answer wins: show it and refresh the cache. `null`: never chosen, so the default. */
export function applyServerTheme(theme: Theme | null): void {
  applyTheme(theme ?? "dark");
  cacheTheme(theme);
}

export function setTheme(theme: Theme): void {
  applyTheme(theme);
  cacheTheme(theme);
  // A failed write leaves the choice in place for this session.
  putUiPrefs({ theme }).catch(() => {});
}
