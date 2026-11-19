/** Words for the multi-disc-layout dialog that tests also check. */
import type { MultiDiscConversionPreview, MultiDiscProgress } from "./types";

const plural = (n: number, word: string) => `${n} ${word}${n === 1 ? "" : "s"}`;

const listed = (items: string[]) =>
  items.length <= 1
    ? items.join("")
    : `${items.slice(0, -1).join(", ")} and ${items[items.length - 1]}`;

const RECORD =
  "This cannot be undone from Musaic, but a record of every old name and tag value is kept";

/** The confirmation: what Convert will do to the user's files, plainly. */
export function confirmationText(p: MultiDiscConversionPreview): string {
  if (p.state === "folders") return gatherText(p);
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
  return `${what.charAt(0).toUpperCase()}${what.slice(1)}. ${RECORD}.`;
}

/** The confirmation for a set kept in several folders, with its numbers. */
function gatherText(p: MultiDiscConversionPreview): string {
  const n = p.track_total ?? p.tracks.length;
  const folders = p.folders.length || p.members.length;
  const name = p.target_folder?.new ?? p.target_folder?.current ?? "the first disc's folder";
  const dest = p.folders.find((f) => f.destination)?.dir_name ?? p.target_folder?.current ?? "";
  const toMove = p.tracks.filter((t) => t.folder !== dest && t.state !== "moved").length;
  const sentences: string[] = [];
  if (p.resume) {
    sentences.push(
      `This finishes the conversion that did not finish: ${plural(toMove, "more file")} will be moved into ${name}`,
    );
  } else {
    sentences.push(
      `${plural(n, "file")} from ${plural(folders, "folder")} will be moved into one folder named ${name}, renumbered 1 to ${n}, and lose their disc tags (DISCNUMBER, DISCTOTAL)`,
    );
  }
  const kept: string[] = [];
  const trashed: string[] = [];
  for (const f of p.folders) {
    for (const c of f.covers) {
      if (c.done) continue;
      if (c.action === "kept" || c.action === "moved") kept.push(`${c.name} from ${f.dir_name}`);
      else trashed.push(`${c.name} from ${f.dir_name}`);
    }
  }
  if (kept.length) sentences.push(`The cover kept is ${listed(kept)}`);
  if (trashed.length) {
    sentences.push(
      `${trashed.length === 1 ? "This cover goes" : "These covers go"} to Musaic's trash: ${listed(trashed)}`,
    );
  }
  const left = p.folders.filter((f) => !f.destination && f.other_files.length > 0);
  for (const f of left) {
    sentences.push(
      `${listed(f.other_files)} ${f.other_files.length === 1 ? "stays" : "stay"} in ${f.dir_name}, which is kept`,
    );
  }
  const others = Math.max(folders - 1, 0);
  if (!p.gather?.folded) {
    sentences.push(
      `The ${plural(folders, "album")} become one; findings you ignored on the other ${plural(others, "disc")} do not carry over`,
    );
  }
  const where = p.records_dir ? ` in ${p.records_dir}` : "";
  sentences.push(`${RECORD}${where}`);
  return `${sentences.join(". ")}.`;
}

const STEP_WORDS: Record<string, string> = {
  rename_files: "Renaming files in their folders",
  write_tags: "Writing the new track numbers",
  move_files: "Moving files into the set's folder",
};

/** One line for a conversion's progress. */
export function progressText(p: MultiDiscProgress): string {
  const what = STEP_WORDS[p.step] ?? "Converting";
  return p.total > 0 ? `${what}… ${p.done} of ${p.total}` : `${what}…`;
}
