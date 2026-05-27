/**
 * Library album sort. Driven by a URL param (`?sort=<key>-<dir>`) so it's
 * shareable + survives reloads, like `?view` and `?filter`.
 */
import { displayArtist } from "./albumFields";
import type { Album } from "./types";

export type SortField = "artist" | "title" | "year" | "scanned";
export type SortDir = "asc" | "desc";
export type SortKey = `${SortField}-${SortDir}`;

export const DEFAULT_SORT: SortKey = "artist-asc";

export const SORT_OPTIONS: ReadonlyArray<{ key: SortKey; label: string }> = [
  { key: "artist-asc",   label: "Artist · A → Z" },
  { key: "artist-desc",  label: "Artist · Z → A" },
  { key: "title-asc",    label: "Title · A → Z" },
  { key: "title-desc",   label: "Title · Z → A" },
  { key: "year-desc",    label: "Year · newest" },
  { key: "year-asc",     label: "Year · oldest" },
  { key: "scanned-desc", label: "Recently scanned" },
  { key: "scanned-asc",  label: "Least recently scanned" },
];

/** Short label for the trigger button (`Artist ↑`). */
export function shortLabel(key: SortKey): string {
  const [field, dir] = key.split("-") as [SortField, SortDir];
  const arrow = dir === "asc" ? "↑" : "↓";
  const fieldLabel =
    field === "artist"  ? "Artist"
    : field === "title" ? "Title"
    : field === "year"  ? "Year"
    : "Scanned";
  return `${fieldLabel} ${arrow}`;
}

export function parseSort(raw: string | null): SortKey {
  if (!raw) return DEFAULT_SORT;
  if (SORT_OPTIONS.some((o) => o.key === raw)) return raw as SortKey;
  return DEFAULT_SORT;
}

export function sortAlbums(albums: Album[], key: SortKey): Album[] {
  const [field, dir] = key.split("-") as [SortField, SortDir];
  const mult = dir === "asc" ? 1 : -1;

  // Stable secondary key (title) avoids row-shuffling jitter when the
  // primary key is the same across many rows (compilations all
  // showing "Various Artists", say).
  const cmp = (a: Album, b: Album): number => {
    const av = primaryValue(a, field);
    const bv = primaryValue(b, field);
    const primary = compareValues(av, bv, field);
    if (primary !== 0) return primary * mult;
    const at = (a.title || a.dir_name || "").toLowerCase();
    const bt = (b.title || b.dir_name || "").toLowerCase();
    return at < bt ? -1 : at > bt ? 1 : 0;
  };
  return [...albums].sort(cmp);
}

function primaryValue(a: Album, field: SortField): string | number {
  switch (field) {
    case "artist":
      return (a.is_compilation
        ? "various artists"
        : displayArtist(a)
      ).toLowerCase();
    case "title":
      return (a.title || a.dir_name || "").toLowerCase().trim();
    case "year":
      // Treat blank years as 0; ascending puts them first, descending last.
      return parseInt(a.year || "", 10) || 0;
    case "scanned":
      return a.dir_mtime || 0;
  }
}

function compareValues(a: string | number, b: string | number, field: SortField): number {
  if (field === "year" || field === "scanned") {
    return (a as number) - (b as number);
  }
  const as = a as string;
  const bs = b as string;
  // Empty strings sort to the end of ascending order.
  if (!as && bs) return 1;
  if (as && !bs) return -1;
  return as < bs ? -1 : as > bs ? 1 : 0;
}
