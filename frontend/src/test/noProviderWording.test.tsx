/**
 * SC-005: the desktop names no model provider. None of "AI job", "AI
 * backend", "Resume queue", "not available in this build", a model-provider
 * setting, or the words "AI" and "Claude" appears in the rendered text,
 * tooltips or accessible labels of Settings, Settings → Agents, Activity,
 * the album panel, Review and the finding buttons. Nothing here seeds a
 * provider list: the desktop no longer asks for one.
 */
import { afterEach, beforeEach, describe, it, expect, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";
import type { ReactNode } from "react";

vi.mock("sonner", () => ({
  toast: Object.assign(vi.fn(), { error: vi.fn(), success: vi.fn(), warning: vi.fn() }),
}));

import Sidebar from "@/components/Sidebar";
import JobStatusBadge from "@/components/JobStatusBadge";
import ActivityPanel from "@/components/ActivityPanel";
import FindingFixButton from "@/components/FindingFixButton";
import ReviewPage from "@/pages/Review";
import SettingsPage from "@/pages/Settings";
import AgentsPanel from "@/pages/SettingsAgents";
import BackupPanel from "@/pages/SettingsBackup";
import PropTable from "@/components/DetailSheet/PropTable";
import AlbumPanel from "@/components/DetailSheet";
import ResolutionBadge from "@/components/DetailSheet/ResolutionBadge";
import { QUEUE_STATE_QUERY_KEY } from "@/hooks/useJobs";
import { openActivityPanel, resetActivityPanel } from "@/lib/activityPanel";
import type { AlbumResolution, DbInfo, Finding, Job } from "@/lib/types";
import { makeAlbum, makeJob } from "@/test/fixtures";

/** The SC-005 strings, plus any provider or model setting. */
const FORBIDDEN: RegExp[] = [
  /AI job/i,
  /AI backend/i,
  /Resume queue/i,
  /not available in this build/i,
  /this build has/i,
  /AI worker/i,
  /Parallel jobs/i,
  /FireCrawl/i,
  /\bprovider\b/i,
  /\bmodel\b/i,
  /\bAI\b/,
  /Claude/,
];

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
  schema_version: 24,
  current_schema: 24,
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

const finding = (checkId: string): Finding => ({
  id: 7,
  check_id: checkId,
  album_id: "a1",
  severity: "warn",
  summary: "s",
  detail: null,
  first_seen_at: "2026-01-01T00:00:00Z",
  last_seen_at: "2026-01-01T00:00:00Z",
  resolved_at: null,
  ignored_at: null,
  ignored_reason: null,
});

const settings = {
  acoustid_api_key_set: false,
  snapshot_retention_count: 5,
  snapshot_min_interval_hours: 24,
  filename_template_single_artist: "",
  filename_template_compilation: "",
  dir_template_single_artist: "",
  dir_template_compilation: "",
};

function client(jobs: Job[]): QueryClient {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
  qc.setQueryData(["config"], { library_dir: "/m", exists: true });
  qc.setQueryData(["settings"], settings);
  qc.setQueryData(["jobs"], jobs);
  qc.setQueryData(QUEUE_STATE_QUERY_KEY, { running: 1, queued: 1, clearable: 2 });
  qc.setQueryData(["research", "work"], []);
  qc.setQueryData(["research", "batches"], []);
  qc.setQueryData(["agent", "approvals", "pending"], []);
  qc.setQueryData(["agent", "pairings"], []);
  qc.setQueryData(["agent", "diagnostics"], []);
  qc.setQueryData(["checks"], []);
  qc.setQueryData(["findings"], []);
  qc.setQueryData(["albums-snapshot"], [album]);
  qc.setQueryData(["album-resolution", "a1"], resolution);
  qc.setQueryData(["tracks", "a1"], { album_id: "a1", tracks: [] });
  qc.setQueryData(["db-info"], dbInfo);
  qc.setQueryData(["db-snapshots"], { snapshots: [] });
  return qc;
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

/** Claude is named only as an assistant host the person may connect (spec 007):
 * the Claude Desktop and skill actions in Settings, Assistants. These phrases
 * are the whole allowance; any other use of the word still fails. */
const HOST_NAME_PHRASES: RegExp[] = [
  /Claude Desktop/g,
  /skill (?:for|in) Claude/g,
  /In Claude, open Settings/g,
  /install a skill into Claude for you/g,
  /Claude on the web and on phones/g,
];

function expectNoProviderWording() {
  let words = visibleWords();
  for (const phrase of HOST_NAME_PHRASES) words = words.replace(phrase, "");
  for (const re of FORBIDDEN) expect(words, `matched ${re}`).not.toMatch(re);
}

beforeEach(() => {
  resetActivityPanel();
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string) => {
      if (String(url) === "/api/scan/status") {
        return new Response(JSON.stringify({ status: "idle", total: 0, scanned: 0, error: null }), {
          status: 200,
          headers: { "content-type": "application/json" },
        });
      }
      return new Response(JSON.stringify({ detail: "not found" }), {
        status: 404,
        headers: { "content-type": "application/json" },
      });
    }),
  );
});
afterEach(() => vi.unstubAllGlobals());

const queued = makeJob({ id: "j0", album_id: "a1", album_title: "Old Title", job_type: "metadata", status: "queued" });
const running = makeJob({ id: "j3", album_id: "a1", album_title: "Old Title", job_type: "cover", status: "running" });
const failed = makeJob({ id: "j4", album_id: "a1", album_title: "Old Title", job_type: "metadata", status: "failed", error: "x" });
const completed = makeJob({
  id: "j2",
  album_id: "a1",
  album_title: "Old Title",
  job_type: "metadata",
  status: "completed",
  result: {
    metadata: { suggestions: { title: "New Title" }, tracks: [], notes: "", confidence: "high" },
  },
});

/** `shows`: a phrase proving the surface rendered what it should. */
const surfaces: { name: string; ui: () => ReactNode; jobs?: Job[]; act?: () => void; shows?: string }[] = [
  { name: "Settings", ui: () => <SettingsPage /> },
  { name: "Settings → Agents", ui: () => <AgentsPanel />, shows: "Add an assistant" },
  { name: "Settings → Backup", ui: () => <BackupPanel onRestored={() => {}} />, shows: "Research requests" },
  {
    name: "Activity",
    ui: () => <ActivityPanel />,
    jobs: [queued, running, failed, completed],
    act: () => openActivityPanel(),
    shows: "Research requests",
  },
  { name: "Sidebar", ui: () => <Sidebar /> },
  { name: "Review, empty", ui: () => <ReviewPage /> },
  { name: "Review, with requests", ui: () => <ReviewPage />, jobs: [completed, failed], shows: "Failed research" },
  { name: "Album panel", ui: () => <AlbumPanel album={album} onClose={() => {}} />, shows: "Request research" },
  { name: "Album panel, a proposal under review", ui: () => <AlbumPanel album={album} onClose={() => {}} />, jobs: [completed] },
  { name: "Album panel, a request waiting", ui: () => <AlbumPanel album={album} onClose={() => {}} />, jobs: [queued] },
  { name: "Album info", ui: () => <PropTable album={album} />, shows: "Reviewed" },
  { name: "Resolution badge", ui: () => <ResolutionBadge albumId="a1" /> },
  { name: "Research request badge", ui: () => <JobStatusBadge job={queued} />, shows: "Research request" },
  ...["no-cover-art", "unidentified-album", "tags-out-of-sync", "track-gaps"].map((check) => ({
    name: `Finding button, ${check}`,
    ui: () => <FindingFixButton finding={finding(check)} fixKind="ai-job" />,
    shows: "sent to your assistant for research",
  })),
  ...["no-cover-art", "unidentified-album"].map((check) => ({
    name: `Finding button, ${check}, hint dialog open`,
    ui: () => <FindingFixButton finding={finding(check)} fixKind="ai-job" />,
    act: () => fireEvent.click(screen.getByRole("button")),
    shows: "Your hint",
  })),
  ...(["queued", "running"] as const).map((status) => ({
    name: `Finding button, request ${status}`,
    ui: () => <FindingFixButton finding={finding("unidentified-album")} fixKind="ai-job" />,
    jobs: [status === "queued" ? queued : { ...queued, status: "running" as const }],
    shows: status === "queued" ? "Research is pending for this album" : "An assistant is working on this",
  })),
];

describe("no provider wording (SC-005)", () => {
  for (const s of surfaces) {
    it(`${s.name}: names no model provider`, async () => {
      render(
        <QueryClientProvider client={client(s.jobs ?? [])}>
          <MemoryRouter>{s.ui()}</MemoryRouter>
        </QueryClientProvider>,
      );
      if (s.act) act(() => s.act!());
      await waitFor(() => expect(document.body.textContent).not.toBe(""));
      if (s.shows) await waitFor(() => expect(visibleWords()).toContain(s.shows));
      expectNoProviderWording();
    });
  }

  it("Activity calls the card Research requests and offers no Resume queue", () => {
    render(
      <QueryClientProvider client={client([queued])}>
        <MemoryRouter>
          <ActivityPanel />
        </MemoryRouter>
      </QueryClientProvider>,
    );
    act(() => openActivityPanel());
    expect(screen.getAllByText("Research requests").length).toBeGreaterThan(0);
    expect(screen.queryByRole("button", { name: /Resume/ })).toBeNull();
  });

  it("Settings: AcoustID help names only AcoustID", () => {
    render(
      <QueryClientProvider client={client([])}>
        <MemoryRouter>
          <SettingsPage />
        </MemoryRouter>
      </QueryClientProvider>,
    );
    expect(
      screen.getByText(
        "Used only by the track-gaps and unidentified-album fix paths. Stored locally; never sent anywhere but AcoustID.",
      ),
    ).toBeInTheDocument();
  });
});
