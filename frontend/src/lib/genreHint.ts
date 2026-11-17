import type { Finding } from "./types";

/**
 * The genre MusicBrainz suggests for the album of a "Tags don't match
 * canonical" finding whose genre is blank, or null. A label recorded when
 * the check last ran: nothing writes it. The album panel's corrections list
 * is the only way to accept it (a row that starts unticked), and it
 * re-derives the value itself.
 */
export function suggestedGenre(finding: Finding): string | null {
  const v = finding.detail?.genre_available;
  return typeof v === "string" && v.trim() !== "" ? v : null;
}

/** The sentence before the pointer to the corrections list. */
export function genreHintLead(genre: string): string {
  return `MusicBrainz suggests “${genre}” as the genre.`;
}

export const GENRE_HINT_POINTER = "Review it in the album’s corrections.";
