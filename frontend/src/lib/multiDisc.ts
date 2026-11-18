/** Words for the multi-disc-layout dialog that tests also check. */
import type { MultiDiscConversionPreview } from "./types";

const plural = (n: number, word: string) => `${n} ${word}${n === 1 ? "" : "s"}`;

/** The confirmation: what Convert will do to the user's files, plainly. */
export function confirmationText(p: MultiDiscConversionPreview): string {
  const n = p.track_total ?? p.tracks.length;
  const toRename = p.tracks.filter((t) => t.new_filename && t.new_filename !== t.filename).length;
  const toTag = p.tracks.filter((t) => !t.tags_done).length;
  const parts: string[] = [];
  parts.push(
    toTag === n
      ? `${plural(n, "file")} will be renumbered 1 to ${n} and lose their disc tags (DISCNUMBER, DISCTOTAL)`
      : `${plural(toTag, "more file")} will be renumbered and lose their disc tags`,
  );
  if (toRename > 0) parts.push(`${plural(toRename, "file")} will be renamed`);
  const f = p.target_folder;
  if (f?.new && f.current !== f.new) parts.push(`the folder will be renamed to ${f.new}`);
  const what = parts.join(", ");
  return `${what.charAt(0).toUpperCase()}${what.slice(1)}. This cannot be undone from Musaic, but a record of every old name and tag value is kept.`;
}
