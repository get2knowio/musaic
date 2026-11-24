/**
 * What each finding's action button says (#329).
 *
 * One table so the inbox row and the album inspector use the same words, and
 * so a label can be checked against what the action does. The four checks
 * with their own dialog open a review first, so their label names the thing
 * reviewed and ends in an ellipsis. The rest run `POST .../fix`
 * (`musaic-services/src/check_fixes.rs`) after a hint dialog: the resolver
 * (MusicBrainz, and the Cover Art Archive for covers) runs first, and what
 * it cannot settle becomes a research request for the user's assistant.
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

const RESEARCH_TAIL =
  "Anything it can't settle is sent to your assistant for research. Add a hint to help.";

/** Checks fixed by `/fix`, the four research-capable ones. */
const FIX_ACTIONS: Record<string, FixActionCopy> = {
  "no-cover-art": {
    label: "Find cover art…",
    title: `Tries MusicBrainz and the Cover Art Archive first. ${RESEARCH_TAIL}`,
  },
  "unidentified-album": {
    label: "Identify album…",
    title: `Tries MusicBrainz first. ${RESEARCH_TAIL}`,
  },
  "tags-out-of-sync": {
    label: "Fill missing tags…",
    title: `Tries MusicBrainz first. ${RESEARCH_TAIL}`,
  },
  "track-gaps": {
    label: "Check track numbers…",
    title: `Tries MusicBrainz first. ${RESEARCH_TAIL}`,
  },
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

/** The button words for a finding's check. */
export function fixActionCopy(checkId: string): FixActionCopy {
  return DIALOG_ACTIONS[checkId] ?? FIX_ACTIONS[checkId] ?? GENERIC;
}
