/**
 * Shared accessors for album-level display fields.
 *
 * Every list, tile, hero, sort dropdown, and finding summary needs
 * "what should we show as this album's artist?" — and every consumer
 * inevitably reaches for `album.album_artist || album.artist` with a
 * slightly different fallback for the empty case. Centralize so the
 * cascade is defined once; the backend has the matching `album_fields`
 * module so server-side display agrees with the client.
 *
 * If a consumer needs a *different* cascade (e.g. compilations want
 * "Various Artists" instead of the album_artist tag), do it explicitly
 * at the callsite — these helpers stay neutral.
 */
import type { Album } from "./types";

/** Return the human-readable artist string for an album, falling back
 *  through album_artist → artist → "" (with optional fallback to a
 *  literal placeholder so empty values don't render as blank).
 *
 *  Does NOT special-case compilations — callers that want
 *  "Various Artists" for the is_compilation case must layer that on
 *  themselves (Tile and AlbumList both do this slightly differently
 *  and we want to keep the helper neutral). */
export function displayArtist(
  album: Pick<Album, "album_artist" | "artist">,
  fallback: string = "",
): string {
  const aa = (album.album_artist || "").trim();
  if (aa) return aa;
  const a = (album.artist || "").trim();
  if (a) return a;
  return fallback;
}
