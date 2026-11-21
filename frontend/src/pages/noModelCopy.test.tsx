/**
 * Copy that describes what happens without a model backend.
 * - Settings → AI worker → Queue status: a job that failed only because no
 *   AI backend is available does not pause the queue (other failures do).
 * - Review → bulk identify dialog: albums the bulk pass can't match
 *   are not sent to the per-finding Fix "to escalate to AI" (that Fix
 *   answers ok:false with the not-available message in this build).
 */
import { afterEach, beforeEach, describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter, Route, Routes } from "react-router-dom";

vi.mock("sonner", () => ({ toast: { error: vi.fn(), success: vi.fn() } }));

import SettingsPage from "@/pages/Settings";
import ReviewPage from "@/pages/Review";
import type { AiProvidersResponse } from "@/lib/api";
import type { CheckDetail, Finding } from "@/lib/types";

// Severity "warn": informational findings are not on the default view.

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

const finding: Finding = {
  id: 1,
  check_id: "unidentified-album",
  album_id: "album-1",
  severity: "warn",
  summary: "Not yet identified on MusicBrainz",
  detail: null,
  first_seen_at: "2026-01-01T00:00:00Z",
  last_seen_at: "2026-01-01T00:00:00Z",
  resolved_at: null,
  ignored_at: null,
  ignored_reason: null,
};

const detail: CheckDetail = {
  meta: {
    id: "unidentified-album",
    label: "Unidentified album",
    description: "d",
    category: "metadata" as CheckDetail["meta"]["category"],
    cost: "cheap" as CheckDetail["meta"]["cost"],
    runs_during_scan: true,
    fix_kind: "ai-job",
    bulk_action_label: "Identify on MusicBrainz",
  },
  runs: [],
  findings: [finding],
};

function client(ready: boolean): QueryClient {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  qc.setQueryData(["ai-providers"], provider(ready));
  qc.setQueryData(["checks"], [{ meta: detail.meta, open_count: 1, last_run: null }]);
  qc.setQueryData(["findings"], [finding]);
  qc.setQueryData(["albums-snapshot"], []);
  qc.setQueryData(["jobs"], []);
  return qc;
}

beforeEach(() => {
  // Anything not preloaded answers 404; the pages show their error states.
  vi.stubGlobal(
    "fetch",
    vi.fn(async () =>
      new Response(JSON.stringify({ detail: "not found" }), {
        status: 404,
        headers: { "content-type": "application/json" },
      }),
    ),
  );
});
afterEach(() => vi.unstubAllGlobals());

describe("no-model copy", () => {
  // Without a ready provider the AI worker card (parallel jobs, queue
  // status) is hidden; with one, the no-model help text stands.
  it("Settings: no AI worker card when no provider can run", () => {
    render(
      <QueryClientProvider client={client(false)}>
        <MemoryRouter>
          <SettingsPage />
        </MemoryRouter>
      </QueryClientProvider>,
    );
    expect(screen.queryByRole("heading", { name: "AI worker" })).toBeNull();
    expect(screen.queryByText("Parallel jobs")).toBeNull();
    expect(screen.queryByText("Queue status")).toBeNull();
  });

  it("Settings: the queue-status help says a no-backend failure does not pause the queue", () => {
    render(
      <QueryClientProvider client={client(true)}>
        <MemoryRouter>
          <SettingsPage />
        </MemoryRouter>
      </QueryClientProvider>,
    );
    expect(screen.getByRole("heading", { name: "AI worker" })).toBeInTheDocument();
    const help = screen.getByText(/The queue auto-pauses when an AI job fails/);
    expect(help.textContent).toBe(
      "The queue auto-pauses when an AI job fails, except when it failed only because no AI backend is available (this build has none). Resume manually after retrying or waiting.",
    );
    expect(help.textContent).not.toMatch(/rate-limit/);
  });

  for (const ready of [false, true]) {
    it(`Review: the identify dialog never offers AI escalation (provider ready: ${ready})`, () => {
      render(
        <QueryClientProvider client={client(ready)}>
          <MemoryRouter initialEntries={[`/review?check=${detail.meta.id}`]}>
            <Routes>
              <Route path="/review" element={<ReviewPage />} />
            </Routes>
          </MemoryRouter>
        </QueryClientProvider>,
      );
      fireEvent.click(screen.getByRole("button", { name: /Identify on MusicBrainz \(1\)/ }));
      const body = screen.getByText(/match stay open/);
      expect(body.textContent).toMatch(
        /match stay open — try editing the directory name to the\s+canonical "Artist - Album" form first\./,
      );
      expect(document.body.textContent).not.toMatch(/escalate to AI/);
    });
  }
});
