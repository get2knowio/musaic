/**
 * The album panel for a Missing album (#195): one message, one action.
 * Sections the backend cannot serve without the files (tracks, analysis,
 * My Tags, findings, corrections, the Cover control) are absent; "Remove from
 * library" asks in a dialog, then sends DELETE /api/albums/{id} and closes.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";

vi.mock("sonner", () => ({
  toast: Object.assign(vi.fn(), { error: vi.fn(), success: vi.fn(), warning: vi.fn() }),
}));

import AlbumPanel from "./index";
import { resetApplyRegistryForTests } from "@/lib/applyRegistry";
import { makeAlbum, makeJob } from "@/test/fixtures";
import type { Album, AppConfig, Job } from "@/lib/types";
import { toast } from "sonner";

const MISSING: Album = makeAlbum({
  id: "gone",
  title: "Gone Album",
  path: "/music/Artist - Gone Album [FLAC]",
  track_count: 4,
  missing_since: "2026-11-01T10:00:00Z",
});

function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
}

let fetchMock: ReturnType<typeof vi.fn>;

beforeEach(() => {
  resetApplyRegistryForTests();
  fetchMock = vi.fn(async (url: string, init?: RequestInit) => {
    if (url === "/api/albums/gone" && init?.method === "DELETE") {
      return json({ ok: true, deleted_album_id: "gone" });
    }
    return json({ detail: "not found" }, 404);
  });
  vi.stubGlobal("fetch", fetchMock);
});
afterEach(() => {
  vi.unstubAllGlobals();
  vi.clearAllMocks();
});

function renderPanel(album: Album, jobs: Job[] = []) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
  qc.setQueryData(["config"], { library_dir: "/music", exists: true } as AppConfig);
  qc.setQueryData(["jobs"], jobs);
  const onClose = vi.fn();
  render(
    <QueryClientProvider client={qc}>
      <MemoryRouter>
        <AlbumPanel album={album} onClose={onClose} />
      </MemoryRouter>
    </QueryClientProvider>,
  );
  return { onClose };
}

const panel = () => screen.getByRole("complementary");
const deleteCalls = () =>
  fetchMock.mock.calls.filter(([, init]) => (init as RequestInit | undefined)?.method === "DELETE");

describe("album panel for a Missing album", () => {
  it("says once that the folder is missing, where and since when, without promising an automatic return", () => {
    renderPanel(MISSING);
    const notice = within(panel()).getByRole("status");
    expect(notice).toHaveTextContent("Folder not found");
    expect(notice).toHaveTextContent(
      `The library scan on ${new Date(MISSING.missing_since!).toLocaleDateString()} did not find this album at /music/Artist - Gone Album [FLAC].`,
    );
    expect(notice).toHaveTextContent("If the folder is back at that path, the next scan brings the album back");
    expect(notice).toHaveTextContent("A folder moved or renamed inside the library is added as a separate album");
    expect(panel()).not.toHaveTextContent(/automatically|no action needed|Re-mount/i);
  });

  it("shows only what is stored: no tracks, analysis, My Tags, findings or cover upload", () => {
    renderPanel(MISSING);
    expect(within(panel()).getByText("Album info")).toBeInTheDocument();
    // The hero keeps the stored track count; nothing contradicts it.
    expect(panel()).toHaveTextContent("4 tracks");
    expect(within(panel()).queryByText("Tracks")).toBeNull();
    expect(panel()).not.toHaveTextContent("0 tracks");
    expect(within(panel()).queryByText("Audio analysis")).toBeNull();
    expect(within(panel()).queryByText("My Tags")).toBeNull();
    expect(within(panel()).queryByRole("button", { name: /cover/i })).toBeNull();
    expect(within(panel()).queryByRole("button", { name: /Apply|Discard|Request research/ })).toBeNull();
    // The hero shows the cached thumbnail, not the full cover read from disk.
    const img = panel().querySelector("img");
    expect(img?.getAttribute("src")).toMatch(/^\/api\/albums\/gone\/cover(\?|$)/);
    expect(img?.getAttribute("src")).not.toContain("size=full");
  });

  it("does not offer a job's result or a retry: neither could be applied", () => {
    renderPanel(MISSING, [
      makeJob({
        id: "j-failed",
        album_id: "gone",
        status: "failed" as Job["status"],
        error: "boom",
      }),
    ]);
    expect(within(panel()).queryByText("Last attempt failed")).toBeNull();
    expect(within(panel()).queryByRole("button", { name: /Retry/ })).toBeNull();
  });

  it("Remove asks first; Cancel sends nothing and returns focus to the button", async () => {
    renderPanel(MISSING);
    const remove = within(panel()).getByRole("button", { name: "Remove from library…" });
    remove.focus();
    fireEvent.click(remove);

    const dialog = await screen.findByRole("dialog", { name: "Remove “Gone Album” from Musaic?" });
    expect(dialog).toHaveTextContent("This deletes Musaic’s record of the album from its database");
    expect(dialog).toHaveTextContent("its MusicBrainz match, and the matches you rejected for it");
    expect(dialog).toHaveTextContent("its audio fingerprints");
    expect(dialog).toHaveTextContent("the My Tags on its tracks");
    expect(dialog).toHaveTextContent("Your music files and folders are not touched.");
    expect(dialog).not.toHaveTextContent(/blocklist/i);
    // A destructive confirm starts on Cancel.
    const cancel = within(dialog).getByRole("button", { name: "Cancel" });
    expect(document.activeElement).toBe(cancel);

    fireEvent.click(cancel);
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(deleteCalls()).toHaveLength(0);
    expect(document.activeElement).toBe(remove);
  });

  it("confirming sends DELETE /api/albums/{id} and closes the panel", async () => {
    const { onClose } = renderPanel(MISSING);
    fireEvent.click(within(panel()).getByRole("button", { name: "Remove from library…" }));
    const dialog = await screen.findByRole("dialog");
    fireEvent.click(within(dialog).getByRole("button", { name: "Remove from library" }));

    await waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
    expect(deleteCalls()).toHaveLength(1);
    expect(deleteCalls()[0][0]).toBe("/api/albums/gone");
    expect(toast.success).toHaveBeenCalledWith("Removed from library", expect.anything());
  });

  it("a failed removal keeps the panel and the dialog open and says why", async () => {
    fetchMock.mockImplementation(async () => json({ detail: "Album 'gone' not in cache" }, 404));
    const { onClose } = renderPanel(MISSING);
    fireEvent.click(within(panel()).getByRole("button", { name: "Remove from library…" }));
    const dialog = await screen.findByRole("dialog");
    fireEvent.click(within(dialog).getByRole("button", { name: "Remove from library" }));

    await waitFor(() => expect(toast.error).toHaveBeenCalled());
    expect(onClose).not.toHaveBeenCalled();
    expect(screen.getByRole("dialog")).toBeInTheDocument();
  });

  it("a Present album shows none of this", () => {
    renderPanel(makeAlbum({ id: "here", title: "Here" }));
    expect(within(panel()).queryByRole("status")).toBeNull();
    expect(within(panel()).queryByRole("button", { name: /Remove from library/ })).toBeNull();
    expect(within(panel()).getByText("Tracks")).toBeInTheDocument();
    expect(within(panel()).getByRole("button", { name: "Change cover" })).toBeInTheDocument();
  });
});
