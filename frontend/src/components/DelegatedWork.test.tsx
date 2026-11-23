/**
 * Review, Delegated work: grouped by state, the holder and the times of held
 * work, "Waiting for an agent", Retry through the existing decision call, and
 * Retry reachable by keyboard.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";

vi.mock("sonner", () => ({
  toast: Object.assign(vi.fn(), { error: vi.fn(), success: vi.fn(), warning: vi.fn() }),
}));

import { DelegatedWork } from "@/components/DelegatedWork";
import type { DelegatedWorkItem } from "@/lib/api";

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

let calls: { url: string; init?: RequestInit }[] = [];

beforeEach(() => {
  calls = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string, init?: RequestInit) => {
      calls.push({ url: String(url), init });
      return new Response(JSON.stringify({ ok: true }), { status: 200, headers: { "content-type": "application/json" } });
    }),
  );
});
afterEach(() => vi.unstubAllGlobals());

function renderWork(items: DelegatedWorkItem[] | null) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
  if (items) qc.setQueryData(["research", "work"], items);
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
    expect(screen.getByText(/Waiting for an agent/)).toBeTruthy();
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
});
