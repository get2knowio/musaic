/**
 * Needs attention: the index of lists and what each one shows, what is
 * counted (on the page and in the Sidebar), the redirects from the old
 * Health and Jobs routes, and Back from an album's panel.
 * Checks run on their own, so there is no "Check now". Fix is disabled on
 * a finding whose album is Missing.
 */
import { afterEach, describe, it, expect, vi } from "vitest";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { Link, MemoryRouter, Route, Routes, useLocation, useNavigate } from "react-router-dom";

vi.mock("sonner", () => ({ toast: { error: vi.fn(), success: vi.fn() } }));

import AttentionPage from "@/pages/Attention";
import { HealthRedirect, JobsRedirect } from "@/pages/attentionRedirects";
import Sidebar from "@/components/Sidebar";
import { attentionBadgeText, FINDINGS_LIST_LIMIT } from "@/hooks/useAttention";
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
  { meta: meta("no-cover-art", "No cover art"), open_count: 1, last_run: null },
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

function client(f: Finding[] = findings, j: Job[] = jobs) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
  qc.setQueryData(["checks"], summaries);
  qc.setQueryData(["findings"], f);
  qc.setQueryData(["jobs"], j);
  qc.setQueryData(["ai-providers"], { selected: null, providers: [], uninstalled: [] });
  qc.setQueryData(["albums-snapshot"], [
    makeAlbum({ id: "present", title: "Here" }),
    makeAlbum({ id: "gone", title: "Gone", missing_since: "2026-01-02T00:00:00Z" }),
  ]);
  return qc;
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
  return render(
    <QueryClientProvider client={qc}>
      <MemoryRouter initialEntries={[url]}>
        <Sidebar />
        <Where />
        <Routes>
          <Route path="/attention" element={<AttentionPage />} />
          <Route path="/health" element={<HealthRedirect />} />
          <Route path="/health/:checkId" element={<HealthRedirect />} />
          <Route path="/jobs" element={<JobsRedirect />} />
          <Route path="/jobs/:jobId" element={<JobsRedirect />} />
          <Route path="/start" element={<Link to="/attention">go</Link>} />
          <Route path="/library/:albumId" element={<BackButton />} />
        </Routes>
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

const where = () => screen.getByTestId("where").textContent;
const lists = () => screen.getByRole("navigation", { name: "Lists" });
/** Open a list from the index (`nth`: a check with both decisions and
 *  suggestions is in the index twice). */
const pick = (label: string, nth = 0) => fireEvent.click(within(lists()).getAllByText(label)[nth]);
const current = () => lists().querySelector('[aria-current="true"]')!.textContent;
const summary = () => document.querySelector("[aria-live]")!.textContent;
const navLink = () => screen.getByRole("link", { name: /Needs attention/ });
const scrollerEl = () => document.querySelector('div[tabindex="-1"]') as HTMLElement;

afterEach(() => {
  vi.unstubAllGlobals();
  vi.clearAllMocks();
  sessionStorage.clear();
});


describe("the index", () => {
  it("lists every non-empty list with its count, decisions first, and opens the first", () => {
    renderAt("/attention");
    const rows = [...lists().querySelectorAll("button")].map((b) => b.textContent);
    expect(rows).toEqual([
      "Proposed changes to review1",
      "Failed jobs1",
      "Track gaps2",
      "No cover art1",
      "Track gaps1",
      "In progress1",
      "Finished jobs1",
      "Ignored findings",
    ]);
    expect(current()).toBe("Proposed changes to review1");
    expect(screen.getByText("Album proposed")).toBeTruthy();
    expect(screen.queryByText("gap 1")).toBeNull();
  });

  it("counts decisions and suggestions separately, on the page and in the Sidebar", () => {
    renderAt("/attention");
    // 3 findings (gap 1, gap 2, no cover) + 1 proposed + 1 failed; 1 informational.
    expect(summary()).toBe("5 to decide, 1 suggestion");
    expect(navLink().textContent).toBe("Needs attention5 to decide1 suggestion");
    const groups = [...lists().querySelectorAll("[data-tier]")].map((g) => g.textContent);
    expect(groups).toEqual(["Needs a decision5", "Suggestions1", "Activity"]);
  });

  it("the Sidebar shows suggestions alone when there is nothing to decide", () => {
    renderAt("/attention", client([finding(3, { severity: "info" })], [job("done", "acted_on")]));
    expect(navLink().textContent).toBe("Needs attention1 suggestion");
    expect(summary()).toBe("0 to decide, 1 suggestion");
    // The suggestions are on the page, not behind anything.
    expect(within(lists()).getByText("Nothing to decide")).toBeTruthy();
    expect(screen.getByText("gap 3")).toBeTruthy();
  });

  it("the Sidebar shows no number when there is neither", () => {
    renderAt("/attention", client([], [job("done", "acted_on")]));
    expect(navLink().textContent).toBe("Needs attention");
    expect(screen.getByText("Album done")).toBeTruthy();
  });

  it("says nothing needs attention only when every list is empty", () => {
    renderAt("/attention", client([], []));
    expect(screen.getByRole("heading", { name: "Nothing needs attention" })).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Ignored findings" }));
    expect(where()).toBe("/attention?list=ignored");
  });

  it("the album filter for jobs is named for jobs, and absent when it would list nothing", () => {
    renderAt("/attention");
    expect(screen.getByRole("button", { name: /Jobs to review/ })).toBeTruthy();
    expect(screen.queryByText("Needs review")).toBeNull();
  });

  it("no jobs filter without a job waiting, whatever the findings", () => {
    renderAt("/attention", client(findings, []));
    expect(screen.queryByRole("button", { name: /Jobs to review/ })).toBeNull();
    expect(navLink().textContent).toContain("3 to decide");
  });

  it("the badge is a lower bound when the findings list hit its limit", () => {
    expect(attentionBadgeText(7, false)).toBe("7");
    expect(attentionBadgeText(7, true)).toBe("7+");
    expect(FINDINGS_LIST_LIMIT).toBe(10_000);
  });
});

describe("a check's list", () => {
  it("shows what the check looks for and all its open findings, whatever their severity", () => {
    renderAt("/attention");
    pick("Track gaps");
    expect(where()).toBe("/attention?check=track-gaps");
    expect(screen.getByRole("heading", { name: "Track gaps" })).toBeTruthy();
    expect(screen.getByText("Track gaps description")).toBeTruthy();
    expect(screen.getByText("2 need a decision, 1 are suggestions")).toBeTruthy();
    expect(screen.getByText("gap 1")).toBeTruthy();
    expect(screen.getByText("gap 2")).toBeTruthy();
    expect(screen.getByText("just so you know")).toBeTruthy();
    expect(screen.queryByText("no cover")).toBeNull();
    expect(screen.queryByText("ignored one")).toBeNull();
    expect(screen.queryByText("Album proposed")).toBeNull();
  });

  it("is the same list from either of the check's index rows", () => {
    renderAt("/attention");
    pick("Track gaps", 1);
    expect(where()).toBe("/attention?check=track-gaps");
    expect(lists().querySelectorAll('[aria-current="true"]')).toHaveLength(2);
  });

  it("says so when the check has nothing open", () => {
    renderAt("/attention?check=track-gaps", client([], []));
    expect(screen.getByText(/Nothing open for this check/)).toBeTruthy();
  });

  it("has no Check now and no run controls", () => {
    renderAt("/attention?check=track-gaps");
    expect(screen.queryByRole("button", { name: /Check now|Run now|Checking/ })).toBeNull();
  });

  it("disables Fix on a Missing album's finding and says why", () => {
    renderAt("/attention?check=track-gaps");
    const fixes = screen.getAllByRole("button", { name: /Fix/ }) as HTMLButtonElement[];
    const gone = fixes.find((b) => b.disabled)!;
    expect(gone.title).toMatch(/folder is missing/);
  });
});

describe("job lists", () => {
  it("each has its own list", () => {
    renderAt("/attention");
    pick("Failed jobs");
    expect(where()).toBe("/attention?jobs=failed");
    expect(screen.getByText("Album failed")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Clear 1 failed" })).toBeTruthy();
    pick("In progress");
    expect(screen.getByText("Album running")).toBeTruthy();
    pick("Finished jobs");
    expect(where()).toBe("/attention?jobs=finished");
    expect(screen.getByText("Album done")).toBeTruthy();
    expect(screen.queryByText("Album failed")).toBeNull();
  });
});

describe("old links", () => {
  for (const view of ["suggestions", "other"]) {
    it(`?view=${view}, a former tab, opens the first list of suggestions`, () => {
      renderAt(`/attention?view=${view}`);
      expect(screen.getByRole("heading", { name: "Track gaps" })).toBeTruthy();
      expect(screen.getByText("just so you know")).toBeTruthy();
    });
  }

  it("/health goes to Needs attention", async () => {
    renderAt("/health");
    await waitFor(() => expect(where()).toBe("/attention"));
  });

  it("/health/:checkId opens that check's list", async () => {
    renderAt("/health/track-gaps");
    await waitFor(() => expect(where()).toBe("/attention?check=track-gaps"));
    expect(screen.getByText("gap 1")).toBeTruthy();
    expect(screen.getByText("just so you know")).toBeTruthy();
    expect(screen.queryByText("no cover")).toBeNull();
  });

  for (const from of ["/jobs?job=done", "/jobs/done"]) {
    it(`${from} opens the list holding that job, with the job highlighted`, async () => {
      renderAt(from);
      await waitFor(() => expect(where()).toBe("/attention?job=done"));
      expect(current()).toBe("Finished jobs1");
      const row = document.querySelector('[data-job-id="done"]');
      expect(row).toBeTruthy();
      expect(row!.className).toContain("bg-accent-soft");
    });
  }

  it("/jobs goes to Needs attention", async () => {
    renderAt("/jobs");
    await waitFor(() => expect(where()).toBe("/attention"));
  });
});

describe("Back from an album", () => {
  it("returns to the same list and scroll position", async () => {
    renderAt("/attention?check=track-gaps");
    scrollerEl().scrollTop = 120;
    fireEvent.scroll(scrollerEl());
    expect(sessionStorage.getItem("mm.attention.scroll.check.track-gaps")).toBe("120");
    // The album link on a row opens the library with that album's panel.
    fireEvent.click(screen.getAllByRole("link", { name: "View album detail" })[0]);
    expect(where()).toMatch(/^\/library\/present/);
    fireEvent.click(screen.getByRole("button", { name: "Back" }));
    await waitFor(() => expect(where()).toBe("/attention?check=track-gaps"));
    expect(screen.getByRole("heading", { name: "Track gaps" })).toBeTruthy();
    expect(scrollerEl().scrollTop).toBe(120);
  });

  it("a fresh visit starts at the top", () => {
    sessionStorage.setItem("mm.attention.scroll.jobs.proposed", "300");
    renderAt("/start");
    fireEvent.click(screen.getByRole("link", { name: "go" }));
    expect(scrollerEl().scrollTop).toBe(0);
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

  it("are not counted or listed; a check's list can show its own, and restore them", async () => {
    const fetchMock = stubIgnored();
    renderAt("/attention?check=track-gaps");
    expect(screen.queryByText("ignored gap")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Show ignored" }));
    expect(where()).toBe("/attention?check=track-gaps&ignored=1");
    expect(await screen.findByText("ignored gap")).toBeTruthy();
    // Still not counted anywhere.
    expect(summary()).toBe("5 to decide, 1 suggestion");
    expect(navLink().textContent).toContain("5 to decide");

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

  it("have a list of their own, grouped by check", async () => {
    stubIgnored();
    renderAt("/attention");
    pick("Ignored findings");
    expect(where()).toBe("/attention?list=ignored");
    expect(await screen.findByText("ignored gap")).toBeTruthy();
    expect(screen.getByText("ignored one")).toBeTruthy();
    expect(screen.getByRole("heading", { name: "Track gaps" })).toBeTruthy();
    expect(screen.queryByText("gap 1")).toBeNull();
  });
});

describe("long lists", () => {
  const many = Array.from({ length: 60 }, (_, i) => finding(100 + i, { summary: `many ${i}` }));

  it("caps a check's list at 50 rows with a Show all N button; counts stay true", () => {
    renderAt("/attention", client(many, []));
    expect(screen.getAllByText(/^many \d+$/)).toHaveLength(50);
    expect(summary()).toBe("60 to decide, 0 suggestions");
    expect(navLink().textContent).toContain("60 to decide");
    expect(document.querySelector("#check-track-gaps")!.textContent).toContain("60");
    fireEvent.click(screen.getByRole("button", { name: "Show all 60" }));
    expect(screen.getAllByText(/^many \d+$/)).toHaveLength(60);
    expect(screen.queryByRole("button", { name: /Show all/ })).toBeNull();
  });
});
