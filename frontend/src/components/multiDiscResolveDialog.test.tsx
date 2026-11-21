/**
 * The multi-disc-layout Resolve dialog shows the conversion preview the
 * server re-derives, the blockers in plain words, an incomplete set's
 * reason, and loading / error states. A set kept in several folders stays
 * read-only (no Convert button, nothing but the preview GET); a
 * convertible set in one folder converts behind a confirmation, sends the
 * preview's digest back, and on a partial failure or a refusal stays open
 * with the reasons.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";

import MultiDiscResolveDialog from "@/components/MultiDiscResolveDialog";
import FindingFixButton from "@/components/FindingFixButton";
import type { Finding, MultiDiscConversionPreview } from "@/lib/types";
import { MemoryRouter } from "react-router-dom";

const ALBUM_ID = "20b0b3ad8877d2df";
const URL = "/api/checks/multi-disc-layout/findings/7/conversion-preview";

let calls: { url: string; method: string }[];
let answer: () => Promise<Response>;

function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

beforeEach(() => {
  calls = [];
  answer = async () => json({ detail: "not found" }, 404);
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string, init?: RequestInit) => {
      calls.push({ url: String(url), method: init?.method ?? "GET" });
      if (String(url) === URL) return answer();
      return json({ detail: "not found" }, 404);
    }),
  );
});
afterEach(() => {
  vi.unstubAllGlobals();
});

const finding: Finding = {
  id: 7,
  check_id: "multi-disc-layout",
  album_id: ALBUM_ID,
  severity: "info",
  summary: "'Band - Record (disc 1 of 2) [FLAC]' is disc 1 of a set kept in 2 folders",
  detail: { headline: "'Band - Record (disc 1 of 2) [FLAC]'" },
  first_seen_at: "2026-11-14T00:00:00Z",
  last_seen_at: "2026-11-14T00:00:00Z",
  resolved_at: null,
  ignored_at: null,
  ignored_reason: null,
};

const folders: MultiDiscConversionPreview = {
  finding_id: 7,
  album_id: ALBUM_ID,
  state: "folders",
  convertible: true,
  conversion_available: true,
  digest: "d1g3st",
  headline: "'Band - Record (disc 1 of 2) [FLAC]'",
  reason: null,
  numbering: null,
  disc_total: 2,
  present_discs: [1, 2],
  absent_discs: [],
  members: [
    { album_id: ALBUM_ID, dir_name: "Band - Record (disc 1 of 2) [FLAC]", disc: 1, missing: false },
    { album_id: "b2", dir_name: "Band - Record (disc 2 of 2) [FLAC]", disc: 2, missing: false },
  ],
  is_compilation: false,
  template: "{tracknumber:02} - {title}",
  dir_template: "{albumartist} - {album} [FLAC]",
  tracks: [
    { album_id: ALBUM_ID, disc: 1, filename: "01 - One.flac", title: "One", tracknumber: "1", track: 1, new_track: 1, new_filename: "01 - One.flac", has_disc_tags: true },
    { album_id: "b2", disc: 2, filename: "01 - Two.flac", title: "Two", tracknumber: "1", track: 1, new_track: 2, new_filename: "02 - Two.flac", has_disc_tags: true },
  ],
  track_total: 2,
  target_folder: { current: "Band - Record (disc 1 of 2) [FLAC]", new: "Band - Record [FLAC]" },
  removes_disc_tags: true,
  folders: [
    { album_id: ALBUM_ID, dir_name: "Band - Record (disc 1 of 2) [FLAC]", disc: 1, destination: true, covers: [{ name: "cover.jpg", action: "kept" }], sidecar: false, other_files: ["notes.txt"] },
    { album_id: "b2", dir_name: "Band - Record (disc 2 of 2) [FLAC]", disc: 2, destination: false, covers: [{ name: "cover.jpg", action: "different" }], sidecar: true, other_files: [] },
  ],
  blockers: [],
};

function wrap(ui: React.ReactElement) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={qc}>
      <MemoryRouter>{ui}</MemoryRouter>
    </QueryClientProvider>,
  );
}

describe("MultiDiscResolveDialog", () => {
  it("shows the members, the old → new table and the covers", async () => {
    answer = async () => json(folders);
    wrap(<MultiDiscResolveDialog finding={finding} open onClose={() => {}} />);
    await screen.findByText("02 - Two.flac");
    expect(screen.getByText(/gather the 2 disc folders into one folder/)).toBeTruthy();
    expect(screen.getByText("Band - Record [FLAC]")).toBeTruthy();
    expect(screen.getByText("Track 2")).toBeTruthy();
    expect(screen.getByText(/different picture/)).toBeTruthy();
    expect(screen.getByText("notes.txt")).toBeTruthy();
    expect(screen.getByText(/This folder is removed once it is empty/)).toBeTruthy();
    expect(screen.getByText("Nothing changes until you confirm.")).toBeTruthy();
    // Convert is offered; nothing but the preview GET happens until then.
    expect(screen.getByRole("button", { name: "Convert…" })).toBeTruthy();
    expect(calls.every((c) => c.method === "GET" && c.url === URL)).toBe(true);
  });

  it("lists the blockers in plain words", async () => {
    answer = async () =>
      json({
        ...folders,
        convertible: false,
        blockers: [
          { code: "track_numbers", message: "Disc 2's tracks are not numbered 1 to 3 (no track 2). Fix the track gaps first." },
          { code: "target_taken", message: "The new folder name Band - Record [FLAC] is already used by another folder (band - record [FLAC])." },
        ],
      });
    wrap(<MultiDiscResolveDialog finding={finding} open onClose={() => {}} />);
    await screen.findByText("2 things would stop a conversion");
    expect(screen.getByText(/Fix the track gaps first/)).toBeTruthy();
    expect(screen.getByText(/already used by another folder/)).toBeTruthy();
  });

  it("says why an incomplete set gets no conversion", async () => {
    answer = async () =>
      json({
        ...folders,
        state: "incomplete",
        convertible: false,
        reason: "Only discs 1, 3 of 3 are here; disc 2 is not.",
        disc_total: 3,
        present_discs: [1, 3],
        absent_discs: [2],
        tracks: [],
        folders: [],
        target_folder: null,
        track_total: null,
      });
    wrap(<MultiDiscResolveDialog finding={finding} open onClose={() => {}} />);
    await screen.findByText("Not every disc is here");
    expect(screen.getByText(/disc 2 is not/)).toBeTruthy();
    expect(screen.queryByText(/new track total/)).toBeNull();
  });

  it("shows a loading state, then an error", async () => {
    let fail: (r: Response) => void = () => {};
    answer = () => new Promise<Response>((resolve) => { fail = resolve; });
    wrap(<MultiDiscResolveDialog finding={finding} open onClose={() => {}} />);
    expect(screen.getByText(/Checking the set's folders/)).toBeTruthy();
    // The dialog retries once; the retry fails at once.
    answer = async () => json({ detail: "Finding not found" }, 404);
    fail(json({ detail: "Finding not found" }, 404));
    await screen.findByText("Couldn't check the set's files", undefined, { timeout: 4000 });
    await waitFor(() => expect(screen.queryByText(/Checking the set's folders/)).toBeNull());
  });
});

describe("FindingFixButton for multi-disc-layout", () => {
  it("offers a Preview disc layout button", () => {
    wrap(<FindingFixButton finding={finding} fixKind="deterministic" />);
    expect(screen.getByRole("button", { name: "Preview disc layout…" })).toBeTruthy();
  });

  it("offers nothing on an ignored finding", () => {
    wrap(<FindingFixButton finding={{ ...finding, ignored_at: "2026-11-14T00:00:00Z" }} fixKind="deterministic" />);
    expect(screen.queryByRole("button", { name: "Preview disc layout…" })).toBeNull();
  });
});
