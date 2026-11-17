/**
 * The two Resolve dialogs say what happens to the user's files (#196):
 * the outcome, not the mechanism; the destructive row action is labelled
 * by its effect. The requests they send are unchanged.
 */
import { afterEach, beforeEach, describe, it, expect, vi } from "vitest";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";

vi.mock("sonner", () => ({
  toast: Object.assign(vi.fn(), { error: vi.fn(), success: vi.fn(), warning: vi.fn() }),
}));

import { toast } from "sonner";
import DuplicateResolveDialog from "@/components/DuplicateResolveDialog";
import ArtistSortResolveDialog from "@/components/ArtistSortResolveDialog";
import FilenameMismatchResolveDialog from "@/components/FilenameMismatchResolveDialog";
import type { Finding } from "@/lib/types";

const ALBUM_ID = "7ddd61e60067be1a";

interface Call {
  url: string;
  method: string;
  body: unknown;
}
let calls: Call[];
let routes: Record<string, unknown>;

function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
}

beforeEach(() => {
  calls = [];
  routes = {};
  vi.mocked(toast.success).mockClear();
  vi.mocked(toast.error).mockClear();
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string, init?: RequestInit) => {
      const method = init?.method ?? "GET";
      const body = typeof init?.body === "string" ? JSON.parse(init.body) : init?.body;
      calls.push({ url: String(url), method, body });
      const hit = routes[`${method} ${String(url)}`];
      return hit === undefined ? json({ detail: "not found" }, 404) : json(hit);
    }),
  );
});
afterEach(() => {
  vi.unstubAllGlobals();
});

function wrap(ui: React.ReactElement) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
  return render(<QueryClientProvider client={qc}>{ui}</QueryClientProvider>);
}

function makeFinding(id: number, check_id: string, detail: Record<string, unknown>): Finding {
  return {
    id,
    check_id,
    album_id: ALBUM_ID,
    severity: "warn",
    summary: "",
    detail,
    first_seen_at: "2026-11-14T00:00:00Z",
    last_seen_at: "2026-11-14T00:00:00Z",
    resolved_at: null,
    ignored_at: null,
    ignored_reason: null,
  };
}

const posts = () => calls.filter((c) => c.method === "POST");

describe("filename Resolve dialog copy", () => {
  const finding = makeFinding(11, "filename-mismatch", {});
  const plan = {
    album_id: ALBUM_ID,
    is_empty: false,
    dir_name: "Artist - Album [FLAC]",
    is_compilation: false,
    template: "{tracknumber:02} - {title}",
    dir_template: "{albumartist} - {album} [FLAC]",
    headline: "",
    dir_rename: { old: "artist - album", new: "Artist - Album [FLAC]" },
    renames: [
      { filename: "1.flac", expected: "01 - One.flac", tracknumber: "1", title: "One", artist: "", collides_with: null },
      { filename: "2.flac", expected: "02 - Two.flac", tracknumber: "2", title: "Two", artist: "", collides_with: null },
      { filename: "3.flac", expected: "02 - Two.flac", tracknumber: "2", title: "Two", artist: "", collides_with: "2.flac" },
    ],
  };

  beforeEach(() => {
    routes[`GET /api/checks/filename-mismatch/findings/11/plan-preview`] = plan;
    routes[`POST /api/checks/filename-mismatch/findings/11/apply`] = {
      ok: true, renamed: 2, errors: [], message: "Renamed 2 files",
    };
  });

  it("describes the outcome and drops the mechanism prose", async () => {
    wrap(<FilenameMismatchResolveDialog finding={finding} open onClose={() => {}} />);
    expect(
      await screen.findByText("Rename the album folder and 2 files in it to match your templates."),
    ).toBeInTheDocument();
    expect(screen.getByText("Rename files to match your templates")).toBeInTheDocument();
    expect(screen.getByText(/File names:/)).toBeInTheDocument();
    expect(screen.getByText(/Folder name:/)).toBeInTheDocument();
    expect(screen.getByText("The folder is renamed first, then the files inside it.")).toBeInTheDocument();
    expect(screen.getByText("Some files would get the same name")).toBeInTheDocument();
    expect(screen.getByText(/Only the names change/)).toBeInTheDocument();
    for (const gone of [/two-pass/, /temp name/, /internal id/, /URL bar/, /Dir template/, /File template/]) {
      expect(screen.queryByText(gone)).toBeNull();
    }
  });

  it("sends the same apply request", async () => {
    const onClose = vi.fn();
    wrap(<FilenameMismatchResolveDialog finding={finding} open onClose={onClose} />);
    const apply = await screen.findByRole("button", { name: "Rename folder and 2 files" });
    fireEvent.click(apply);
    await waitFor(() => expect(onClose).toHaveBeenCalled());
    expect(posts()).toEqual([
      {
        url: "/api/checks/filename-mismatch/findings/11/apply",
        method: "POST",
        body: {
          plan: [
            { old: "1.flac", new: "01 - One.flac" },
            { old: "2.flac", new: "02 - Two.flac" },
          ],
          dir_rename: { old: "artist - album", new: "Artist - Album [FLAC]" },
        },
      },
    ]);
  });

  it("says plainly when there is nothing to rename", async () => {
    routes[`GET /api/checks/filename-mismatch/findings/11/plan-preview`] = {
      ...plan, is_empty: true, dir_rename: null, renames: [],
    };
    wrap(<FilenameMismatchResolveDialog finding={finding} open onClose={() => {}} />);
    expect(await screen.findByText("Already named correctly")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Nothing to rename" })).toBeDisabled();
  });

  const applyUrl = "POST /api/checks/filename-mismatch/findings/11/apply";
  const clickRename = async () =>
    fireEvent.click(await screen.findByRole("button", { name: "Rename folder and 2 files" }));

  it("keeps the dialog open and names the files that were not found", async () => {
    routes[applyUrl] = {
      ok: true,
      renamed: 1,
      errors: [],
      message: "Renamed 1 files; 1 not found, skipped: '2.flac'",
      skipped: [{ old: "2.flac", new: "02 - Two.flac", reason: "source_missing" }],
    };
    const onClose = vi.fn();
    wrap(<FilenameMismatchResolveDialog finding={finding} open onClose={onClose} />);
    await clickRename();
    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("Renamed 1 file");
    expect(alert).toHaveTextContent("2.flac");
    expect(alert).toHaveTextContent("wasn't in the folder");
    expect(onClose).not.toHaveBeenCalled();
  });

  it("keeps the dialog open and says nothing changed when the rename is refused", async () => {
    routes[applyUrl] = {
      ok: false,
      renamed: 0,
      errors: [],
      message: "Refusing to overwrite an existing unrelated file: 01 - One.flac",
    };
    const onClose = vi.fn();
    wrap(<FilenameMismatchResolveDialog finding={finding} open onClose={onClose} />);
    await clickRename();
    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("Couldn't rename the files");
    expect(alert).toHaveTextContent("Refusing to overwrite an existing unrelated file: 01 - One.flac");
    expect(alert).toHaveTextContent("No file was changed");
    expect(onClose).not.toHaveBeenCalled();
  });

  it("says the folder was renamed when only the files failed", async () => {
    routes[applyUrl] = {
      ok: false,
      renamed: 0,
      errors: ["Rename staging failed for '1.flac': Permission denied"],
      dir_renamed: true,
      new_album_id: "aaaaaaaaaaaaaaaa",
      message: "Rename staging failed for '1.flac': Permission denied; no file was renamed",
    };
    wrap(<FilenameMismatchResolveDialog finding={finding} open onClose={() => {}} />);
    await clickRename();
    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("The album folder was renamed");
    expect(alert).toHaveTextContent("1.flac");
  });
});

describe("duplicate Resolve dialog copy", () => {
  const finding = makeFinding(7, "duplicate-tracks", {
    filenames: ["01 - A.flac", "01 - A (1).flac"],
    durations_s: [200, 200],
  });

  beforeEach(() => {
    routes[`GET /api/checks/duplicate-tracks/findings/7/cluster-preview`] = {
      album_id: ALBUM_ID,
      is_empty: false,
      recording_mbid: null,
      filenames: ["01 - A.flac", "01 - A (1).flac"],
      durations_s: [200, 200],
      duplicate_count: 2,
      recommended_keep: null,
      recommended_reason: null,
      removed_filenames: [],
    };
    routes[`POST /api/checks/duplicate-tracks/findings/7/delete-file`] = {
      ok: true, message: "moved",
    };
  });

  it("labels the row action by its effect and drops the mechanism prose", async () => {
    wrap(<DuplicateResolveDialog finding={finding} open onClose={() => {}} />);
    expect(await screen.findByText("Remove duplicate copies")).toBeInTheDocument();
    expect(screen.getByText(/contain the same recording/)).toBeInTheDocument();
    expect(screen.getAllByRole("button", { name: "Move to trash" })).toHaveLength(2);
    expect(screen.queryByRole("button", { name: "Delete" })).toBeNull();
    for (const gone of [/canonical/, /unlinked/, /~\/\.musaic/]) {
      expect(screen.queryByText(gone)).toBeNull();
    }
  });

  it("confirms, then sends the same delete-file request", async () => {
    wrap(<DuplicateResolveDialog finding={finding} open onClose={() => {}} />);
    const rows = await screen.findAllByRole("button", { name: "Move to trash" });
    fireEvent.click(rows[1]);
    const confirm = await screen.findByRole("dialog", { name: "Move this file to trash?" });
    expect(within(confirm).getByText(/will be moved to Musaic's trash folder/)).toBeInTheDocument();
    fireEvent.click(within(confirm).getByRole("button", { name: "Move to trash" }));
    await waitFor(() => expect(posts()).toHaveLength(1));
    expect(posts()[0]).toEqual({
      url: "/api/checks/duplicate-tracks/findings/7/delete-file",
      method: "POST",
      body: { filename: "01 - A (1).flac" },
    });
    await waitFor(() =>
      expect(toast.success).toHaveBeenCalledWith("Moved 01 - A (1).flac to trash", {
        description: "You can move it back from Musaic's trash folder.",
      }),
    );
  });

  describe("MusicBrainz lookup line", () => {
    const mbid = "c0000000-0000-4000-8000-00000000000c";
    const canonicalUrl = "GET /api/checks/duplicate-tracks/findings/7/canonical";
    const failure = (reason_code: string, reason: string) => ({
      title: null, artist: null, mbid, reason, reason_code,
    });

    it.each([
      ["not_found", "MusicBrainz didn't return a recording for that MBID"],
      [
        "unreachable",
        "Couldn't reach MusicBrainz. Check the internet connection and that offline mode is off, then reopen this dialog.",
      ],
      ["rate_limited", "MusicBrainz is busy or limiting requests. Wait a minute, then reopen this dialog."],
      ["service_error", "MusicBrainz reported an error (HTTP 500). Try again later."],
      ["unreadable_answer", "MusicBrainz sent an answer Musaic could not read. Try again later."],
    ])("shows the sentence for %s", async (code, sentence) => {
      routes[canonicalUrl] = failure(code, sentence);
      wrap(<DuplicateResolveDialog finding={finding} open onClose={() => {}} />);
      const line = await screen.findByText(sentence);
      expect(line.closest("[role=status]")).toHaveAttribute("data-reason-code", code);
    });

    it("says nothing for a cluster with no recording id", async () => {
      routes[canonicalUrl] = {
        title: null, artist: null, mbid: null,
        reason: "no recording MBID for this cluster", reason_code: "no_mbid",
      };
      wrap(<DuplicateResolveDialog finding={finding} open onClose={() => {}} />);
      await screen.findByText("Remove duplicate copies");
      await waitFor(() => expect(calls.some((c) => c.url.endsWith("/canonical"))).toBe(true));
      expect(screen.queryByText(/no recording MBID/)).toBeNull();
      expect(screen.queryByRole("status")).toBeNull();
    });

    it("shows the recording when MusicBrainz answered", async () => {
      routes[canonicalUrl] = {
        title: "Bridge", artist: "Someone", mbid, reason: null, reason_code: null,
      };
      wrap(<DuplicateResolveDialog finding={finding} open onClose={() => {}} />);
      expect(await screen.findByText(/MusicBrainz says this audio is/)).toBeInTheDocument();
    });
  });
});

describe("artist sort Resolve dialog copy", () => {
  const finding = makeFinding(5, "inconsistent-artist-sort", {
    state: "conflicting",
    members: [
      { kind: "album", album_id: "a1", dir_name: "Ray Conniff - One", album_artist: "Ray Conniff", sort: "Conniff, Ray" },
      { kind: "album", album_id: "a2", dir_name: "Ray Conniff - Two", album_artist: "Ray Conniff Orchestra", sort: "Ray Conniff" },
      { kind: "track", album_id: "a3", dir_name: "Various", artist: "Ray Conniff", filenames: ["01 - X.flac"], sort: "" },
    ],
  });
  const APPLY = "Set sort name on 2 albums + 1 track";

  beforeEach(() => {
    routes[`POST /api/checks/inconsistent-artist-sort/findings/5/apply-sort`] = {
      ok: true,
      applied: 2,
      cluster_size: 3,
      distinct_albums: 3,
      sort_value: "Conniff, Ray",
      message: "Wrote sort='Conniff, Ray' to 2 of 3 album(s) in the cluster.",
    };
  });

  it("says what will be written in plain words, without field keys or cluster", async () => {
    wrap(<ArtistSortResolveDialog finding={finding} open onClose={() => {}} />);
    expect(await screen.findByText("Give this artist one sort name")).toBeInTheDocument();
    expect(screen.getByText(/credit this artist in slightly different ways/)).toBeInTheDocument();
    expect(screen.getByText("Sort name")).toBeInTheDocument();
    expect(screen.getByText("3 items to update")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: APPLY })).toBeInTheDocument();
    for (const gone of [/ALBUMARTISTSORT/, /ARTISTSORT/, /album_artist/, /cluster/i, /canonical/i, /Normalize/]) {
      expect(screen.queryByText(gone)).toBeNull();
    }
  });

  it("sends the same request and reports the result in plain words", async () => {
    wrap(<ArtistSortResolveDialog finding={finding} open onClose={() => {}} />);
    const input = await screen.findByRole("textbox");
    fireEvent.change(input, { target: { value: "Conniff, Ray" } });
    fireEvent.click(screen.getByRole("button", { name: APPLY }));
    await waitFor(() => expect(posts()).toHaveLength(1));
    expect(posts()[0]).toEqual({
      url: "/api/checks/inconsistent-artist-sort/findings/5/apply-sort",
      method: "POST",
      body: { sort_value: "Conniff, Ray" },
    });
    await waitFor(() =>
      expect(toast.success).toHaveBeenCalledWith('Set the sort name to "Conniff, Ray" on 2 of 3 albums.'),
    );
  });
});
