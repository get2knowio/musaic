/**
 * Converting a multi-disc set kept in one folder from its Resolve dialog:
 * Convert asks first and says plainly what will happen, sends back the
 * preview's digest, closes on success, and stays open with the reasons
 * (per file, and the record's location) when the conversion stopped part
 * way or the server refused. A set kept in several folders offers no
 * Convert button.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";

import MultiDiscResolveDialog from "@/components/MultiDiscResolveDialog";
import { confirmationText } from "@/lib/multiDisc";
import type { Finding, MultiDiscConversionPreview } from "@/lib/types";

const ALBUM_ID = "20b0b3ad8877d2df";
const PREVIEW = "/api/checks/multi-disc-layout/findings/7/conversion-preview";
const CONVERT = "/api/checks/multi-disc-layout/findings/7/convert";

let calls: { url: string; method: string; body: string | null }[];
let previewAnswer: () => Promise<Response>;
let convertAnswer: () => Promise<Response>;

function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

beforeEach(() => {
  calls = [];
  previewAnswer = async () => json(oneFolder);
  convertAnswer = async () => json({ detail: "not found" }, 404);
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string, init?: RequestInit) => {
      calls.push({
        url: String(url),
        method: init?.method ?? "GET",
        body: (init?.body as string | undefined) ?? null,
      });
      if (String(url) === PREVIEW) return previewAnswer();
      if (String(url) === CONVERT) return convertAnswer();
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
  summary: "'Band - Record (disc 1 of 2) [FLAC]' holds 2 discs in one folder",
  detail: { headline: "'Band - Record (disc 1 of 2) [FLAC]'" },
  first_seen_at: "2026-11-14T00:00:00Z",
  last_seen_at: "2026-11-14T00:00:00Z",
  resolved_at: null,
  ignored_at: null,
  ignored_reason: null,
};

const oneFolder: MultiDiscConversionPreview = {
  finding_id: 7,
  album_id: ALBUM_ID,
  state: "one_folder",
  convertible: true,
  conversion_available: true,
  digest: "d1g3st",
  resume: null,
  headline: "'Band - Record (disc 1 of 2) [FLAC]'",
  reason: null,
  numbering: "per_disc",
  disc_total: null,
  present_discs: [1, 2],
  absent_discs: [],
  members: [
    { album_id: ALBUM_ID, dir_name: "Band - Record (disc 1 of 2) [FLAC]", disc: 1, missing: false },
    { album_id: ALBUM_ID, dir_name: "Band - Record (disc 1 of 2) [FLAC]", disc: 2, missing: false },
  ],
  is_compilation: false,
  template: "{tracknumber:02} - {title}",
  dir_template: "{albumartist} - {album} [FLAC]",
  tracks: [
    { album_id: ALBUM_ID, disc: 1, filename: "01 - One.flac", title: "One", tracknumber: "1", track: 1, new_track: 1, new_filename: "01 - One.flac", has_disc_tags: true, tags_done: false },
    { album_id: ALBUM_ID, disc: 2, filename: "01 - Two.flac", title: "Two", tracknumber: "1", track: 1, new_track: 2, new_filename: "02 - Two.flac", has_disc_tags: true, tags_done: false },
  ],
  track_total: 2,
  target_folder: { current: "Band - Record (disc 1 of 2) [FLAC]", new: "Band - Record [FLAC]" },
  removes_disc_tags: true,
  folders: [],
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

describe("converting a set kept in one folder", () => {
  it("says what will happen before anything is sent", () => {
    expect(confirmationText(oneFolder)).toBe(
      "2 files will be renumbered 1 to 2 and lose their disc tags (DISCNUMBER, DISCTOTAL), 1 file will be renamed, the folder will be renamed to Band - Record [FLAC]. This cannot be undone from Musaic, but a record of every old name and tag value is kept.",
    );
  });

  it("asks, then sends the preview's digest and closes on success", async () => {
    convertAnswer = async () =>
      json({
        ok: true,
        interrupted: false,
        album_id: "new-id",
        user_message: "Converted: 2 files renumbered, 1 renamed, the folder renamed.",
        files: [],
        folder: { old: "a", new: "b", outcome: "renamed" },
        record: "/cfg/conversions/x.json",
        attempts: 1,
      });
    const onClose = vi.fn();
    wrap(<MultiDiscResolveDialog finding={finding} open onClose={onClose} />);
    fireEvent.click(await screen.findByRole("button", { name: "Convert…" }));
    // Nothing is sent until the user confirms.
    expect(calls.some((c) => c.method === "POST")).toBe(false);
    expect(screen.getByText(/This cannot be undone from Musaic, but a record/)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Convert now" }));
    await waitFor(() => expect(onClose).toHaveBeenCalled());
    const post = calls.find((c) => c.method === "POST");
    expect(post?.url).toBe(CONVERT);
    expect(JSON.parse(post?.body ?? "{}")).toEqual({ digest: "d1g3st" });
  });

  it("stays open with the reason for each file when it stopped part way", async () => {
    convertAnswer = async () =>
      json({
        ok: false,
        interrupted: false,
        album_id: ALBUM_ID,
        user_message: "The tags of 1 file could not be written; the folder was not renamed.",
        files: [
          { filename: "01 - One.flac", new_filename: "01 - One.flac", rename: "already", tags: "written" },
          {
            filename: "01 - Two.flac",
            new_filename: "02 - Two.flac",
            rename: "renamed",
            tags: "failed",
            reason_code: "in_use",
            user_message: "The track number and disc tags of “02 - Two.flac” could not be written because another program is using it.",
          },
        ],
        folder: { old: "a", new: "b", outcome: "not_reached" },
        record: "/cfg/conversions/x.json",
        attempts: 1,
      });
    const onClose = vi.fn();
    wrap(<MultiDiscResolveDialog finding={finding} open onClose={onClose} />);
    fireEvent.click(await screen.findByRole("button", { name: "Convert…" }));
    fireEvent.click(screen.getByRole("button", { name: "Convert now" }));
    await screen.findByText("The set was not fully converted");
    expect(screen.getByText(/another program is using it/)).toBeTruthy();
    expect(screen.getByText(/What was done stays done/)).toBeTruthy();
    expect(screen.getByText("/cfg/conversions/x.json")).toBeTruthy();
    expect(onClose).not.toHaveBeenCalled();
    // The plan is fetched again to show what is left.
    await waitFor(() => expect(calls.filter((c) => c.url === PREVIEW).length).toBeGreaterThan(1));
  });

  it("stays open and says nothing was converted on a refusal", async () => {
    convertAnswer = async () =>
      json(
        {
          detail: "The folder or the settings changed since the preview was made, so nothing was changed.",
          reason_code: "plan_changed",
          user_message: "The folder or the settings changed since the preview was made, so nothing was changed. Look at the new preview, then convert again.",
          digest: "other",
        },
        409,
      );
    const onClose = vi.fn();
    wrap(<MultiDiscResolveDialog finding={finding} open onClose={onClose} />);
    fireEvent.click(await screen.findByRole("button", { name: "Convert…" }));
    fireEvent.click(screen.getByRole("button", { name: "Convert now" }));
    await screen.findByText("Nothing was converted");
    expect(screen.getByText(/changed since the preview was made/)).toBeTruthy();
    expect(onClose).not.toHaveBeenCalled();
  });

  it("offers to finish an interrupted conversion", async () => {
    previewAnswer = async () =>
      json({
        ...oneFolder,
        resume: { record: "/cfg/conversions/x.json", started_at: "2026-11-14T00:00:00Z", attempts: 1 },
        tracks: oneFolder.tracks.map((t, i) => ({ ...t, tags_done: i === 0 })),
      });
    wrap(<MultiDiscResolveDialog finding={finding} open onClose={() => {}} />);
    await screen.findByText("A conversion of this set did not finish");
    fireEvent.click(screen.getByRole("button", { name: "Finish converting…" }));
    expect(screen.getByText(/1 more file will be renumbered/)).toBeTruthy();
  });
});

describe("a set kept in several folders", () => {
  it("still says converting it is not available, and offers no Convert", async () => {
    previewAnswer = async () =>
      json({ ...oneFolder, state: "folders", conversion_available: false, folders: [] });
    wrap(<MultiDiscResolveDialog finding={finding} open onClose={() => {}} />);
    await screen.findByText(/several folders is not available yet/);
    expect(screen.queryByRole("button", { name: /convert/i })).toBeNull();
    expect(calls.every((c) => c.method === "GET")).toBe(true);
  });
});
