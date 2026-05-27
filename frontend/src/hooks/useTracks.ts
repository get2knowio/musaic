import { useQuery } from "@tanstack/react-query";
import { getTracks } from "@/lib/api";

/**
 * Per-album track listing including BPM + ReplayGain values where present.
 *
 * useLibraryEventsStream invalidates ["tracks", album_id] on
 * AlbumTagsChanged + TrackFingerprinted; the rename event clears the
 * old/new ids. SSE keeps it fresh, so staleTime: Infinity is safe.
 */
export function useTracks(albumId: string | null | undefined) {
  return useQuery({
    queryKey: ["tracks", albumId],
    queryFn: () => getTracks(albumId!),
    enabled: !!albumId,
    staleTime: Infinity,
  });
}
