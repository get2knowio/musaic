/**
 * Bulk quick-fix dialog: the run ends with a list of every finding it
 * handled, grouped by outcome (problems first), naming each album and giving
 * a plain-language reason for each one that was not fixed.
 */
import { afterEach, beforeEach, describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent, within } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";

vi.mock("sonner", () => ({ toast: { error: vi.fn(), success: vi.fn() } }));

import { BulkQuickFixDialog, RESULT_ROW_LIMIT } from "@/components/BulkQuickFixDialog";
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

function sse(steps: Step[], done: Record<string, number>): string {
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
