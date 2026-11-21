/**
 * What each finding's action button says (#329).
 *
 * One table so the inbox row and the album inspector use the same words, and
 * so a label can be checked against what the action does. The four checks
 * with their own dialog open a review first, so their label names the thing
 * reviewed and ends in an ellipsis. The rest run `POST .../fix`
 * (`musaic-services/src/check_fixes.rs`), whose path depends on whether a
 * model is available:
 *
 *  - model-free (no hint dialog): MusicBrainz / AcoustID first and nothing
 *    else. A confident match is written at once; otherwise the answer is a
 *    message and nothing changes.
 *  - model available (hint dialog first): the same lookup runs first, and
 *    what it cannot settle becomes a queued proposal to review.
 */

export interface FixActionCopy {
  label: string;
  /** Tooltip: the consequence, in one or two sentences. */
  title: string;
}

/** Checks whose button opens a dialog that previews the change. */
const DIALOG_ACTIONS: Record<string, FixActionCopy> = {
  "filename-mismatch": {
    label: "Review filenames…",
    title: "Review the old and new file names, then rename the files",
  },
  "duplicate-tracks": {
    label: "Compare duplicate copies…",
    title: "Compare the copies and choose which ones to move to the trash",
  },
  "inconsistent-artist-sort": {
    label: "Choose artist sort name…",
    title: "Choose one sort name and write it to every album of this artist",
  },
  "multi-disc-layout": {
    label: "Preview disc layout…",
    title: "See what putting this set in one folder does, then convert it",
  },
};

/** Checks fixed by `/fix`: [model-free, with a model available]. */
const FIX_ACTIONS: Record<string, [FixActionCopy, FixActionCopy]> = {
  "no-cover-art": [
    {
      label: "Embed MusicBrainz cover",
      title:
        "Looks this album up on MusicBrainz and, if a verified cover exists, embeds it in this album's files right away. Otherwise nothing changes.",
    },
    {
      label: "Find cover art…",
      title:
        "Add a hint, then look on MusicBrainz. A verified cover is embedded in this album's files right away; otherwise a suggestion is queued for you to review.",
    },
  ],
  "unidentified-album": [
    {
      label: "Look up on MusicBrainz",
      title:
        "Searches MusicBrainz for this album and, on a confident match, saves the match in Musaic. No files are changed.",
    },
    {
      label: "Identify album…",
      title:
        "Add a hint, then look on MusicBrainz. A confident match is saved in Musaic right away; otherwise a suggestion is queued for you to review.",
    },
  ],
  "tags-out-of-sync": [
    {
      label: "Write MusicBrainz tags",
      title:
        "Writes the tags MusicBrainz is sure of to this album's files right away. Tags it can't settle stay as they are.",
    },
    {
      label: "Fill missing tags…",
      title:
        "Add a hint. Tags MusicBrainz is sure of are written to this album's files right away; the rest is queued as a suggestion for you to review.",
    },
  ],
  "track-gaps": [
    {
      label: "Renumber tracks",
      title:
        "Matches this album's tracks to MusicBrainz and, if they line up, rewrites the track numbers in this album's files right away. Otherwise nothing changes.",
    },
    {
      label: "Check track numbers…",
      title:
        "Add a hint. A clean match rewrites the track numbers in this album's files right away; otherwise a suggestion is queued for you to review.",
    },
  ],
};

/** The generic wording for a check this table does not know. */
const GENERIC: FixActionCopy = {
  label: "Fix",
  title: "Queue a fix for this finding",
};

/** Whether the check's button opens a dialog of its own. */
export function hasDialogAction(checkId: string): boolean {
  return checkId in DIALOG_ACTIONS;
}

/**
 * The button words for a finding's check.
 * @param withModel true when the hint dialog is offered (a model is ready)
 */
export function fixActionCopy(checkId: string, withModel: boolean): FixActionCopy {
  const dialog = DIALOG_ACTIONS[checkId];
  if (dialog) return dialog;
  const pair = FIX_ACTIONS[checkId];
  if (pair) return withModel ? pair[1] : pair[0];
  return GENERIC;
}
