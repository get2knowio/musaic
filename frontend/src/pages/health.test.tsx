/**
 * Health pages after checks became automatic: no per-check "Run now", no
 * run history, no category column or check count. A single "Check now"
 * runs the checks that do not run during a scan, and Fix is disabled on a
 * finding whose album is Missing.
 */
import { afterEach, describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter, Route, Routes } from "react-router-dom";

vi.mock("sonner", () => ({ toast: { error: vi.fn(), success: vi.fn() } }));

import HealthPage from "@/pages/Health";
import HealthDetailPage from "@/pages/HealthDetail";
import { makeAlbum } from "@/test/fixtures";
import type { CheckDetail, CheckMeta, CheckRun, CheckSummary, Finding } from "@/lib/types";

const meta = (id: string, label: string, runsDuringScan: boolean): CheckMeta => ({
  id,
  label,
  description: `${label} description`,
  category: "metadata",
  cost: runsDuringScan ? "cheap" : "medium",
  runs_during_scan: runsDuringScan,
  fix_kind: "deterministic",
  bulk_action_label: null,
});

const run = (checkId: string, status: CheckRun["status"] = "completed"): CheckRun => ({
  id: 1,
  check_id: checkId,
  started_at: "2026-01-01T00:00:00Z",
  completed_at: "2026-01-01T00:00:01Z",
  status,
  library_path: "/music",
  scope: "all",
  issue_count: 0,
  error: null,
  duration_ms: 12,
});

const summaries: CheckSummary[] = [
  { meta: meta("track-gaps", "Track gaps", true), open_count: 2, last_run: run("track-gaps") },
  { meta: meta("tags-out-of-sync", "Tags don't match canonical", false), open_count: 0, last_run: null },
  { meta: meta("filename-mismatch", "Filenames don't match template", false), open_count: 0, last_run: null },
];

function renderHealth(checks: CheckSummary[] = summaries) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  qc.setQueryData(["checks"], checks);
  render(
    <QueryClientProvider client={qc}>
      <MemoryRouter initialEntries={["/health"]}>
        <Routes>
          <Route path="/health" element={<HealthPage />} />
        </Routes>
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

afterEach(() => {
  vi.unstubAllGlobals();
  vi.clearAllMocks();
});

describe("Health page", () => {
  it("lists checks without run controls, run times, categories or a check count", () => {
    renderHealth();
    expect(screen.getByText("Track gaps")).toBeTruthy();
    expect(screen.queryByText(/Run now/)).toBeNull();
    expect(screen.queryByText(/Last run/i)).toBeNull();
    expect(screen.queryByText(/Never run/i)).toBeNull();
    expect(screen.queryByText(/Category/i)).toBeNull();
    expect(screen.queryByText(/registered/i)).toBeNull();
    expect(screen.getByText(/open finding/)).toBeTruthy();
  });

  it("Check now runs only the checks that do not run during a scan, one at a time", async () => {
    const resolvers: Array<() => void> = [];
    const fetchMock = vi.fn(
      (_url: string) =>
        new Promise<Response>((resolve) => {
          resolvers.push(() =>
            resolve(
              new Response(JSON.stringify(run("x")), {
                status: 200,
                headers: { "content-type": "application/json" },
              }),
            ),
          );
        }),
    );
    vi.stubGlobal("fetch", fetchMock);
    renderHealth();

    fireEvent.click(screen.getByRole("button", { name: /Check now/ }));

    const button = await screen.findByRole("button", { name: /Checking 1 of 2/ });
    expect((button as HTMLButtonElement).disabled).toBe(true);
    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(String(fetchMock.mock.calls[0][0])).toContain("/api/checks/tags-out-of-sync/run");

    resolvers[0]();
    await screen.findByRole("button", { name: /Checking 2 of 2/ });
    expect(fetchMock).toHaveBeenCalledTimes(2);
    expect(String(fetchMock.mock.calls[1][0])).toContain("/api/checks/filename-mismatch/run");

    resolvers[1]();
    await screen.findByRole("button", { name: /Check now/ });
    const urls = fetchMock.mock.calls.map((c) => String(c[0]));
    expect(urls.some((u) => u.includes("track-gaps"))).toBe(false);
  });

  it("Check now is disabled while one of those checks is already running", () => {
    renderHealth([
      summaries[0],
      { ...summaries[1], last_run: run("tags-out-of-sync", "running") },
      summaries[2],
    ]);
    const button = screen.getByRole("button", { name: /Checking/ }) as HTMLButtonElement;
    expect(button.disabled).toBe(true);
  });

  it("has no Check now when every check runs during a scan", () => {
    renderHealth([summaries[0]]);
    expect(screen.queryByRole("button", { name: /Check now/ })).toBeNull();
  });
});

const finding = (id: number, albumId: string): Finding => ({
  id,
  check_id: "track-gaps",
  album_id: albumId,
  severity: "warn",
  summary: "gap after track 3",
  detail: null,
  first_seen_at: "2026-01-01T00:00:00Z",
  last_seen_at: "2026-01-01T00:00:00Z",
  resolved_at: null,
  ignored_at: null,
  ignored_reason: null,
});

function renderDetail() {
  const detail: CheckDetail = {
    meta: meta("track-gaps", "Track gaps", true),
    runs: [run("track-gaps")],
    findings: [finding(1, "present"), finding(2, "gone")],
  };
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  qc.setQueryData(["check", "track-gaps"], detail);
  qc.setQueryData(["albums-snapshot"], [
    makeAlbum({ id: "present", title: "Here" }),
    makeAlbum({ id: "gone", title: "Gone", missing_since: "2026-01-02T00:00:00Z" }),
  ]);
  qc.setQueryData(["jobs"], []);
  render(
    <QueryClientProvider client={qc}>
      <MemoryRouter initialEntries={["/health/track-gaps"]}>
        <Routes>
          <Route path="/health/:checkId" element={<HealthDetailPage />} />
        </Routes>
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

describe("Health detail page", () => {
  it("has no Run now and no run history", () => {
    renderDetail();
    expect(screen.getByText("Track gaps")).toBeTruthy();
    expect(screen.queryByText(/Run now/)).toBeNull();
    expect(screen.queryByText(/Recent runs/i)).toBeNull();
    expect(screen.queryByText(/No runs yet/i)).toBeNull();
  });

  it("disables Fix on a Missing album's finding and says why", async () => {
    renderDetail();
    await waitFor(() => expect(screen.getAllByRole("button", { name: /Fix/ })).toHaveLength(2));
    const [present, gone] = screen.getAllByRole("button", { name: /Fix/ }) as HTMLButtonElement[];
    expect(present.disabled).toBe(false);
    expect(gone.disabled).toBe(true);
    expect(gone.title).toMatch(/folder is missing/);
  });
});
