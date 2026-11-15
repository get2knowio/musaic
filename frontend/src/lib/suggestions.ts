/**
 * What an apply does with a suggested value. The backend skips a null,
 * empty or whitespace-only suggestion: a suggestion never removes or blanks
 * a tag, whatever its source (MusicBrainz, a check fix, a model). Removing
 * a tag takes an explicit `clear` / `track_clear` request. Review screens
 * use these helpers so they never show such a value as a change.
 */

/** Whether `v` would be written (non-empty after trimming). */
export function writesValue(v: string | null | undefined): v is string {
  return typeof v === "string" && v.trim() !== "";
}

/** The entries of `s` that would be written, in their original order. */
export function writtenSuggestions<K extends string>(
  s: Partial<Record<K, string | null>>,
): Partial<Record<K, string>> {
  const out: Partial<Record<K, string>> = {};
  for (const [k, v] of Object.entries(s) as [K, string | null | undefined][]) {
    if (writesValue(v)) out[k] = v;
  }
  return out;
}
