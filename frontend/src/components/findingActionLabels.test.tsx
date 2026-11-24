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
import type { Finding } from "@/lib/types";

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

function renderButton(checkId: string, fixKind: "ai-job" | "deterministic") {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
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
    it(`${check} says "${label}"`, () => {
      renderButton(check, "deterministic");
      expect(screen.getByRole("button", { name: label })).toBeEnabled();
      expect(screen.queryByRole("button", { name: /^(Resolve|Fix)/ })).toBeNull();
    });
  }

  const RESEARCH =
    "Anything it can't settle is sent to your assistant for research. Add a hint to help.";
  // One copy per research-capable check (contracts/wording.md, FR-007).
  const fixLabels: [string, string, string][] = [
    ["no-cover-art", "Find cover art…", `Tries MusicBrainz and the Cover Art Archive first. ${RESEARCH}`],
    ["unidentified-album", "Identify album…", `Tries MusicBrainz first. ${RESEARCH}`],
    ["tags-out-of-sync", "Fill missing tags…", `Tries MusicBrainz first. ${RESEARCH}`],
    ["track-gaps", "Check track numbers…", `Tries MusicBrainz first. ${RESEARCH}`],
  ];
  for (const [check, label, title] of fixLabels) {
    it(`${check}: "${label}" says the resolver goes first and the rest goes to the assistant`, () => {
      renderButton(check, "ai-job");
      const btn = screen.getByRole("button", { name: label });
      expect(btn).toBeEnabled();
      expect(btn.getAttribute("title")).toBe(title);
    });
  }
});
