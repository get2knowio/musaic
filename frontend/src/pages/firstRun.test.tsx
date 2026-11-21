/**
 * From folder selection to the first scan and recovery (#328).
 *
 *  - Desktop: Choose music folder… is the one primary action and saves at
 *    once; a typed path is a link away and still works. In a browser the
 *    path field is the way in.
 *  - After the first scan: the album count, whether checks are running,
 *    Review findings once there is something to review; the AcoustID
 *    suggestion stays secondary. A cancelled or aborted scan says so and
 *    offers Scan again, never "complete".
 *  - A folder with no FLAC albums offers Choose another folder… in place.
 *  - An unavailable folder leads with Try again; Change folder… is a
 *    separate, secondary action.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter, Route, Routes } from "react-router-dom";

vi.mock("sonner", () => ({
  toast: Object.assign(vi.fn(), { error: vi.fn(), success: vi.fn(), warning: vi.fn() }),
}));

import LibraryPage from "@/pages/Library";
import { requestScanOnNextLoad } from "@/hooks/useScanStream";
import { markFirstScan } from "@/lib/firstScan";
import { resetScanActivity } from "@/lib/scanActivity";
import { MockEventSource } from "@/test/setup";
import { makeAlbum } from "@/test/fixtures";
import type { Album, AppConfig, CheckSweepStatus, Finding } from "@/lib/types";

const PICKED = "/srv/music/FLAC";

interface Call {
  url: string;
  method: string;
  body: unknown;
}

let calls: Call[];
let serverAlbums: Album[];
let serverDir: string;

function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
}

beforeEach(() => {
  calls = [];
  serverAlbums = [];
  serverDir = PICKED;
  sessionStorage.clear();
  resetScanActivity();
  requestScanOnNextLoad();
  MockEventSource.instances.length = 0;
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string, init?: RequestInit) => {
      const method = init?.method ?? "GET";
      calls.push({ url: String(url), method, body: init?.body ? JSON.parse(String(init.body)) : undefined });
      if (url === "/__musaic/dialog/folder" && method === "POST") return json({ path: PICKED });
      if (url === "/api/config" && method === "POST") return json({ library_dir: PICKED, exists: true });
      if (String(url).startsWith("/api/albums?") || url === "/api/albums") {
        return json({ albums: serverAlbums, count: serverAlbums.length, library_dir: serverDir, scanned_at: null });
      }
      if (String(url).startsWith("/api/scan/rescan")) return json({ status: "started" });
      return json({ detail: "not found" }, 404);
    }),
  );
});
afterEach(() => {
  vi.unstubAllGlobals();
  delete (window as { __musaicDesktop?: unknown }).__musaicDesktop;
});

const desktop = () => {
  (window as { __musaicDesktop?: unknown }).__musaicDesktop = Object.freeze({ folderPicker: true });
};

const finding = (id: number): Finding => ({
  id,
  check_id: "track-gaps",
  album_id: "a1",
  severity: "warn",
  summary: `gap ${id}`,
  detail: null,
  first_seen_at: "2026-01-01T00:00:00Z",
  last_seen_at: "2026-01-01T00:00:00Z",
  resolved_at: null,
  ignored_at: null,
  ignored_reason: null,
});

const sweep = (status: CheckSweepStatus["status"]): CheckSweepStatus => ({
  status,
  total: 4,
  completed: status === "idle" ? 4 : 1,
  current_check: null,
  started_at: null,
  completed_at: null,
});

function client(config: AppConfig, opts: { findings?: Finding[]; sweep?: CheckSweepStatus } = {}): QueryClient {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
  qc.setQueryData(["config"], config);
  qc.setQueryData(["ai-providers"], { selected: null, providers: [], uninstalled: [] });
  qc.setQueryData(["jobs"], []);
  qc.setQueryData(["checks"], []);
  qc.setQueryData(["findings"], opts.findings ?? []);
  qc.setQueryData(["albums-snapshot"], serverAlbums);
  qc.setQueryData(["settings"], { acoustid_api_key_set: false });
  qc.setQueryData(["acoustid-prompt"], { dismissed: false });
  if (opts.sweep) qc.setQueryData(["check-sweep-status"], opts.sweep);
  return qc;
}

function renderLibrary(qc: QueryClient) {
  return render(
    <QueryClientProvider client={qc}>
      <MemoryRouter initialEntries={["/library"]}>
        <Routes>
          <Route path="/library" element={<LibraryPage />} />
          <Route path="/review" element={<div>REVIEW PAGE</div>} />
        </Routes>
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

const field = () => screen.queryByRole("textbox", { name: "Library directory path" });
const scanStream = async () => {
  await waitFor(() => expect(MockEventSource.last?.url).toContain("/api/scan/stream"));
  return MockEventSource.last!;
};
const report = () => screen.queryByRole("status", { name: "First scan" });

describe("choosing the folder", () => {
  it("desktop: Choose music folder… is the primary action; a typed path is a link away and still saves", async () => {
    desktop();
    renderLibrary(client({ library_dir: null, exists: false } as AppConfig));
    const choose = screen.getByRole("button", { name: "Choose music folder…" });
    expect(choose.className).toContain("bg-accent");
    expect(field()).toBeNull();
    expect(screen.queryByRole("button", { name: "Save" })).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "Enter a path manually" }));
    fireEvent.change(field()!, { target: { value: PICKED } });
    const save = screen.getByRole("button", { name: "Save" });
    expect(save.className).not.toContain("bg-accent");
    fireEvent.click(save);
    await scanStream();
    expect(calls.find((c) => c.url === "/api/config" && c.method === "POST")?.body).toEqual({ library_dir: PICKED });
    expect(calls.some((c) => c.url === "/__musaic/dialog/folder")).toBe(false);
  });

  it("browser: the path field is the way in, with Save as its primary action", () => {
    renderLibrary(client({ library_dir: null, exists: false } as AppConfig));
    expect(field()).not.toBeNull();
    expect(screen.queryByRole("button", { name: /Choose .*folder/ })).toBeNull();
    expect(screen.queryByRole("button", { name: "Enter a path manually" })).toBeNull();
    expect(screen.getByRole("button", { name: "Save" }).className).toContain("bg-accent");
  });
});

describe("after the first scan", () => {
  const configured = { library_dir: PICKED, exists: true } as AppConfig;

  it("reports the album count and that checks are running; the AcoustID suggestion stays secondary", async () => {
    serverAlbums = [makeAlbum({ id: "a1" }), makeAlbum({ id: "a2" })];
    markFirstScan();
    renderLibrary(client(configured, { sweep: sweep("running") }));
    act(() => {
      void scanStream().then((s) => s.emit("done", { count: 2, scanned_at: "2026-01-01T00:00:00Z" }));
    });
    await waitFor(() => expect(report()).not.toBeNull());
    expect(report()).toHaveTextContent("Scan complete: 2 albums in your library.");
    expect(report()).toHaveTextContent("Checks are running");
    expect(within(report()!).queryByRole("button", { name: "Review findings" })).toBeNull();

    const prompt = screen.getByRole("region", { name: "Improve matching" });
    expect(within(prompt).getByRole("button", { name: "Add a key in Settings" }).className).not.toContain(
      "bg-accent",
    );
  });

  it("offers Review findings once checks found something", async () => {
    serverAlbums = [makeAlbum({ id: "a1" })];
    markFirstScan();
    renderLibrary(client(configured, { sweep: sweep("idle"), findings: [finding(1), finding(2)] }));
    const stream = await scanStream();
    act(() => stream.emit("done", { count: 1, scanned_at: "2026-01-01T00:00:00Z" }));
    await waitFor(() => expect(report()).toHaveTextContent("Scan complete: 1 album in your library."));
    expect(report()).toHaveTextContent("Checks found 2 items to review.");
    fireEvent.click(within(report()!).getByRole("button", { name: "Review findings" }));
    expect(screen.getByText("REVIEW PAGE")).toBeInTheDocument();
    expect(sessionStorage.getItem("mm.library.firstScan")).toBeNull();
  });

  it("a cancelled scan keeps its result: nothing saved, Scan again, never 'complete'", async () => {
    markFirstScan();
    renderLibrary(client(configured, { sweep: sweep("idle"), findings: [finding(1)] }));
    const stream = await scanStream();
    act(() => stream.emit("cancelled", { detail: "Scan cancelled" }));
    await waitFor(() => expect(report()).not.toBeNull());
    expect(report()).toHaveTextContent("Scan cancelled. Nothing was saved.");
    expect(report()).not.toHaveTextContent(/complete/);
    expect(within(report()!).queryByRole("button", { name: "Review findings" })).toBeNull();
    // An empty list after a cancel is not "no FLAC albums".
    expect(screen.queryByText("No FLAC albums in this folder")).toBeNull();
    fireEvent.click(within(report()!).getByRole("button", { name: "Scan again" }));
    await waitFor(() => expect(calls.some((c) => c.url.startsWith("/api/scan/rescan"))).toBe(true));
  });

  it("an aborted scan says why and what the library still shows", async () => {
    serverAlbums = [makeAlbum({ id: "a1" }), makeAlbum({ id: "a2" }), makeAlbum({ id: "a3" })];
    markFirstScan();
    renderLibrary(client(configured));
    const stream = await scanStream();
    act(() => stream.emit("aborted", { detail: "Too many albums vanished at once" }));
    await waitFor(() => expect(report()).not.toBeNull());
    expect(report()).toHaveTextContent(
      "Scan stopped without saving. Too many albums vanished at once. The library still shows its previous scan (3 albums).",
    );
  });

  it("is dismissed for good", async () => {
    serverAlbums = [makeAlbum({ id: "a1" })];
    markFirstScan();
    renderLibrary(client(configured));
    const stream = await scanStream();
    act(() => stream.emit("done", { count: 1, scanned_at: "2026-01-01T00:00:00Z" }));
    await waitFor(() => expect(report()).not.toBeNull());
    fireEvent.click(within(report()!).getByRole("button", { name: "Dismiss" }));
    expect(report()).toBeNull();
    expect(sessionStorage.getItem("mm.library.firstScan")).toBeNull();
  });

  it("only follows a newly chosen folder", async () => {
    serverAlbums = [makeAlbum({ id: "a1" })];
    renderLibrary(client(configured));
    const stream = await scanStream();
    act(() => stream.emit("done", { count: 1, scanned_at: "2026-01-01T00:00:00Z" }));
    await waitFor(() => expect(document.querySelectorAll("[data-album-id]").length).toBe(1));
    expect(report()).toBeNull();
  });
});

describe("a folder with no FLAC albums", () => {
  it("offers Choose another folder… in place (desktop)", async () => {
    desktop();
    renderLibrary(client({ library_dir: "/srv/empty", exists: true } as AppConfig));
    const stream = await scanStream();
    act(() => stream.emit("done", { count: 0, scanned_at: "2026-01-01T00:00:00Z" }));
    expect(await screen.findByRole("heading", { name: "No FLAC albums in this folder" })).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Choose another folder…" }));
    await waitFor(() =>
      expect(calls.find((c) => c.url === "/api/config" && c.method === "POST")?.body).toEqual({
        library_dir: PICKED,
      }),
    );
  });

  it("offers Choose another folder… in place (browser: the path field)", async () => {
    serverDir = "/srv/empty";
    renderLibrary(client({ library_dir: "/srv/empty", exists: true } as AppConfig));
    const stream = await scanStream();
    act(() => stream.emit("done", { count: 0, scanned_at: "2026-01-01T00:00:00Z" }));
    fireEvent.click(await screen.findByRole("button", { name: "Choose another folder…" }));
    expect((field() as HTMLInputElement).value).toBe("/srv/empty");
  });
});

describe("an unavailable folder", () => {
  it("leads with Try again; Change folder… is separate and secondary (desktop)", () => {
    desktop();
    renderLibrary(client({ library_dir: "/Volumes/Music", exists: false } as AppConfig));
    const retry = screen.getByRole("button", { name: "Try again" });
    const change = screen.getByRole("button", { name: "Change folder…" });
    expect(retry.className).toContain("bg-accent");
    expect(change.className).not.toContain("bg-accent");
    expect(retry.compareDocumentPosition(change) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(field()).toBeNull();
    // A typed path still works.
    fireEvent.click(screen.getByRole("button", { name: "Enter a path manually" }));
    expect((field() as HTMLInputElement).value).toBe("/Volumes/Music");
  });
});
