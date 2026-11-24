/**
 * Bulk quick-fix dialog: the run ends with a list of every finding it
 * handled, grouped by outcome (problems first), naming each album and giving
 * a plain-language reason for each one that was not fixed.
 */
import { afterEach, beforeEach, describe, it, expect, vi } from "vitest";
import { act, render, screen, fireEvent, waitFor, within } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";

vi.mock("sonner", () => ({ toast: Object.assign(vi.fn(), { error: vi.fn(), success: vi.fn() }) }));

import ActivityPanel from "@/components/ActivityPanel";
import { BulkQuickFixDialog, RESULT_ROW_LIMIT } from "@/components/BulkQuickFixDialog";
import { openActivityPanel, resetActivityPanel } from "@/lib/activityPanel";
import { resetBulkRuns } from "@/lib/bulkRuns";
import type { Album, Finding } from "@/lib/types";

const finding = (id: number): Finding => ({
  id,
  check_id: "tags-out-of-sync",
  album_id: `album-${id}`,
  severity: "warn",
  summary: "s",
  detail: null,
  first_seen_at: "2026-01-01T00:00:00Z",
  last_seen_at: "2026-01-01T00:00:00Z",
  resolved_at: null,
  ignored_at: null,
  ignored_reason: null,
});

const album = (id: number): Album =>
  ({
    id: `album-${id}`,
    dir_name: `Dir ${id}`,
    title: `Title ${id}`,
    artist: `Artist ${id}`,
    album_artist: null,
  }) as unknown as Album;

type Step = { id: number; outcome: string; message: string };

function sse(steps: Step[], done: Record<string, unknown>): string {
  const progress = steps
    .map(
      (s) =>
        `event: progress\ndata: ${JSON.stringify({ finding_id: s.id, outcome: s.outcome, message: s.message })}\n\n`,
    )
    .join("");
  return `event: start\ndata: {"total": ${steps.length}}\n\n${progress}event: done\ndata: ${JSON.stringify(done)}\n\n`;
}

function run(steps: Step[], done: Record<string, number>) {
  vi.stubGlobal(
    "fetch",
    vi.fn(
      async () =>
        new Response(sse(steps, done), { status: 200, headers: { "content-type": "text/event-stream" } }),
    ),
  );
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  qc.setQueryData(["findings"], steps.map((s) => finding(s.id)));
  qc.setQueryData(["albums-snapshot"], steps.map((s) => album(s.id)));
  qc.setQueryData(["jobs"], []);
  render(
    <QueryClientProvider client={qc}>
      <BulkQuickFixDialog
        open
        onClose={() => {}}
        checkId="tags-out-of-sync"
        checkLabel="Tags"
        bulkActionLabel="Apply canonical values"
        fixKind="deterministic"
        totalFindings={steps.length}
      />
    </QueryClientProvider>,
  );
  fireEvent.click(screen.getByRole("button", { name: /Start/ }));
}

const counts = (o: Partial<Record<string, number>>) => ({
  total: 0,
  fixed: 0,
  skipped: 0,
  errors: 0,
  not_applicable: 0,
  still_open: 0,
  ...o,
});

beforeEach(() => vi.clearAllMocks());
afterEach(() => vi.unstubAllGlobals());

describe("bulk fix results list", () => {
  it("lists skipped and failed albums by name with a plain reason, problems first", async () => {
    run(
      [
        { id: 1, outcome: "fixed", message: "Wrote 1 album field(s)" },
        { id: 2, outcome: "skipped", message: "No deterministic fixes available (anchor or canonical missing)" },
        { id: 3, outcome: "error", message: "OperationalError: database is locked" },
        { id: 4, outcome: "not-applicable", message: "Not applicable: Album is missing from disk (missing since X)." },
      ],
      counts({ total: 4, fixed: 1, skipped: 1, errors: 1, not_applicable: 1 }),
    );
    const list = await screen.findByTestId("bulk-results");

    const sections = within(list).getAllByRole("region").map((s) => s.getAttribute("aria-label"));
    expect(sections).toEqual(["Failed", "Skipped", "Not applicable", "Fixed"]);

    expect(within(list).getByText("Artist 3 – Title 3")).toBeInTheDocument();
    expect(list.textContent).toMatch(/Something went wrong with this album/);
    expect(within(list).getByText("Artist 2 – Title 2")).toBeInTheDocument();
    expect(list.textContent).toMatch(/MusicBrainz has no confirmed match for this album yet/);
    expect(list.textContent).toMatch(/folder is missing from disk/);

    // No raw server text, no ids, and no pointer to messages that do not exist.
    expect(document.body.textContent).not.toMatch(/OperationalError|database is locked|anchor or canonical/);
    expect(document.body.textContent).not.toMatch(/album-\d|per-finding messages/);
  });

  it("collapses fixed findings until asked", async () => {
    run(
      [{ id: 1, outcome: "fixed", message: "Wrote 1 album field(s)" }],
      counts({ total: 1, fixed: 1 }),
    );
    const list = await screen.findByTestId("bulk-results");
    expect(within(list).queryByText("Artist 1 – Title 1")).toBeNull();
    fireEvent.click(within(list).getByRole("button", { name: /Fixed \(1\)/ }));
    expect(within(list).getByText("Artist 1 – Title 1")).toBeInTheDocument();
  });

  it("falls back to a generic sentence for a reason it does not know", async () => {
    run(
      [{ id: 1, outcome: "skipped", message: "some internal detail: token=abc123" }],
      counts({ total: 1, skipped: 1 }),
    );
    const list = await screen.findByTestId("bulk-results");
    expect(list.textContent).toMatch(/could not be fixed automatically/);
    expect(document.body.textContent).not.toMatch(/abc123/);
  });

  it("caps a long group at 50 rows with Show all N", async () => {
    const n = 230;
    const steps = Array.from({ length: n }, (_, i) => ({
      id: i + 1,
      outcome: "skipped",
      message: "No deterministic fix found (MB title + fingerprint both inconclusive)",
    }));
    run(steps, counts({ total: n, skipped: n }));
    const list = await screen.findByTestId("bulk-results");
    expect(within(list).getAllByRole("listitem")).toHaveLength(RESULT_ROW_LIMIT);
    fireEvent.click(within(list).getByRole("button", { name: `Show all ${n}` }));
    expect(within(list).getAllByRole("listitem")).toHaveLength(n);
  });

  it("says how many fixes left their finding open", async () => {
    run(
      [{ id: 1, outcome: "fixed", message: "Wrote 1 album field(s)" }],
      counts({ total: 1, fixed: 0, still_open: 1 }),
    );
    const list = await screen.findByTestId("bulk-results");
    expect(list.textContent).toMatch(/1 of these did not fully resolve their finding and is still open/);
  });
});

/**
 * A run whose stream the test feeds by hand: `push` sends SSE blocks,
 * `end` closes the response (without `done` unless one was pushed).
 */
function liveRun(total: number) {
  const enc = new TextEncoder();
  let ctrl!: ReadableStreamDefaultController<Uint8Array>;
  const body = new ReadableStream<Uint8Array>({
    start(c) {
      ctrl = c;
    },
  });
  const aborted = vi.fn();
  vi.stubGlobal(
    "fetch",
    vi.fn(async (_url: string, init?: RequestInit) => {
      init?.signal?.addEventListener("abort", () => {
        aborted();
        try {
          ctrl.error(Object.assign(new Error("aborted"), { name: "AbortError" }));
        } catch {
          /* already closed */
        }
      });
      return new Response(body, { status: 200, headers: { "content-type": "text/event-stream" } });
    }),
  );
  const ids = Array.from({ length: total }, (_, i) => i + 1);
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  qc.setQueryData(["findings"], ids.map(finding));
  qc.setQueryData(["albums-snapshot"], ids.map(album));
  qc.setQueryData(["jobs"], []);
  qc.setQueryData(["check-sweep-status"], { status: "idle", total: 0, completed: 0, current_check: null, started_at: null, completed_at: null });
  qc.setQueryData(["fingerprint-pool-status"], { status: "idle", library_dir: null, total: 0, completed: 0, errors: 0, current_album_id: null, started_at: null, completed_at: null, skip_reason: null });
  qc.setQueryData(["jobs", "queueState"], { running: 0, queued: 0, clearable: 0 });
  const onClose = vi.fn();
  const view = render(
    <QueryClientProvider client={qc}>
      <MemoryRouter>
        <BulkQuickFixDialog
          open
          onClose={onClose}
          checkId="tags-out-of-sync"
          checkLabel="Tags"
          bulkActionLabel="Apply canonical values"
          fixKind="deterministic"
          totalFindings={total}
        />
        <ActivityPanel />
      </MemoryRouter>
    </QueryClientProvider>,
  );
  fireEvent.click(screen.getByRole("button", { name: /Start/ }));
  return {
    push: (event: string, data: unknown) =>
      act(async () => {
        ctrl.enqueue(enc.encode(`event: ${event}\ndata: ${JSON.stringify(data)}\n\n`));
        await new Promise((r) => setTimeout(r, 0));
      }),
    end: () =>
      act(async () => {
        ctrl.close();
        await new Promise((r) => setTimeout(r, 0));
      }),
    aborted,
    onClose,
    view,
  };
}

describe("stopping or losing a bulk run keeps what it did (#186)", () => {
  beforeEach(() => {
    resetBulkRuns();
    resetActivityPanel();
  });

  it("Stop after one completed item: the partial outcome stays, Close is separate, Activity keeps it", async () => {
    const run = liveRun(3);
    await run.push("start", { total: 3 });
    await run.push("progress", { finding_id: 1, outcome: "fixed", message: "Wrote 1 album field(s)" });
    await screen.findByText("Processing 1 of 3");

    fireEvent.click(screen.getByRole("button", { name: "Stop" }));
    expect(run.aborted).toHaveBeenCalled();
    // Still open, with the summary: confirmed vs. not known.
    const partial = await waitFor(() => document.querySelector("[data-bulk-partial]") as HTMLElement);
    expect(partial.textContent).toMatch(/Stopped after 1 of 3/);
    expect(partial.textContent).toMatch(/fixes made stay made/);
    expect(partial.textContent).toMatch(/other 2 have no confirmed outcome/);
    expect(run.onClose).not.toHaveBeenCalled();
    fireEvent.click(within(screen.getByTestId("bulk-results")).getByRole("button", { name: /Fixed \(1\)/ }));
    expect(screen.getByText("Artist 1 – Title 1")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Stop" })).toBeNull();

    // Dismissed, the summary is still in Activity.
    run.view.unmount();
    render(
      <QueryClientProvider client={new QueryClient()}>
        <MemoryRouter>
          <ActivityPanel />
        </MemoryRouter>
      </QueryClientProvider>,
    );
    act(() => openActivityPanel());
    const panel = await screen.findByRole("dialog", { name: "Activity" });
    const card = panel.querySelector('[data-activity^="bulk-"]') as HTMLElement;
    expect(card.querySelector("[data-activity-state]")!.getAttribute("data-activity-state")).toBe("stopped");
    fireEvent.click(within(card).getByRole("button", { name: "Show results (1)" }));
    fireEvent.click(within(card).getByRole("button", { name: /Fixed \(1\)/ }));
    // The result links to its album.
    expect(within(card).getByRole("link", { name: "Artist 1 – Title 1" })).toHaveAttribute("href", "/library/album-1");
  });

  it("normal completion: completed, every outcome listed", async () => {
    const run = liveRun(2);
    await run.push("start", { total: 2 });
    await run.push("progress", { finding_id: 1, outcome: "fixed", message: "Wrote 1 album field(s)" });
    await run.push("progress", { finding_id: 2, outcome: "skipped", message: "x" });
    await run.push("done", counts({ total: 2, fixed: 1, skipped: 1 }));
    await screen.findByTestId("bulk-results");
    expect(document.body.textContent).toMatch(/Resolved 1 of 2 findings/);
    expect(document.querySelector("[data-bulk-partial]")).toBeNull();
  });

  it("an interrupted stream says what is confirmed and that the rest is not known", async () => {
    const run = liveRun(3);
    await run.push("start", { total: 3 });
    await run.push("progress", { finding_id: 1, outcome: "fixed", message: "Wrote 1 album field(s)" });
    await run.end();
    const partial = await waitFor(() => document.querySelector("[data-bulk-partial]") as HTMLElement);
    expect(partial.textContent).toMatch(/connection to the run was lost after 1 of 3/);
    expect(partial.textContent).toMatch(/other 2 have no confirmed outcome/);
    expect(screen.getByTestId("bulk-results")).toBeInTheDocument();
  });

  it("closing the dialog mid-run does not stop the run", async () => {
    const run = liveRun(2);
    await run.push("start", { total: 2 });
    fireEvent.click(screen.getByRole("button", { name: "Close" }));
    expect(run.onClose).toHaveBeenCalled();
    expect(run.aborted).not.toHaveBeenCalled();
  });
});

describe("sending what the resolver can't settle to the assistant", () => {
  const pairing = (revokedAt: string | null) => ({
    id: revokedAt ? "old" : "p1",
    name: "Assistant",
    scopes: ["research"],
    policy: {},
    createdAt: "2026-11-01T00:00:00Z",
    lastUsedAt: null,
    revokedAt,
  });

  function open(opts: {
    pairings: ReturnType<typeof pairing>[];
    fixKind?: "ai-job" | "deterministic";
    steps?: Step[];
    done?: Record<string, unknown>;
  }) {
    const urls: string[] = [];
    vi.stubGlobal(
      "fetch",
      vi.fn(async (url: string) => {
        // Only the run's own request is of interest here.
        if (String(url).includes("/quick-fix-all")) urls.push(String(url));
        const body = sse(
          opts.steps ?? [{ id: 1, outcome: "skipped", message: "No match" }],
          opts.done ?? counts({ total: 1, skipped: 1 }),
        );
        return new Response(body, { status: 200, headers: { "content-type": "text/event-stream" } });
      }),
    );
    const qc = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
    qc.setQueryData(["findings"], [finding(1), finding(2), finding(3)]);
    qc.setQueryData(["albums-snapshot"], [album(1), album(2), album(3)]);
    qc.setQueryData(["jobs"], []);
    qc.setQueryData(["agent", "pairings"], opts.pairings);
    render(
      <QueryClientProvider client={qc}>
        <MemoryRouter>
          <BulkQuickFixDialog
            open
            onClose={() => {}}
            checkId="unidentified-album"
            checkLabel="Unidentified album"
            bulkActionLabel="Identify on MusicBrainz"
            fixKind={opts.fixKind ?? "ai-job"}
            totalFindings={3}
          />
        </MemoryRouter>
      </QueryClientProvider>,
    );
    return urls;
  }

  const choice = () => screen.queryByRole("checkbox", { name: /Send what the resolver can't settle to my assistant/ });

  beforeEach(() => resetBulkRuns());

  it("is on by default when an assistant has been added, shows the count in scope, and asks for research", async () => {
    const urls = open({ pairings: [pairing(null)] });
    expect(choice()).toBeChecked();
    expect(choice()).toHaveAccessibleDescription("3 findings in scope");
    fireEvent.click(screen.getByRole("button", { name: /Start/ }));
    await waitFor(() => expect(urls).toHaveLength(1));
    expect(urls[0]).toBe("/api/checks/unidentified-album/quick-fix-all?research=1");
  });

  it("is off by default with no assistant (or only a revoked one), and the run is resolver-only", async () => {
    const urls = open({ pairings: [pairing("2026-11-02T00:00:00Z")] });
    expect(choice()).not.toBeChecked();
    fireEvent.click(screen.getByRole("button", { name: /Start/ }));
    await waitFor(() => expect(urls).toHaveLength(1));
    expect(urls[0]).toBe("/api/checks/unidentified-album/quick-fix-all");
  });

  it("can be changed before the run starts", async () => {
    const urls = open({ pairings: [] });
    expect(choice()).not.toBeChecked();
    fireEvent.click(choice()!);
    expect(choice()).toBeChecked();
    fireEvent.click(screen.getByRole("button", { name: /Start/ }));
    await waitFor(() => expect(urls).toHaveLength(1));
    expect(urls[0]).toBe("/api/checks/unidentified-album/quick-fix-all?research=1");
  });

  it("has no model gating: offered and worded the same without any provider, naming no AI", async () => {
    // Nothing seeds a provider list; the dialog does not ask for one.
    open({ pairings: [] });
    expect(screen.getByRole("button", { name: /Start/ })).toBeEnabled();
    const text = document.body.textContent ?? "";
    expect(text).not.toMatch(/\bAI\b|Claude|quota/);
    fireEvent.click(screen.getByRole("button", { name: /Start/ }));
    await waitFor(() => expect(screen.getByText(/The reasons are listed below|the reasons are listed below/)).toBeInTheDocument());
    expect(document.body.textContent ?? "").not.toMatch(/\bAI\b|Claude|quota|individually/);
  });

  it("is not offered for a check whose Fix never goes to research", async () => {
    const urls = open({ pairings: [pairing(null)], fixKind: "deterministic" });
    expect(choice()).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: /Start/ }));
    await waitFor(() => expect(urls).toHaveLength(1));
    expect(urls[0]).toBe("/api/checks/unidentified-album/quick-fix-all");
  });

  it("the summary says what was settled, sent for research in one batch, already requested, skipped and failed", async () => {
    open({
      pairings: [pairing(null)],
      steps: [
        { id: 1, outcome: "sent-for-research", message: "Sent for research." },
        { id: 2, outcome: "already-requested", message: "Research is already requested for this album." },
        { id: 3, outcome: "fixed", message: "Saved the match." },
      ],
      done: {
        ...counts({ total: 10, fixed: 4, skipped: 1, errors: 1 }),
        sent_for_research: 3,
        already_requested: 1,
        batch_id: "11111111-2222-4333-8444-555555555555",
      },
    });
    fireEvent.click(screen.getByRole("button", { name: /Start/ }));
    const line = await waitFor(() => {
      const el = document.querySelector("[data-bulk-research-summary]");
      expect(el).not.toBeNull();
      return el!;
    });
    expect(line.textContent?.replace(/\s+/g, " ").trim()).toBe(
      "Settled 4. Sent 3 for research (one batch). 1 already requested. Skipped 1, failed 1.",
    );
    // Each album is listed under what happened to it, in plain words.
    const list = screen.getByTestId("bulk-results");
    const sections = within(list).getAllByRole("region").map((r) => r.getAttribute("aria-label"));
    expect(sections).toEqual(["Sent for research", "Already requested", "Fixed"]);
    expect(within(list).getByText("Sent to your assistant for research.")).toBeInTheDocument();
    expect(within(list).getByText("Research is already requested for this album.")).toBeInTheDocument();
  });

  it("a resolver-only run keeps its summary as before", async () => {
    open({ pairings: [], done: counts({ total: 1, skipped: 1 }) });
    fireEvent.click(screen.getByRole("button", { name: /Start/ }));
    await screen.findByTestId("bulk-results");
    expect(document.querySelector("[data-bulk-research-summary]")).toBeNull();
  });
});
