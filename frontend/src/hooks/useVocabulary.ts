import { useQuery } from "@tanstack/react-query";
import { getAlbumTags, getVocabulary } from "@/lib/api";

/**
 * The user's curated tag vocabulary + any orphan tags found in the
 * track_tags table (tags applied to tracks that aren't yet in the
 * vocabulary). Drives the Settings "My Tags" panel and the picker
 * popovers in the album / track tag editors.
 */
export function useVocabulary() {
  return useQuery({
    queryKey: ["vocabulary"],
    queryFn: getVocabulary,
    staleTime: 30_000,
  });
}

/**
 * Per-album tag state: the per-track map plus the album-level
 * intersection. Shared between the AlbumHero chip strip and the
 * MyTagsSection editor so they stay in sync without each component
 * holding its own copy of the same query.
 */
export function useAlbumTags(albumId: string | undefined) {
  return useQuery({
    queryKey: ["album-tags", albumId],
    queryFn: () => getAlbumTags(albumId!),
    enabled: !!albumId,
    staleTime: 30_000,
  });
}
