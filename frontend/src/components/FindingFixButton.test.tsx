import { afterEach, beforeEach, describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";

const toasts = vi.hoisted(() => ({
  error: vi.fn(),
  success: vi.fn(),
}));
vi.mock("sonner", () => ({ toast: toasts }));

import FindingFixButton from "@/components/FindingFixButton";
import type { Finding, Job } from "@/lib/types";
import { makeJob } from "@/test/fixtures";

const finding = (checkId: string): Finding => ({
  id: 7,
  check_id: checkId,
  album_id: "album-1",
  severity: "warning" as Finding["severity"],
  summary: "s",
  detail: null,
  first_seen_at: "2026-01-01T00:00:00Z",
  last_seen_at: "2026-01-01T00:00:00Z",
  resolved_at: null,
  ignored_at: null,
  ignored_reason: null,
});

function renderButton(checkId: string, fixKind: "ai-job" | "deterministic", jobs: Job[] = []) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  qc.setQueryData(["jobs"], jobs);
  qc.setQueryData(["research", "work"], []);
  qc.setQueryData(["albums-snapshot"], []);
  return render(
    <QueryClientProvider client={qc}>
      <MemoryRouter>
        <FindingFixButton finding={finding(checkId)} fixKind={fixKind} />
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

describe("FindingFixButton research-capable fixes", () => {
  let fetchMock: ReturnType<typeof vi.fn>;
  let fixAnswer: { ok: boolean; message: string; job_id: string | null };
  beforeEach(() => {
    toasts.error.mockClear();
    toasts.success.mockClear();
    fixAnswer = { ok: true, message: "done", job_id: null };
    fetchMock = vi.fn(async (url: string) => {
      const body = String(url).startsWith("/api/ai/jobs") ? { jobs: [] } : fixAnswer;
      return new Response(JSON.stringify(body), {
        status: 200,
        headers: { "content-type": "application/json" },
      });
    });
    vi.stubGlobal("fetch", fetchMock);
  });
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  // Every research-capable fix offers the hint first, whatever the
  // assistant situation (FR-007); submitting sends the hint with the fix.
  for (const [check, label] of [
    ["no-cover-art", "Find cover art…"],
    ["unidentified-album", "Identify album…"],
    ["tags-out-of-sync", "Fill missing tags…"],
    ["track-gaps", "Check track numbers…"],
  ] as const) {
    it(`${check}: offers the hint dialog, then posts the fix with the hint`, async () => {
      fixAnswer = { ok: true, message: "Research requested. You can follow it in Review.", job_id: "g1" };
      const invalidate = vi.spyOn(QueryClient.prototype, "invalidateQueries");
      renderButton(check, "ai-job");
      const btn = screen.getByRole("button", { name: label });
      expect(btn).toBeEnabled();
      expect(btn.getAttribute("title")).not.toMatch(/\bAI\b|Claude/);
      fireEvent.click(btn);
      const dialog = screen.getByRole("dialog");
      expect(dialog.textContent).not.toMatch(/\bAI\b|Claude/);
      fireEvent.change(screen.getByRole("textbox"), { target: { value: "2012 reissue" } });
      const submit = Array.from(dialog.querySelectorAll("button")).find((b) =>
        /^(Find cover art|Suggest fixes)/.test(b.textContent ?? ""),
      );
      fireEvent.click(submit!);
      await waitFor(() =>
        expect(fetchMock.mock.calls.some(([u]) => String(u).includes(`/api/checks/${check}/findings/7/fix`))).toBe(true),
      );
      const [, init] = fetchMock.mock.calls.find(([u]) => String(u).includes("/findings/7/fix"))!;
      expect((init as RequestInit).method).toBe("POST");
      expect(JSON.parse(String((init as RequestInit).body))).toEqual({ hint: "2012 reissue" });
      // A request was made (job_id set): the jobs stream turns the button
      // into "Queued", so no toast.
      await new Promise((r) => setTimeout(r, 0));
      expect(toasts.error).not.toHaveBeenCalled();
      // The work events and the jobs stream refresh the jobs (rule 5).
      expect(invalidate).not.toHaveBeenCalled();
      invalidate.mockRestore();
    });
  }

  it("a refused fix shows the server's message and stays usable", async () => {
    fixAnswer = { ok: false, message: "Refused for a reason.", job_id: null };
    renderButton("unidentified-album", "ai-job");
    fireEvent.click(screen.getByRole("button", { name: "Identify album…" }));
    fireEvent.click(screen.getByRole("button", { name: /Suggest fixes/ }));
    await waitFor(() => expect(toasts.error).toHaveBeenCalledWith("Refused for a reason."));
    expect(toasts.success).not.toHaveBeenCalled();
    expect(screen.getByRole("button", { name: "Identify album…" })).toBeEnabled();
  });

  it("names research, not a model, while a request is waiting or held", () => {
    const queued = makeJob({ id: "g1", album_id: "album-1", status: "queued", job_type: "metadata" });
    const first = renderButton("unidentified-album", "ai-job", [queued]);
    expect(screen.getByRole("button", { name: /Queued/ }).getAttribute("title")).toBe(
      "Research is pending for this album",
    );
    first.unmount();
    renderButton("unidentified-album", "ai-job", [{ ...queued, status: "running" }]);
    expect(screen.getByRole("button", { name: /Running/ }).getAttribute("title")).toBe(
      "An assistant is working on this",
    );
  });

  it("a Fix all batch's request for the album counts like a card: no second Fix while it is pending", () => {
    // Batched requests have no job card; the delegated work list carries them.
    const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    qc.setQueryData(["jobs"], []);
    qc.setQueryData(["albums-snapshot"], []);
    qc.setQueryData(["research", "work"], [
      {
        workId: "w1",
        albumId: "album-1",
        albumLabel: "Artist - Album",
        kind: "metadata.identify",
        state: "queued",
        revision: 1,
        holder: null,
        lastHeartbeatAt: null,
        leaseExpiresAt: null,
        attempts: 0,
        maxAttempts: 3,
        lastError: null,
        waitingForAgent: true,
        groupId: "g1",
        batchId: "11111111-2222-4333-8444-555555555555",
      },
    ]);
    const tree = (checkId: string) => (
      <QueryClientProvider client={qc}>
        <MemoryRouter>
          <FindingFixButton finding={finding(checkId)} fixKind="ai-job" />
        </MemoryRouter>
      </QueryClientProvider>
    );
    const view = render(tree("unidentified-album"));
    expect(screen.getByRole("button", { name: /Queued/ })).toBeDisabled();
    // A metadata request does not hold a cover finding's button.
    view.rerender(tree("no-cover-art"));
    expect(screen.getByRole("button", { name: "Find cover art…" })).toBeEnabled();
  });

  it("leaves deterministic fixes alone", async () => {
    renderButton("track-gaps-like-deterministic", "deterministic");
    const btn = screen.getByRole("button", { name: /fix/i });
    expect(btn).toBeEnabled();
    fireEvent.click(btn);
    await waitFor(() => expect(fetchMock).toHaveBeenCalled());
    expect(String(fetchMock.mock.calls[0][0])).toContain("/findings/7/fix");
  });
});
