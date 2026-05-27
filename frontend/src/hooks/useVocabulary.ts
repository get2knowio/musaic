import { useQuery } from "@tanstack/react-query";
import { getAlbumTags, getVocabulary } from "@/lib/api";

/**
 * The user's curated tag vocabulary + any orphan tags found in the
 * track_tags table (tags applied to tracks that aren't yet in the
 * vocabulary). Drives the Settings "My Tags" panel and the picker
 * popovers in the album / track tag editors.
 *
 * Mutation handlers (SettingsMyTags / MyTagsSection) write directly
 * into the cache via setQueryData/invalidateQueries, so refetching
 * on focus isn't useful — staleTime: Infinity avoids that race.
 */
export function useVocabulary() {
  return useQuery({
    queryKey: ["vocabulary"],
    queryFn: getVocabulary,
    staleTime: Infinity,
  });
}

/**
 * Per-album tag state: the per-track map plus the album-level
 * intersection. Shared between the AlbumHero chip strip and the
 * MyTagsSection editor so they stay in sync without each component
 * holding its own copy of the same query.
 *
 * MyTagsSection writes the canonical next-state via setQueryData on
 * every mutation, so this cache is always current. staleTime:
 * Infinity matches.
 */
export function useAlbumTags(albumId: string | undefined) {
  return useQuery({
    queryKey: ["album-tags", albumId],
    queryFn: () => getAlbumTags(albumId!),
    enabled: !!albumId,
    staleTime: Infinity,
  });
}
