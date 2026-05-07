import { useQuery } from "@tanstack/react-query";
import { getTracks } from "@/lib/api";

/**
 * Per-album track listing including BPM + ReplayGain values where present.
 * Cached aggressively — only invalidated when the album itself changes
 * (apply flow handles invalidation explicitly via queryClient).
 */
export function useTracks(albumId: string | null | undefined) {
  return useQuery({
    queryKey: ["tracks", albumId],
    queryFn: () => getTracks(albumId!),
    enabled: !!albumId,
    staleTime: 60_000,
  });
}
