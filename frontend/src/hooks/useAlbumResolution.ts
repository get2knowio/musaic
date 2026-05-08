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
    // Resolver output rarely changes within a session (only when scan
    // detects new tags or the user applies). Stale time keeps the UI
    // snappy across navigate-away-and-back.
    staleTime: 5 * 60 * 1000,
    // Network failure shouldn't keep the album sheet from rendering —
    // the badge component handles `data === undefined` gracefully.
    retry: false,
  });
}
