/**
 * Review (#325): one inbox of open findings of every severity, proposed
 * changes and failed jobs, most pressing first; one neutral Sidebar badge
 * with the header's count; optional check, type and album filters; jobs in
 * progress, job history and ignored findings beside it and never counted;
 * the old Needs attention, Health and Jobs links; Back from an album.
 * Checks run on their own, so there is no "Check now". Fix is disabled on
 * a finding whose album is Missing.
 */
import { afterEach, describe, it, expect, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { Link, MemoryRouter, Route, Routes, useLocation, useNavigate } from "react-router-dom";

vi.mock("sonner", () => ({ toast: { error: vi.fn(), success: vi.fn() } }));

import ReviewPage from "@/pages/Review";
import ActivityPanel from "@/components/ActivityPanel";
import { useActivityLinks } from "@/hooks/useActivityLinks";
import { resetActivityPanel } from "@/lib/activityPanel";
import { AttentionRedirect, HealthRedirect, JobsRedirect } from "@/pages/reviewRedirects";
import { legacyAttentionSearch, reviewUrl } from "@/lib/reviewLinks";
import Sidebar from "@/components/Sidebar";
import {
  buildReviewItems,
  countText,
  FINDINGS_LIST_LIMIT,
  sortReviewItems,
} from "@/hooks/useReview";
import { makeAlbum, makeJob } from "@/test/fixtures";
import type { CheckMeta, CheckSummary, Finding, Job } from "@/lib/types";

const meta = (id: string, label: string, bulk: string | null = null): CheckMeta => ({
  id,
  label,
  description: `${label} description`,
  category: "metadata",
  cost: "cheap",
  runs_during_scan: true,
  fix_kind: "deterministic",
  bulk_action_label: bulk,
});

const summaries: CheckSummary[] = [
  { meta: meta("track-gaps", "Track gaps"), open_count: 3, last_run: null },
  { meta: meta("no-cover-art", "No cover art", "Find covers"), open_count: 1, last_run: null },
];

const finding = (id: number, over: Partial<Finding> = {}): Finding => ({
  id,
  check_id: "track-gaps",
  album_id: "present",
  severity: "warn",
  summary: `gap ${id}`,
  detail: null,
  first_seen_at: "2026-01-01T00:00:00Z",
  last_seen_at: "2026-01-01T00:00:00Z",
  resolved_at: null,
  ignored_at: null,
  ignored_reason: null,
  ...over,
});

const job = (id: string, status: Job["status"], over: Partial<Job> = {}): Job =>
  makeJob({ id, status, album_id: "present", album_title: `Album ${id}`, job_type: "metadata", ...over });

const findings: Finding[] = [
  finding(1),
  finding(2, { severity: "error", album_id: "gone" }),
  finding(3, { severity: "info", summary: "just so you know" }),
  finding(4, { ignored_at: "2026-01-02T00:00:00Z", summary: "ignored one" }),
  finding(5, { check_id: "no-cover-art", summary: "no cover" }),
];
const jobs: Job[] = [
  job("proposed", "completed"),
  job("failed", "failed", { error: "boom" }),
  job("running", "running"),
  job("done", "acted_on", { acted_on_action: "applied" }),
];

function client(f: Finding[] = findings, j: Job[] = jobs, checks: CheckSummary[] = summaries) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
  qc.setQueryData(["checks"], checks);
  qc.setQueryData(["findings"], f);
  qc.setQueryData(["jobs"], j);
  qc.setQueryData(["agent", "approvals", "pending"], []);
  qc.setQueryData(["ai-providers"], { selected: null, providers: [], uninstalled: [] });
  qc.setQueryData(["albums-snapshot"], [
    makeAlbum({ id: "present", title: "Here" }),
    makeAlbum({ id: "gone", title: "Gone", missing_since: "2026-01-02T00:00:00Z" }),
  ]);
  return qc;
}

/** What App mounts for the Activity panel: the panel and its `?activity=` links. */
function ActivityHost() {
  useActivityLinks();
  return <ActivityPanel />;
}

function Where() {
  const l = useLocation();
  return <div data-testid="where">{l.pathname + l.search}</div>;
}

function BackButton() {
  const navigate = useNavigate();
  return <button onClick={() => navigate(-1)}>Back</button>;
}

function renderAt(url: string, qc = client()) {
  render(
    <QueryClientProvider client={qc}>
      <MemoryRouter initialEntries={[url]}>
        <Sidebar />
        <Where />
        <ActivityHost />
        <Routes>
          <Route path="/review" element={<ReviewPage />} />
          <Route path="/attention" element={<AttentionRedirect />} />
          <Route path="/health" element={<HealthRedirect />} />
          <Route path="/health/:checkId" element={<HealthRedirect />} />
          <Route path="/jobs" element={<JobsRedirect />} />
          <Route path="/jobs/:jobId" element={<JobsRedirect />} />
          <Route path="/start" element={<Link to="/review">go</Link>} />
          <Route path="/library/:albumId" element={<BackButton />} />
        </Routes>
      </MemoryRouter>
    </QueryClientProvider>,
  );
  return qc;
}

const where = () => screen.getByTestId("where").textContent;
const rail = () => screen.getByRole("navigation", { name: "Filters" });
const pick = (label: string) => fireEvent.click(within(rail()).getByText(label));
const railRows = () => [...rail().querySelectorAll("button")].map((b) => b.textContent);
const current = () => [...rail().querySelectorAll('[aria-current="true"]')].map((b) => b.textContent);
const summary = () => document.querySelector("[data-review-summary]")!.textContent;
const navLink = () => screen.getByRole("link", { name: /Review/ });
/** The badge as shown, without its screen-reader words. */
const badge = () => {
  const b = navLink().querySelector("[data-badge]");
  return b ? b.firstChild!.textContent : null;
};
const rows = () => [...document.querySelectorAll("[data-review-item]")] as HTMLElement[];
const rowKeys = () => rows().map((r) => r.dataset.reviewItem);
const scrollerEl = () => document.querySelector('div[tabindex="-1"]') as HTMLElement;

afterEach(() => {
  vi.unstubAllGlobals();
  vi.clearAllMocks();
  sessionStorage.clear();
  resetActivityPanel();
});

describe("the acceptance example", () => {
  const twoWarnOneInfo = [
    finding(1),
    finding(2),
    finding(3, { severity: "info", summary: "just so you know" }),
  ];

  it("two warnings and one informational track-gap finding: Review 3, one Track gaps entry, three rows", () => {
    renderAt("/review", client(twoWarnOneInfo, []));
    expect(badge()).toBe("3");
    expect(navLink().textContent).toBe("Review3 items to review");
    expect(summary()).toBe("3 items · 1 album");
    expect(within(rail()).getAllByText("Track gaps")).toHaveLength(1);
    expect(current()).toEqual(["All open3"]);
    expect(rowKeys()).toEqual(["finding:1", "finding:2", "finding:3"]);
    // Informational on the first visit, labelled, nothing to open first.
    expect(screen.getByText("just so you know")).toBeTruthy();
    expect([...document.querySelectorAll("[data-urgency]")].map((e) => e.textContent)).toEqual([
      "Warning",
      "Warning",
      "Suggestion",
    ]);
  });

  it("with only the informational finding left, Review 1 opens to it; ignoring it clears the badge", async () => {
    const info = finding(3, { severity: "info", summary: "just so you know" });
    const fetchMock = vi.fn(async () =>
      new Response(JSON.stringify({ ...info, ignored_at: "2026-01-03T00:00:00Z" }), {
        status: 200,
        headers: { "content-type": "application/json" },
      }),
    );
    vi.stubGlobal("fetch", fetchMock);
    const qc = renderAt("/review", client([info], []));
    expect(badge()).toBe("1");
    expect(summary()).toBe("1 item · 1 album");
    expect(rowKeys()).toEqual(["finding:3"]);

    fireEvent.pointerDown(screen.getByRole("button", { name: "Ignore" }), { button: 0, pointerType: "mouse" });
    fireEvent.click(await screen.findByRole("button", { name: "Ignore finding" }));
    await waitFor(() =>
      expect(fetchMock).toHaveBeenCalledWith(
        "/api/checks/track-gaps/findings/3/ignore",
        expect.objectContaining({ method: "POST" }),
      ),
    );
    // The checks stream refreshes the findings: the ignored one is gone.
    act(() => qc.setQueryData(["findings"], []));
    await waitFor(() => expect(badge()).toBeNull());
    expect(navLink().textContent).toBe("Review");
    expect(screen.getByRole("heading", { name: "Nothing to review" })).toBeTruthy();
  });

  it("a failed job and a ready proposal are items; a running job is not", () => {
    renderAt("/review", client(twoWarnOneInfo, [job("proposed", "completed"), job("failed", "failed"), job("running", "running")]));
    expect(badge()).toBe("5");
    expect(rowKeys()).toEqual(["job:failed", "finding:1", "finding:2", "job:proposed", "finding:3"]);
    expect(screen.queryByText("Album running")).toBeNull();
    // The proposal opens in the album inspector.
    fireEvent.click(screen.getByText("Album proposed"));
    expect(where()).toBe("/library/present");
  });

  it("a capped result keeps the lower-bound mark on the badge and in the header", () => {
    const many = Array.from({ length: FINDINGS_LIST_LIMIT }, (_, i) => finding(1000 + i, { severity: "info" }));
    renderAt("/review", client(many, []));
    expect(badge()).toBe("10000+");
    expect(summary()).toBe("10000+ items · 1 album");
    expect(rows()).toHaveLength(50);
  });

  it("a source failure: the badge is a lower bound and the page says what is missing", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => new Response(JSON.stringify({ detail: "boom" }), { status: 500, headers: { "content-type": "application/json" } })),
    );
    const qc = client(twoWarnOneInfo, []);
    qc.removeQueries({ queryKey: ["jobs"], exact: true });
    renderAt("/review", qc);
    expect(await screen.findByRole("button", { name: "Retry jobs" })).toBeTruthy();
    expect(badge()).toBe("3+");
    expect(rows()).toHaveLength(3);
  });
});

describe("All open", () => {
  it("lists every open item, most pressing first, with the rail of filters", () => {
    renderAt("/review");
    expect(rowKeys()).toEqual([
      "finding:2", // error
      "job:failed",
      "finding:1", // warnings, in the checks' order
      "finding:5",
      "job:proposed",
      "finding:3", // informational
    ]);
    expect(railRows()).toEqual([
      "All open6",
      "Proposed changes1",
      "Failed jobs1",
      "Track gaps3",
      "No cover art1",
      "Ignored findings",
      "Jobs in progress and history",
    ]);
    expect(current()).toEqual(["All open6"]);
    expect(summary()).toBe("6 items · 2 albums");
    expect(badge()).toBe("6");
    // Ignored, running and finished are not listed.
    expect(screen.queryByText("ignored one")).toBeNull();
    expect(screen.queryByText("Album running")).toBeNull();
    expect(screen.queryByText("Album done")).toBeNull();
  });

  it("the order is the same whatever order the items came in", () => {
    const order = ["track-gaps", "no-cover-art"];
    const a = buildReviewItems(findings, jobs, order).map((i) => i.key);
    const b = sortReviewItems(buildReviewItems([...findings].reverse(), [...jobs].reverse(), order).reverse(), order).map(
      (i) => i.key,
    );
    expect(b).toEqual(a);
  });

  it("the badge is neutral, not a warning", () => {
    renderAt("/review");
    const b = navLink().querySelector("[data-badge]")!;
    expect(b.className).not.toContain("text-warn");
    expect(b.className).not.toContain("text-err");
  });

  it("countText marks a lower bound", () => {
    expect(countText(7, false)).toBe("7");
    expect(countText(7, true)).toBe("7+");
  });
});

describe("filters", () => {
  it("a check filter shows N of M items and leaves the badge alone; exactly one filter is current", () => {
    renderAt("/review");
    pick("Track gaps");
    expect(where()).toBe("/review?check=track-gaps");
    expect(summary()).toBe("3 of 6 items · 2 albums");
    expect(badge()).toBe("6");
    expect(current()).toEqual(["Track gaps3"]);
    expect(screen.getByRole("heading", { name: "Track gaps" })).toBeTruthy();
    expect(screen.getByText("Track gaps description")).toBeTruthy();
    expect(rowKeys()).toEqual(["finding:2", "finding:1", "finding:3"]);
    expect(screen.queryByText("no cover")).toBeNull();
  });

  it("type filters list proposed changes or failed jobs", () => {
    renderAt("/review");
    pick("Failed jobs");
    expect(where()).toBe("/review?kind=failed");
    expect(rowKeys()).toEqual(["job:failed"]);
    expect(screen.getByRole("button", { name: "Clear 1 failed" })).toBeTruthy();
    pick("Proposed changes");
    expect(where()).toBe("/review?kind=proposed");
    expect(rowKeys()).toEqual(["job:proposed"]);
    expect(summary()).toBe("1 of 6 items · 1 album");
  });

  it("an album filter narrows any type, shows as a chip, and clears", () => {
    renderAt("/review?album=gone");
    expect(rowKeys()).toEqual(["finding:2"]);
    expect(summary()).toBe("1 of 6 items · 1 album");
    expect(screen.getByText("Album: Gone — Artist")).toBeTruthy();
    pick("Track gaps");
    expect(where()).toBe("/review?check=track-gaps&album=gone");
    expect(rowKeys()).toEqual(["finding:2"]);
    fireEvent.click(screen.getByRole("button", { name: "Clear the album filter" }));
    expect(where()).toBe("/review?check=track-gaps");
    expect(rowKeys()).toHaveLength(3);
  });

  it("a check with nothing open, opened by link, says so", () => {
    renderAt("/review?check=track-gaps", client([], []));
    expect(screen.getByText(/Nothing open for this check/)).toBeTruthy();
    expect(current()).toEqual(["Track gaps0"]);
  });

  it("has no Check now and no run controls", () => {
    renderAt("/review?check=track-gaps");
    expect(screen.queryByRole("button", { name: /Check now|Run now|Checking/ })).toBeNull();
  });

  it("disables the fix on a Missing album's finding and says why", () => {
    renderAt("/review?check=track-gaps");
    const fixes = screen.getAllByRole("button", { name: /Renumber tracks|Check track numbers/ }) as HTMLButtonElement[];
    const gone = fixes.find((b) => b.disabled)!;
    expect(gone.title).toMatch(/folder is missing/);
  });
});

describe("a check-wide bulk command", () => {
  const coverChecks: CheckSummary[] = [
    summaries[0],
    { meta: meta("no-cover-art", "No cover art", "Find covers"), open_count: 4, last_run: null },
  ];
  const covers = [
    finding(11, { check_id: "no-cover-art", summary: "c1" }),
    finding(12, { check_id: "no-cover-art", summary: "c2", album_id: "gone" }),
    finding(13, { check_id: "no-cover-art", summary: "c3", album_id: "elsewhere" }),
    finding(14, { check_id: "no-cover-art", summary: "c4", album_id: "elsewhere2" }),
  ];

  it("states the whole library's count and scope", () => {
    renderAt("/review?check=no-cover-art", client(covers, [], coverChecks));
    expect(screen.getByRole("button", { name: "Find covers (4)" })).toBeTruthy();
    expect(document.querySelector("[data-bulk-scope]")!.textContent).toBe(
      "Find covers works on all 4 open No cover art findings in the library.",
    );
  });

  it("still states it under a narrower album filter", () => {
    renderAt("/review?check=no-cover-art&album=gone", client(covers, [], coverChecks));
    expect(rowKeys()).toEqual(["finding:12"]);
    expect(screen.getByRole("button", { name: "Find covers (4)" })).toBeTruthy();
    expect(document.querySelector("[data-bulk-scope]")!.textContent).toBe(
      "Find covers works on all 4 open No cover art findings in the library, not only the 1 shown here.",
    );
  });

  it("is offered only on a check's filter", () => {
    renderAt("/review", client(covers, [], coverChecks));
    expect(screen.queryByRole("button", { name: /Find covers/ })).toBeNull();
  });
});

describe("jobs in progress and job history are in the Activity panel", () => {
  it("the rail opens Activity over Review; the jobs stay uncounted", async () => {
    renderAt("/review");
    expect(badge()).toBe("6");
    pick("Jobs in progress and history");
    const panel = await screen.findByRole("dialog", { name: "Activity" });
    expect(where()).toBe("/review");
    expect(within(panel).getByText("Album running")).toBeTruthy();
    expect(within(panel).getByText("Album done")).toBeTruthy();
    expect(summary()).toBe("6 items · 2 albums");
  });
});

describe("old links", () => {
  for (const [from, to] of [
    ["/attention", "/review"],
    ["/attention?check=track-gaps", "/review?check=track-gaps"],
    ["/attention?view=suggestions", "/review"],
    ["/attention?view=other", "/review"],
    ["/attention?jobs=proposed", "/review?kind=proposed"],
    ["/attention?jobs=failed", "/review?kind=failed"],
    ["/attention?list=ignored", "/review?show=ignored"],
    ["/attention?job=failed", "/review?job=failed"],
    ["/attention?check=track-gaps&ignored=1", "/review?check=track-gaps&ignored=1"],
    ["/health", "/review"],
    ["/health/track-gaps", "/review?check=track-gaps"],
    ["/jobs/failed", "/review?job=failed"],
  ]) {
    it(`${from} lands on ${to}`, async () => {
      renderAt(from);
      await waitFor(() => expect(where()).toBe(to));
    });
  }

  // Jobs in progress and job history moved to the Activity panel: their
  // old addresses open it over Review, at the section (and job) they named.
  for (const [from, section, job] of [
    ["/attention?jobs=running", "jobs", null],
    ["/attention?jobs=finished", "history", null],
    ["/review?show=running", "jobs", null],
    ["/review?show=history", "history", null],
    ["/jobs", "jobs", null],
    ["/jobs/done", "history", "done"],
    ["/jobs?job=done", "history", "done"],
    ["/review?job=running", "jobs", "running"],
  ] as const) {
    it(`${from} opens Activity at ${section}${job ? ` with ${job} marked` : ""}`, async () => {
      renderAt(from);
      const panel = await screen.findByRole("dialog", { name: "Activity" });
      await waitFor(() => expect(where()).toBe("/review"));
      // Over All open (the page behind the modal panel is hidden from roles).
      const behind = document.querySelector('nav[aria-label="Filters"] [aria-current="true"]');
      expect(behind?.textContent).toBe("All open6");
      await waitFor(() =>
        expect(document.activeElement).toBe(panel.querySelector(`[data-activity-section="${section}"]`)),
      );
      if (job) {
        expect(panel.querySelector(`[data-activity-job="${job}"]`)!.getAttribute("data-current")).toBe("true");
      }
    });
  }

  it("the translation of Needs attention's query string", () => {
    expect(legacyAttentionSearch("?jobs=finished&job=x")).toBe("activity=history&job=x");
    expect(legacyAttentionSearch("?jobs=running")).toBe("activity=jobs");
    expect(legacyAttentionSearch("?view=suggestions")).toBe("");
    expect(reviewUrl({ check: "track-gaps", album: "a1" })).toBe("/review?check=track-gaps&album=a1");
    expect(reviewUrl({ show: "ignored" })).toBe("/review?show=ignored");
  });

  it("?view=suggestions shows informational findings at once", async () => {
    renderAt("/attention?view=suggestions");
    await waitFor(() => expect(where()).toBe("/review"));
    expect(screen.getByText("just so you know")).toBeTruthy();
  });

  it("a linked failed job is highlighted in All open", async () => {
    renderAt("/jobs/failed");
    await waitFor(() => expect(where()).toBe("/review?job=failed"));
    expect(current()).toEqual(["All open6"]);
    expect(document.querySelector('[data-job-id="failed"]')!.className).toContain("bg-accent-soft");
  });

});

describe("Back from an album", () => {
  it("returns to the same filter, item and scroll position", async () => {
    renderAt("/review?check=track-gaps");
    scrollerEl().scrollTop = 120;
    fireEvent.scroll(scrollerEl());
    expect(sessionStorage.getItem("mm.review.scroll.check.track-gaps")).toBe("120");
    // Open the second row's album.
    fireEvent.click(within(rows()[1]).getByRole("link", { name: "View album detail" }));
    expect(where()).toMatch(/^\/library\/present/);
    fireEvent.click(screen.getByRole("button", { name: "Back" }));
    await waitFor(() => expect(where()).toBe("/review?check=track-gaps"));
    expect(screen.getByRole("heading", { name: "Track gaps" })).toBeTruthy();
    expect(scrollerEl().scrollTop).toBe(120);
    expect(rows().filter((r) => r.dataset.current === "true").map((r) => r.dataset.reviewItem)).toEqual([
      "finding:1",
    ]);
  });

  it("a fresh visit starts at the top with nothing marked", () => {
    sessionStorage.setItem("mm.review.scroll.all", "300");
    sessionStorage.setItem("mm.review.item.all", "finding:1");
    renderAt("/start");
    fireEvent.click(screen.getByRole("link", { name: "go" }));
    expect(scrollerEl().scrollTop).toBe(0);
    expect(rows().some((r) => r.dataset.current === "true")).toBe(false);
  });
});

describe("ignored findings", () => {
  const ignored = finding(9, {
    ignored_at: "2026-01-02T00:00:00Z",
    summary: "ignored gap",
    severity: "warn",
  });

  function stubIgnored() {
    const fetchMock = vi.fn(async (url: string) => {
      if (String(url).startsWith("/api/checks/findings?include_ignored=true")) {
        return new Response(JSON.stringify({ findings: [...findings, ignored] }), {
          status: 200,
          headers: { "content-type": "application/json" },
        });
      }
      return new Response(JSON.stringify({}), { status: 200, headers: { "content-type": "application/json" } });
    });
    vi.stubGlobal("fetch", fetchMock);
    return fetchMock;
  }

  it("are not counted or listed; a check's filter can show its own, and restore them", async () => {
    const fetchMock = stubIgnored();
    renderAt("/review?check=track-gaps");
    expect(screen.queryByText("ignored gap")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Show ignored" }));
    expect(where()).toBe("/review?check=track-gaps&ignored=1");
    expect(await screen.findByText("ignored gap")).toBeTruthy();
    expect(badge()).toBe("6");
    expect(summary()).toBe("3 of 6 items · 2 albums");

    const row = screen.getByText("ignored gap").closest("div.px-3")!;
    fireEvent.click(within(row as HTMLElement).getByRole("button", { name: "Restore" }));
    await waitFor(() =>
      expect(fetchMock).toHaveBeenCalledWith(
        "/api/checks/track-gaps/findings/9/unignore",
        expect.objectContaining({ method: "POST" }),
      ),
    );
    fireEvent.click(screen.getByRole("button", { name: "Hide ignored" }));
    expect(screen.queryByText("ignored gap")).toBeNull();
  });

  it("have a view of their own, grouped by check", async () => {
    stubIgnored();
    renderAt("/review");
    pick("Ignored findings");
    expect(where()).toBe("/review?show=ignored");
    expect(await screen.findByText("ignored gap")).toBeTruthy();
    expect(screen.getByText("ignored one")).toBeTruthy();
    expect(screen.queryByText("gap 1")).toBeNull();
  });
});

describe("long lists", () => {
  const many = Array.from({ length: 60 }, (_, i) => finding(100 + i, { summary: `many ${i}` }));

  it("caps the list at 50 rows with a Show all N button kept in the URL; counts stay true", () => {
    renderAt("/review", client(many, []));
    expect(screen.getAllByText(/^many \d+$/)).toHaveLength(50);
    expect(summary()).toBe("60 items · 1 album");
    expect(badge()).toBe("60");
    fireEvent.click(screen.getByRole("button", { name: "Show all 60" }));
    expect(where()).toBe("/review?all=1");
    expect(screen.getAllByText(/^many \d+$/)).toHaveLength(60);
    expect(screen.queryByRole("button", { name: /Show all/ })).toBeNull();
  });
});
