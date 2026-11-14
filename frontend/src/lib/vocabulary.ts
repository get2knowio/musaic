/**
 * Pure helpers for the My Tags settings panel.
 */

import type { VocabularyResponse } from "./types";

/**
 * The name to send for a rename, or null when there is nothing to send.
 *
 * Only a blank draft or a draft identical to the current name is a no-op.
 * A case-only change ("cocktail hour" → "Cocktail Hour") is a real rename:
 * the backend respells the entry and every track row carrying it.
 */
export function renameTarget(current: string, draft: string): string | null {
  const next = draft.trim();
  if (!next || next === current) return null;
  return next;
}

/** Album ids whose tag sidecar wasn't rewritten by the mutation that
 *  produced `res` (rename / merge / delete), deduplicated. */
export function sidecarsNotWritten(res: VocabularyResponse | undefined | null): string[] {
  if (!res) return [];
  const ids = [
    ...(res.rename_result?.sidecars_not_written ?? []),
    ...(res.merge_result?.sidecars_not_written ?? []),
    ...(res.delete_result?.sidecars_not_written ?? []),
  ];
  return Array.from(new Set(ids));
}

/** One line for the toast naming the albums whose sidecar wasn't written.
 *  `nameOf` maps an album id to a display name (null when unknown). */
export function sidecarsNotWrittenMessage(
  ids: string[],
  nameOf: (id: string) => string | null,
): string | null {
  if (ids.length === 0) return null;
  const names = ids.map((id) => nameOf(id) ?? id);
  const shown = names.slice(0, 3).join(", ");
  const more = names.length > 3 ? ` and ${names.length - 3} more` : "";
  const count = ids.length === 1 ? "1 album" : `${ids.length} albums`;
  return (
    `The library is updated, but the tag file in the folder of ${count} ` +
    `wasn't rewritten (folder missing or not writable): ${shown}${more}.`
  );
}
