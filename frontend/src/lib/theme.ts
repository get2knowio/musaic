/**
 * The light/dark choice, kept in localStorage so it survives a reload.
 * Dark is the default (index.html starts with the `dark` class). Storage can
 * be unavailable or throw (private windows), so every access is guarded and
 * the page simply falls back to the default.
 */
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

export function applyTheme(theme: Theme): void {
  document.documentElement.classList.toggle("dark", theme === "dark");
}

/** Called once before the first render so the stored choice shows at once. */
export function applyStoredTheme(): void {
  const stored = readStoredTheme();
  if (stored) applyTheme(stored);
}

export function setTheme(theme: Theme): void {
  applyTheme(theme);
  try {
    localStorage.setItem(STORAGE_KEY, theme);
  } catch {
    /* the choice still applies for this session */
  }
}
