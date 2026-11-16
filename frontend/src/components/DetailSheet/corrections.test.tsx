/**
 * The album panel's one corrections diff (#184, #183, #189): resolver data
 * and a finished job's result become the same rows with the same ticks and
 * the same apply; Apply / Discard live in the footer; the cover row
 * previews through the album or the job route and degrades on 404 / 409 /
 * 502; the hero's Cover control is the other way to a cover; AI-only entry
 * points need a ready AI backend.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";

vi.mock("sonner", () => ({
  toast: Object.assign(vi.fn(), { error: vi.fn(), success: vi.fn(), warning: vi.fn() }),
}));
vi.mock("@/lib/sse", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/lib/sse")>()),
  postSse: vi.fn(),
}));

import AlbumPanel from "./index";
import { postSse } from "@/lib/sse";
import { resetApplyRegistryForTests } from "@/lib/applyRegistry";
import { makeAlbum, makeJob } from "@/test/fixtures";
import type { AiProvidersResponse } from "@/lib/api";
import type { Album, AlbumResolution, AppConfig, Job, Track } from "@/lib/types";

const ANCHOR = "11111111-1111-1111-1111-111111111111";
const CAA = `https://coverartarchive.org/release-group/${ANCHOR}/front`;
const fact = (value: string, confidence = 0.95) => ({ value, source: "musicbrainz" as const, confidence });

const ALBUM: Album = makeAlbum({
  id: "a1",
  title: "So Much In Love",
  album_artist: "Artist",
  artist: "Artist",
  year: "2001",
  genre: "Rock",
  has_embedded_cover: true,
});
const NO_COVER: Album = { ...ALBUM, has_embedded_cover: false, cover_file: null };

const TRACKS: Track[] = [
  {
    filename: "01 - One.flac", tracknumber: "1", title: "One", artist: "Artist", duration: 200,
    bpm: null, replaygain_track_gain: null, replaygain_track_peak: null,
    replaygain_album_gain: null, replaygain_album_peak: null,
  },
];

function resolution(over: Partial<AlbumResolution> = {}): AlbumResolution {
  return {
    album_id: "a1",
    anchor: fact(ANCHOR),
    anchor_strategy: "cache",
    canonical_title: fact("Too Much Love"),
    canonical_album_artist: fact("Artist"),
    canonical_year: fact("2001"),
    canonical_album_artist_sort: null,
    canonical_genre: fact("soul", 0.55),
    cover_url: fact(CAA),
    tracks: [
      {
        filename: "01 - One.flac", recording_mbid: null, fingerprint_score: null, fingerprinted: false,
        canonical_title: fact("One!"), canonical_artist: fact("Artist"), canonical_tracknumber: fact("1"),
        canonical_duration_s: null,
      },
    ],
    unresolved_track_count: 0,
    notes: [],
    ...over,
  };
}

/** The job the synthesizer would write for the same resolver data. */
function finishedJob(over: Partial<Job> = {}): Job {
  return makeJob({
    id: "j1",
    album_id: "a1",
    job_type: "metadata",
    status: "completed",
    result: {
      metadata: {
        suggestions: { title: "Too Much Love", album_artist: "Artist", artist: "Artist", year: "2001", genre: "soul" },
        tracks: [{ filename: "01 - One.flac", suggestions: { title: "One!" } }],
        notes: "Derived from MusicBrainz.",
        confidence: "high",
        mb_release_group_mbid: ANCHOR,
      },
    },
    ...over,
  });
}

const providers = (ready: boolean): AiProvidersResponse =>
  ready
    ? {
        selected: { provider_id: "claude", model_id: "m", api_key_set: false },
        providers: [{ id: "claude", accepts_api_key: false, auth_hint: null, ready: true, requirement: null, remedy: null }],
        uninstalled: [],
      }
    : { selected: null, providers: [], uninstalled: [] } as unknown as AiProvidersResponse;

function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
}
const jpeg = () =>
  new Response(new Uint8Array([0xff, 0xd8, 0xff]), { headers: { "content-type": "image/jpeg" } });

/** What each cover-preview request answers, in turn (last one repeats). */
let previewAnswers: (() => Response)[];
let fetchMock: ReturnType<typeof vi.fn>;

beforeEach(() => {
  resetApplyRegistryForTests();
  previewAnswers = [() => jpeg()];
  fetchMock = vi.fn(async (url: string, init?: RequestInit) => {
    if (url.endsWith("/cover-preview")) {
      const next = previewAnswers.length > 1 ? previewAnswers.shift()! : previewAnswers[0];
      return next();
    }
    if (url.endsWith("/acted-on") && init?.method === "POST") return json(finishedJob());
    return json({ detail: "not found" }, 404);
  });
  vi.stubGlobal("fetch", fetchMock);
  vi.mocked(postSse).mockReset();
});
afterEach(() => {
  vi.unstubAllGlobals();
  vi.clearAllMocks();
});

interface Setup {
  album?: Album;
  res?: AlbumResolution | null;
  jobs?: Job[];
  aiReady?: boolean;
}

function renderPanel({ album = ALBUM, res = resolution(), jobs = [], aiReady = false }: Setup = {}) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
  qc.setQueryData(["config"], { library_dir: "/music", exists: true } as AppConfig);
  qc.setQueryData(["ai-providers"], providers(aiReady));
  qc.setQueryData(["jobs"], jobs);
  qc.setQueryData(["checks"], []);
  qc.setQueryData(["tracks", album.id], { album_id: album.id, tracks: TRACKS });
  if (res) qc.setQueryData(["album-resolution", album.id], res);
  const utils = render(
    <QueryClientProvider client={qc}>
      <MemoryRouter>
        <AlbumPanel album={album} onClose={() => {}} />
      </MemoryRouter>
    </QueryClientProvider>,
  );
  return { ...utils, qc };
}

const panel = () => screen.getByRole("complementary");
const diff = () => screen.queryByRole("region", { name: /corrections/i });
const rowLabels = () =>
  within(diff()!).getAllByRole("checkbox").map((c) => c.getAttribute("aria-label"));
const box = (name: string) => within(diff()!).getByRole("checkbox", { name });
const footerButton = (name: string | RegExp) => within(panel()).queryByRole("button", { name });
const sentPayload = () => vi.mocked(postSse).mock.calls.at(-1)?.[1];

/** Answer the apply stream: one file written, then the rewritten album. */
function applySucceeds() {
  vi.mocked(postSse).mockImplementation(async (_url, _body, events) => {
    events.start?.({ file_count: 1 });
    events.done?.({ saved_count: 1, errors: 0, art_changed_count: 0 });
    events.updated_album?.({ ...ALBUM, title: "Too Much Love" });
  });
}

describe("one diff, two feeds", () => {
  it("resolver data: per-field rows with ticks, genre and the cover; Apply / Discard in the footer", async () => {
    renderPanel({ album: NO_COVER });
    expect(diff()).toHaveAccessibleName("MusicBrainz corrections");
    expect(rowLabels()).toEqual([
      "Apply title correction",
      "Apply genre correction",
      "Apply 01 - One.flac · title correction",
      "Apply cover correction",
    ]);
    expect(box("Apply title correction")).toBeChecked();
    expect(box("Apply genre correction")).not.toBeChecked();
    await waitFor(() => expect(box("Apply cover correction")).toBeChecked());
    expect(footerButton("Apply 3 changes")).toBeInTheDocument();
    expect(footerButton("Discard")).toBeInTheDocument();
    // The old routes are gone.
    for (const old of [/Find MusicBrainz corrections/, /Suggest fixes/, /Get cover from MusicBrainz/, /Find cover art/]) {
      expect(footerButton(old)).toBeNull();
    }
    // Tracks and the other sections stay visible beneath the diff.
    expect(within(panel()).getByText("Tracks")).toBeInTheDocument();
    expect(within(panel()).getByText("Album info")).toBeInTheDocument();
  });

  it("a finished job's result shows the same rows as the resolver for the same data, without taking over the panel", async () => {
    const fromResolver = renderPanel();
    const resolverRows = rowLabels();
    fromResolver.unmount();

    renderPanel({ jobs: [finishedJob()], res: resolution({ anchor: null }) });
    expect(rowLabels()).toEqual(resolverRows);
    expect(within(panel()).getByText("Tracks")).toBeInTheDocument();
    expect(within(panel()).getByText("Album info")).toBeInTheDocument();
    // No identifier status line.
    expect(panel()).not.toHaveTextContent(/identified as/);
    expect(panel()).toHaveTextContent("Derived from MusicBrainz.");
  });

  it("whitespace-only or unchanged proposals make no rows", () => {
    const job = finishedJob({
      result: {
        metadata: {
          suggestions: { title: "  So Much  In Love ", year: "   ", genre: "" },
          tracks: [{ filename: "01 - One.flac", suggestions: { title: " " } }],
          notes: "",
          confidence: "high",
        },
      },
    });
    renderPanel({ jobs: [job] });
    expect(within(diff()!).queryAllByRole("checkbox")).toHaveLength(0);
    expect(diff()).toHaveTextContent("No corrections to apply.");
    expect(footerButton(/^Apply/)).toBeNull();
    expect(footerButton("Discard")).toBeInTheDocument();
  });

  it("a case-only proposal is a row, applied as proposed", async () => {
    applySucceeds();
    renderPanel({
      res: resolution({ canonical_title: fact("So Much in Love"), canonical_genre: null, tracks: [] }),
    });
    expect(rowLabels()).toEqual(["Apply title correction"]);
    expect(diff()).toHaveTextContent("So Much In Love");
    expect(diff()).toHaveTextContent("So Much in Love");
    fireEvent.click(footerButton("Apply 1 change")!);
    await waitFor(() => expect(sentPayload()).toBeDefined());
    expect(sentPayload()).toMatchObject({ suggestions: { title: "So Much in Love" } });
  });

  it("an album the resolver has nothing to correct shows no diff and no footer", () => {
    renderPanel({
      res: resolution({ canonical_title: fact("So Much In Love"), canonical_genre: null, tracks: [] }),
    });
    expect(diff()).toBeNull();
    expect(footerButton(/^Apply/)).toBeNull();
    expect(footerButton("Discard")).toBeNull();
  });
});

describe("per-row selection shapes the apply", () => {
  it("sends exactly the ticked rows (resolver)", async () => {
    applySucceeds();
    renderPanel();
    fireEvent.click(box("Apply 01 - One.flac · title correction"));
    fireEvent.click(box("Apply genre correction"));
    expect(footerButton("Apply 2 changes")).toBeInTheDocument();
    fireEvent.click(footerButton("Apply 2 changes")!);
    await waitFor(() => expect(postSse).toHaveBeenCalledTimes(1));
    expect(vi.mocked(postSse).mock.calls[0][0]).toBe("/api/albums/a1/apply/stream");
    expect(sentPayload()).toEqual({
      suggestions: { title: "Too Much Love", genre: "soul" },
      track_suggestions: {},
      cover_url: null,
    });
  });

  it("Apply is disabled with nothing ticked", () => {
    renderPanel({ res: resolution({ canonical_genre: null, tracks: [] }) });
    fireEvent.click(box("Apply title correction"));
    expect(footerButton("Apply")).toBeDisabled();
  });

  it("a job's apply sends its ticked rows and marks the job applied", async () => {
    applySucceeds();
    renderPanel({ jobs: [finishedJob()] });
    fireEvent.click(footerButton("Apply 2 changes")!);
    await waitFor(() => expect(postSse).toHaveBeenCalledTimes(1));
    expect(sentPayload()).toEqual({
      suggestions: { title: "Too Much Love" },
      track_suggestions: { "01 - One.flac": { title: "One!" } },
      cover_url: null,
    });
    await waitFor(() =>
      expect(fetchMock).toHaveBeenCalledWith(
        "/api/ai/jobs/j1/acted-on",
        expect.objectContaining({ method: "POST", body: JSON.stringify({ action: "applied" }) }),
      ),
    );
  });

  it("Discard marks a job discarded; on resolver data it hides the diff", async () => {
    const { unmount } = renderPanel({ jobs: [finishedJob()] });
    fireEvent.click(footerButton("Discard")!);
    await waitFor(() =>
      expect(fetchMock).toHaveBeenCalledWith(
        "/api/ai/jobs/j1/acted-on",
        expect.objectContaining({ body: JSON.stringify({ action: "discarded" }) }),
      ),
    );
    unmount();

    renderPanel();
    fireEvent.click(footerButton("Discard")!);
    expect(diff()).toBeNull();
    expect(postSse).not.toHaveBeenCalled();
  });

  it("while an apply runs the diff is gone and the footer says so (apply registry)", async () => {
    vi.mocked(postSse).mockImplementation(async (_url, _body, events) => {
      events.start?.({ file_count: 1 });
      await new Promise(() => {});
    });
    renderPanel();
    fireEvent.click(footerButton("Apply 2 changes")!);
    await screen.findByText(/Writing FLAC tags/);
    expect(diff()).toBeNull();
    expect(panel()).toHaveTextContent("Writing — you can switch albums or close the panel");
    expect(footerButton(/^Apply/)).toBeNull();
  });
});

describe("the cover row", () => {
  it("resolver: previews through the album route and applies the cover when ticked", async () => {
    applySucceeds();
    renderPanel({ album: NO_COVER, res: resolution({ canonical_title: fact("So Much In Love"), canonical_genre: null, tracks: [] }) });
    const img = await within(diff()!).findByAltText("proposed cover");
    expect(img.getAttribute("src")).toBe("data:image/jpeg;base64,/9j/");
    expect(fetchMock).toHaveBeenCalledWith("/api/albums/a1/cover-preview", expect.anything());
    // Never the remote address as an image.
    for (const i of Array.from(panel().querySelectorAll("img"))) {
      expect(i.getAttribute("src") ?? "").not.toMatch(/^https?:/);
    }
    fireEvent.click(footerButton("Apply 1 change")!);
    await waitFor(() => expect(postSse).toHaveBeenCalled());
    expect(sentPayload()).toEqual({ suggestions: {}, track_suggestions: {}, cover_url: CAA });
  });

  it("job: previews through the job route", async () => {
    const job = finishedJob({
      job_type: "metadata+cover",
      result: {
        ...finishedJob().result!,
        cover: { cover_url: CAA, source: "Cover Art Archive", notes: "", confidence: "high" },
      },
    });
    renderPanel({ jobs: [job] });
    await within(diff()!).findByAltText("proposed cover");
    expect(fetchMock).toHaveBeenCalledWith("/api/ai/jobs/j1/cover-preview", expect.anything());
    expect(fetchMock).not.toHaveBeenCalledWith("/api/albums/a1/cover-preview", expect.anything());
  });

  it("404: nothing to preview, and nothing to tick", async () => {
    previewAnswers = [() => json({ detail: "This album has no MusicBrainz cover to preview" }, 404)];
    renderPanel({ album: NO_COVER });
    await within(diff()!).findByText(/No cover to preview/);
    expect(box("Apply cover correction")).toBeDisabled();
    expect(box("Apply cover correction")).not.toBeChecked();
    expect(within(diff()!).queryByAltText("proposed cover")).toBeNull();
    expect(footerButton("Apply 2 changes")).toBeInTheDocument();
  });

  it("409: the album is Missing as far as the server knows", async () => {
    previewAnswers = [() => json({ detail: "missing" }, 409)];
    renderPanel({ album: NO_COVER });
    await within(diff()!).findByText(/cannot change/);
    expect(box("Apply cover correction")).toBeDisabled();
  });

  it("502: unavailable, not applied unseen, and Retry fetches again", async () => {
    previewAnswers = [() => json({ detail: "refused" }, 502), () => jpeg()];
    renderPanel({ album: NO_COVER });
    await within(diff()!).findByText(/Preview unavailable/);
    expect(box("Apply cover correction")).toBeDisabled();
    fireEvent.click(within(diff()!).getByRole("button", { name: "Retry preview" }));
    await within(diff()!).findByAltText("proposed cover");
    await waitFor(() => expect(box("Apply cover correction")).toBeChecked());
  });
});

describe("the hero's Cover control", () => {
  const openCoverMenu = async (name: string) => {
    const trigger = within(panel()).getByRole("button", { name });
    fireEvent.keyDown(trigger, { key: "Enter" });
    return screen.findByRole("menu");
  };

  it("an album with a cover: 'Change cover'; 'Use MusicBrainz cover' adds the cover row to the diff", async () => {
    renderPanel({ res: resolution({ canonical_title: fact("So Much In Love"), canonical_genre: null, tracks: [] }) });
    expect(diff()).toBeNull();
    const menu = await openCoverMenu("Change cover");
    expect(within(menu).getAllByRole("menuitem").map((i) => i.textContent?.trim())).toEqual([
      "Use MusicBrainz cover",
      "Upload, paste or URL…",
    ]);
    fireEvent.click(within(menu).getByRole("menuitem", { name: "Use MusicBrainz cover" }));
    await waitFor(() => expect(diff()).not.toBeNull());
    expect(rowLabels()).toEqual(["Apply cover correction"]);
    await within(diff()!).findByAltText("proposed cover");
    expect(footerButton("Apply 1 change")).toBeInTheDocument();
  });

  it("an album without a cover: 'Add cover', the MusicBrainz cover is already proposed, upload opens 'Add cover'", async () => {
    renderPanel({ album: NO_COVER });
    const button = within(panel()).getByRole("button", { name: "Add cover" });
    const menu = await openCoverMenu("Add cover");
    expect(within(menu).queryByRole("menuitem", { name: "Use MusicBrainz cover" })).toBeNull();
    fireEvent.click(within(menu).getByRole("menuitem", { name: "Upload, paste or URL…" }));
    const dialog = await screen.findByRole("dialog", { name: "Add cover" });
    // Focus goes into the dialog, and back to the Cover button after it.
    await waitFor(() => expect(dialog.contains(document.activeElement)).toBe(true));
    fireEvent.click(within(dialog).getByRole("button", { name: "Cancel" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(document.activeElement).toBe(button);
  });

  it("no MusicBrainz cover to offer: only upload", async () => {
    renderPanel({ res: resolution({ cover_url: null }) });
    const menu = await openCoverMenu("Change cover");
    expect(within(menu).getAllByRole("menuitem").map((i) => i.textContent?.trim())).toEqual(["Upload, paste or URL…"]);
  });
});

describe("AI-only entry points", () => {
  it("without a ready AI backend: no AI button, no AI cover item, no hint dialog; corrections still work", async () => {
    renderPanel();
    expect(footerButton(/Ask AI/)).toBeNull();
    expect(panel()).not.toHaveTextContent(/\bAI\b|Claude/);
    expect(footerButton("Apply 2 changes")).toBeInTheDocument();
    const trigger = within(panel()).getByRole("button", { name: "Change cover" });
    fireEvent.keyDown(trigger, { key: "Enter" });
    const menu = await screen.findByRole("menu");
    expect(within(menu).queryByRole("menuitem", { name: /AI/ })).toBeNull();
  });

  it("with AI ready: 'Ask AI for corrections…' opens the hint dialog", async () => {
    renderPanel({ aiReady: true });
    fireEvent.click(footerButton("Ask AI for corrections…")!);
    expect(await screen.findByRole("dialog", { name: /Suggest metadata fixes/ })).toBeInTheDocument();
  });

  it("with AI ready: the Cover menu offers an AI search", async () => {
    renderPanel({ aiReady: true });
    fireEvent.keyDown(within(panel()).getByRole("button", { name: "Change cover" }), { key: "Enter" });
    const menu = await screen.findByRole("menu");
    fireEvent.click(within(menu).getByRole("menuitem", { name: "Ask AI to find a cover…" }));
    expect(await screen.findByRole("dialog", { name: /Find cover art/ })).toBeInTheDocument();
  });

  it("with AI ready and a job under review, the AI button is disabled", () => {
    renderPanel({ aiReady: true, jobs: [finishedJob()] });
    expect(diff()).toHaveAccessibleName("Suggested corrections");
    expect(footerButton("Ask AI for corrections…")).toBeDisabled();
  });
});

describe("a Missing album", () => {
  it("shows no corrections from either feed and no Cover control", async () => {
    const missing = { ...NO_COVER, missing_since: "2026-11-01T10:00:00Z" };
    renderPanel({ album: missing, jobs: [finishedJob()] });
    await act(async () => {});
    expect(diff()).toBeNull();
    expect(within(panel()).queryByRole("button", { name: /cover/i })).toBeNull();
    expect(footerButton(/^Apply|Discard/)).toBeNull();
    expect(fetchMock).not.toHaveBeenCalledWith(expect.stringMatching(/cover-preview$/), expect.anything());
  });
});
