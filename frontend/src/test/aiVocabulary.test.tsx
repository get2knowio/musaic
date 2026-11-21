/**
 * Without a ready AI provider no label, tooltip or message names AI or
 * Claude; with one, the original wording stands. Each surface that names
 * AI is rendered both ways. (The Settings page keeps one
 * deliberate notice, AI_UNAVAILABLE, under its "AI" group; it is checked
 * separately at the end.)
 */
import { afterEach, beforeEach, describe, it, expect, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";
import type { ReactNode } from "react";

vi.mock("sonner", () => ({
  toast: Object.assign(vi.fn(), { error: vi.fn(), success: vi.fn(), warning: vi.fn() }),
}));

import Sidebar from "@/components/Sidebar";
import JobStatusBadge from "@/components/JobStatusBadge";
import ReviewPage from "@/pages/Review";
import SettingsPage from "@/pages/Settings";
import BackupPanel from "@/pages/SettingsBackup";
import PropTable from "@/components/DetailSheet/PropTable";
import AlbumPanel from "@/components/DetailSheet";
import ResolutionBadge from "@/components/DetailSheet/ResolutionBadge";
import { AI_UNAVAILABLE } from "@/lib/aiGating";
import type { AiProvidersResponse } from "@/lib/api";
import type { AlbumResolution, DbInfo, Job } from "@/lib/types";
import { makeAlbum, makeJob } from "@/test/fixtures";

const AI_WORDS = /\bAI\b|Claude/;

const providers = (ready: boolean): AiProvidersResponse => ({
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

const album = makeAlbum({ id: "a1", title: "Old Title", metadata_ai_acted_on_at: null });

const fact = (value: string) => ({ value, source: "musicbrainz", confidence: 1 });

const resolution = {
  album_id: "a1",
  anchor: fact("11111111-1111-1111-1111-111111111111"),
  anchor_strategy: "ai",
  canonical_title: fact("New Title"),
  canonical_album_artist: null,
  canonical_year: null,
  canonical_album_artist_sort: null,
  canonical_genre: null,
  cover_url: null,
  tracks: [],
  unresolved_track_count: 0,
  notes: ["trace line"],
} as unknown as AlbumResolution;

const dbInfo = {
  path: "/x/library.db",
  exists: true,
  size_bytes: 1024,
  schema_version: 16,
  current_schema: 16,
  modified_at: null,
  counts: {
    albums: 3,
    albums_with_mbid: 1,
    track_fingerprints: 0,
    track_tags: 0,
    tag_vocabulary: 0,
    ai_jobs: 2,
    check_findings_open: 0,
  },
} as unknown as DbInfo;

function client(ready: boolean, jobs: Job[] = []): QueryClient {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
  qc.setQueryData(["ai-providers"], providers(ready));
  qc.setQueryData(["jobs"], jobs);
  qc.setQueryData(["agent", "approvals", "pending"], []);
  qc.setQueryData(["checks"], []);
  qc.setQueryData(["findings"], []);
  qc.setQueryData(["albums-snapshot"], [album]);
  qc.setQueryData(["album-resolution", "a1"], resolution);
  qc.setQueryData(["tracks", "a1"], { album_id: "a1", tracks: [] });
  qc.setQueryData(["db-info"], dbInfo);
  qc.setQueryData(["db-snapshots"], { snapshots: [] });
  return qc;
}

function renderWith(ready: boolean, ui: ReactNode, jobs: Job[] = []) {
  return render(
    <QueryClientProvider client={client(ready, jobs)}>
      <MemoryRouter>{ui}</MemoryRouter>
    </QueryClientProvider>,
  );
}

/** Everything a user can read: text plus tooltips and accessible labels. */
function visibleWords(): string {
  const attrs = Array.from(document.querySelectorAll("[title],[aria-label],[placeholder]")).flatMap((e) => [
    e.getAttribute("title") ?? "",
    e.getAttribute("aria-label") ?? "",
    e.getAttribute("placeholder") ?? "",
  ]);
  return [document.body.textContent ?? "", ...attrs].join("\n");
}

beforeEach(() => {
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

const completedJob = makeJob({
  id: "j1",
  album_id: "a1",
  album_title: "Old Title",
  job_type: "metadata",
  status: "completed",
});

const reviewJob = makeJob({
  id: "j2",
  album_id: "a1",
  album_title: "Old Title",
  job_type: "metadata",
  status: "completed",
  result: {
    metadata: { suggestions: { title: "New Title" }, tracks: [], notes: "", confidence: "high" },
  },
});

/** Each surface that names AI, and the wording it shows when ready. */
const surfaces: { name: string; ui: () => ReactNode; jobs?: Job[]; act?: () => void; namesAi: string[] }[] = [
  { name: "Sidebar", ui: () => <Sidebar />, namesAi: ["Review"] },
  { name: "Review, empty", ui: () => <ReviewPage />, namesAi: ["Nothing to review"] },
  {
    name: "Review, delete dialog",
    ui: () => <ReviewPage />,
    jobs: [completedJob],
    act: () => fireEvent.click(screen.getByRole("button", { name: "Delete job" })),
    namesAi: ["discard Claude's suggestions"],
  },
  { name: "Job status badge", ui: () => <JobStatusBadge job={makeJob({ status: "queued", job_type: "metadata" })} />, namesAi: ["AI job: queued"] },
  { name: "Album info", ui: () => <PropTable album={album} aiReady={READY_FLAG.value} />, namesAi: ["AI reviewed"] },
  {
    name: "Album panel, a job's corrections under review",
    ui: () => <AlbumPanel album={album} onClose={() => {}} />,
    jobs: [reviewJob],
    namesAi: ["Suggested corrections", "Ask AI for corrections…"],
  },
  {
    name: "Album panel, MusicBrainz corrections",
    ui: () => <AlbumPanel album={album} onClose={() => {}} />,
    namesAi: ["MusicBrainz corrections", "Ask AI for corrections…"],
  },
  { name: "Resolution badge", ui: () => <ResolutionBadge albumId="a1" />, namesAi: ["Matched via Claude AI", "without Claude AI"] },
  { name: "Backup panel", ui: () => <BackupPanel onRestored={() => {}} />, namesAi: ["AI-driven MBID anchors", "AI jobs"] },
];

/** PropTable takes readiness as a prop from the panel. */
const READY_FLAG = { value: false };

describe("AI vocabulary follows ai.ready", () => {
  for (const s of surfaces) {
    it(`${s.name}: no AI or Claude wording without a ready provider`, async () => {
      READY_FLAG.value = false;
      renderWith(false, s.ui(), s.jobs);
      s.act?.();
      await waitFor(() => expect(document.body.textContent).not.toBe(""));
      expect(visibleWords()).not.toMatch(AI_WORDS);
    });
    it(`${s.name}: the AI wording with a ready provider`, async () => {
      READY_FLAG.value = true;
      renderWith(true, s.ui(), s.jobs);
      s.act?.();
      for (const phrase of s.namesAi) {
        await waitFor(() => expect(visibleWords()).toContain(phrase));
      }
    });
  }

  it("Settings: the AI worker card is hidden and the AcoustID help names no AI; the one notice stays", () => {
    renderWith(false, <SettingsPage />);
    expect(screen.queryByRole("heading", { name: "AI worker" })).toBeNull();
    expect(
      screen.getByText("Used only by the track-gaps and unidentified-album fix paths. Stored locally."),
    ).toBeInTheDocument();
    expect(document.body.textContent).not.toContain("Claude");
    // The deliberate notice that this build has no AI backend, the only place
    // that says AI is missing.
    expect(screen.getByText(AI_UNAVAILABLE)).toBeInTheDocument();
  });

  it("Settings: with a ready provider the AI worker card and the original AcoustID help stay", () => {
    renderWith(true, <SettingsPage />);
    expect(screen.getByRole("heading", { name: "AI worker" })).toBeInTheDocument();
    expect(
      screen.getByText(
        "Used only by the track-gaps and unidentified-album fix paths. Stored locally; never sent to Claude.",
      ),
    ).toBeInTheDocument();
  });
});
