/**
 * The Library album list follows ["albums-snapshot"] while no scan runs
 * (public issue: an uploaded cover did not appear until Library was
 * reopened). A snapshot fetched after the list settled replaces it; an
 * older cached snapshot does not; nothing is replaced during a scan.
 */
import { beforeEach, afterEach, describe, it, expect, vi } from "vitest";
import { act, renderHook, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import type { ReactNode } from "react";

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

beforeEach(() => {
  MockEventSource.instances.length = 0;
  requestScanOnNextLoad();
  server = [makeAlbum({ id: "a", has_embedded_cover: false, cover_file: null })];
  vi.stubGlobal(
    "fetch",
    vi.fn(async () =>
      new Response(
        JSON.stringify({ albums: server, count: server.length, library_dir: "/music", scanned_at: 1 }),
        { status: 200, headers: { "content-type": "application/json" } },
      ),
    ),
  );
});
afterEach(() => vi.unstubAllGlobals());

async function finishScan() {
  await waitFor(() => expect(MockEventSource.last?.url).toContain("/api/scan/stream"));
  act(() => MockEventSource.last!.emit("done", { count: server.length, scanned_at: 2 }));
}

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
