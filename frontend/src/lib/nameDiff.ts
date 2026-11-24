/**
 * Where two names differ: the text they share at the start, the part that
 * changes in each, and the text they share at the end. Used to keep the
 * changed part of a long file name on screen when the name is cut short.
 *
 * Works on code points, so a surrogate pair is never split.
 */
export interface NameDiff {
  prefix: string;
  /** The changed part of the old name ("" when text was only added). */
  oldMid: string;
  /** The changed part of the new name ("" when text was only removed). */
  newMid: string;
  suffix: string;
}

export function nameDiff(oldName: string, newName: string): NameDiff {
  const a = Array.from(oldName);
  const b = Array.from(newName);
  const max = Math.min(a.length, b.length);
  let p = 0;
  while (p < max && a[p] === b[p]) p++;
  let s = 0;
  while (s < max - p && a[a.length - 1 - s] === b[b.length - 1 - s]) s++;
  return {
    prefix: a.slice(0, p).join(""),
    oldMid: a.slice(p, a.length - s).join(""),
    newMid: b.slice(p, b.length - s).join(""),
    suffix: s > 0 ? a.slice(a.length - s).join("") : "",
  };
}
