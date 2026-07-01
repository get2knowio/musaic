/**
 * Pins the event → cache-invalidation contract documented at the top of
 * useLibraryEventsStream.ts. This is the load-bearing reason the harness
 * exists: the mapping compiles regardless of what it does, so only a
 * behavioural test catches a regression when React Query / the bus
 * taxonomy shifts under a major dependency bump.
 */
import { describe, it, expect, vi } from "vitest";
import type { ReactNode } from "react";
import { renderHook } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { useLibraryEventsStream } from "@/hooks/useLibraryEventsStream";
import { MockEventSource } from "@/test/setup";

function setup() {
  const qc = new QueryClient();
  const invalidate = vi
    .spyOn(qc, "invalidateQueries")
    .mockResolvedValue(undefined);
  const remove = vi.spyOn(qc, "removeQueries").mockReturnValue(undefined);

  const wrapper = ({ children }: { children: ReactNode }) => (
    <QueryClientProvider client={qc}>{children}</QueryClientProvider>
  );
  const { unmount } = renderHook(() => useLibraryEventsStream(), { wrapper });

  const es = MockEventSource.last;
  if (!es) throw new Error("hook did not construct an EventSource");
  return { qc, invalidate, remove, es, unmount };
}

/** queryKeys passed to a spied invalidate/removeQueries call. */
const keysOf = (spy: { mock: { calls: unknown[][] } }) =>
  spy.mock.calls.map((c) => (c[0] as { queryKey: unknown[] }).queryKey);

describe("useLibraryEventsStream", () => {
  it("subscribes to the library-events SSE channel on mount", () => {
    const { es } = setup();
    expect(es.url).toBe("/api/library-events/stream");
  });

  it("AlbumTagsChanged invalidates snapshot + that album's tracks & resolution", () => {
    const { invalidate, es } = setup();
    es.emit("AlbumTagsChanged", { album_id: "a1" });
    expect(keysOf(invalidate)).toEqual([
      ["albums-snapshot"],
      ["tracks", "a1"],
      ["album-resolution", "a1"],
    ]);
  });

  it("AlbumCoverChanged only invalidates the snapshot", () => {
    const { invalidate, es } = setup();
    es.emit("AlbumCoverChanged", { album_id: "a1" });
    expect(keysOf(invalidate)).toEqual([["albums-snapshot"]]);
  });

  it("AlbumMbAnchorSet invalidates snapshot + that album's resolution", () => {
    const { invalidate, es } = setup();
    es.emit("AlbumMbAnchorSet", { album_id: "a1" });
    expect(keysOf(invalidate)).toEqual([
      ["albums-snapshot"],
      ["album-resolution", "a1"],
    ]);
  });

  it("dir rename (old != new) drops old caches and invalidates new", () => {
    const { invalidate, remove, es } = setup();
    es.emit("AlbumRenamed", { old_album_id: "old", new_album_id: "new" });
    // Old id is gone forever (id = sha1(path)) → removeQueries.
    expect(keysOf(remove)).toEqual([
      ["tracks", "old"],
      ["album-resolution", "old"],
    ]);
    expect(keysOf(invalidate)).toEqual([
      ["albums-snapshot"],
      ["tracks", "new"],
      ["album-resolution", "new"],
    ]);
  });

  it("file-only rename (old == new) invalidates without removing", () => {
    const { invalidate, remove, es } = setup();
    es.emit("AlbumRenamed", { old_album_id: "same", new_album_id: "same" });
    expect(remove).not.toHaveBeenCalled();
    expect(keysOf(invalidate)).toEqual([
      ["albums-snapshot"],
      ["tracks", "same"],
      ["album-resolution", "same"],
    ]);
  });

  it("AlbumDeleted invalidates snapshot and drops the album's caches", () => {
    const { invalidate, remove, es } = setup();
    es.emit("AlbumDeleted", { album_id: "gone" });
    expect(keysOf(invalidate)).toEqual([["albums-snapshot"]]);
    expect(keysOf(remove)).toEqual([
      ["tracks", "gone"],
      ["album-resolution", "gone"],
    ]);
  });

  it("AlbumMarkedMissing invalidates only the snapshot and preserves cached track/resolution data", () => {
    const { invalidate, remove, es } = setup();
    es.emit("AlbumMarkedMissing", { album_id: "m1" });
    expect(keysOf(invalidate)).toEqual([["albums-snapshot"]]);
    // Contract: keep cached data so the DetailSheet can still render a
    // Missing album's known metadata.
    expect(remove).not.toHaveBeenCalled();
  });

  it("LibraryScanCompleted does a broad prefix-matched refresh", () => {
    const { invalidate, es } = setup();
    es.emit("LibraryScanCompleted", { album_count: 42 });
    expect(keysOf(invalidate)).toEqual([
      ["albums-snapshot"],
      ["tracks"],
      ["album-resolution"],
    ]);
  });

  it("BulkBatchCompleted does a broad prefix-matched refresh", () => {
    const { invalidate, es } = setup();
    es.emit("BulkBatchCompleted");
    expect(keysOf(invalidate)).toEqual([
      ["albums-snapshot"],
      ["tracks"],
      ["album-resolution"],
    ]);
  });

  it("re-invalidates every bus cache on (re)connect via the open event", () => {
    const { invalidate, es } = setup();
    es.emit("open");
    expect(keysOf(invalidate)).toEqual([
      ["albums-snapshot"],
      ["tracks"],
      ["album-resolution"],
    ]);
  });

  it("ignores a payload with no album_id (guards against a relay shape change)", () => {
    const { invalidate, es } = setup();
    es.emit("AlbumTagsChanged", {});
    expect(invalidate).not.toHaveBeenCalled();
  });

  it("ignores a malformed (non-JSON) payload without throwing", () => {
    const { invalidate, es } = setup();
    expect(() => es.emit("AlbumTagsChanged", "}{ not json")).not.toThrow();
    expect(invalidate).not.toHaveBeenCalled();
  });

  it("closes the EventSource on unmount", () => {
    const { es, unmount } = setup();
    expect(es.closed).toBe(false);
    unmount();
    expect(es.closed).toBe(true);
  });
});
