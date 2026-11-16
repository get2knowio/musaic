/**
 * The background check sweep's titlebar pill: hidden while the sweep is
 * idle, "Checking… n of m" while it works or waits, and kept fresh by the
 * `sweep` event of /api/checks/stream (useChecksStream), not by refetching.
 */
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ReactNode } from "react";
import { act, render, renderHook, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";

import CheckSweepPill from "@/components/CheckSweepPill";
import { CHECK_SWEEP_STATUS_KEY } from "@/hooks/useCheckSweepStatus";
import { useChecksStream } from "@/hooks/useChecksStream";
import type { CheckSweepStatus } from "@/lib/types";
import { MockEventSource } from "@/test/setup";

const state = (over: Partial<CheckSweepStatus> = {}): CheckSweepStatus => ({
  status: "idle",
  total: 0,
  completed: 0,
  current_check: null,
  started_at: null,
  completed_at: null,
  ...over,
});

function renderPill(qc: QueryClient) {
  return render(
    <QueryClientProvider client={qc}>
      <CheckSweepPill />
    </QueryClientProvider>,
  );
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("CheckSweepPill", () => {
  it("is hidden while the sweep is idle", () => {
    const qc = new QueryClient();
    qc.setQueryData(CHECK_SWEEP_STATUS_KEY, state());
    renderPill(qc);
    expect(screen.queryByRole("status")).toBeNull();
  });

  it("shows progress while the sweep runs, and while it waits", async () => {
    const qc = new QueryClient();
    qc.setQueryData(
      CHECK_SWEEP_STATUS_KEY,
      state({ status: "running", total: 120, completed: 7, current_check: "track-gaps" }),
    );
    renderPill(qc);
    expect(screen.getByRole("status").textContent).toContain("Checking… 7 of 120");
    act(() => {
      qc.setQueryData(CHECK_SWEEP_STATUS_KEY, state({ status: "waiting", total: 120, completed: 9 }));
    });
    await waitFor(() =>
      expect(screen.getByRole("status").textContent).toContain("Checking… 9 of 120"),
    );
    expect(screen.getByRole("status").title).toMatch(/waiting/);
  });

  it("follows the sweep events of the checks stream", async () => {
    const qc = new QueryClient();
    // The first read (GET /api/checks/sweep) already answered.
    qc.setQueryData(CHECK_SWEEP_STATUS_KEY, state());
    const fetchMock = vi.fn(async () => new Response(JSON.stringify(state()), { status: 200 }));
    vi.stubGlobal("fetch", fetchMock);
    const wrapper = ({ children }: { children: ReactNode }) => (
      <QueryClientProvider client={qc}>{children}</QueryClientProvider>
    );
    renderHook(() => useChecksStream(), { wrapper });
    const es = MockEventSource.last;
    if (!es) throw new Error("no EventSource");
    expect(es.url).toBe("/api/checks/stream");
    renderPill(qc);
    act(() => {
      es.emit("sweep", state({ status: "running", total: 3, completed: 1 }));
    });
    await waitFor(() =>
      expect(screen.getByRole("status").textContent).toContain("Checking… 1 of 3"),
    );
    act(() => {
      es.emit("sweep", state({ status: "idle", total: 3, completed: 3 }));
    });
    await waitFor(() => expect(screen.queryByRole("status")).toBeNull());
    // Kept fresh by the stream, not by refetching.
    expect(fetchMock).not.toHaveBeenCalled();
  });
});
