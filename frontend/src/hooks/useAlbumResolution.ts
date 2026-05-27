/**
 * Lazy fetch of the deterministic-resolver output for one album.
 *
 * The backend resolver memoizes per album_id + content_hash, so opening
 * the same detail sheet repeatedly is cheap. First call may take a few
 * seconds (MB lookup + optional fpcalc) — that's why this is a query
 * rather than something we eager-fetch on the album list.
 */
import { useQuery } from "@tanstack/react-query";

import { getAlbumResolution } from "@/lib/api";
import type { AlbumResolution } from "@/lib/types";

export const ALBUM_RESOLUTION_KEY = (id: string) => ["album-resolution", id] as const;

export function useAlbumResolution(albumId: string | null | undefined) {
  return useQuery<AlbumResolution>({
    queryKey: ALBUM_RESOLUTION_KEY(albumId ?? ""),
    queryFn: () => getAlbumResolution(albumId!),
    enabled: !!albumId,
    // useLibraryEventsStream invalidates this cache on
    // AlbumTagsChanged / AlbumMbAnchorSet / TrackFingerprinted —
    // every mutation that could change the resolver's output. SSE
    // keeps it fresh, so refetch-on-focus would just race.
    staleTime: Infinity,
    // Network failure shouldn't keep the album sheet from rendering —
    // the badge component handles `data === undefined` gracefully.
    retry: false,
  });
}
