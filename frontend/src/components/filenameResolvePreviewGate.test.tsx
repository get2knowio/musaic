/**
 * Rename in the filename Resolve dialog is offered only for a plan that was
 * re-derived successfully, for the finding on screen (#322). A stored plan
 * on the finding never stands in for it.
 */
import { afterEach, beforeEach, describe, it, expect, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";

vi.mock("sonner", () => ({
  toast: Object.assign(vi.fn(), { error: vi.fn(), success: vi.fn(), warning: vi.fn() }),
}));

import FilenameMismatchResolveDialog from "@/components/FilenameMismatchResolveDialog";
import type { Finding } from "@/lib/types";

const SLOW = 4000;

type Handler = () => Response | Promise<Response>;
let queues: Record<string, Handler[]>;
let calls: { key: string; body: unknown }[];

const json = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });

/** Queue one answer for a route; the last answer is reused once the queue is down to one. */
const answer = (key: string, ...hs: Handler[]) => {
  queues[key] = [...(queues[key] ?? []), ...hs];
};

beforeEach(() => {
  queues = {};
  calls = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string, init?: RequestInit) => {
      const key = `${init?.method ?? "GET"} ${String(url)}`;
      const body = typeof init?.body === "string" ? JSON.parse(init.body) : undefined;
      calls.push({ key, body });
      const q = queues[key];
      if (!q || q.length === 0) return json({ detail: "not found" }, 404);
      return (q.length > 1 ? q.shift()! : q[0])();
    }),
  );
});
afterEach(() => {
  vi.unstubAllGlobals();
});

const previewKey = (id: number) => `GET /api/checks/filename-mismatch/findings/${id}/plan-preview`;
const applyKey = (id: number) => `POST /api/checks/filename-mismatch/findings/${id}/apply`;
const applies = () => calls.filter((c) => c.key.startsWith("POST"));

const entry = (filename: string, expected: string) => ({
  filename, expected, tracknumber: "1", title: "T", artist: "", collides_with: null,
});
const plan = (renames: ReturnType<typeof entry>[]) => ({
  album_id: "a",
  is_empty: false,
  dir_name: "Artist - Album [FLAC]",
  is_compilation: false,
  template: "{title}",
  dir_template: "",
  headline: "",
  dir_rename: null,
  renames,
});

function finding(id: number, renames: ReturnType<typeof entry>[] = []): Finding {
  return {
    id,
    check_id: "filename-mismatch",
    album_id: `album-${id}`,
    severity: "warn",
    summary: "",
    detail: { renames },
    first_seen_at: "2026-11-14T00:00:00Z",
    last_seen_at: "2026-11-14T00:00:00Z",
    resolved_at: null,
    ignored_at: null,
    ignored_reason: null,
  };
}

function wrap(ui: React.ReactElement) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
  return render(<QueryClientProvider client={qc}>{ui}</QueryClientProvider>);
}

const renameButton = () => screen.getByRole("button", { name: /^Rename|^Loading|^Renaming/ });
const failure = () => json({ detail: "boom" }, 500);

describe("filename Resolve dialog preview gate", () => {
  it("a stored plan plus a failed preview cannot issue a rename; a successful retry uses the new names", async () => {
    const stored = finding(11, [entry("old.flac", "STORED.flac")]);
    // The query retries once by itself, so the failure is queued twice.
    answer(previewKey(11), failure, failure, () => json(plan([entry("old.flac", "FRESH.flac")])));
    answer(applyKey(11), () => json({ ok: true, renamed: 1, errors: [], message: "Renamed 1 file" }));
    const onClose = vi.fn();
    wrap(<FilenameMismatchResolveDialog finding={stored} open onClose={onClose} />);

    await screen.findByText("Couldn't verify the current filenames", undefined, { timeout: SLOW });
    expect(renameButton()).toBeDisabled();
    expect(screen.queryByText("STORED.flac")).toBeNull();
    fireEvent.click(renameButton());
    expect(applies()).toEqual([]);

    fireEvent.click(screen.getByRole("button", { name: "Retry preview" }));
    const rename = await screen.findByRole("button", { name: "Rename 1 file" });
    expect(rename).toBeEnabled();
    expect(screen.queryByText("Couldn't verify the current filenames")).toBeNull();
    fireEvent.click(rename);
    await waitFor(() => expect(onClose).toHaveBeenCalled());
    expect(applies()).toEqual([
      { key: applyKey(11), body: { plan: [{ old: "old.flac", new: "FRESH.flac" }], dir_rename: null } },
    ]);
  });

  it("keeps Rename disabled while a retry is in flight, and does not offer a second one", async () => {
    let release: (r: Response) => void = () => {};
    const slow = new Promise<Response>((res) => { release = res; });
    answer(previewKey(11), failure, failure, () => slow);
    wrap(<FilenameMismatchResolveDialog finding={finding(11)} open onClose={() => {}} />);

    await screen.findByText("Couldn't verify the current filenames", undefined, { timeout: SLOW });
    fireEvent.click(screen.getByRole("button", { name: "Retry preview" }));
    await waitFor(() => expect(screen.queryByRole("button", { name: "Retry preview" })).toBeNull());
    expect(renameButton()).toBeDisabled();
    expect(applies()).toEqual([]);

    release(json(plan([entry("a.flac", "b.flac")])));
    expect(await screen.findByRole("button", { name: "Rename 1 file" })).toBeEnabled();
  });

  it("after a partial rename keeps the outcome, disables Rename, and Refresh preview offers what remains", async () => {
    answer(
      previewKey(11),
      () => json(plan([entry("1.flac", "A.flac"), entry("2.flac", "B.flac")])),
      () => json(plan([entry("2.flac", "B.flac")])),
    );
    answer(applyKey(11), () => json({
      ok: false, renamed: 1, errors: ["x"], message: "m", user_message: "2.flac is in use.",
    }));
    wrap(<FilenameMismatchResolveDialog finding={finding(11)} open onClose={() => {}} />);

    fireEvent.click(await screen.findByRole("button", { name: "Rename 2 files" }));
    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("2.flac is in use.");
    expect(alert).toHaveTextContent("Refresh preview");
    expect(alert).not.toHaveTextContent("Run the check again");
    expect(renameButton()).toBeDisabled();

    fireEvent.click(screen.getByRole("button", { name: "Refresh preview" }));
    expect(await screen.findByRole("button", { name: "Rename 1 file" })).toBeEnabled();
  });

  it("after a partial rename a failed refresh keeps Rename disabled", async () => {
    answer(previewKey(11), () => json(plan([entry("1.flac", "A.flac")])), failure, failure);
    answer(applyKey(11), () => json({ ok: false, renamed: 0, errors: ["x"], message: "m" }));
    wrap(<FilenameMismatchResolveDialog finding={finding(11)} open onClose={() => {}} />);

    fireEvent.click(await screen.findByRole("button", { name: "Rename 1 file" }));
    await screen.findByRole("alert");
    fireEvent.click(screen.getByRole("button", { name: "Refresh preview" }));
    await screen.findByText("Couldn't verify the current filenames", undefined, { timeout: SLOW });
    expect(renameButton()).toBeDisabled();
    fireEvent.click(renameButton());
    expect(applies()).toHaveLength(1);
  });

  it("switching findings never sends the previous finding's plan", async () => {
    let release: (r: Response) => void = () => {};
    const slow = new Promise<Response>((res) => { release = res; });
    const one = finding(11, [entry("stored.flac", "STORED.flac")]);
    const two = finding(12);
    answer(previewKey(11), () => json(plan([entry("1.flac", "A.flac")])));
    answer(previewKey(12), () => slow);
    answer(applyKey(11), () => json({ ok: true, renamed: 1, errors: [], message: "ok" }));
    answer(applyKey(12), () => json({ ok: true, renamed: 1, errors: [], message: "ok" }));
    wrap(<FilenameMismatchResolveDialog finding={one} findings={[one, two]} open onClose={() => {}} />);

    fireEvent.click(await screen.findByRole("button", { name: "Rename 1 file & next" }));
    // Finding 12's preview is still loading: Rename is off and nothing leaks over.
    await screen.findByText(/2 of 2/);
    expect(renameButton()).toBeDisabled();
    expect(screen.queryByText("A.flac")).toBeNull();
    fireEvent.click(renameButton());
    expect(applies().map((c) => c.key)).toEqual([applyKey(11)]);

    release(json(plan([entry("9.flac", "Z.flac")])));
    fireEvent.click(await screen.findByRole("button", { name: "Rename 1 file" }));
    await waitFor(() => expect(applies()).toHaveLength(2));
    expect(applies()[1]).toEqual({
      key: applyKey(12),
      body: { plan: [{ old: "9.flac", new: "Z.flac" }], dir_rename: null },
    });
  });

  it("a plan the server could not derive is not renamable", async () => {
    answer(previewKey(11), () => json({ ...plan([entry("1.flac", "A.flac")]), error: "disk went away" }));
    wrap(<FilenameMismatchResolveDialog finding={finding(11)} open onClose={() => {}} />);
    await screen.findByText("Couldn't verify the current filenames");
    expect(screen.getByText(/disk went away/)).toBeInTheDocument();
    expect(renameButton()).toBeDisabled();
    expect(screen.getByRole("button", { name: "Retry preview" })).toBeInTheDocument();
  });
});
