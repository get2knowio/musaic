import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";

import { ApprovalRow } from "@/components/ApprovalRow";
import { approvalSentence, diffSummary, timeLeftText } from "@/lib/agentCopy";
import type { AgentApproval } from "@/lib/api";

const base: AgentApproval = {
  id: "ap1",
  pairingId: "p1",
  pairingName: "Helper",
  action: "fix_apply",
  albumId: "a1",
  albumLabel: "Kind of Blue",
  kinds: ["tags.write"],
  state: "pending",
  reason: null,
  createdAt: "2026-01-01T00:00:00Z",
  expiresAt: new Date(Date.now() + 90 * 60_000).toISOString(),
  decidedAt: null,
  diff: {
    tags: [
      { scope: "album", field: "date", before: "1958", after: "1959" },
      { scope: "01.flac", field: "title", before: null, after: "So What" },
    ],
    files: [],
  },
};

function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
}

let fetchMock: ReturnType<typeof vi.fn>;
beforeEach(() => {
  fetchMock = vi.fn(async () => json(base));
  vi.stubGlobal("fetch", fetchMock);
});
afterEach(() => vi.unstubAllGlobals());

function renderRow(a: AgentApproval = base) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const invalidate = vi.spyOn(qc, "invalidateQueries");
  render(
    <QueryClientProvider client={qc}>
      <ApprovalRow approval={a} />
    </QueryClientProvider>,
  );
  return { invalidate };
}

describe("ApprovalRow", () => {
  it("shows the request, the change summary and the time left, and sends nothing by itself", () => {
    renderRow();
    expect(screen.getByText("Helper wants to change tags for Kind of Blue")).toBeTruthy();
    expect(screen.getByText(/2 tag changes \(date, title\)/)).toBeTruthy();
    expect(screen.getByText(/1 h 30 min left|1 h 29 min left/)).toBeTruthy();
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("Approve and Deny post to the decision routes and leave the cache to the event stream", async () => {
    const { invalidate } = renderRow();
    fireEvent.click(screen.getByRole("button", { name: /^Approve/ }));
    await waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(1));
    expect(fetchMock.mock.calls[0]![0]).toBe("/api/agent/approvals/ap1/approve");
    fireEvent.click(screen.getByRole("button", { name: /^Deny/ }));
    await waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(2));
    expect(fetchMock.mock.calls[1]![0]).toBe("/api/agent/approvals/ap1/deny");
    expect(invalidate).not.toHaveBeenCalled();
  });

  it("a request that is no longer pending is explained in plain words", async () => {
    fetchMock.mockImplementation(async () => json({ detail: "no longer pending" }, 409));
    renderRow();
    fireEvent.click(screen.getByRole("button", { name: /^Approve/ }));
    expect((await screen.findByRole("alert")).textContent).toMatch(/no longer waiting/);
  });
});

describe("agent copy", () => {
  it("words each action", () => {
    const s = (over: Partial<AgentApproval>) => approvalSentence({ ...base, ...over });
    expect(s({ kinds: ["covers.write"] })).toBe("Helper wants to set the cover for Kind of Blue");
    expect(s({ kinds: ["tags.write", "files.rename"] })).toBe("Helper wants to change tags and rename files for Kind of Blue");
    expect(s({ action: "scan_start", albumLabel: null, kinds: ["compute.scan"] })).toBe("Helper wants to scan the library");
    expect(s({ action: "checks_start", albumLabel: null })).toBe("Helper wants to run health checks");
    expect(s({ action: "analysis_start", albumLabel: null })).toBe("Helper wants to analyse and write tags");
  });

  it("summarises a diff as a label, and says Expired when time is up", () => {
    expect(diffSummary(null)).toBeNull();
    expect(diffSummary({ tags: [], cover: "x", files: [{ from: "a", to: "b" }], folder: { from: "a", to: "b" } })).toBe(
      "new cover; 1 file renamed; folder renamed",
    );
    expect(timeLeftText("2026-01-01T00:00:00Z", Date.parse("2026-01-01T00:00:01Z"))).toBe("Expired");
    expect(timeLeftText("2026-01-01T00:00:30Z", Date.parse("2026-01-01T00:00:00Z"))).toBe("under a minute left");
  });
});
