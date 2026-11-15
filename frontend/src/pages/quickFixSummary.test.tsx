/**
 * Health detail → bulk quick-fix dialog: the closing summary reports only
 * the findings that are gone as resolved. A fix that wrote part of what its
 * finding asks for comes back as `still_open` (the backend reruns the check
 * over the albums it fixed), and the dialog says so instead of claiming
 * "Resolved N of N".
 */
import { afterEach, beforeEach, describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter, Route, Routes } from "react-router-dom";

vi.mock("sonner", () => ({ toast: { error: vi.fn(), success: vi.fn() } }));

import HealthDetailPage from "@/pages/HealthDetail";
import type { CheckDetail, Finding } from "@/lib/types";

const finding = (id: number): Finding => ({
  id,
  check_id: "tags-out-of-sync",
  album_id: `album-${id}`,
  severity: "info" as Finding["severity"],
  summary: "missing: genre · 1 canonical drift",
  detail: null,
  first_seen_at: "2026-01-01T00:00:00Z",
  last_seen_at: "2026-01-01T00:00:00Z",
  resolved_at: null,
  ignored_at: null,
  ignored_reason: null,
});

const detail: CheckDetail = {
  meta: {
    id: "tags-out-of-sync",
    label: "Tags don't match canonical",
    description: "d",
    category: "metadata" as CheckDetail["meta"]["category"],
    cost: "medium" as CheckDetail["meta"]["cost"],
    runs_during_scan: false,
    fix_kind: "deterministic",
    bulk_action_label: "Apply canonical values",
  },
  runs: [],
  findings: [finding(1), finding(2), finding(3)],
};

function client(): QueryClient {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  qc.setQueryData(["check", "tags-out-of-sync"], detail);
  qc.setQueryData(["albums-snapshot"], []);
  qc.setQueryData(["jobs"], []);
  return qc;
}

function sse(done: Record<string, number>): string {
  const progress = [1, 2, 3]
    .map(
      (id) =>
        `event: progress\ndata: ${JSON.stringify({ finding_id: id, outcome: "fixed", message: "Wrote 1 album field(s)" })}\n\n`,
    )
    .join("");
  return `event: start\ndata: {"total": 3}\n\n${progress}event: done\ndata: ${JSON.stringify(done)}\n\n`;
}

function stubFetch(body: string) {
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string) =>
      String(url).endsWith("/quick-fix-all")
        ? new Response(body, { status: 200, headers: { "content-type": "text/event-stream" } })
        : new Response(JSON.stringify({ detail: "not found" }), {
            status: 404,
            headers: { "content-type": "application/json" },
          }),
    ),
  );
}

function renderPage() {
  render(
    <QueryClientProvider client={client()}>
      <MemoryRouter initialEntries={["/health/tags-out-of-sync"]}>
        <Routes>
          <Route path="/health/:checkId" element={<HealthDetailPage />} />
        </Routes>
      </MemoryRouter>
    </QueryClientProvider>,
  );
  fireEvent.click(screen.getByRole("button", { name: /Apply canonical values \(3\)/ }));
  fireEvent.click(screen.getByRole("button", { name: /Start/ }));
}

beforeEach(() => vi.clearAllMocks());
afterEach(() => vi.unstubAllGlobals());

describe("quick-fix summary", () => {
  it("does not report fixes whose findings stay open as resolved", async () => {
    stubFetch(sse({ total: 3, fixed: 0, skipped: 0, errors: 0, not_applicable: 0, still_open: 3 }));
    renderPage();
    const text = await screen.findByText(/No findings were fully resolved/);
    expect(text.textContent).toMatch(/3 fixes were applied but their findings are still open\./);
    expect(screen.getByText("Still open")).toBeInTheDocument();
    expect(document.body.textContent).not.toMatch(/Resolved 3 of 3/);
  });

  it("counts resolved and still-open findings separately", async () => {
    stubFetch(sse({ total: 3, fixed: 2, skipped: 0, errors: 0, not_applicable: 0, still_open: 1 }));
    renderPage();
    const text = await screen.findByText(/Resolved/);
    expect(text.textContent).toMatch(
      /Resolved 2 of 3 findings\. 1 fix was applied but its finding is still open\./,
    );
  });

  it("has no still-open column when every fix resolved its finding", async () => {
    stubFetch(sse({ total: 3, fixed: 3, skipped: 0, errors: 0, not_applicable: 0, still_open: 0 }));
    renderPage();
    const text = await screen.findByText(/Resolved/);
    expect(text.textContent).toBe("Resolved 3 of 3 findings.");
    expect(screen.queryByText("Still open")).toBeNull();
  });
});
