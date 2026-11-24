/**
 * FR-036: Cancel on a research request's card (the Review row and its
 * confirmation dialog) cancels the request through the research cancel,
 * `POST /api/ai/jobs/{id}/cancel`, and never deletes it; only finished
 * history is removed (`DELETE /api/ai/jobs/{id}`). No cache is invalidated
 * from the handler: the `work` event family refreshes the lists (rule 5).
 */
import { useState } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";

const toastFns = vi.hoisted(() => ({ error: vi.fn(), success: vi.fn(), warning: vi.fn() }));
vi.mock("sonner", () => ({ toast: Object.assign(vi.fn(), toastFns) }));

import { DeleteConfirmDialog, JobRow, type PendingDelete } from "@/components/JobRow";
import { makeJob } from "@/test/fixtures";
import type { Job } from "@/lib/types";

const json = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });

function stubFetch(answer: (url: string, method: string) => Response) {
  const calls: { url: string; method: string }[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input);
      const method = init?.method ?? "GET";
      calls.push({ url, method });
      return answer(url, method);
    }),
  );
  return calls;
}

/** The row and the dialog, wired as Review wires them. */
function Harness({ job }: { job: Job }) {
  const [pending, setPending] = useState<PendingDelete | null>(null);
  return (
    <>
      <JobRow job={job} onRequestDelete={(j) => setPending({ kind: "single", job: j })} />
      <DeleteConfirmDialog pending={pending} onClose={() => setPending(null)} />
    </>
  );
}

function renderRow(job: Job) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
  const spy = vi.spyOn(qc, "invalidateQueries");
  render(
    <QueryClientProvider client={qc}>
      <MemoryRouter>
        <Harness job={job} />
      </MemoryRouter>
    </QueryClientProvider>,
  );
  return spy;
}

afterEach(() => {
  vi.unstubAllGlobals();
  vi.clearAllMocks();
});

describe("Cancel on a research request's row", () => {
  for (const status of ["queued", "running"] as const) {
    it(`a ${status} request is cancelled through the research cancel, never deleted`, async () => {
      const calls = stubFetch((url, method) =>
        url === "/api/ai/jobs/j1/cancel" && method === "POST" ? json({ id: "j1", cancelled: 1 }) : json({}, 404),
      );
      const spy = renderRow(makeJob({ id: "j1", status, job_type: "cover", album_title: "Some album" }));
      fireEvent.click(screen.getByRole("button", { name: "Cancel research request" }));
      expect(await screen.findByRole("heading", { name: "Cancel this research request?" })).toBeInTheDocument();
      fireEvent.click(screen.getByRole("button", { name: "Cancel request" }));
      await waitFor(() => expect(calls).toContainEqual({ url: "/api/ai/jobs/j1/cancel", method: "POST" }));
      expect(calls.some((c) => c.method === "DELETE")).toBe(false);
      expect(spy).not.toHaveBeenCalled();
    });
  }

  it("a 409 (nothing left to cancel) shows the server's message", async () => {
    stubFetch(() => json({ detail: "Nothing is waiting or in progress for this request." }, 409));
    renderRow(makeJob({ id: "j1", status: "queued", album_title: "Some album" }));
    fireEvent.click(screen.getByRole("button", { name: "Cancel research request" }));
    fireEvent.click(await screen.findByRole("button", { name: "Cancel request" }));
    await waitFor(() =>
      expect(toastFns.warning).toHaveBeenCalledWith(
        "Nothing is waiting or in progress for this request.",
        expect.anything(),
      ),
    );
  });

  it("an old job's history-only record left queued is history: Remove, never Cancel", () => {
    stubFetch(() => json({}, 404));
    renderRow(makeJob({ id: "h1", status: "queued", job_type: "lyrics" as Job["job_type"], album_title: "Some album" }));
    expect(screen.queryByRole("button", { name: "Cancel research request" })).toBeNull();
    expect(screen.getByRole("button", { name: "Remove from history" })).toBeInTheDocument();
  });

  it("finished history is removed, with Remove in the label and the confirmation", async () => {
    const calls = stubFetch((url, method) =>
      url === "/api/ai/jobs/f1" && method === "DELETE" ? json({ deleted: true, id: "f1" }) : json({}, 404),
    );
    const spy = renderRow(makeJob({ id: "f1", status: "failed", error: "boom", album_title: "Some album" }));
    expect(screen.queryByRole("button", { name: "Cancel research request" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Remove from history" }));
    expect(await screen.findByRole("heading", { name: "Remove from the history?" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Remove" }));
    await waitFor(() => expect(calls).toContainEqual({ url: "/api/ai/jobs/f1", method: "DELETE" }));
    expect(calls.some((c) => c.url.endsWith("/cancel"))).toBe(false);
    // The jobs stream and the work events refresh the list (rule 5).
    await waitFor(() => expect(screen.queryByRole("heading", { name: "Remove from the history?" })).toBeNull());
    expect(spy).not.toHaveBeenCalled();
  });
});
