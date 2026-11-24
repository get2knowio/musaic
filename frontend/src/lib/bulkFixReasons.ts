/**
 * Plain-language text for the outcome of one finding in a bulk fix run.
 *
 * The server reports, per finding, an outcome and a free-text `message`
 * written for logs and for the single Fix (some carry error text such as
 * "OperationalError: …" or "panic: …"). Nothing the dialog shows is that
 * raw text: every non-fixed outcome is mapped here, in one place, to a
 * sentence a user can act on, and an unrecognised message falls back to a
 * generic sentence rather than being echoed.
 */
import type { QuickFixProgress } from "@/hooks/useChecks";

export type BulkOutcome = QuickFixProgress["outcome"];

export const GENERIC_SKIPPED = "It could not be fixed automatically.";
export const GENERIC_ERROR =
  "Something went wrong with this album, so it may be unchanged or only partly changed. Try Fix on it by itself.";
export const GENERIC_NOT_APPLICABLE = "An automatic fix does not apply to this finding.";

const NOT_IDENTIFIED =
  "The album could not be matched on MusicBrainz from its folder name or fingerprints. " +
  'Try renaming the folder to the form "Artist - Album" and run this again.';
const NO_ACOUSTID_KEY =
  " Fingerprinting also needs an AcoustID key, which can be added under Settings, Track fingerprinting.";

const SKIPPED_RULES: ReadonlyArray<readonly [RegExp, string]> = [
  [
    /no deterministic fixes available/i,
    "MusicBrainz has no confirmed match for this album yet, so there is nothing safe to write. Identify the album first.",
  ],
  [
    /no deterministic fix found/i,
    "The track order could not be worked out from MusicBrainz or the audio fingerprints, so the track numbers were left alone.",
  ],
  [/doesn't have a cover/i, "No confident cover was found on MusicBrainz or the Cover Art Archive."],
  [/not found in cache|no album to/i, "The album is no longer in the library."],
  [
    /skipped \(state=/i,
    "The artist sort names disagree and need your choice. Open the finding and use Resolve.",
  ],
  [
    /resolver error/i,
    "MusicBrainz could not be reached or did not give a usable answer. Try again later.",
  ],
  [/apply failed/i, "The changes could not be written to the album's files."],
];

/** What to tell the user about one finding that was not fixed ("" when fixed). */
export function plainReason(outcome: BulkOutcome, message: string): string {
  if (outcome === "fixed") return "";
  if (outcome === "sent-for-research") return "Sent to your assistant for research.";
  if (outcome === "already-requested") return "Research is already requested for this album.";
  if (outcome === "error") return GENERIC_ERROR;
  if (outcome === "not-applicable") {
    if (/missing from disk/i.test(message)) {
      return "The album's folder is missing from disk. Rescan once it is back, then try again.";
    }
    if (/no quick-fix path/i.test(message)) {
      return "This check has no automatic fix. Open the album and resolve it by hand.";
    }
    return GENERIC_NOT_APPLICABLE;
  }
  if (/couldn't identify the album/i.test(message)) {
    return NOT_IDENTIFIED + (/acoustid api key not set/i.test(message) ? NO_ACOUSTID_KEY : "");
  }
  for (const [re, text] of SKIPPED_RULES) {
    if (re.test(message)) return text;
  }
  return GENERIC_SKIPPED;
}
