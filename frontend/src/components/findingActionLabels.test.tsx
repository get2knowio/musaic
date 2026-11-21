/**
 * Each finding action says what it does (#329). FindingFixButton is what both
 * the inbox row and the album inspector render, so one wording covers both.
 */
import { afterEach, beforeEach, describe, it, expect, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";

vi.mock("sonner", () => ({ toast: { error: vi.fn(), success: vi.fn() } }));

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
  severity: "warn",
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

beforeEach(() => {
  vi.stubGlobal(
    "fetch",
    vi.fn(async () => new Response(JSON.stringify({ jobs: [] }), {
      status: 200,
      headers: { "content-type": "application/json" },
    })),
  );
});
afterEach(() => {
  vi.unstubAllGlobals();
});

describe("finding action labels", () => {
  const dialogLabels: [string, string][] = [
    ["filename-mismatch", "Review filenames…"],
    ["duplicate-tracks", "Compare duplicate copies…"],
    ["inconsistent-artist-sort", "Choose artist sort name…"],
    ["multi-disc-layout", "Preview disc layout…"],
  ];
  for (const [check, label] of dialogLabels) {
    for (const ready of [false, true]) {
      it(`${check} says "${label}" ${ready ? "with" : "without"} a model`, () => {
        renderButton(ready, check, "deterministic");
        expect(screen.getByRole("button", { name: label })).toBeEnabled();
        expect(screen.queryByRole("button", { name: /^(Resolve|Fix)/ })).toBeNull();
      });
    }
  }

  const fixLabels: [string, string, string][] = [
    ["no-cover-art", "Embed MusicBrainz cover", "Find cover art…"],
    ["unidentified-album", "Look up on MusicBrainz", "Identify album…"],
    ["tags-out-of-sync", "Write MusicBrainz tags", "Fill missing tags…"],
    ["track-gaps", "Renumber tracks", "Check track numbers…"],
  ];
  for (const [check, modelFree, withModel] of fixLabels) {
    it(`${check}: lookup/write wording without a model, hint dialog wording with one`, () => {
      const first = renderButton(false, check, "ai-job");
      expect(screen.getByRole("button", { name: modelFree })).toBeEnabled();
      first.unmount();
      renderButton(true, check, "ai-job");
      expect(screen.getByRole("button", { name: withModel })).toBeEnabled();
    });
  }
});
