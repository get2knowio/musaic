/**
 * The Activity panel (#332): one global entry for background work, opened
 * over whatever page is current, fed only by the streams the app already
 * has open (no polling).
 *
 *  - several activities at once: the toolbar names the first and counts
 *    the rest, the panel lists each with its own state;
 *  - a scan started away from Library is followed to completion there,
 *    without leaving the page;
 *  - stale and unavailable progress say so;
 *  - Cancel only where the API can (scan, queued job);
 *  - keyboard: the control opens it, focus moves in, Escape closes it and
 *    focus returns to the control.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter, Route, Routes, useLocation } from "react-router-dom";

vi.mock("sonner", () => ({ toast: Object.assign(vi.fn(), { error: vi.fn(), success: vi.fn() }) }));

import ActivityPanel from "@/components/ActivityPanel";
import Titlebar from "@/components/Titlebar";
import { CHECK_SWEEP_STATUS_KEY } from "@/hooks/useCheckSweepStatus";
import { useChecksStream } from "@/hooks/useChecksStream";
import { FINGERPRINT_POOL_STATUS_KEY } from "@/hooks/useFingerprintPoolStatus";
import { QUEUE_STATE_QUERY_KEY } from "@/hooks/useJobs";
import { useScanFollower } from "@/hooks/useScanFollower";
import {
  bulkSummary,
  scanSummary,
  sweepSummary,
  toolbarText,
} from "@/lib/activityModel";
import { getActivityPanel, openActivityPanel, resetActivityPanel } from "@/lib/activityPanel";
import { resetBulkRuns } from "@/lib/bulkRuns";
import { IDLE_SCAN, noteScanEvent, noteScanStarting, reduceScan, resetScanActivity } from "@/lib/scanActivity";
import { resetStreamHealth } from "@/lib/streamHealth";
import { makeJob } from "@/test/fixtures";
import { MockEventSource } from "@/test/setup";
import type { AppConfig, CheckSweepStatus, FingerprintPoolStatus, Job } from "@/lib/types";

const sweep = (over: Partial<CheckSweepStatus> = {}): CheckSweepStatus => ({
  status: "idle",
  total: 0,
  completed: 0,
  current_check: null,
  started_at: null,
  completed_at: null,
  ...over,
});

const pool = (over: Partial<FingerprintPoolStatus> = {}): FingerprintPoolStatus => ({
  status: "idle",
  library_dir: "/m",
  total: 0,
  completed: 0,
  errors: 0,
  current_album_id: null,
  started_at: null,
  completed_at: null,
  skip_reason: null,
  ...over,
});

function client(opts: { jobs?: Job[]; sweep?: CheckSweepStatus; pool?: FingerprintPoolStatus; paused?: boolean } = {}) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
  qc.setQueryData(["config"], { library_dir: "/m", exists: true } as AppConfig);
  qc.setQueryData(["ai-providers"], { selected: null, providers: [], uninstalled: [] });
  qc.setQueryData(["jobs"], opts.jobs ?? []);
  qc.setQueryData(QUEUE_STATE_QUERY_KEY, { paused: !!opts.paused, max_parallel: 1, running: 0, queued: 0 });
  qc.setQueryData(CHECK_SWEEP_STATUS_KEY, opts.sweep ?? sweep());
  qc.setQueryData(FINGERPRINT_POOL_STATUS_KEY, opts.pool ?? pool());
  return qc;
}

type Handler = (url: string, init?: RequestInit) => Response | undefined;

function stubFetch(handler: Handler = () => undefined) {
  const calls: { url: string; method: string }[] = [];
  const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = String(input);
    calls.push({ url, method: init?.method ?? "GET" });
    const r = handler(url, init);
    if (r) return r;
    if (url === "/api/scan/status") return json({ status: "idle", total: 0, scanned: 0, error: null });
    return new Response("{}", { status: 404 });
  });
  vi.stubGlobal("fetch", fetchMock);
  return calls;
}

const json = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });

function Where() {
  const l = useLocation();
  return <div data-testid="where">{l.pathname + l.search}</div>;
}

/** What App mounts for activity: the streams' followers and the panel. */
function Host() {
  useScanFollower();
  useChecksStream();
  return <ActivityPanel />;
}

function renderApp(url: string, qc = client()) {
  render(
    <QueryClientProvider client={qc}>
      <MemoryRouter initialEntries={[url]}>
        <Titlebar />
        <Where />
        <Host />
        <Routes>
          <Route path="/settings" element={<button>Settings field</button>} />
          <Route path="/review" element={<div>Review page</div>} />
          <Route path="/library/*" element={<div>Library page</div>} />
        </Routes>
      </MemoryRouter>
    </QueryClientProvider>,
  );
  return qc;
}

const toggle = () => document.getElementById("activity-toggle") as HTMLButtonElement;
const toggleText = () => toggle().querySelector("[data-activity-text]")!.textContent;
const where = () => screen.getByTestId("where").textContent;
const card = (id: string) => document.querySelector(`[data-activity="${id}"]`) as HTMLElement;
const stateOf = (id: string) => card(id).querySelector("[data-activity-state]")!.getAttribute("data-activity-state");

beforeEach(() => {
  resetActivityPanel();
  resetScanActivity();
  resetBulkRuns();
  resetStreamHealth();
});
afterEach(() => {
  vi.unstubAllGlobals();
  vi.clearAllMocks();
});

describe("several activities at once", () => {
  it("the toolbar names the first and counts the rest; the panel gives each its own state", async () => {
    stubFetch();
    renderApp(
      "/review",
      client({
        sweep: sweep({ status: "waiting", total: 12, completed: 3 }),
        pool: pool({ status: "running", total: 50, completed: 10 }),
        jobs: [
          makeJob({ id: "run", status: "running", job_type: "metadata", album_title: "Running album" }),
          makeJob({ id: "q", status: "queued", job_type: "cover", album_title: "Queued album" }),
        ],
      }),
    );
    act(() => {
      noteScanStarting();
      noteScanEvent({ type: "start", library_dir: "/m", total: 180, cached_count: 0, force: true });
      noteScanEvent({ type: "progress", scanned: 42, total: 180 });
    });
    expect(toggleText()).toBe("Scanning · 42 of 180 +3");
    expect(toggle()).toHaveAccessibleName("Activity: Scanning · 42 of 180 +3");

    fireEvent.click(toggle());
    const panel = await screen.findByRole("dialog", { name: "Activity" });
    expect(where()).toBe("/review");
    expect(stateOf("scan")).toBe("running");
    expect(stateOf("checks")).toBe("waiting");
    expect(stateOf("fingerprint")).toBe("running");
    expect(stateOf("jobs")).toBe("running");
    expect(within(card("scan")).getByRole("progressbar")).toHaveAttribute("aria-valuetext", "42 of 180");
    expect(within(card("fingerprint")).getByRole("progressbar")).toHaveAttribute("aria-valuetext", "10 of 50");

    // Cancel only where the API has one: the scan and the queued job.
    expect(within(card("scan")).getByRole("button", { name: "Cancel scan" })).toBeEnabled();
    expect(within(card("checks")).queryByRole("button")).toBeNull();
    expect(within(card("fingerprint")).queryByRole("button")).toBeNull();
    const jobs = within(panel).getByRole("list", { name: "Jobs in progress" });
    const rows = within(jobs).getAllByRole("listitem");
    expect(rows.map((r) => r.getAttribute("data-activity-job")).sort()).toEqual(["q", "run"]);
    const running = rows.find((r) => r.getAttribute("data-activity-job") === "run")!;
    const queued = rows.find((r) => r.getAttribute("data-activity-job") === "q")!;
    expect(within(running).queryByRole("button", { name: /Cancel/ })).toBeNull();
    expect(within(queued).getByRole("button", { name: /Cancel the queued/ })).toBeInTheDocument();
  });

  it("cancels the scan through the API and shows it stopping, then stopped", async () => {
    const calls = stubFetch((url) =>
      url === "/api/scan/cancel" ? json({ outcome: "requested", scan: { status: "cancelling" } }) : undefined,
    );
    renderApp("/settings");
    act(() => {
      noteScanStarting();
      noteScanEvent({ type: "start", library_dir: "/m", total: 10, cached_count: 0, force: false });
    });
    act(() => openActivityPanel());
    fireEvent.click(await screen.findByRole("button", { name: "Cancel scan" }));
    await waitFor(() => expect(screen.getByRole("button", { name: "Cancelling…" })).toBeDisabled());
    expect(calls).toContainEqual({ url: "/api/scan/cancel", method: "POST" });
    act(() =>
      noteScanEvent({ type: "cancelled", reason: "cancelled", phase: "walk", scanned: 4, total: 10, detail: "cancelled" }),
    );
    expect(stateOf("scan")).toBe("stopped");
    expect(card("scan").textContent).toMatch(/Nothing was saved/);
  });
});

describe("a scan started away from Library", () => {
  it("is followed to completion on Settings, and the user stays there", async () => {
    stubFetch((url) =>
      url === "/api/scan/rescan" ? json({ status: "scan started", library_dir: "/m" }) : undefined,
    );
    renderApp("/settings");
    expect(toggleText()).toBe("Activity");

    fireEvent.click(screen.getByRole("button", { name: "Rescan" }));
    // The follower joins the scan's stream (it was started by the rescan).
    const es = await waitFor(() => {
      const s = MockEventSource.instances.find((i) => i.url === "/api/scan/stream");
      if (!s) throw new Error("not following yet");
      return s;
    });
    act(() => {
      es.emit("start", { library_dir: "/m", total: 10, cached_count: 0, force: true });
      es.emit("progress", { scanned: 3, total: 10 });
    });
    expect(toggleText()).toBe("Scanning · 3 of 10");
    expect(where()).toBe("/settings");

    act(() => es.emit("done", { count: 10, marked_missing_count: 2, scanned_at: "2026-11-20T00:00:00Z" }));
    expect(toggleText()).toBe("Activity");
    expect(es.closed).toBe(true);
    expect(where()).toBe("/settings");

    fireEvent.click(toggle());
    await screen.findByRole("dialog", { name: "Activity" });
    expect(stateOf("scan")).toBe("completed");
    // The result links to the affected albums.
    const missing = within(card("scan")).getByRole("link", { name: "Show 2 missing albums" });
    expect(missing).toHaveAttribute("href", "/library?filter=missing");
    fireEvent.click(missing);
    expect(where()).toBe("/library?filter=missing");
    expect(getActivityPanel().open).toBe(false);
  });

  it("a scan already running at startup is joined; none is started just to look", async () => {
    stubFetch((url) =>
      url === "/api/scan/status" ? json({ status: "running", total: 20, scanned: 5, error: null }) : undefined,
    );
    renderApp("/review");
    await waitFor(() => expect(toggleText()).toBe("Scanning · 5 of 20"));
    expect(MockEventSource.instances.filter((i) => i.url === "/api/scan/stream")).toHaveLength(1);
  });

  it("opens no scan stream when no scan runs", async () => {
    const calls = stubFetch();
    renderApp("/review");
    await waitFor(() => expect(calls.some((c) => c.url === "/api/scan/status")).toBe(true));
    expect(MockEventSource.instances.some((i) => i.url === "/api/scan/stream")).toBe(false);
  });
});

describe("honest states", () => {
  it("a lost checks stream turns running progress into 'out of date'", async () => {
    stubFetch();
    renderApp("/review", client({ sweep: sweep({ status: "running", total: 8, completed: 2 }) }));
    const es = MockEventSource.instances.find((i) => i.url === "/api/checks/stream")!;
    act(() => es.emit("open"));
    expect(toggleText()).toBe("Checking · 2 of 8");
    act(() => es.emit("error"));
    expect(toggle()).toHaveAccessibleName("Activity: Checking · 2 of 8 (out of date)");
    act(() => openActivityPanel());
    await screen.findByRole("dialog", { name: "Activity" });
    expect(stateOf("checks")).toBe("stale");
    expect(within(card("checks")).getByRole("progressbar")).toHaveAttribute("aria-valuetext", "2 of 8 (out of date)");
    // The next event means it is live again.
    act(() => es.emit("sweep", sweep({ status: "running", total: 8, completed: 3 })));
    expect(stateOf("checks")).toBe("running");
  });

  it("a scan whose connection broke is not reported as finished or failed", async () => {
    stubFetch();
    renderApp("/review");
    act(() => {
      noteScanStarting();
      noteScanEvent({ type: "start", library_dir: "/m", total: 10, cached_count: 0, force: false });
      noteScanEvent({ type: "progress", scanned: 6, total: 10 });
      noteScanEvent({ type: "error", detail: "Scan stream error", lost: true });
    });
    act(() => openActivityPanel());
    await screen.findByRole("dialog", { name: "Activity" });
    expect(stateOf("scan")).toBe("stale");
    expect(card("scan").textContent).toMatch(/may still be running/);
    expect(within(card("scan")).getByRole("button", { name: "Check again" })).toBeInTheDocument();
  });

  it("status that never loaded is unavailable, not idle", () => {
    expect(sweepSummary(undefined, { failed: true, health: { state: "unknown", lastEventAt: null } }).state).toBe(
      "unavailable",
    );
  });
});

describe("keyboard", () => {
  it("opens from the toolbar control, moves focus in, hides the page behind, Escape closes and returns focus", async () => {
    stubFetch();
    renderApp("/settings");
    const button = toggle();
    button.focus();
    expect(button).toHaveAttribute("aria-expanded", "false");
    fireEvent.click(button);
    const panel = await screen.findByRole("dialog", { name: "Activity" });
    expect(button).toHaveAttribute("aria-expanded", "true");
    await waitFor(() => expect(panel.contains(document.activeElement)).toBe(true));
    expect(document.activeElement).toHaveTextContent("Activity");

    // Everything outside is hidden from assistive tech while it is open.
    expect(screen.queryByRole("button", { name: "Settings field" })).toBeNull();

    fireEvent.keyDown(document.activeElement!, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(document.activeElement).toBe(button);
    expect(button).toHaveAttribute("aria-expanded", "false");
    expect(where()).toBe("/settings");
  });

  it("the close button closes it", async () => {
    stubFetch();
    renderApp("/review");
    fireEvent.click(toggle());
    const panel = await screen.findByRole("dialog", { name: "Activity" });
    fireEvent.click(within(panel).getByRole("button", { name: "Close Activity" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  });

  // `?activity=` links: pages/review.test.tsx ("old links").
  it("opens from code (a native menu item)", async () => {
    stubFetch();
    renderApp("/review");
    act(() => openActivityPanel());
    expect(await screen.findByRole("dialog", { name: "Activity" })).toBeInTheDocument();
  });
});

describe("the model", () => {
  it("names one busy activity, or just Activity", () => {
    expect(toolbarText([]).text).toBe("Activity");
    expect(toolbarText([scanSummary({ ...IDLE_SCAN, phase: "done" })]).text).toBe("Activity");
    const scanning = scanSummary({ ...IDLE_SCAN, phase: "running", scanned: 1, total: 0 });
    expect(toolbarText([scanning]).text).toBe("Scanning · 1 so far");
  });

  it("keeps running, waiting, stopped, failed and completed apart", () => {
    const at = (phase: typeof IDLE_SCAN.phase) => scanSummary({ ...IDLE_SCAN, phase }).state;
    expect(at("running")).toBe("running");
    expect(at("cancelled")).toBe("stopped");
    expect(at("aborted")).toBe("stopped");
    expect(at("error")).toBe("failed");
    expect(at("done")).toBe("completed");
    expect(at("lost")).toBe("stale");
    expect(sweepSummary(sweep({ status: "waiting" }), { failed: false, health: { state: "live", lastEventAt: 1 } }).state).toBe(
      "waiting",
    );
  });

  it("follows a scan's events", () => {
    let s = reduceScan(IDLE_SCAN, { type: "start", library_dir: "/m", total: 3, cached_count: 0, force: false }, 1);
    s = reduceScan(s, { type: "progress", scanned: 2, total: 3 }, 2);
    expect([s.phase, s.scanned, s.total]).toEqual(["running", 2, 3]);
    s = reduceScan(s, { type: "done", count: 3, marked_missing_count: 0, scanned_at: "x" }, 3);
    expect([s.phase, s.message]).toEqual(["done", "Library up to date"]);
  });

  it("an interrupted bulk run is out of date, not completed", () => {
    const run = {
      id: "b",
      checkId: "c",
      checkLabel: "Tags",
      actionLabel: "Fix all",
      fixKind: "deterministic" as const,
      phase: "interrupted" as const,
      total: 3,
      started: true,
      counts: { fixed: 1, skipped: 0, errors: 0, not_applicable: 0 },
      items: [{ findingId: 1, albumId: "a", album: "A", outcome: "fixed" as const, reason: "" }],
      summary: null,
      error: "x",
      startedAt: 0,
      endedAt: 1,
    };
    expect(bulkSummary(run).state).toBe("stale");
    expect(toolbarText([bulkSummary(run)]).text).toBe("Activity");
  });
});
