/**
 * The album panel's Request research (FR-009) and Cancel (FR-036).
 *
 * Request research is offered for any present album that is not being
 * applied and has no unresolved request, whatever the assistant situation;
 * a job card or a Fix all batch's item (no card of its own, read from the
 * delegated work list) both count as an unresolved request. Cancel on a
 * waiting or running request cancels the research behind the card
 * (`POST /api/ai/jobs/{id}/cancel`) and never deletes the card.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";

vi.mock("sonner", () => ({
  toast: Object.assign(vi.fn(), { error: vi.fn(), success: vi.fn(), warning: vi.fn() }),
}));
vi.mock("@/lib/sse", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/lib/sse")>()),
  postSse: vi.fn(),
}));

import AlbumPanel from "./index";
import { toast } from "sonner";
import { resetApplyRegistryForTests } from "@/lib/applyRegistry";
import { makeAlbum, makeJob } from "@/test/fixtures";
import type { DelegatedWorkItem } from "@/lib/api";
import type { Album, AppConfig, Job } from "@/lib/types";

const ALBUM: Album = makeAlbum({ id: "a1", title: "So Much In Love", has_embedded_cover: true });

function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
}

function workItem(over: Partial<DelegatedWorkItem> = {}): DelegatedWorkItem {
  return {
    workId: "w1",
    albumId: "a1",
    albumLabel: "Artist - So Much In Love",
    kind: "metadata.identify",
    state: "queued",
    revision: 1,
    holder: null,
    lastHeartbeatAt: null,
    leaseExpiresAt: null,
    attempts: 0,
    maxAttempts: 3,
    lastError: null,
    waitingForAgent: true,
    groupId: "g1",
    batchId: "b1",
    ...over,
  };
}

let fetchMock: ReturnType<typeof vi.fn>;
let cancelAnswer: () => Response;

beforeEach(() => {
  resetApplyRegistryForTests();
  cancelAnswer = () => json({ id: "j1", cancelled: 1 });
  fetchMock = vi.fn(async (url: string, init?: RequestInit) => {
    if (url.endsWith("/cancel") && init?.method === "POST") return cancelAnswer();
    return json({ detail: "not found" }, 404);
  });
  vi.stubGlobal("fetch", fetchMock);
  vi.spyOn(window, "confirm").mockReturnValue(true);
});
afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  vi.clearAllMocks();
});

function renderPanel({
  album = ALBUM,
  jobs = [],
  work = [],
}: { album?: Album; jobs?: Job[]; work?: DelegatedWorkItem[] } = {}) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
  qc.setQueryData(["config"], { library_dir: "/music", exists: true } as AppConfig);
  qc.setQueryData(["jobs"], jobs);
  qc.setQueryData(["research", "work"], work);
  qc.setQueryData(["checks"], []);
  qc.setQueryData(["tracks", album.id], { album_id: album.id, tracks: [] });
  return render(
    <QueryClientProvider client={qc}>
      <MemoryRouter>
        <AlbumPanel album={album} onClose={() => {}} />
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

const panel = () => screen.getByRole("complementary");
const requestButton = () => within(panel()).queryByRole("button", { name: "Request research…" });

async function coverMenuItems(): Promise<string[]> {
  fireEvent.keyDown(within(panel()).getByRole("button", { name: "Change cover" }), { key: "Enter" });
  const menu = await screen.findByRole("menu");
  return within(menu).getAllByRole("menuitem").map((i) => i.textContent?.trim() ?? "");
}

const deleteCalls = () =>
  fetchMock.mock.calls.filter(([, init]) => (init as RequestInit | undefined)?.method === "DELETE");

describe("Request research", () => {
  it("is offered for a present, idle album with no request", async () => {
    renderPanel();
    expect(requestButton()).toBeEnabled();
    expect(requestButton()).not.toHaveAttribute("title");
    expect(await coverMenuItems()).toContain("Request cover research…");
  });

  it("is not offered while a job card has an unresolved request", async () => {
    renderPanel({ jobs: [makeJob({ id: "j1", album_id: "a1", job_type: "metadata", status: "queued" })] });
    expect(requestButton()).toBeDisabled();
    expect(requestButton()).toHaveAttribute("title", "Research is already requested for this album");
    expect(await coverMenuItems()).not.toContain("Request cover research…");
  });

  it("is not offered while a batched request (no job card) is unresolved", async () => {
    for (const state of ["queued", "claimed", "running", "awaiting_review"] as const) {
      const { unmount } = renderPanel({ work: [workItem({ state })] });
      expect(requestButton()).toBeDisabled();
      expect(requestButton()).toHaveAttribute("title", "Research is already requested for this album");
      unmount();
    }
  });

  it("is offered again once a batched request is finished, or when it is another album's", () => {
    const { unmount } = renderPanel({ work: [workItem({ state: "failed" })] });
    expect(requestButton()).toBeEnabled();
    unmount();
    renderPanel({ work: [workItem({ albumId: "other" })] });
    expect(requestButton()).toBeEnabled();
  });

  it("is not offered on a Missing album", () => {
    renderPanel({ album: { ...ALBUM, missing_since: "2026-11-01T10:00:00Z" } });
    expect(requestButton()).toBeNull();
  });
});

describe("Cancel on the album panel's request (FR-036)", () => {
  it("cancels a queued job card's research and never deletes the card", async () => {
    renderPanel({ jobs: [makeJob({ id: "j1", album_id: "a1", job_type: "metadata", status: "queued" })] });
    fireEvent.click(within(panel()).getByRole("button", { name: "Cancel" }));
    await waitFor(() =>
      expect(fetchMock).toHaveBeenCalledWith("/api/ai/jobs/j1/cancel", expect.objectContaining({ method: "POST" })),
    );
    expect(deleteCalls()).toHaveLength(0);
    expect(window.confirm).toHaveBeenCalledWith(expect.stringMatching(/^Cancel this research request\?/));
  });

  it("cancels a running job card's research too", async () => {
    renderPanel({ jobs: [makeJob({ id: "j1", album_id: "a1", job_type: "metadata", status: "running" })] });
    fireEvent.click(within(panel()).getByRole("button", { name: "Cancel" }));
    await waitFor(() =>
      expect(fetchMock).toHaveBeenCalledWith("/api/ai/jobs/j1/cancel", expect.objectContaining({ method: "POST" })),
    );
    expect(deleteCalls()).toHaveLength(0);
  });

  it("cancels a research card's every unresolved request in one action", async () => {
    const item = (id: string, kind: string, state: string) => ({
      id, kind, state, revision: 1, validation: null, digest: null, note: null,
      historicalResult: null, result: null,
    });
    fetchMock.mockImplementation(async (url: string, init?: RequestInit) => {
      if (url.endsWith("/cancel") && init?.method === "POST") return json({ id: "g7", cancelled: 2 });
      if (url === "/api/research/groups/g7")
        return json([item("r1", "metadata.identify", "queued"), item("r2", "cover.research", "claimed")]);
      return json({ detail: "not found" }, 404);
    });
    const invalidate = vi.spyOn(QueryClient.prototype, "invalidateQueries");
    renderPanel({
      jobs: [
        makeJob({
          id: "g7", album_id: "a1", job_type: "metadata+cover", status: "running",
          result: { delegated: true } as Job["result"],
        }),
      ],
    });
    fireEvent.click(await within(panel()).findByRole("button", { name: "Cancel request" }));
    await waitFor(() =>
      expect(fetchMock).toHaveBeenCalledWith("/api/ai/jobs/g7/cancel", expect.objectContaining({ method: "POST" })),
    );
    expect(deleteCalls()).toHaveLength(0);
    expect(fetchMock.mock.calls.some(([u]) => String(u).includes("/decision"))).toBe(false);
    // The `work` events refresh the card (rule 5).
    expect(invalidate).not.toHaveBeenCalled();
  });

  it("cancels a batched request through its group", async () => {
    renderPanel({ work: [workItem({ groupId: "g1", state: "queued" })] });
    fireEvent.click(within(panel()).getByRole("button", { name: "Cancel" }));
    await waitFor(() =>
      expect(fetchMock).toHaveBeenCalledWith("/api/ai/jobs/g1/cancel", expect.objectContaining({ method: "POST" })),
    );
    expect(deleteCalls()).toHaveLength(0);
  });

  it("offers no Cancel for a batched request awaiting review", () => {
    renderPanel({ work: [workItem({ state: "awaiting_review" })] });
    expect(within(panel()).queryByRole("button", { name: "Cancel" })).toBeNull();
    expect(within(panel()).getByRole("link", { name: "View in Review" })).toBeInTheDocument();
  });

  it("shows the server's message when nothing is left to cancel (409)", async () => {
    cancelAnswer = () => json({ detail: "Nothing is waiting or in progress to cancel" }, 409);
    renderPanel({ jobs: [makeJob({ id: "j1", album_id: "a1", job_type: "metadata", status: "queued" })] });
    fireEvent.click(within(panel()).getByRole("button", { name: "Cancel" }));
    await waitFor(() =>
      expect(toast.warning).toHaveBeenCalledWith("Nothing is waiting or in progress to cancel"),
    );
    expect(deleteCalls()).toHaveLength(0);
  });

  it("does nothing when the confirm is declined", () => {
    vi.mocked(window.confirm).mockReturnValue(false);
    renderPanel({ jobs: [makeJob({ id: "j1", album_id: "a1", job_type: "metadata", status: "queued" })] });
    fireEvent.click(within(panel()).getByRole("button", { name: "Cancel" }));
    expect(fetchMock).not.toHaveBeenCalledWith(expect.stringMatching(/\/cancel$/), expect.anything());
  });
});
