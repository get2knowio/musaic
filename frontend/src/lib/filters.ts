/**
 * Shared filter logic — single source of truth for what each sidebar filter
 * means. Library page and Sidebar both consume from here so counts and
 * filtered results never disagree.
 */
import type { Album, Job } from "./types";

export type FilterKey = "missing-art" | "compilations" | "needs-review";

export interface FilterDef {
  key: FilterKey;
  label: string;
  /** color of the dot indicator in the sidebar */
  dotColor: string;
  /** test for an album, given an optional set of albums-needing-attention from the jobs cache */
  matches: (album: Album, needsAttention: Set<string>) => boolean;
}

export const FILTERS: FilterDef[] = [
  {
    key: "needs-review",
    label: "Needs review",
    dotColor: "var(--warn)",
    matches: (album, needsAttention) => needsAttention.has(album.id),
  },
  {
    key: "missing-art",
    label: "Missing art",
    dotColor: "var(--err)",
    matches: (album) => !album.has_embedded_cover && !album.cover_file,
  },
  {
    key: "compilations",
    label: "Compilations",
    dotColor: "var(--ok)",
    matches: (album) => album.is_compilation,
  },
];

/** Map<filter key, FilterDef> for quick lookup. */
export const FILTERS_BY_KEY: Record<FilterKey, FilterDef> = Object.fromEntries(
  FILTERS.map((f) => [f.key, f]),
) as Record<FilterKey, FilterDef>;

export function isValidFilterKey(s: string | null): s is FilterKey {
  return s !== null && s in FILTERS_BY_KEY;
}

/**
 * Build the set of album ids that have a completed-or-failed job awaiting
 * action — these are the albums that "Needs review" highlights.
 */
export function buildNeedsAttentionSet(jobs: Job[]): Set<string> {
  const out = new Set<string>();
  for (const j of jobs) {
    if (j.status === "completed" || j.status === "failed") {
      out.add(j.album_id);
    }
  }
  return out;
}

/** Apply a filter to an album list. Returns the input unchanged if filter is null. */
export function applyFilter(
  albums: Album[],
  filterKey: FilterKey | null,
  needsAttention: Set<string>,
): Album[] {
  if (filterKey === null) return albums;
  const def = FILTERS_BY_KEY[filterKey];
  if (!def) return albums;
  return albums.filter((a) => def.matches(a, needsAttention));
}
