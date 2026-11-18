/**
 * The Library album list follows ["albums-snapshot"] while no scan runs
 * (public issue: an uploaded cover did not appear until Library was
 * reopened). A snapshot fetched after the list settled replaces it; an
 * older cached snapshot does not; nothing is replaced during a scan.
 */
import { beforeEach, afterEach, describe, it, expect, vi } from "vitest";
import { act, renderHook, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { StrictMode, type ReactNode } from "react";

const toast = vi.hoisted(() =>
  Object.assign(vi.fn(), { warning: vi.fn(), error: vi.fn(), success: vi.fn() }),
);
vi.mock("sonner", () => ({ toast }));

import { requestScanOnNextLoad, useScanStream } from "@/hooks/useScanStream";
import { MockEventSource } from "@/test/setup";
import { makeAlbum } from "@/test/fixtures";
import type { Album } from "@/lib/types";

let server: Album[];

function setup() {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const wrapper = ({ children }: { children: ReactNode }) => (
    <QueryClientProvider client={qc}>{children}</QueryClientProvider>
  );
  const hook = renderHook(() => useScanStream({ enabled: true }), { wrapper });
  return { qc, hook };
}

let cancelOutcome = "requested";
let cancelCalls = 0;

beforeEach(() => {
  MockEventSource.instances.length = 0;
  requestScanOnNextLoad();
  toast.mockClear();
  toast.warning.mockClear();
  cancelOutcome = "requested";
  cancelCalls = 0;
  server = [makeAlbum({ id: "a", has_embedded_cover: false, cover_file: null })];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string) => {
      if (String(url).includes("/api/scan/cancel")) {
        cancelCalls += 1;
        return new Response(
          JSON.stringify({ outcome: cancelOutcome, scan: { status: "cancelling" } }),
          { status: 200, headers: { "content-type": "application/json" } },
        );
      }
      return new Response(
        JSON.stringify({ albums: server, count: server.length, library_dir: "/music", scanned_at: 1 }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    }),
  );
});
afterEach(() => vi.unstubAllGlobals());

async function finishScan() {
  await waitFor(() => expect(MockEventSource.last?.url).toContain("/api/scan/stream"));
  act(() => MockEventSource.last!.emit("done", { count: server.length, scanned_at: 2 }));
}

describe("useScanStream first scan", () => {
  it("still scans when the effect mounts twice (dev StrictMode)", async () => {
    const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    renderHook(() => useScanStream({ enabled: true }), {
      wrapper: ({ children }: { children: ReactNode }) => (
        <StrictMode>
          <QueryClientProvider client={qc}>{children}</QueryClientProvider>
        </StrictMode>
      ),
    });
    await waitFor(() =>
      expect(
        MockEventSource.instances.some((i) => i.url.includes("/api/scan/stream") && !i.closed),
      ).toBe(true),
    );
  });

  it("reattaches when the page left mid-scan, and only reloads the cache after a scan ended", async () => {
    const first = setup();
    await waitFor(() => expect(MockEventSource.last?.url).toContain("/api/scan/stream"));
    first.hook.unmount();
    const before = MockEventSource.instances.length;
    const second = setup();
    await waitFor(() => expect(MockEventSource.instances.length).toBeGreaterThan(before));
    act(() => MockEventSource.last!.emit("done", { count: 1, scanned_at: 2 }));
    await waitFor(() => expect(second.hook.result.current.scan.status).toBe("done"));
    second.hook.unmount();

    const after = MockEventSource.instances.length;
    const third = setup();
    await waitFor(() => expect(third.hook.result.current.scan.status).toBe("done"));
    expect(MockEventSource.instances.length).toBe(after);
  });
});

describe("useScanStream follows the albums snapshot", () => {
  it("takes a snapshot fetched after the scan settled", async () => {
    const { qc, hook } = setup();
    await finishScan();
    await waitFor(() => expect(hook.result.current.scan.status).toBe("done"));
    expect(hook.result.current.albums[0].has_embedded_cover).toBe(false);

    // The server changes the album (cover upload) and the bus invalidates.
    server = [makeAlbum({ id: "a", has_embedded_cover: true, dir_mtime: 99 })];
    await new Promise((r) => setTimeout(r, 5));
    await act(async () => {
      await qc.invalidateQueries({ queryKey: ["albums-snapshot"] });
    });
    await waitFor(() => expect(hook.result.current.albums[0].has_embedded_cover).toBe(true));
    expect(hook.result.current.albums[0].dir_mtime).toBe(99);
    expect(hook.result.current.snapshotSyncs).toBe(1);
  });

  it("drops an album the server no longer lists", async () => {
    const { qc, hook } = setup();
    await finishScan();
    await waitFor(() => expect(hook.result.current.albums).toHaveLength(1));
    server = [];
    await new Promise((r) => setTimeout(r, 5));
    await act(async () => {
      await qc.invalidateQueries({ queryKey: ["albums-snapshot"] });
    });
    await waitFor(() => expect(hook.result.current.albums).toHaveLength(0));
  });

  it("does not replace the list while a scan runs, nor with an older snapshot", async () => {
    const { qc, hook } = setup();
    await waitFor(() => expect(MockEventSource.last?.url).toContain("/api/scan/stream"));
    // A snapshot lands mid-scan with a row the stream has not announced.
    server = [makeAlbum({ id: "stale" })];
    await act(async () => {
      await qc.invalidateQueries({ queryKey: ["albums-snapshot"] });
    });
    act(() => MockEventSource.last!.emit("album", makeAlbum({ id: "fresh" })));
    await new Promise((r) => setTimeout(r, 5));
    act(() => MockEventSource.last!.emit("done", { count: 1, scanned_at: 2 }));
    await waitFor(() => expect(hook.result.current.scan.status).toBe("done"));
    expect(hook.result.current.albums.map((a) => a.id)).toContain("fresh");
    expect(hook.result.current.albums.map((a) => a.id)).not.toContain("stale");
    expect(hook.result.current.snapshotSyncs).toBe(0);
  });
});

describe("useScanStream: names that cannot be stored", () => {
  it("says how many were skipped when the scan finishes", async () => {
    const { hook } = setup();
    await waitFor(() => expect(MockEventSource.last?.url).toContain("/api/scan/stream"));
    act(() => MockEventSource.last!.emit("done", { count: 0, scanned_at: 2, skipped_non_utf8_count: 3 }));
    await waitFor(() => expect(hook.result.current.scan.status).toBe("done"));
    expect(hook.result.current.scan.message).toBe("Library up to date; 3 skipped (name is not valid UTF-8)");
  });

  it("adds nothing when the count is absent", async () => {
    const { hook } = setup();
    await waitFor(() => expect(MockEventSource.last?.url).toContain("/api/scan/stream"));
    act(() => MockEventSource.last!.emit("done", { count: 0, scanned_at: 2 }));
    await waitFor(() => expect(hook.result.current.scan.status).toBe("done"));
    expect(hook.result.current.scan.message).toBe("Library up to date");
  });
});

describe("useScanStream: a change made during a scan", () => {
  it("shows an album changed mid-scan at once, keeps streamed rows the database lacks", async () => {
    server = [
      makeAlbum({ id: "a", has_embedded_cover: false, dir_mtime: 1 }),
      makeAlbum({ id: "b", has_embedded_cover: false, dir_mtime: 1 }),
    ];
    const { qc, hook } = setup();
    await waitFor(() => expect(MockEventSource.last?.url).toContain("/api/scan/stream"));
    await waitFor(() => expect(hook.result.current.albums).toHaveLength(2));
    act(() => MockEventSource.last!.emit("start", { library_dir: "/music", total: 3, cached_count: 2, force: false }));
    act(() => MockEventSource.last!.emit("album", makeAlbum({ id: "new", _status: "fresh" })));
    await new Promise((r) => setTimeout(r, 10));

    // A cover is uploaded for "a" while the scan runs: the database row changes
    // and the bus invalidates the snapshot.
    server = [makeAlbum({ id: "a", has_embedded_cover: true, dir_mtime: 99 }), server[1]];
    await act(async () => {
      await qc.invalidateQueries({ queryKey: ["albums-snapshot"] });
    });

    await waitFor(() => {
      const a = hook.result.current.albums.find((x) => x.id === "a");
      expect(a?.has_embedded_cover).toBe(true);
      expect(a?.dir_mtime).toBe(99);
    });
    // The scan has not ended, and its unsaved row is still shown.
    expect(hook.result.current.scan.status).toBe("scanning");
    expect(hook.result.current.albums.map((a) => a.id)).toContain("new");
  });

  it("does not let an older snapshot row replace one the scan streamed later", async () => {
    const { qc, hook } = setup();
    await waitFor(() => expect(MockEventSource.last?.url).toContain("/api/scan/stream"));
    await waitFor(() => expect(hook.result.current.albums).toHaveLength(1));
    act(() => MockEventSource.last!.emit("start", { library_dir: "/music", total: 1, cached_count: 0, force: false }));
    await act(async () => {
      await qc.invalidateQueries({ queryKey: ["albums-snapshot"] });
    });
    await new Promise((r) => setTimeout(r, 10));
    act(() => MockEventSource.last!.emit("album", makeAlbum({ id: "a", dir_mtime: 50, _status: "fresh" })));
    await new Promise((r) => setTimeout(r, 10));
    expect(hook.result.current.albums[0].dir_mtime).toBe(50);
  });
});

/**
 * A scan that ends without saving (cancelled, or aborted by the backend's
 * sanity check) is not reported as a finished scan: no "done" status, no
 * "Library up to date" message; the list goes back to what the database
 * holds (albums the stream showed but never stored drop out), and a toast
 * says what happened.
 */
describe("useScanStream: scans that end without saving", () => {
  it("cancel: requests it, shows cancelling, then reloads from the cache", async () => {
    const { hook } = setup();
    await waitFor(() => expect(MockEventSource.last?.url).toContain("/api/scan/stream"));
    await waitFor(() => expect(hook.result.current.scan.canCancel).toBe(true));
    act(() => MockEventSource.last!.emit("start", { library_dir: "/music", total: 3, cached_count: 1, force: false }));
    act(() => MockEventSource.last!.emit("album", makeAlbum({ id: "unsaved" })));
    expect(hook.result.current.albums.map((a) => a.id)).toContain("unsaved");

    await act(async () => {
      await hook.result.current.cancel();
    });
    expect(cancelCalls).toBe(1);
    expect(hook.result.current.scan.status).toBe("cancelling");

    act(() => MockEventSource.last!.emit("cancelling", { scanned: 1, total: 3 }));
    act(() =>
      MockEventSource.last!.emit("cancelled", {
        reason: "cancelled",
        phase: "albums",
        scanned: 1,
        total: 3,
        detail: "Scan cancelled after 1 of 3 album(s). Nothing was saved.",
      }),
    );
    expect(MockEventSource.last!.closed).toBe(true);
    await waitFor(() => expect(hook.result.current.albums.map((a) => a.id)).toEqual(["a"]));
    const scan = hook.result.current.scan;
    expect(scan.status).toBe("cancelled");
    expect(scan.canCancel).toBe(false);
    expect(scan.message).toContain("Nothing was saved");
    expect(toast).toHaveBeenCalledWith("Scan cancelled", expect.anything());
  });

  it("a cancel that comes too late leaves the scan to finish", async () => {
    cancelOutcome = "too_late";
    const { hook } = setup();
    await waitFor(() => expect(hook.result.current.scan.canCancel).toBe(true));
    act(() => MockEventSource.last!.emit("start", { library_dir: "/music", total: 1, cached_count: 1, force: false }));
    await act(async () => {
      await hook.result.current.cancel();
    });
    expect(hook.result.current.scan.status).toBe("scanning");
    act(() => MockEventSource.last!.emit("done", { count: 1, scanned_at: 2 }));
    await waitFor(() => expect(hook.result.current.scan.status).toBe("done"));
  });

  it("aborted: warns with the backend's reason and does not report done", async () => {
    const { hook } = setup();
    await waitFor(() => expect(MockEventSource.last?.url).toContain("/api/scan/stream"));
    act(() => MockEventSource.last!.emit("start", { library_dir: "/music", total: 1, cached_count: 12, force: false }));
    act(() => MockEventSource.last!.emit("album", makeAlbum({ id: "unsaved" })));
    act(() =>
      MockEventSource.last!.emit("aborted", {
        reason: "vanish_ratio_exceeded",
        detail: "Scan aborted: 11 of 12 cached album(s) appear to be missing from disk.",
        vanish_count: 11,
        cached_count: 12,
      }),
    );
    await waitFor(() => expect(hook.result.current.albums.map((a) => a.id)).toEqual(["a"]));
    expect(hook.result.current.scan.status).toBe("aborted");
    expect(hook.result.current.scan.message).toContain("missing from disk");
    expect(toast.warning).toHaveBeenCalledWith(
      "Scan stopped without saving",
      expect.objectContaining({ description: expect.stringContaining("missing from disk") }),
    );
    expect(toast).not.toHaveBeenCalled();
  });
});
