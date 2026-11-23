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
import { DELEGATED_WORK_QUERY_KEY } from "@/hooks/useDelegatedWork";
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
      ["agent", "changes"],
      ["albums-snapshot"],
      ["tracks", "a1"],
      ["album-resolution", "a1"],
    ]);
  });

  it("AlbumCoverChanged invalidates the snapshot and the assistant changes", () => {
    const { invalidate, es } = setup();
    es.emit("AlbumCoverChanged", { album_id: "a1" });
    expect(keysOf(invalidate)).toEqual([["agent", "changes"], ["albums-snapshot"]]);
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
      ["agent", "changes"],
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
      ["agent", "changes"],
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

  it("AlbumMarkedMissing invalidates the snapshot and the assistant changes and preserves cached track/resolution data", () => {
    const { invalidate, remove, es } = setup();
    es.emit("AlbumMarkedMissing", { album_id: "m1" });
    expect(keysOf(invalidate)).toEqual([["agent", "changes"], ["albums-snapshot"]]);
    // Contract: keep cached data so the DetailSheet can still render a
    // Missing album's known metadata.
    expect(remove).not.toHaveBeenCalled();
  });

  it("LibraryScanCompleted does a broad prefix-matched refresh", () => {
    const { invalidate, es } = setup();
    es.emit("LibraryScanCompleted", { album_count: 42 });
    expect(keysOf(invalidate)).toEqual([
      ["agent", "changes"],
      ["albums-snapshot"],
      ["tracks"],
      ["album-resolution"],
    ]);
  });

  it("LibraryScanStopped refreshes only the snapshot (the scan saved nothing)", () => {
    const { invalidate, es } = setup();
    es.emit("LibraryScanStopped", { reason: "cancelled", album_count: 3 });
    expect(keysOf(invalidate)).toEqual([["albums-snapshot"]]);
  });

  it("BulkBatchCompleted does a broad prefix-matched refresh", () => {
    const { invalidate, es } = setup();
    es.emit("BulkBatchCompleted");
    expect(keysOf(invalidate)).toEqual([
      ["agent", "changes"],
      ["albums-snapshot"],
      ["tracks"],
      ["album-resolution"],
    ]);
  });

  it("ChecksRerunFinished invalidates findings, checks and check details, reusing an in-flight refetch", () => {
    const { invalidate, es } = setup();
    es.emit("ChecksRerunFinished", {
      checks: ["track-gaps"],
      failed: [],
      album_ids: ["a1"],
    });
    expect(keysOf(invalidate)).toEqual([["findings"], ["checks"], ["check"]]);
    for (const call of invalidate.mock.calls) {
      expect(call[1]).toEqual({ cancelRefetch: false });
    }
  });

  it("ChecksRerunFinished leaves the album caches alone", () => {
    const { invalidate, remove, es } = setup();
    es.emit("ChecksRerunFinished", { checks: [], failed: [], album_ids: [] });
    expect(keysOf(invalidate)).not.toContainEqual(["albums-snapshot"]);
    expect(remove).not.toHaveBeenCalled();
  });

  it("re-invalidates every bus cache on (re)connect via the open event", () => {
    const { invalidate, es } = setup();
    es.emit("open");
    expect(keysOf(invalidate)).toEqual([
      ["research"],
      ["albums-snapshot"],
      ["tracks"],
      ["album-resolution"],
      ["agent"],
    ]);
  });

  it("refreshes research and the jobs cache when submitted work changes", () => {
    const { invalidate, es } = setup();
    es.emit("AgentStateChanged", { what: "work", ids: ["item"] });
    expect(keysOf(invalidate)).toEqual([["research"], ["jobs"], ["agent", "diagnostics"]]);
  });

  it("AgentStateChanged invalidates only the named agent family", () => {
    const { invalidate, remove, es } = setup();
    es.emit("AgentStateChanged", { what: "approvals", ids: ["x"] });
    es.emit("AgentStateChanged", { what: "pairings", ids: [] });
    es.emit("AgentStateChanged", { what: "activity", ids: ["p1"] });
    es.emit("AgentStateChanged", { what: "changes", ids: ["p1"] });
    expect(keysOf(invalidate)).toEqual([
      ["agent", "approvals"],
      ["agent", "pairings"],
      // A new pairing's facts are shown before its host connects.
      ["agent", "diagnostics"],
      ["agent", "activity"],
      ["agent", "changes"],
    ]);
    expect(remove).not.toHaveBeenCalled();
  });

  it("AgentStateChanged with an unknown family refreshes every agent cache", () => {
    const { invalidate, es } = setup();
    es.emit("AgentStateChanged", { what: "other", ids: [] });
    expect(keysOf(invalidate)).toEqual([["agent"]]);
  });

  it("AgentStateChanged hosts refreshes the diagnostics and the delegated-work list", () => {
    const { invalidate, es } = setup();
    es.emit("AgentStateChanged", { what: "hosts", ids: ["p1"] });
    // "Waiting for an agent" depends on whether anyone is listening.
    expect(keysOf(invalidate)).toEqual([["agent", "diagnostics"], ["research", "work"]]);
  });

  it("AgentStateChanged work refreshes research (the delegated-work list) and the diagnostics", () => {
    const { invalidate, es } = setup();
    es.emit("AgentStateChanged", { what: "work", ids: ["w1"] });
    expect(keysOf(invalidate)).toEqual([["research"], ["jobs"], ["agent", "diagnostics"]]);
    // The delegated-work list sits under the key that is invalidated.
    expect(DELEGATED_WORK_QUERY_KEY.slice(0, 1)).toEqual(["research"]);
  });

  it("SettingsChanged refreshes the settings, the AcoustID prompt and the UI preferences", () => {
    const { invalidate, es } = setup();
    es.emit("SettingsChanged", { keys: ["acoustid_prompt_dismissed"] });
    expect(keysOf(invalidate)).toEqual([["settings"], ["acoustid-prompt"], ["ui-prefs"]]);
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
