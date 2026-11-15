/**
 * useLibraryEventsStream — single subscriber to /api/library-events/stream.
 *
 * Mounted at the App level alongside useJobsStream / useChecksStream.
 * Maps backend `library_events` taxonomy onto TanStack Query cache
 * invalidations so mutation handlers don't have to remember which
 * cache keys to bust after every operation.
 *
 * Division of responsibility with useChecksStream:
 *
 *   useLibraryEventsStream owns **album-state** caches —
 *     ["albums-snapshot"], ["tracks", id], ["album-resolution", id]
 *   useChecksStream owns **findings/checks** caches —
 *     ["findings"], ["checks"], ["check"]
 *
 * The split matters because the backend's `_post_scan_rerun` subscriber
 * runs in a background thread (registered `background=True` in
 * `event_subscribers.py`). The relay's SSE event arrives on the
 * frontend BEFORE the rerun finishes; invalidating findings/checks
 * here would refetch stale data and flicker. Instead we let
 * useChecksStream invalidate findings/checks when `notify_changed()`
 * fires from inside `run_check` — which is reliably after the rerun.
 *
 * Event → invalidation map (mirrors the bus contract in
 * `musaic/library_events.py`):
 *
 *   AlbumTagsChanged(album_id)        → ["albums-snapshot"],
 *                                       ["tracks", album_id],
 *                                       ["album-resolution", album_id]
 *   AlbumCoverChanged(album_id)       → ["albums-snapshot"]
 *                                       (cover URL is mtime-busted via
 *                                        coverUrl(); see lib/api.ts)
 *   AlbumMbAnchorSet(album_id)        → ["albums-snapshot"],
 *                                       ["album-resolution", album_id]
 *   TrackFingerprinted(album_id)      → ["tracks", album_id],
 *                                       ["album-resolution", album_id]
 *   AlbumRenamed(old, new)            → ["albums-snapshot"]; when
 *                                       old != new, drop tracks/
 *                                       resolution under the old id
 *                                       (gone forever) AND the new id;
 *                                       when old == new (file-only
 *                                       rename) invalidate them
 *                                       instead so open sheets refetch
 *                                       smoothly without a skeleton
 *                                       flicker.
 *   AlbumDeleted(album_id)            → ["albums-snapshot"]; drop
 *                                       tracks/resolution for the id.
 *   LibraryScanCompleted              → broad refresh (snapshot + every
 *                                       per-album tracks/resolution).
 *   BulkBatchCompleted                → same.
 *   LibraryScanStopped(reason)        → ["albums-snapshot"] only: an
 *                                       aborted / cancelled / failed scan
 *                                       wrote no album, but a cancelled
 *                                       Rescan leaves the library's
 *                                       scanned_at reset. Nothing per-album.
 *
 * Reconnect resync: EventSource auto-reconnects on transient network
 * drops, but events published during the gap are lost (relay's per-
 * connection queue is dropped on disconnect; on reconnect we get a
 * fresh empty queue and a `ready` sentinel — no replay buffer). The
 * `open` listener below invalidates every bus-driven cache on every
 * open (initial + reconnect) so the client self-heals after any blip.
 * On initial mount the invalidations are cheap (queries are either
 * already in-flight or not yet mounted, both of which dedupe).
 */
import { useEffect } from "react";
import { useQueryClient, type QueryClient } from "@tanstack/react-query";

type EventPayload = {
  album_id?: string;
  old_album_id?: string;
  new_album_id?: string;
};

export function useLibraryEventsStream() {
  const qc = useQueryClient();

  useEffect(() => {
    const es = new EventSource("/api/library-events/stream");

    const onAlbumScopedTags = (album_id: string) => {
      qc.invalidateQueries({ queryKey: ["albums-snapshot"] });
      qc.invalidateQueries({ queryKey: ["tracks", album_id] });
      qc.invalidateQueries({ queryKey: ["album-resolution", album_id] });
    };

    const onAlbumScopedCover = (_album_id: string) => {
      qc.invalidateQueries({ queryKey: ["albums-snapshot"] });
      // Cover URLs are mtime-busted via coverUrl() in lib/api.ts —
      // the snapshot refresh picks up the new dir_mtime and forces
      // <img> elements to re-fetch. No separate cover cache to drop.
    };

    const onAlbumScopedAnchor = (album_id: string) => {
      qc.invalidateQueries({ queryKey: ["albums-snapshot"] });
      qc.invalidateQueries({ queryKey: ["album-resolution", album_id] });
    };

    const onTrackFingerprinted = (album_id: string) => {
      qc.invalidateQueries({ queryKey: ["tracks", album_id] });
      qc.invalidateQueries({ queryKey: ["album-resolution", album_id] });
    };

    const onAlbumRenamed = (old_id: string, new_id: string) => {
      qc.invalidateQueries({ queryKey: ["albums-snapshot"] });
      // Two distinct cases — see the AlbumRenamed docstring in
      // library_events.py:
      //   old != new → dir rename, the old album_id is gone forever
      //     (id is sha1(path)). Drop its tracks/resolution caches so
      //     they can't 404 on a future read; invalidate the new id's
      //     so they refetch.
      //   old == new → file-only rename, album id is unchanged.
      //     Invalidate (smooth background refetch), don't remove
      //     (would flip the open sheet's TrackList / ResolutionBadge
      //     to skeleton state mid-chain-apply).
      if (old_id && new_id && old_id !== new_id) {
        qc.removeQueries({ queryKey: ["tracks", old_id] });
        qc.removeQueries({ queryKey: ["album-resolution", old_id] });
        qc.invalidateQueries({ queryKey: ["tracks", new_id] });
        qc.invalidateQueries({ queryKey: ["album-resolution", new_id] });
      } else {
        const id = new_id || old_id;
        if (id) {
          qc.invalidateQueries({ queryKey: ["tracks", id] });
          qc.invalidateQueries({ queryKey: ["album-resolution", id] });
        }
      }
    };

    const onAlbumDeleted = (album_id: string) => {
      qc.invalidateQueries({ queryKey: ["albums-snapshot"] });
      qc.removeQueries({ queryKey: ["tracks", album_id] });
      qc.removeQueries({ queryKey: ["album-resolution", album_id] });
    };

    const onLibraryWide = () => {
      qc.invalidateQueries({ queryKey: ["albums-snapshot"] });
      // Prefix-match every per-album entry — a scan or bulk-fix may
      // have touched any open album, and without this an open
      // DetailSheet would silently render pre-event track data
      // forever (useTracks is staleTime: Infinity).
      qc.invalidateQueries({ queryKey: ["tracks"] });
      qc.invalidateQueries({ queryKey: ["album-resolution"] });
    };

    const onAlbumMarkedMissing = (album_id: string) => {
      // The album row is preserved (only `missing_since` was set), so
      // tracks/resolution caches are still valid against the cached
      // data — but the snapshot now carries the missing_since marker,
      // which the Library grid uses to grey out the tile and the
      // DetailSheet uses to swap to the "missing" banner. So we only
      // need to bust the snapshot.
      //
      // We deliberately do NOT removeQueries on ['tracks', id] /
      // ['album-resolution', id] — keeping the cached data means the
      // DetailSheet can still display the album's known metadata when
      // the user opens a Missing tile.
      qc.invalidateQueries({ queryKey: ["albums-snapshot"] });
      // No-op album_id reference; consumers read it from the snapshot.
      void album_id;
    };

    const parse = (ev: MessageEvent): EventPayload | null => {
      try { return JSON.parse(ev.data); } catch { return null; }
    };

    es.addEventListener("open", () => resyncAllBusCaches(qc));

    es.addEventListener("AlbumTagsChanged", (ev) => {
      const p = parse(ev as MessageEvent);
      if (p?.album_id) onAlbumScopedTags(p.album_id);
    });
    es.addEventListener("AlbumCoverChanged", (ev) => {
      const p = parse(ev as MessageEvent);
      if (p?.album_id) onAlbumScopedCover(p.album_id);
    });
    es.addEventListener("AlbumMbAnchorSet", (ev) => {
      const p = parse(ev as MessageEvent);
      if (p?.album_id) onAlbumScopedAnchor(p.album_id);
    });
    es.addEventListener("TrackFingerprinted", (ev) => {
      const p = parse(ev as MessageEvent);
      if (p?.album_id) onTrackFingerprinted(p.album_id);
    });
    es.addEventListener("AlbumRenamed", (ev) => {
      const p = parse(ev as MessageEvent);
      onAlbumRenamed(p?.old_album_id ?? "", p?.new_album_id ?? "");
    });
    es.addEventListener("AlbumDeleted", (ev) => {
      const p = parse(ev as MessageEvent);
      if (p?.album_id) onAlbumDeleted(p.album_id);
    });
    es.addEventListener("AlbumMarkedMissing", (ev) => {
      const p = parse(ev as MessageEvent);
      if (p?.album_id) onAlbumMarkedMissing(p.album_id);
    });
    es.addEventListener("LibraryScanCompleted", () => onLibraryWide());
    es.addEventListener("BulkBatchCompleted", () => onLibraryWide());
    es.addEventListener("LibraryScanStopped", () => {
      qc.invalidateQueries({ queryKey: ["albums-snapshot"] });
    });

    es.addEventListener("error", () => {
      // EventSource auto-reconnects; the "open" listener above
      // re-invalidates on every (re)connect to catch up on events
      // that were published during the disconnect window.
    });

    return () => es.close();
  }, [qc]);
}

/**
 * Re-invalidate every bus-driven cache. Fires on every EventSource
 * `open` event (initial connect AND every auto-reconnect) so a
 * transient SSE drop can't leave caches indefinitely stale — staleTime
 * is Infinity on most of these, so refetch-on-focus won't save us.
 *
 * Findings/checks/check live on useChecksStream's own
 * /api/checks/stream channel and are kept fresh there; not our concern.
 */
function resyncAllBusCaches(qc: QueryClient): void {
  qc.invalidateQueries({ queryKey: ["albums-snapshot"] });
  qc.invalidateQueries({ queryKey: ["tracks"] });
  qc.invalidateQueries({ queryKey: ["album-resolution"] });
}
