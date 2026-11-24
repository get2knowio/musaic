/**
 * Shared filter logic — single source of truth for what each sidebar filter
 * means. Library page and Sidebar both consume from here so counts and
 * filtered results never disagree.
 */
import type { DelegatedWorkItem } from "./api";
import { batchedNeedsAttention } from "./batchedResearch";
import type { Album, Job } from "./types";

const RESEARCH_JOB_TYPES: ReadonlySet<string> = new Set(["metadata", "cover", "metadata+cover"]);

/** An old job of a type no research request matches (the jobs list names
 *  it with its stored status). It is history only: never active, never
 *  reviewed, and it blocks nothing (FR-020). Every job of a known type is
 *  a research card. */
export function isHistoryOnlyJob(j: Job): boolean {
  return !RESEARCH_JOB_TYPES.has(j.job_type);
}

export type FilterKey = "missing-art" | "compilations" | "needs-review" | "missing";

export interface FilterDef {
  key: FilterKey;
  label: string;
  /** color of the dot indicator in the sidebar */
  dotColor: string;
  /** test for an album, given an optional set of albums-needing-attention from the jobs cache */
  matches: (album: Album, needsAttention: Set<string>) => boolean;
}

// Sidebar order. The labels name what is missing (#185: "Missing" and
// "Missing art" side by side read as the same thing).
export const FILTERS: FilterDef[] = [
  {
    key: "missing-art",
    label: "Missing artwork",
    dotColor: "var(--err)",
    matches: (album) => !album.has_embedded_cover && !album.cover_file,
  },
  {
    // v15 — albums the scanner doesn't see on disk right now. The
    // row + cached metadata persist; the user can either wait for
    // the album to return (transient mount issue) or remove the
    // album from the library explicitly via the detail sheet.
    key: "missing",
    label: "Missing folders",
    dotColor: "var(--warn)",
    matches: (album) => !!album.missing_since,
  },
  {
    key: "compilations",
    label: "Compilations",
    dotColor: "var(--ok)",
    matches: (album) => album.is_compilation,
  },
  {
    // Albums with a job whose result is waiting (proposed changes, or a
    // failure). Named for the jobs so it is not read as the album count of
    // Review, which counts findings too.
    key: "needs-review",
    label: "Jobs to review",
    dotColor: "var(--warn)",
    matches: (album, needsAttention) => needsAttention.has(album.id),
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
 * action — these are the albums that "Jobs to review" lists. Requests of a
 * Fix all batch have no job card, so they come from the delegated work list
 * (`work`): a batched result waiting for review, or a failed one, counts too.
 */
export function buildNeedsAttentionSet(jobs: Job[], work?: DelegatedWorkItem[]): Set<string> {
  const out = batchedNeedsAttention(work);
  for (const j of jobs) {
    if ((j.status === "completed" || j.status === "failed") && !isHistoryOnlyJob(j)) {
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
