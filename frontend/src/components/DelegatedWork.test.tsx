/**
 * Review, Delegated work: grouped by state, the holder and the times of held
 * work, "Waiting for an assistant", Retry through the existing decision call, and
 * Retry reachable by keyboard. Requests of a Fix all batch fold into one row
 * per batch with its counts, which opens to its items and cancels as a whole.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";

vi.mock("sonner", () => ({
  toast: Object.assign(vi.fn(), { error: vi.fn(), success: vi.fn(), warning: vi.fn() }),
}));

import { DelegatedWork } from "@/components/DelegatedWork";
import type { BatchDetail, BatchSummary, DelegatedWorkItem } from "@/lib/api";

const item = (over: Partial<DelegatedWorkItem>): DelegatedWorkItem => ({
  workId: "w1",
  albumId: "0123456789abcdef",
  albumLabel: "Example Artist - Sample Record",
  kind: "metadata.identify",
  state: "queued",
  revision: 3,
  holder: null,
  lastHeartbeatAt: null,
  leaseExpiresAt: null,
  attempts: 0,
  maxAttempts: 3,
  lastError: null,
  waitingForAgent: false,
  ...over,
});

const batch = (over: Partial<BatchSummary>): BatchSummary => ({
  batchId: "11111111-2222-4333-8444-555555555555",
  checkId: "unidentified-album",
  startedAt: new Date().toISOString(),
  finishedAt: null,
  unfinished: false,
  size: 3,
  counts: { queued: 2, awaiting_review: 1 },
  ...over,
});

let calls: { url: string; init?: RequestInit }[] = [];
let answers: Record<string, unknown> = {};

beforeEach(() => {
  calls = [];
  answers = {};
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string, init?: RequestInit) => {
      calls.push({ url: String(url), init });
      const body = String(url) in answers ? answers[String(url)] : { ok: true };
      return new Response(JSON.stringify(body), { status: 200, headers: { "content-type": "application/json" } });
    }),
  );
});
afterEach(() => vi.unstubAllGlobals());

function renderWork(items: DelegatedWorkItem[] | null, batches: BatchSummary[] = []) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
  if (items) qc.setQueryData(["research", "work"], items);
  qc.setQueryData(["research", "batches"], batches);
  qc.setQueryData(["checks"], [
    { meta: { id: "unidentified-album", label: "Unidentified album" }, open_count: 0, last_run: null },
  ]);
  render(
    <QueryClientProvider client={qc}>
      <MemoryRouter>
        <DelegatedWork />
      </MemoryRouter>
    </QueryClientProvider>,
  );
  return qc;
}

describe("Delegated work", () => {
  it("shows nothing when there is no delegated work", () => {
    renderWork([]);
    expect(screen.queryByText("Delegated work")).toBeNull();
  });

  it("groups by state and labels a running item Investigating", () => {
    renderWork([
      item({ workId: "a", state: "running", holder: "Alice", lastHeartbeatAt: new Date().toISOString(), leaseExpiresAt: new Date(Date.now() + 240_000).toISOString() }),
      item({ workId: "b", state: "queued", waitingForAgent: true }),
      item({ workId: "c", state: "failed", attempts: 3, lastError: "gave_up: no source" }),
      item({ workId: "d", state: "cancelled" }),
      item({ workId: "e", state: "awaiting_review" }),
    ]);
    const heads = screen.getAllByRole("heading", { level: 3 }).map((h) => h.textContent?.replace(/\s*\(\d+\)/, ""));
    expect(heads).toEqual(["Investigating", "Waiting", "Result waiting for you", "Failed", "Cancelled"]);
    const running = within(screen.getByRole("list", { name: "Investigating" }));
    expect(running.getByText(/Held by Alice/)).toBeTruthy();
    expect(running.getByText(/Last heard from just now/)).toBeTruthy();
    expect(running.getByText(/Claim lapses/)).toBeTruthy();
    expect(running.getAllByRole("link")[0].getAttribute("href")).toBe("/library/0123456789abcdef");
    expect(screen.getByText(/Waiting for an assistant\. No connected assistant is listening/)).toBeTruthy();
    expect(screen.getByText(/Attempt 3 of 3: gave_up: no source/)).toBeTruthy();
  });

  it("Retry is a button reached by keyboard on failed and cancelled work only, and sends the existing decision", async () => {
    renderWork([
      item({ workId: "c", state: "failed", revision: 7 }),
      item({ workId: "d", state: "cancelled", revision: 2 }),
      item({ workId: "q", state: "queued" }),
    ]);
    const retries = screen.getAllByRole("button", { name: /Retry/ });
    expect(retries).toHaveLength(2);
    retries[0].focus();
    expect(document.activeElement).toBe(retries[0]);
    fireEvent.click(retries[0]);
    await waitFor(() => expect(calls).toHaveLength(1));
    expect(calls[0].url).toBe("/api/research/c/decision");
    expect(calls[0].init?.method).toBe("POST");
    expect(JSON.parse(String(calls[0].init?.body))).toEqual({ revision: 7, action: "retry" });
  });

  it("the Retry handler does not invalidate any cache itself (rule 5)", async () => {
    const qc = renderWork([item({ workId: "c", state: "failed" })]);
    const spy = vi.spyOn(qc, "invalidateQueries");
    fireEvent.click(screen.getByRole("button", { name: /Retry/ }));
    await waitFor(() => expect(calls).toHaveLength(1));
    expect(spy).not.toHaveBeenCalled();
  });

  describe("Fix all batches", () => {
    const B = batch({}).batchId;
    const batched = [
      item({ workId: "x1", albumId: "aaaaaaaaaaaaaaaa", albumLabel: "First - One", state: "queued", batchId: B }),
      item({ workId: "x2", albumId: "bbbbbbbbbbbbbbbb", albumLabel: "Second - Two", state: "queued", batchId: B }),
      item({ workId: "x3", albumId: "cccccccccccccccc", albumLabel: "Third - Three", state: "awaiting_review", batchId: B }),
    ];

    it("folds a batch's items into one row with its origin, size and counts", () => {
      renderWork([...batched, item({ workId: "solo", state: "queued", batchId: null })], [batch({ unfinished: true })]);
      const rows = screen.getByRole("list", { name: "From Fix all" });
      expect(within(rows).getAllByRole("listitem")).toHaveLength(1);
      const entry = within(rows).getByRole("region", { name: "3 research requests from Fix all · Unidentified album" });
      expect(within(entry).getByText("2 waiting · 1 waiting for you")).toBeTruthy();
      expect(within(entry).getByText("from an unfinished run")).toBeTruthy();
      // Batched items are not listed one by one; the lone request still is.
      expect(screen.queryByText("First - One")).toBeNull();
      expect(within(screen.getByRole("list", { name: "Waiting" })).getAllByRole("listitem")).toHaveLength(1);
    });

    it("says nothing about an unfinished run for a run that reached its end", () => {
      renderWork(batched, [batch({ finishedAt: new Date().toISOString() })]);
      expect(screen.queryByText("from an unfinished run")).toBeNull();
    });

    it("shows a batch even when none of its items is listed as work any more", () => {
      renderWork([], [batch({ size: 2, counts: { applied: 1, failed: 1 } })]);
      expect(screen.getByRole("region", { name: "2 research requests from Fix all · Unidentified album" })).toBeTruthy();
      expect(screen.getByText("1 applied · 1 failed")).toBeTruthy();
      // Nothing waiting or held: nothing to cancel.
      expect(screen.queryByRole("button", { name: /Cancel the/ })).toBeNull();
    });

    it("opens to the batch's items, by keyboard", async () => {
      const detail: BatchDetail = { ...batch({}), items: batched.map((i) => ({ ...i })) };
      answers[`/api/research/batches/${B}`] = detail;
      renderWork(batched, [batch({})]);
      const open = screen.getByRole("button", { name: "Show requests (3)" });
      expect(open.getAttribute("aria-expanded")).toBe("false");
      open.focus();
      expect(document.activeElement).toBe(open);
      fireEvent.click(open);
      expect(open.getAttribute("aria-expanded")).toBe("true");
      const list = await screen.findByRole("list", { name: "Requests in this batch" });
      expect(within(list).getAllByRole("listitem")).toHaveLength(3);
      expect(within(list).getByText("Result waiting for you")).toBeTruthy();
      expect(within(list).getByRole("link", { name: "First - One" }).getAttribute("href")).toBe("/library/aaaaaaaaaaaaaaaa");
      expect(calls.map((c) => c.url)).toEqual([`/api/research/batches/${B}`]);
    });

    it("a failed or cancelled item of the batch keeps its Retry; one of an earlier library has none", async () => {
      const failed = item({ workId: "x4", albumLabel: "Fourth - Four", state: "failed", revision: 9, batchId: B });
      const cancelled = item({ workId: "x5", albumLabel: "Fifth - Five", state: "cancelled", revision: 4, batchId: B });
      // x6 is not in the work list: an item of an earlier library epoch.
      const elsewhere = item({ workId: "x6", albumLabel: "Sixth - Six", state: "failed", batchId: B });
      answers[`/api/research/batches/${B}`] = {
        ...batch({ size: 3, counts: { failed: 2, cancelled: 1 } }),
        items: [failed, cancelled, elsewhere],
      } satisfies BatchDetail;
      renderWork([failed, cancelled], [batch({ size: 3, counts: { failed: 2, cancelled: 1 } })]);
      fireEvent.click(screen.getByRole("button", { name: "Show requests (3)" }));
      const list = await screen.findByRole("list", { name: "Requests in this batch" });
      expect(within(list).getAllByRole("button", { name: /^Retry / })).toHaveLength(2);
      expect(within(list).queryByRole("button", { name: "Retry Sixth - Six" })).toBeNull();
      fireEvent.click(within(list).getByRole("button", { name: "Retry Fourth - Four" }));
      await waitFor(() => expect(calls.map((c) => c.url)).toContain("/api/research/x4/decision"));
      const call = calls.find((c) => c.url === "/api/research/x4/decision")!;
      expect(JSON.parse(String(call.init?.body))).toEqual({ revision: 9, action: "retry" });
    });

    it("Cancel asks first, then cancels the batch in one call and invalidates nothing itself (rule 5)", async () => {
      answers[`/api/research/batches/${B}/cancel`] = { ...batch({ counts: { cancelled: 2, awaiting_review: 1 } }), cancelled: 2 };
      const qc = renderWork(batched, [batch({ counts: { queued: 1, running: 1, awaiting_review: 1 } })]);
      const spy = vi.spyOn(qc, "invalidateQueries");
      const ask = screen.getByRole("button", { name: "Cancel the 2 waiting or held requests of this batch" });
      ask.focus();
      expect(document.activeElement).toBe(ask);
      fireEvent.click(ask);
      expect(calls).toHaveLength(0);
      expect(screen.getByText(/Results waiting for you stay/)).toBeTruthy();
      fireEvent.click(screen.getByRole("button", { name: "Cancel requests" }));
      await waitFor(() => expect(calls).toHaveLength(1));
      expect(calls[0].url).toBe(`/api/research/batches/${B}/cancel`);
      expect(calls[0].init?.method).toBe("POST");
      expect(spy).not.toHaveBeenCalled();
    });

    it("Keep leaves the batch alone", () => {
      renderWork(batched, [batch({})]);
      fireEvent.click(screen.getByRole("button", { name: /Cancel the 2 waiting or held/ }));
      fireEvent.click(screen.getByRole("button", { name: "Keep" }));
      expect(calls).toHaveLength(0);
      expect(screen.getByRole("button", { name: /Cancel the 2 waiting or held/ })).toBeTruthy();
    });
  });
});
