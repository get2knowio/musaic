/**
 * Health pages after checks became automatic: no per-check "Run now", no
 * run history, no category column or check count, and no global "Check
 * now" (the background check sweep runs every check). Fix is disabled on a
 * finding whose album is Missing.
 */
import { afterEach, describe, it, expect, vi } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
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

  it("has no Check now: every check runs on its own in the background", () => {
    renderHealth();
    expect(screen.queryByRole("button", { name: /Check now/ })).toBeNull();
    expect(screen.queryByRole("button", { name: /Checking/ })).toBeNull();
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
