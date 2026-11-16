/**
 * What an apply does with a suggested value, and when a proposed value is a
 * change. The backend skips a null, empty or whitespace-only suggestion: a
 * suggestion never removes or blanks a tag, whatever its source
 * (MusicBrainz, a check fix, a model). Removing a tag takes an explicit
 * `clear` / `track_clear` request. Review screens use these helpers so they
 * never show such a value as a change.
 *
 * The album panel shows every proposed correction — from the resolver or
 * from a job's result — as rows built here, so both sources follow one
 * comparison rule (`isChange`) and one payload shape (`correctionsPayload`).
 */

import type { Album, ApplyPayload, Track } from "./types";

/** Whether `v` would be written (non-empty after trimming). */
export function writesValue(v: string | null | undefined): v is string {
  return typeof v === "string" && v.trim() !== "";
}

/**
 * Whether writing `proposed` over `current` is a correction worth listing.
 *
 * - A proposal that would not be written (empty or whitespace-only) is no
 *   change.
 * - Otherwise the two are compared ignoring case and runs of whitespace:
 *   the rule of the "Tags don't match canonical" check
 *   (`musaic-checks/src/checks/tags_out_of_sync.rs`, `normalize`), so the
 *   panel lists what Health reports. Case-only differences are left out on
 *   purpose: the album's stored title prefers the folder name over the
 *   ALBUM tag (`musaic-scan`), so a case-only title fix would stay listed
 *   after it was applied.
 */
export function isChange(
  current: string | null | undefined,
  proposed: string | null | undefined,
): boolean {
  if (!writesValue(proposed)) return false;
  return normalizeTagValue(proposed) !== normalizeTagValue(current ?? "");
}

/** Lower case, whitespace runs collapsed, trimmed. */
export function normalizeTagValue(s: string): string {
  return s.toLowerCase().split(/\s+/).filter(Boolean).join(" ");
}

/** Album-level fields a correction can set (keys of the apply's `suggestions`). */
export const CORRECTION_ALBUM_FIELDS = [
  "title",
  "album_artist",
  "artist",
  "year",
  "genre",
  "album_artist_sort",
] as const;
export type CorrectionAlbumField = (typeof CORRECTION_ALBUM_FIELDS)[number];

/** Per-track fields a correction can set, in display order. */
export const CORRECTION_TRACK_FIELDS = ["tracknumber", "title", "artist"] as const;
export type CorrectionTrackField = (typeof CORRECTION_TRACK_FIELDS)[number];

/** What a source proposes, before it is compared with the album. */
export interface Proposal {
  album: Partial<Record<CorrectionAlbumField, string | null>>;
  tracks: { filename: string; fields: Partial<Record<CorrectionTrackField, string | null>> }[];
  /** A remote cover the apply downloads, and the same-origin route that
   *  previews it (the desktop CSP blocks remote images). */
  cover: { url: string; previewSrc: string } | null;
}

export type CorrectionRow =
  | {
      kind: "album";
      key: string;
      field: CorrectionAlbumField;
      current: string;
      proposed: string;
      /** Ticked until the user unticks it. */
      defaultSelected: boolean;
    }
  | {
      kind: "track";
      key: string;
      filename: string;
      field: CorrectionTrackField;
      current: string;
      proposed: string;
      defaultSelected: boolean;
    }
  | {
      kind: "cover";
      key: typeof COVER_ROW_KEY;
      url: string;
      previewSrc: string;
      defaultSelected: boolean;
    };

export const COVER_ROW_KEY = "cover";

/**
 * Genre starts unticked. MusicBrainz's genre is its community's top-voted
 * tag (the resolver gives it 0.55 confidence, against 0.95 for the other
 * fields), often spelled differently from a library's own genres, and
 * writing it replaces the user's choice on every file.
 */
function selectedByDefault(field: CorrectionAlbumField): boolean {
  return field !== "genre";
}

/**
 * The rows a proposal shows for this album: one per field whose proposed
 * value `isChange` from the current one, then the cover. A track the
 * listing does not have (renamed or gone since the proposal was made) has
 * no rows: there is no file to write.
 */
export function correctionRows(
  proposal: Proposal,
  album: Album,
  tracks: readonly Track[],
): CorrectionRow[] {
  const rows: CorrectionRow[] = [];
  for (const field of CORRECTION_ALBUM_FIELDS) {
    const proposed = proposal.album[field];
    const current = album[field] ?? "";
    if (!isChange(current, proposed)) continue;
    rows.push({
      kind: "album",
      key: `album:${field}`,
      field,
      current: current.trim(),
      proposed: proposed!.trim(),
      defaultSelected: selectedByDefault(field),
    });
  }
  const byName = new Map(tracks.map((t) => [t.filename, t]));
  for (const entry of proposal.tracks) {
    const track = byName.get(entry.filename);
    if (!track) continue;
    for (const field of CORRECTION_TRACK_FIELDS) {
      const proposed = entry.fields[field];
      const current = String(track[field] ?? "");
      if (!isChange(current, proposed)) continue;
      rows.push({
        kind: "track",
        key: `track:${entry.filename}:${field}`,
        filename: entry.filename,
        field,
        current: current.trim(),
        proposed: proposed!.trim(),
        defaultSelected: true,
      });
    }
  }
  if (proposal.cover && writesValue(proposal.cover.url)) {
    rows.push({
      kind: "cover",
      key: COVER_ROW_KEY,
      url: proposal.cover.url.trim(),
      previewSrc: proposal.cover.previewSrc,
      defaultSelected: true,
    });
  }
  return rows;
}

/**
 * The apply request for the selected rows, and nothing else: no `clear` /
 * `track_clear` (a correction never removes a tag), `cover_url` only when
 * the cover row is selected.
 */
export function correctionsPayload(
  rows: readonly CorrectionRow[],
  selected: ReadonlySet<string>,
): ApplyPayload {
  const suggestions: Partial<Record<CorrectionAlbumField, string>> = {};
  const trackSuggestions: Record<string, Partial<Record<CorrectionTrackField, string>>> = {};
  let coverUrl: string | null = null;
  for (const row of rows) {
    if (!selected.has(row.key)) continue;
    if (row.kind === "album") suggestions[row.field] = row.proposed;
    else if (row.kind === "track") (trackSuggestions[row.filename] ??= {})[row.field] = row.proposed;
    else coverUrl = row.url;
  }
  return {
    // The apply route also accepts `album_artist_sort` (musaic-tags
    // FIELD_TO_VORBIS); ApplyPayload's type lists only the fields a job
    // result carries.
    suggestions: suggestions as ApplyPayload["suggestions"],
    track_suggestions: trackSuggestions,
    cover_url: coverUrl,
  };
}
