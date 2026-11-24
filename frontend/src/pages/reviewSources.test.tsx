/**
 * Unavailable is not empty (#323): each source the review page reads
 * (findings, jobs) is loading, answered, unavailable or last-known, and
 * the all-clear shows only when every one of them answered. A failed
 * source keeps the other usable, says so, and offers Retry; the Sidebar
 * badge carries the same uncertainty. The Library page tells a failed
 * settings load (retry) from an unconfigured library (setup) and an
 * unreachable folder (recovery).
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter, Route, Routes } from "react-router-dom";

vi.mock("sonner", () => ({
  toast: Object.assign(vi.fn(), { error: vi.fn(), success: vi.fn(), warning: vi.fn() }),
}));

import ReviewPage from "@/pages/Review";
import LibraryPage from "@/pages/Library";
import Sidebar from "@/components/Sidebar";
import { makeAlbum, makeJob } from "@/test/fixtures";
import type { AgentApproval } from "@/lib/api";
import type { AppConfig, CheckSummary, Finding, Job } from "@/lib/types";

const summaries: CheckSummary[] = [
  {
    meta: {
      id: "track-gaps",
      label: "Track gaps",
      description: "Track gaps description",
      category: "metadata",
      cost: "cheap",
      runs_during_scan: true,
      fix_kind: "deterministic",
      bulk_action_label: null,
    },
    open_count: 1,
    last_run: null,
  },
];

const gap: Finding = {
  id: 1,
  check_id: "track-gaps",
  album_id: "a1",
  severity: "warn",
  summary: "gap 1",
  detail: null,
  first_seen_at: "2026-01-01T00:00:00Z",
  last_seen_at: "2026-01-01T00:00:00Z",
  resolved_at: null,
  ignored_at: null,
  ignored_reason: null,
};
const failedJob: Job = makeJob({ id: "f1", status: "failed", album_id: "a1", album_title: "Album f1", error: "boom" });

const pending: AgentApproval = {
  id: "ap1",
  pairingId: "p1",
  pairingName: "Helper",
  action: "fix_apply",
  albumId: "a1",
  albumLabel: "Here",
  kinds: ["tags.write"],
  state: "pending",
  reason: null,
  createdAt: "2026-01-01T00:00:00Z",
  expiresAt: "2999-01-01T00:00:00Z",
  decidedAt: null,
  diff: { tags: [{ scope: "album", field: "title", before: "a", after: "b" }], files: [] },
};

/** What each endpoint answers: a body, or a failing status. */
const answers: Record<string, unknown> = {};
const FAIL = Symbol("fail");
const PENDING = Symbol("pending");

function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
}

beforeEach(() => {
  for (const k of Object.keys(answers)) delete answers[k];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string) => {
      const path = String(url).split("?")[0];
      const a = answers[path];
      if (a === FAIL) return json({ detail: "server error" }, 500);
      if (a === PENDING) return new Promise<Response>(() => {});
      if (a !== undefined) return json(a);
      return json({ detail: "not found" }, 404);
    }),
  );
});
afterEach(() => {
  vi.unstubAllGlobals();
  sessionStorage.clear();
});

function client(seed: { findings?: Finding[]; jobs?: Job[]; approvals?: AgentApproval[] | null } = {}) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
  qc.setQueryData(["checks"], summaries);
  qc.setQueryData(["albums-snapshot"], [makeAlbum({ id: "a1", title: "Here" })]);
  if (seed.findings) qc.setQueryData(["findings"], seed.findings);
  if (seed.jobs) qc.setQueryData(["jobs"], seed.jobs);
  // The assistant requests are answered (none) unless a test says otherwise.
  if (seed.approvals !== null) qc.setQueryData(["agent", "approvals", "pending"], seed.approvals ?? []);
  return qc;
}

function renderReview(qc: QueryClient) {
  render(
    <QueryClientProvider client={qc}>
      <MemoryRouter initialEntries={["/review"]}>
        <Sidebar />
        <Routes>
          <Route path="/review" element={<ReviewPage />} />
        </Routes>
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

const navLink = () => screen.getByRole("link", { name: /Review/ });
const ALL_CLEAR = { name: "Nothing to review" };

describe("review sources", () => {
  it("initial failure of every source: no all-clear, a Retry for each, the badge says it does not know", async () => {
    answers["/api/checks/findings"] = FAIL;
    answers["/api/ai/jobs"] = FAIL;
    renderReview(client());
    expect(await screen.findByRole("heading", { name: "Review data isn't available" })).toBeTruthy();
    expect(screen.queryByRole("heading", ALL_CLEAR)).toBeNull();
    expect(screen.getByRole("button", { name: "Retry findings" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Retry jobs" })).toBeTruthy();
    expect(navLink().textContent).toContain("?");
  });

  it("partial failure: the jobs that loaded stay usable, the findings are labelled missing, the count is a lower bound", async () => {
    answers["/api/checks/findings"] = FAIL;
    renderReview(client({ jobs: [failedJob] }));
    expect(await screen.findByText("Couldn't load findings. What is shown may be incomplete.")).toBeTruthy();
    expect(screen.getByText("Album f1")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Retry jobs" })).toBeNull();
    expect(screen.queryByRole("heading", ALL_CLEAR)).toBeNull();
    expect(navLink().textContent).toContain("1+");
  });

  it("refresh failure keeps the cached findings and labels them last known", async () => {
    const qc = client({ findings: [gap], jobs: [] });
    renderReview(qc);
    expect(screen.getByText("gap 1")).toBeTruthy();
    answers["/api/checks/findings"] = FAIL;
    await act(async () => {
      await qc.refetchQueries({ queryKey: ["findings"], exact: true });
    });
    expect(await screen.findByText(/Showing findings as last loaded/)).toBeTruthy();
    expect(screen.getByText("gap 1")).toBeTruthy();
    expect(navLink().querySelector("[data-badge]")!.getAttribute("title")).toMatch(/last known/);
  });

  it("a successful retry clears the notice and only then shows the all-clear", async () => {
    answers["/api/checks/findings"] = FAIL;
    renderReview(client({ jobs: [] }));
    const retry = await screen.findByRole("button", { name: "Retry findings" });
    expect(screen.queryByRole("heading", ALL_CLEAR)).toBeNull();
    answers["/api/checks/findings"] = { findings: [] };
    fireEvent.click(retry);
    expect(await screen.findByRole("heading", ALL_CLEAR)).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Retry findings" })).toBeNull();
    expect(navLink().textContent).toBe("Review");
  });

  it("assistant requests that fail to load are not an empty inbox", async () => {
    answers["/api/checks/findings"] = { findings: [] };
    answers["/api/ai/jobs"] = { jobs: [] };
    answers["/api/agent/approvals"] = FAIL;
    renderReview(client({ approvals: null }));
    expect(await screen.findByText("Couldn't load assistant requests. What is shown may be incomplete.")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Retry assistant requests" })).toBeTruthy();
    expect(screen.queryByRole("heading", ALL_CLEAR)).toBeNull();
    expect(navLink().textContent).toContain("?");
    answers["/api/agent/approvals"] = [];
    fireEvent.click(screen.getByRole("button", { name: "Retry assistant requests" }));
    expect(await screen.findByRole("heading", ALL_CLEAR)).toBeTruthy();
  });

  it("a pending assistant request counts in the badge and is listed with Approve and Deny", async () => {
    answers["/api/checks/findings"] = { findings: [] };
    answers["/api/ai/jobs"] = { jobs: [] };
    renderReview(client({ approvals: [pending] }));
    expect(await screen.findByText("Helper wants to change tags for Here")).toBeTruthy();
    expect(screen.getByRole("button", { name: /^Approve/ })).toBeTruthy();
    expect(screen.getByRole("button", { name: /^Deny/ })).toBeTruthy();
    expect(navLink().querySelector("[data-badge]")!.getAttribute("title")).toBe("1 item to review");
    expect(screen.queryByRole("heading", ALL_CLEAR)).toBeNull();
  });

  it("genuinely empty answers show the all-clear and no badge", async () => {
    answers["/api/checks/findings"] = { findings: [] };
    answers["/api/ai/jobs"] = { jobs: [] };
    renderReview(client());
    expect(await screen.findByRole("heading", ALL_CLEAR)).toBeTruthy();
    expect(screen.queryByRole("status")).toBeNull();
    expect(navLink().textContent).toBe("Review");
  });

  it("while nothing has answered it says loading, not all-clear", () => {
    answers["/api/checks/findings"] = PENDING;
    answers["/api/ai/jobs"] = PENDING;
    answers["/api/agent/approvals"] = PENDING;
    renderReview(client({ approvals: null }));
    expect(screen.getAllByText("Loading…").length).toBeGreaterThan(0);
    expect(navLink().textContent).toBe("Review");
    expect(screen.queryByRole("heading", ALL_CLEAR)).toBeNull();
  });
});

describe("library configuration", () => {
  function renderLibrary(qc: QueryClient) {
    qc.setQueryData(["jobs"], []);
    render(
      <QueryClientProvider client={qc}>
        <MemoryRouter initialEntries={["/library"]}>
          <Routes>
            <Route path="/library" element={<LibraryPage />} />
          </Routes>
        </MemoryRouter>
      </QueryClientProvider>,
    );
  }

  it("a failed settings load offers Retry, not setup; a retry that answers shows what it says", async () => {
    answers["/api/config"] = FAIL;
    renderLibrary(client());
    expect(await screen.findByRole("heading", { name: "Couldn't load the library settings" })).toBeTruthy();
    expect(screen.queryByText("Configure your library")).toBeNull();
    expect(screen.queryByRole("textbox", { name: "Library directory path" })).toBeNull();
    answers["/api/config"] = { library_dir: null, exists: false } satisfies AppConfig;
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    expect(await screen.findByRole("heading", { name: "Configure your library" })).toBeTruthy();
  });

  it("an unconfigured library offers setup", () => {
    const qc = client();
    qc.setQueryData(["config"], { library_dir: null, exists: false });
    renderLibrary(qc);
    expect(screen.getByRole("heading", { name: "Configure your library" })).toBeTruthy();
    expect(screen.getByRole("textbox", { name: "Library directory path" })).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Try again" })).toBeNull();
  });

  it("an unreachable folder offers recovery: try again, or choose another folder", async () => {
    const qc = client();
    qc.setQueryData(["config"], { library_dir: "/Volumes/Music", exists: false });
    renderLibrary(qc);
    expect(screen.getByRole("heading", { name: "Library folder not found" })).toBeTruthy();
    expect(screen.getByText("/Volumes/Music")).toBeTruthy();
    // Reconnecting leads; choosing another folder is a separate action.
    expect(screen.queryByRole("textbox", { name: "Library directory path" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Change folder…" }));
    expect((screen.getByRole("textbox", { name: "Library directory path" }) as HTMLInputElement).value).toBe(
      "/Volumes/Music",
    );
    answers["/api/config"] = { library_dir: "/Volumes/Music", exists: true } satisfies AppConfig;
    answers["/api/albums"] = { albums: [], count: 0, library_dir: "/Volumes/Music", scanned_at: null };
    fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    await waitFor(() => expect(screen.queryByRole("heading", { name: "Library folder not found" })).toBeNull());
  });
});
