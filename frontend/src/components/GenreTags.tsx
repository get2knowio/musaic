/**
 * Renders an album's genre tag(s) as chips.
 *
 * Vorbis Comments officially support multiple GENRE entries per file
 * (mutagen returns them as a list). Most ad-hoc-tagged libraries
 * collapse them into a single delimited string instead, with `;` being
 * the de-facto convention; some tools use newlines. We split on those
 * two and ONLY those two — comma, slash, and ampersand all legitimately
 * appear inside real genre names ("R&B", "Drum & Bass", "Folk/Rock",
 * "Country, Western"), so splitting on them would shred valid tags.
 *
 * Single-value inputs render as one chip, which is what the app shows
 * today. The component sets up the view contract for the eventual
 * multi-value model: when the scanner is upgraded to read all GENRE
 * entries (and the cache stores them as a list), this component
 * doesn't change at all — it'll just naturally render multiple chips.
 *
 * Empty / null input renders nothing (the caller should normally gate
 * with a truthiness check too, to avoid an empty wrapper element).
 */
interface GenreTagsProps {
  /** Raw `genre` string from the album row. May be null/empty/single
   *  value/`;`-delimited list. */
  genre: string | null | undefined;
  /** Optional className applied to the wrapper span. Useful when the
   *  caller wants to hint inline-flex / wrap behavior in its layout. */
  className?: string;
}

export default function GenreTags({ genre, className }: GenreTagsProps) {
  const tags = parseGenres(genre);
  if (tags.length === 0) return null;
  return (
    <span className={`inline-flex flex-wrap items-center gap-1 ${className ?? ""}`}>
      {tags.map((t) => (
        <span
          key={t}
          className="mono text-[11px] text-fg-2 bg-bg-3 border border-line px-2 py-0.5 rounded-full whitespace-nowrap"
          title={`Genre: ${t}`}
        >
          {t}
        </span>
      ))}
    </span>
  );
}

/** Split a raw genre string into an array of trimmed, non-empty tags.
 *  Exported so callers (search/filter logic, future tests) can reuse
 *  the same parser. */
export function parseGenres(genre: string | null | undefined): string[] {
  if (!genre) return [];
  return genre
    .split(/[;\n]/)
    .map((s) => s.trim())
    .filter(Boolean);
}
