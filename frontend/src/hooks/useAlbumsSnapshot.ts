/**
 * Lightweight read of /api/albums for components that need the album list
 * but aren't responsible for the live SSE scan stream (e.g. the Sidebar
 * for filter counts). Library page owns the SSE-driven state; this just
 * polls the persistent snapshot.
 *
 * TanStack Query dedupes — Library page also reads /api/albums on mount,
 * and both share the same cache entry under ["albums-snapshot"].
 */
import { useQuery } from "@tanstack/react-query";
import { listAlbums } from "@/lib/api";

export function useAlbumsSnapshot() {
  return useQuery({
    queryKey: ["albums-snapshot"],
    queryFn: async () => (await listAlbums()).albums,
    // useLibraryEventsStream invalidates this cache on every album-
    // scoped mutation (tags, cover, anchor, rename, delete) plus on
    // LibraryScanCompleted / BulkBatchCompleted. SSE keeps it fresh —
    // refetch-on-focus would race with it.
    staleTime: Infinity,
  });
}
