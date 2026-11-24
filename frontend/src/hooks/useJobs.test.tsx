/**
 * The Library's per-album status: job cards, plus the requests of a Fix all
 * batch, which have no card and come from the delegated work list. A batched
 * album gets the same pill as any album with a request, in every active state.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { render, renderHook, screen } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";
import type { ReactNode } from "react";

import TileGrid from "@/components/TileGrid";
import { useActiveJobAlbumMap, useActiveJobForAlbum } from "@/hooks/useJobs";
import { buildReviewItems } from "@/hooks/useReview";
import { buildNeedsAttentionSet, isHistoryOnlyJob } from "@/lib/filters";
import type { DelegatedWorkItem, DelegatedWorkState } from "@/lib/api";
import { makeAlbum, makeJob } from "@/test/fixtures";
import type { Job } from "@/lib/types";

const workItem = (over: Partial<DelegatedWorkItem>): DelegatedWorkItem => ({
  workId: `w-${over.albumId ?? "x"}-${over.kind ?? "m"}`,
  albumId: "x",
  albumLabel: "Artist - Title",
  kind: "metadata.identify",
  state: "queued",
  revision: 1,
  holder: null,
  lastHeartbeatAt: null,
  leaseExpiresAt: null,
  attempts: 0,
  maxAttempts: 3,
  lastError: null,
  waitingForAgent: false,
  batchId: "11111111-2222-4333-8444-555555555555",
  ...over,
});

beforeEach(() => {
  // Every read here is seeded; a request would mean a key moved.
  vi.stubGlobal("fetch", vi.fn(async () => new Response("{}", { status: 404 })));
});
afterEach(() => vi.unstubAllGlobals());

function client(jobs: Job[], work: DelegatedWorkItem[]) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
  qc.setQueryData(["jobs"], jobs);
  qc.setQueryData(["research", "work"], work);
  return qc;
}

function mapFor(jobs: Job[], work: DelegatedWorkItem[]) {
  const qc = client(jobs, work);
  const wrapper = ({ children }: { children: ReactNode }) => (
    <QueryClientProvider client={qc}>{children}</QueryClientProvider>
  );
  return renderHook(() => useActiveJobAlbumMap(), { wrapper }).result.current;
}

describe("useActiveJobAlbumMap with batched requests", () => {
  const cases: [DelegatedWorkState, Job["status"]][] = [
    ["queued", "queued"],
    ["claimed", "running"],
    ["running", "running"],
    ["awaiting_review", "completed"],
  ];
  for (const [state, status] of cases) {
    it(`a batched ${state} request gives its album the ${status} pill`, () => {
      const map = mapFor([], [workItem({ albumId: "a", state })]);
      expect(map.get("a")?.status).toBe(status);
      expect(map.get("a")?.album_id).toBe("a");
    });
  }

  it("finished, failed and cancelled batched requests give no pill", () => {
    const map = mapFor([], [
      workItem({ albumId: "f", state: "failed" }),
      workItem({ albumId: "c", state: "cancelled" }),
    ]);
    expect(map.size).toBe(0);
  });

  it("a request with no batch is left to its job card", () => {
    const map = mapFor([], [workItem({ albumId: "solo", state: "queued", batchId: null })]);
    expect(map.has("solo")).toBe(false);
  });

  it("a job card wins over a batched request of the same album", () => {
    const card = makeJob({ id: "card", album_id: "a", status: "queued" });
    const map = mapFor([card], [workItem({ albumId: "a", state: "awaiting_review" })]);
    expect(map.get("a")?.id).toBe("card");
  });

  it("one album with a metadata and a cover request shows the more advanced one, as both kinds", () => {
    const map = mapFor([], [
      workItem({ albumId: "a", kind: "metadata.identify", state: "queued" }),
      workItem({ albumId: "a", kind: "cover.research", state: "running" }),
    ]);
    expect(map.get("a")?.status).toBe("running");
    expect(map.get("a")?.job_type).toBe("metadata+cover");
  });

  it("the Library tile of a batched album shows the pill", () => {
    const qc = client([], [workItem({ albumId: "a", state: "awaiting_review" })]);
    function Grid() {
      const activeJobs = useActiveJobAlbumMap();
      return <TileGrid albums={[makeAlbum({ id: "a" }), makeAlbum({ id: "b", path: "/music/B" })]} activeJobs={activeJobs} />;
    }
    render(
      <QueryClientProvider client={qc}>
        <MemoryRouter>
          <Grid />
        </MemoryRouter>
      </QueryClientProvider>,
    );
    expect(screen.getAllByText("Ready")).toHaveLength(1);
  });
});

/** An old job of a type no research request matches (the v24 fold keeps it
 *  as a history-only record, listed with its stored status) is history only:
 *  never active, never to review, and it blocks nothing (FR-020). */
describe("an old job's history-only record", () => {
  const lyrics = (status: Job["status"]) =>
    makeJob({ id: `old-${status}`, album_id: "a", job_type: "lyrics" as Job["job_type"], status });

  for (const status of ["queued", "running", "completed"] as const) {
    it(`left ${status}, it gives its album no pill and is not the album's active request`, () => {
      expect(mapFor([lyrics(status)], []).has("a")).toBe(false);
      const qc = client([lyrics(status)], []);
      const wrapper = ({ children }: { children: ReactNode }) => (
        <QueryClientProvider client={qc}>{children}</QueryClientProvider>
      );
      expect(renderHook(() => useActiveJobForAlbum("a"), { wrapper }).result.current).toBeNull();
    });
  }

  it("is neither to review nor in the needs-attention set, and is listed as history", () => {
    const jobs = [lyrics("completed"), lyrics("failed")];
    expect(buildReviewItems([], jobs)).toEqual([]);
    expect(buildNeedsAttentionSet(jobs).size).toBe(0);
    expect(isHistoryOnlyJob(lyrics("queued"))).toBe(true);
    expect(isHistoryOnlyJob(makeJob({ job_type: "metadata+cover" }))).toBe(false);
  });
});
