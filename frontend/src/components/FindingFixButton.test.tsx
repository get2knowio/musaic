import { afterEach, beforeEach, describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";

const toasts = vi.hoisted(() => ({
  error: vi.fn(),
  success: vi.fn(),
}));
vi.mock("sonner", () => ({ toast: toasts }));

// The no-model build's answer where an ai-job fix would queue a job: the
// job's not-available message, as the Rust services build it
// (musaic_ai::provider::NOT_AVAILABLE; it names no AI).
const NOT_AVAILABLE =
  "MusicBrainz has nothing more for this album, and this build has no other source to try.";

import FindingFixButton from "@/components/FindingFixButton";
import type { AiProvidersResponse } from "@/lib/api";
import type { Finding } from "@/lib/types";

const provider = (ready: boolean): AiProvidersResponse => ({
  selected: { provider_id: "claude", model_id: "m", api_key_set: false },
  providers: [
    {
      id: ready ? "claude" : "unconfigured",
      accepts_api_key: false,
      auth_hint: null,
      ready,
      requirement: ready ? null : "an AI backend",
      remedy: null,
    },
  ],
  uninstalled: [],
});

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

function renderButton(ready: boolean, checkId: string, fixKind: "ai-job" | "deterministic") {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  qc.setQueryData(["ai-providers"], provider(ready));
  qc.setQueryData(["jobs"], []);
  qc.setQueryData(["albums-snapshot"], []);
  return render(
    <QueryClientProvider client={qc}>
      <MemoryRouter>
        <FindingFixButton finding={finding(checkId)} fixKind={fixKind} />
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

describe("FindingFixButton AI gating", () => {
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

  // The MusicBrainz part of an ai-job fix runs without a model.
  it("offers an ai-job fix without a ready provider, with no hint dialog", async () => {
    renderButton(false, "no-cover-art", "ai-job");
    const btn = screen.getByRole("button", { name: "Embed MusicBrainz cover" });
    expect(btn).toBeEnabled();
    // Says what it does (an immediate write, with its scope), and names no
    // AI when no model can run.
    expect(btn.getAttribute("title")).toContain("embeds it in this album's files right away");
    expect(btn.getAttribute("title")).not.toMatch(/\bAI\b|Claude/);
    fireEvent.click(btn);
    expect(screen.queryByRole("dialog")).toBeNull();
    await waitFor(() => expect(fetchMock).toHaveBeenCalled());
    const [url, init] = fetchMock.mock.calls[0];
    expect(String(url)).toContain("/api/checks/no-cover-art/findings/7/fix");
    expect((init as RequestInit).method).toBe("POST");
    expect((init as RequestInit).body).toBeUndefined();
    await waitFor(() => expect(toasts.success).toHaveBeenCalledWith("done", { duration: 3000 }));
  });

  // Where the fix would need a model, the server's ok:false
  // not-available message is what the user sees.
  it("shows the not-available message when the fix needs a model", async () => {
    fixAnswer = { ok: false, message: NOT_AVAILABLE, job_id: null };
    renderButton(false, "unidentified-album", "ai-job");
    fireEvent.click(screen.getByRole("button", { name: "Look up on MusicBrainz" }));
    await waitFor(() => expect(toasts.error).toHaveBeenCalledWith(NOT_AVAILABLE));
    expect(toasts.success).not.toHaveBeenCalled();
    // The button stays usable (the finding is still open).
    expect(screen.getByRole("button", { name: "Look up on MusicBrainz" })).toBeEnabled();
  });

  it("offers an ai-job fix with the hint dialog when a provider is ready", () => {
    renderButton(true, "no-cover-art", "ai-job");
    const btn = screen.getByRole("button", { name: "Find cover art…" });
    expect(btn).toBeEnabled();
    fireEvent.click(btn);
    expect(screen.getByRole("dialog")).toBeInTheDocument();
  });

  it("leaves deterministic fixes alone without a provider", async () => {
    renderButton(false, "track-gaps-like-deterministic", "deterministic");
    const btn = screen.getByRole("button", { name: /fix/i });
    expect(btn).toBeEnabled();
    fireEvent.click(btn);
    await waitFor(() => expect(fetchMock).toHaveBeenCalled());
    expect(String(fetchMock.mock.calls[0][0])).toContain("/findings/7/fix");
  });
});
