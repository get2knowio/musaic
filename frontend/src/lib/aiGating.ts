/**
 * What the deterministic resolver already settles for an album.
 *
 * The album panel's MusicBrainz corrections come from the resolver alone;
 * this says which parts of an album (its tags, its cover) the resolver's
 * own evidence covers with enough confidence to offer them without any
 * research.
 */

import type { AlbumResolution } from "./types";

/** The resolver's confidence threshold for a settled answer. */
export const MODEL_FREE_CONFIDENCE = 0.85;

export interface ModelFreeCoverage {
  /** The resolver settles the album's tags. */
  metadata: boolean;
  /** The resolver has a verified cover. */
  cover: boolean;
}

export function modelFreeCoverage(
  resolution: AlbumResolution | undefined | null,
): ModelFreeCoverage {
  return {
    metadata:
      resolution?.anchor != null &&
      resolution.anchor.confidence >= MODEL_FREE_CONFIDENCE &&
      resolution.unresolved_track_count === 0,
    cover:
      resolution?.cover_url != null &&
      resolution.cover_url.confidence >= MODEL_FREE_CONFIDENCE,
  };
}
