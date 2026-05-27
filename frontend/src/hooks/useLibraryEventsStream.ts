/**
 * useLibraryEventsStream — single subscriber to /api/library-events/stream.
 *
 * Mounted at the App level alongside useJobsStream / useChecksStream.
 * Maps backend `library_events` taxonomy onto TanStack Query cache
 * invalidations so mutation handlers don't have to remember which
 * cache keys to bust after every operation.
 *
 * Event → invalidation map (mirrors the bus contract in
 * `musaic/library_events.py`):
 *
 *   AlbumTagsChanged(album_id)        → ["albums-snapshot"],
 *                                       ["tracks", album_id],
 *                                       ["album-resolution", album_id],
 *                                       ["findings"], ["checks"], ["check"]
 *   AlbumCoverChanged(album_id)       → ["albums-snapshot"]
 *                                       (cover URL is mtime-busted client-side
 *                                        via coverUrl(); see lib/api.ts)
 *   AlbumMbAnchorSet(album_id)        → ["albums-snapshot"],
 *                                       ["album-resolution", album_id],
 *                                       ["findings"], ["checks"], ["check"]
 *   TrackFingerprinted(album_id)      → ["tracks", album_id],
 *                                       ["album-resolution", album_id]
 *   AlbumRenamed(old, new)            → ["albums-snapshot"], ["findings"],
 *                                       ["checks"], ["check"]; drop any
 *                                       query keyed by either id.
 *   AlbumDeleted(album_id)            → ["albums-snapshot"], ["findings"],
 *                                       ["checks"], ["check"]; drop any
 *                                       query keyed by the id.
 *   LibraryScanCompleted              → broad refresh (snapshot, findings,
 *                                       checks).
 *   BulkBatchCompleted                → broad refresh (snapshot, findings,
 *                                       checks).
 *
 * Findings/checks invalidations on most events are belt-and-suspenders:
 * `useChecksStream` already pushes them when the runner's state
 * changes. Keeping them here too means the cache lands as soon as the
 * mutation is observable (one RTT earlier than waiting on the runner
 * to re-emit), at the cost of a duplicate fetch the rare time both
 * arrive interleaved — TanStack dedupes in-flight refetches so the
 * extra is rarely visible.
 */
import { useEffect } from "react";
import { useQueryClient } from "@tanstack/react-query";

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
      qc.invalidateQueries({ queryKey: ["findings"] });
      qc.invalidateQueries({ queryKey: ["checks"] });
      qc.invalidateQueries({ queryKey: ["check"] });
    };

    const onAlbumScopedCover = (_album_id: string) => {
      qc.invalidateQueries({ queryKey: ["albums-snapshot"] });
      // Cover URLs are mtime-busted via coverUrl() in lib/api.ts —
      // the snapshot refresh picks up the new dir_mtime and forces
      // <img> elements to re-fetch. No separate cover cache to drop.
      qc.invalidateQueries({ queryKey: ["findings"] });
      qc.invalidateQueries({ queryKey: ["checks"] });
      qc.invalidateQueries({ queryKey: ["check"] });
    };

    const onAlbumScopedAnchor = (album_id: string) => {
      qc.invalidateQueries({ queryKey: ["albums-snapshot"] });
      qc.invalidateQueries({ queryKey: ["album-resolution", album_id] });
      qc.invalidateQueries({ queryKey: ["findings"] });
      qc.invalidateQueries({ queryKey: ["checks"] });
      qc.invalidateQueries({ queryKey: ["check"] });
    };

    const onTrackFingerprinted = (album_id: string) => {
      qc.invalidateQueries({ queryKey: ["tracks", album_id] });
      qc.invalidateQueries({ queryKey: ["album-resolution", album_id] });
      // Fingerprints drive duplicate-tracks + unidentified-album reruns
      // which surface as findings.
      qc.invalidateQueries({ queryKey: ["findings"] });
      qc.invalidateQueries({ queryKey: ["checks"] });
    };

    const onAlbumRenamed = (old_id: string, new_id: string) => {
      qc.invalidateQueries({ queryKey: ["albums-snapshot"] });
      qc.invalidateQueries({ queryKey: ["findings"] });
      qc.invalidateQueries({ queryKey: ["checks"] });
      qc.invalidateQueries({ queryKey: ["check"] });
      // Drop anything keyed by either id. The renamed album's URL
      // changes (album id is sha1(path)), and we don't want a stale
      // tracks/resolution cache lingering under the old id.
      for (const id of [old_id, new_id]) {
        if (!id) continue;
        qc.removeQueries({ queryKey: ["tracks", id] });
        qc.removeQueries({ queryKey: ["album-resolution", id] });
      }
    };

    const onAlbumDeleted = (album_id: string) => {
      qc.invalidateQueries({ queryKey: ["albums-snapshot"] });
      qc.invalidateQueries({ queryKey: ["findings"] });
      qc.invalidateQueries({ queryKey: ["checks"] });
      qc.invalidateQueries({ queryKey: ["check"] });
      qc.removeQueries({ queryKey: ["tracks", album_id] });
      qc.removeQueries({ queryKey: ["album-resolution", album_id] });
    };

    const onLibraryWide = () => {
      qc.invalidateQueries({ queryKey: ["albums-snapshot"] });
      qc.invalidateQueries({ queryKey: ["findings"] });
      qc.invalidateQueries({ queryKey: ["checks"] });
      qc.invalidateQueries({ queryKey: ["check"] });
    };

    const parse = (ev: MessageEvent): EventPayload | null => {
      try { return JSON.parse(ev.data); } catch { return null; }
    };

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
    es.addEventListener("LibraryScanCompleted", () => onLibraryWide());
    es.addEventListener("BulkBatchCompleted", () => onLibraryWide());

    es.addEventListener("error", () => {
      // EventSource auto-reconnects; nothing to do.
    });

    return () => es.close();
  }, [qc]);
}
