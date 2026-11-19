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

const D1 = "Band - Record (disc 1 of 3) [FLAC]";
const D2 = "Band - Record (disc 2 of 3) [FLAC]";
const D3 = "Band - Record (disc 3 of 3) [FLAC]";

const folders: MultiDiscConversionPreview = {
  ...oneFolder,
  state: "folders",
  disc_total: 3,
  present_discs: [1, 2, 3],
  members: [
    { album_id: ALBUM_ID, dir_name: D1, disc: 1, missing: false },
    { album_id: "b2", dir_name: D2, disc: 2, missing: false },
    { album_id: "b3", dir_name: D3, disc: 3, missing: false },
  ],
  tracks: [
    { album_id: ALBUM_ID, disc: 1, filename: "01 - A1.flac", title: "A1", tracknumber: "1", track: 1, new_track: 1, new_filename: "01 - A1.flac", has_disc_tags: true, tags_done: false, state: "untouched", folder: D1 },
    { album_id: "b2", disc: 2, filename: "01 - B1.flac", title: "B1", tracknumber: "1", track: 1, new_track: 2, new_filename: "02 - B1.flac", has_disc_tags: true, tags_done: false, state: "untouched", folder: D2 },
    { album_id: "b3", disc: 3, filename: "01 - C1.flac", title: "C1", tracknumber: "1", track: 1, new_track: 3, new_filename: "03 - C1.flac", has_disc_tags: true, tags_done: false, state: "untouched", folder: D3 },
  ],
  track_total: 3,
  target_folder: { current: D1, new: "Band - Record [FLAC]" },
  folders: [
    { album_id: ALBUM_ID, dir_name: D1, disc: 1, destination: true, covers: [{ name: "cover.jpg", action: "kept", done: false }], sidecar: false, other_files: [], junk: [], exists: true },
    { album_id: "b2", dir_name: D2, disc: 2, destination: false, covers: [{ name: "cover.jpg", action: "identical", done: false }], sidecar: true, other_files: [], strays: [{ name: "notes.txt", action: "moved", new_name: null, done: false }, { name: "rip.log", action: "moved", new_name: "rip (disc 2).log", done: false }], junk: [], exists: true },
    { album_id: "b3", dir_name: D3, disc: 3, destination: false, covers: [{ name: "cover.jpg", action: "different", done: false }], sidecar: false, other_files: [], strays: [{ name: "rip.log", action: "identical", new_name: null, done: false }], junk: [{ name: ".DS_Store", done: false }], exists: true },
  ],
  gather: { folded: false, folder_renamed: false, merges: 2, orphan_rows: 0 },
  records_dir: "/cfg/conversions",
};

/** A text/event-stream body the test feeds frame by frame. */
function stream() {
  let push: (s: string) => void = () => {};
  let end: () => void = () => {};
  const body = new ReadableStream<Uint8Array>({
    start(c) {
      const enc = new TextEncoder();
      push = (s) => c.enqueue(enc.encode(s));
      end = () => c.close();
    },
  });
  const response = new Response(body, {
    status: 200,
    headers: { "content-type": "text/event-stream" },
  });
  const frame = (event: string, data: unknown) =>
    push(`event: ${event}\ndata: ${JSON.stringify(data)}\n\n`);
  return { response, frame, end };
}

describe("converting a set kept in several folders", () => {
  it("says with the real numbers what will happen", () => {
    expect(confirmationText(folders)).toBe(
      "3 files from 3 folders will be moved into one folder named Band - Record [FLAC], renumbered 1 to 3, and lose their disc tags (DISCNUMBER, DISCTOTAL). " +
        `The cover kept is cover.jpg from ${D1}. ` +
        `These covers go to Musaic's trash: cover.jpg from ${D2} and cover.jpg from ${D3}. ` +
        `These other files move into the set's folder too: notes.txt from ${D2} and rip.log from ${D2} (as rip (disc 2).log). ` +
        `This copy of a file the set's folder already has goes to Musaic's trash: rip.log from ${D3}. ` +
        "The 3 albums become one; findings you ignored on the other 2 discs do not carry over. " +
        "This cannot be undone from Musaic, but a record of every old name and tag value is kept in /cfg/conversions.",
    );
  });

  it("says where each other file of a disc's folder goes, and leaves no folder behind", async () => {
    previewAnswer = async () => json(folders);
    wrap(<MultiDiscResolveDialog finding={finding} open onClose={() => {}} />);
    await screen.findByRole("button", { name: "Convert…" });
    const moved = document.querySelectorAll('[data-stray-action="moved"]');
    expect(Array.from(moved).map((e) => e.textContent)).toEqual([
      "notes.txt moves into the set's folder.",
      "rip.log moves into the set's folder as rip (disc 2).log: a different file named rip.log is already there.",
    ]);
    const copy = document.querySelector('[data-stray-action="identical"]');
    expect(copy?.textContent).toBe(
      "rip.log is the same file as the one in the set's folder, so this copy goes to Musaic's trash.",
    );
    // Both disc folders are emptied, and nothing is said to stay in one.
    expect(screen.getAllByText("This folder is removed once it is empty.")).toHaveLength(2);
    expect(screen.queryByText(/This folder stays/)).toBeNull();
    expect(screen.queryByText(/Stays in this folder/)).toBeNull();
    expect(screen.queryByText(/This folder is kept/)).toBeNull();
  });

  it("says so when a folder is kept because it holds a subfolder", async () => {
    const withSub = {
      ...folders,
      folders: folders.folders.map((f) =>
        f.dir_name === D2 ? { ...f, other_files: ["Scans"] } : f,
      ),
    };
    previewAnswer = async () => json(withSub);
    wrap(<MultiDiscResolveDialog finding={finding} open onClose={() => {}} />);
    await screen.findByRole("button", { name: "Convert…" });
    expect(screen.getByText("This folder is kept: not everything in it is moved.")).toBeTruthy();
    expect(screen.getByText(/Not moved \(Musaic never moves a subfolder\):/)).toBeTruthy();
    expect(confirmationText(withSub)).toContain(`Scans is not moved, so ${D2} is kept. `);
  });

  it("streams the progress, then closes on success", async () => {
    previewAnswer = async () => json(folders);
    const s = stream();
    convertAnswer = async () => s.response;
    const onClose = vi.fn();
    wrap(<MultiDiscResolveDialog finding={finding} open onClose={onClose} />);
    fireEvent.click(await screen.findByRole("button", { name: "Convert…" }));
    expect(screen.getByText(/3 files from 3 folders will be moved/)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Convert now" }));
    s.frame("progress", { step: "move_files", done: 1, total: 2 });
    await screen.findByText("Moving files into the set's folder… 1 of 2");
    s.frame("result", {
      ok: true,
      interrupted: false,
      album_id: "new-id",
      user_message: "Converted: the 3 files of 3 discs are in Band - Record [FLAC], numbered 1 to 3.",
      files: [],
      folders: [],
      merged: ["b2", "b3"],
      folder: { old: D1, new: "Band - Record [FLAC]", outcome: "renamed" },
      record: "/cfg/conversions/x.json",
      attempts: 1,
    });
    s.end();
    await waitFor(() => expect(onClose).toHaveBeenCalled());
    const post = calls.find((c) => c.method === "POST");
    expect(post?.url).toBe(CONVERT);
    expect(JSON.parse(post?.body ?? "{}")).toEqual({ digest: "d1g3st" });
  });

  it("stays open with the reasons when a move failed, and offers to finish", async () => {
    previewAnswer = async () => json(folders);
    const s = stream();
    convertAnswer = async () => s.response;
    const onClose = vi.fn();
    wrap(<MultiDiscResolveDialog finding={finding} open onClose={onClose} />);
    fireEvent.click(await screen.findByRole("button", { name: "Convert…" }));
    fireEvent.click(screen.getByRole("button", { name: "Convert now" }));
    s.frame("result", {
      ok: false,
      interrupted: false,
      album_id: ALBUM_ID,
      user_message: "1 file was moved into the set's folder; the rest were not, and the folders were not merged.",
      files: [
        {
          filename: "01 - C1.flac",
          new_filename: "03 - C1.flac",
          folder: D3,
          rename: "renamed",
          tags: "written",
          move: "failed",
          reason_code: "in_use",
          user_message: "“03 - C1.flac” could not be moved because another program is using it.",
        },
      ],
      folder: { old: D1, new: "Band - Record [FLAC]", outcome: "not_reached" },
      record: "/cfg/conversions/x.json",
      attempts: 1,
    });
    s.end();
    await screen.findByText("The set was not fully converted");
    expect(screen.getByText(/another program is using it/)).toBeTruthy();
    expect(onClose).not.toHaveBeenCalled();
    // The fresh preview says where each file is and offers to finish.
    previewAnswer = async () =>
      json({
        ...folders,
        resume: { record: "/cfg/conversions/x.json", started_at: "t", attempts: 1 },
        tracks: folders.tracks.map((t, i) =>
          i === 1 ? { ...t, state: "moved", tags_done: true, folder: D1 } : t,
        ),
      });
    await waitFor(() => expect(calls.filter((c) => c.url === PREVIEW).length).toBeGreaterThan(1));
  });

  it("finishes an interrupted one, or stops treating it as a conversion", async () => {
    const ABANDON = "/api/checks/multi-disc-layout/findings/7/abandon-conversion";
    previewAnswer = async () =>
      json({
        ...folders,
        resume: { record: "/cfg/conversions/x.json", started_at: "t", attempts: 1 },
        tracks: folders.tracks.map((t, i) =>
          i === 1 ? { ...t, state: "moved", tags_done: true, folder: D1 } : t,
        ),
      });
    const onClose = vi.fn();
    vi.stubGlobal(
      "fetch",
      vi.fn(async (url: string, init?: RequestInit) => {
        calls.push({ url: String(url), method: init?.method ?? "GET", body: null });
        if (String(url) === PREVIEW) return previewAnswer();
        if (String(url) === ABANDON) return json({ ok: true, user_message: "Musaic no longer treats this as a conversion in progress." });
        return json({ detail: "not found" }, 404);
      }),
    );
    wrap(<MultiDiscResolveDialog finding={finding} open onClose={onClose} />);
    await screen.findByText("A conversion of this set did not finish");
    expect(screen.getByText(`(done, in ${D1})`)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Finish converting…" }));
    expect(screen.getByText(/This finishes the conversion that did not finish: 1 more file will be moved/)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    fireEvent.click(await screen.findByRole("button", { name: "Stop this conversion…" }));
    expect(screen.getByText(/Every file stays where it is now/)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Stop it" }));
    await waitFor(() => expect(onClose).toHaveBeenCalled());
    expect(calls.some((c) => c.url === ABANDON && c.method === "POST")).toBe(true);
  });
});
