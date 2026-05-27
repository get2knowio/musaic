import { useQuery } from "@tanstack/react-query";
import { getAlbumTags, getVocabulary } from "@/lib/api";

/**
 * The user's curated tag vocabulary + any orphan tags found in the
 * track_tags table (tags applied to tracks that aren't yet in the
 * vocabulary). Drives the Settings "My Tags" panel and the picker
 * popovers in the album / track tag editors.
 *
 * `staleTime: 30s` (not Infinity) because tag mutations don't ride
 * the `library_events` bus — api/tags.py doesn't publish events, and
 * useLibraryEventsStream has no vocabulary handler. Mutation handlers
 * (SettingsMyTags / MyTagsSection) write through `setQueryData`, but
 * that doesn't help a second browser tab or an external SQLite edit.
 * The 30s staleness window + default refetch-on-window-focus catches
 * those cross-tab / out-of-band changes.
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
 *
 * Same 30s window as `useVocabulary` for the same reason — tag
 * mutations aren't on the library_events bus, so refetch-on-focus is
 * the cross-tab safety net.
 */
export function useAlbumTags(albumId: string | undefined) {
  return useQuery({
    queryKey: ["album-tags", albumId],
    queryFn: () => getAlbumTags(albumId!),
    enabled: !!albumId,
    staleTime: 30_000,
  });
}
