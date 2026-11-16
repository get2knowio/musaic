/**
 * Needs attention: what is listed and counted, the filters, the redirects
 * from the old Health and Jobs routes, and Back from an album's panel.
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
const scrollerEl = () => document.querySelector('div[tabindex="-1"]') as HTMLElement;

afterEach(() => {
  vi.unstubAllGlobals();
  vi.clearAllMocks();
  sessionStorage.clear();
});

describe("what is listed and counted", () => {
  it("lists warn/error findings, proposed and failed jobs, and counts exactly those", () => {
    renderAt("/attention");
    expect(screen.getByText("gap 1")).toBeTruthy();
    expect(screen.getByText("gap 2")).toBeTruthy();
    expect(screen.getByText("no cover")).toBeTruthy();
    expect(screen.getByText("Album proposed")).toBeTruthy();
    expect(screen.getByText("Album failed")).toBeTruthy();
    // Not on the default view: info, ignored, finished.
    expect(screen.queryByText("just so you know")).toBeNull();
    expect(screen.queryByText("ignored one")).toBeNull();
    expect(screen.queryByText("Album done")).toBeNull();
    // 3 findings (gap 1, gap 2, no cover) + 1 proposed + 1 failed.
    expect(document.querySelector("[aria-live]")!.textContent).toBe("5 items");
    const badge = screen.getByRole("link", { name: /Needs attention/ });
    expect(badge.textContent).toContain("5");
    // A running job is shown as progress but not counted.
    expect(screen.getByText("In progress")).toBeTruthy();
  });

  it("the Suggestions view lists informational findings, then finished jobs", () => {
    renderAt("/attention");
    // The tab counts suggestion findings only (one), not the finished job.
    fireEvent.click(screen.getByRole("button", { name: "Suggestions (1)" }));
    expect(where()).toBe("/attention?view=suggestions");
    expect(screen.getByText("just so you know")).toBeTruthy();
    expect(screen.getByText("Album done")).toBeTruthy();
    expect(screen.queryByText("gap 1")).toBeNull();
    // Still 5 in the badge.
    expect(screen.getByRole("link", { name: /Needs attention/ }).textContent).toContain("5");
  });

  it("?view=other, the old name, lands on the Suggestions view", () => {
    renderAt("/attention?view=other");
    expect(screen.getByRole("button", { name: "Suggestions (1)", pressed: true })).toBeTruthy();
    expect(screen.getByText("just so you know")).toBeTruthy();
    expect(screen.queryByText("gap 1")).toBeNull();
  });

  it("lists finished jobs in their own section below the findings", () => {
    renderAt("/attention?view=suggestions");
    const finding = screen.getByText("just so you know");
    const heading = screen.getByRole("heading", { name: "Finished jobs" });
    expect(finding.compareDocumentPosition(heading) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  });

  it("shows finished jobs, not the empty state, when there are no suggestion findings", () => {
    renderAt("/attention?view=suggestions", client([finding(1)], [job("done", "acted_on")]));
    expect(screen.getByText("Album done")).toBeTruthy();
    expect(screen.queryByRole("heading", { name: "No suggestions" })).toBeNull();
    expect(screen.getByRole("button", { name: "Suggestions (0)" })).toBeTruthy();
  });

  it("the Suggestions view is empty when it has neither findings nor jobs", () => {
    renderAt("/attention?view=suggestions", client([finding(1)], []));
    expect(screen.getByRole("heading", { name: "No suggestions" })).toBeTruthy();
  });

  describe("the pointer to suggestions", () => {
    it("shows the count and switches to the Suggestions view", () => {
      renderAt("/attention");
      fireEvent.click(screen.getByRole("button", { name: "1 suggestion" }));
      expect(where()).toBe("/attention?view=suggestions");
      // Not shown on the view it points to.
      expect(screen.queryByRole("button", { name: /^\d+ suggestions?$/ })).toBeNull();
    });

    it("is plural for more than one", () => {
      renderAt("/attention", client([finding(1), finding(3, { severity: "info" }), finding(6, { severity: "info" })]));
      expect(screen.getByRole("button", { name: "2 suggestions" })).toBeTruthy();
    });

    it("is absent at zero", () => {
      renderAt("/attention", client([finding(1)]));
      expect(screen.queryByRole("button", { name: /suggestion/ })).toBeNull();
    });

    it("is on the empty state, so a quiet badge is not read as nothing to do", () => {
      renderAt("/attention", client([finding(3, { severity: "info" })], []));
      expect(screen.getByRole("heading", { name: "Nothing needs attention" })).toBeTruthy();
      expect(screen.getByRole("button", { name: "1 suggestion" })).toBeTruthy();
    });

    it("is absent under a check filter", () => {
      renderAt("/attention?check=track-gaps");
      expect(screen.queryByRole("button", { name: /^\d+ suggestions?$/ })).toBeNull();
    });
  });

  it("says nothing needs attention when there is nothing to decide", () => {
    renderAt("/attention", client([finding(3, { severity: "info" })], [job("done", "acted_on")]));
    expect(screen.getByRole("heading", { name: "Nothing needs attention" })).toBeTruthy();
    expect(screen.getByRole("link", { name: /Needs attention/ }).textContent).not.toMatch(/\d/);
  });

  it("has no Check now and no run controls", () => {
    renderAt("/attention");
    expect(screen.queryByRole("button", { name: /Check now|Run now|Checking/ })).toBeNull();
  });

  it("disables Fix on a Missing album's finding and says why", () => {
    renderAt("/attention");
    const fixes = screen.getAllByRole("button", { name: /Fix/ }) as HTMLButtonElement[];
    const gone = fixes.find((b) => b.disabled)!;
    expect(gone.title).toMatch(/folder is missing/);
  });

  it("the badge is a lower bound when the findings list hit its limit", () => {
    expect(attentionBadgeText(7, false)).toBe("7");
    expect(attentionBadgeText(7, true)).toBe("7+");
    expect(FINDINGS_LIST_LIMIT).toBe(10_000);
  });
});

describe("redirects keep deep links", () => {
  it("/health goes to Needs attention", async () => {
    renderAt("/health");
    await waitFor(() => expect(where()).toBe("/attention"));
  });

  it("/health/:checkId lands filtered to that check, informational findings included", async () => {
    renderAt("/health/track-gaps");
    await waitFor(() => expect(where()).toBe("/attention?check=track-gaps"));
    expect(screen.getByText("gap 1")).toBeTruthy();
    expect(screen.getByText("just so you know")).toBeTruthy();
    expect(screen.queryByText("no cover")).toBeNull();
    expect(screen.queryByText("Album proposed")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: /Show everything/ }));
    expect(where()).toBe("/attention");
  });

  for (const from of ["/jobs?job=done", "/jobs/done"]) {
    it(`${from} shows that job even though finished jobs do not need attention`, async () => {
      renderAt(from);
      await waitFor(() => expect(where()).toBe("/attention?job=done"));
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
  it("returns to the same view and scroll position", async () => {
    renderAt("/attention?view=suggestions");
    scrollerEl().scrollTop = 120;
    fireEvent.scroll(scrollerEl());
    expect(sessionStorage.getItem("mm.attention.scroll.suggestions.")).toBe("120");
    // The album link on a row opens the library with that album's panel.
    fireEvent.click(screen.getAllByRole("link", { name: "View album detail" })[0]);
    expect(where()).toMatch(/^\/library\/present/);
    fireEvent.click(screen.getByRole("button", { name: "Back" }));
    await waitFor(() => expect(where()).toBe("/attention?view=suggestions"));
    expect(screen.getByRole("button", { name: /Suggestions/, pressed: true })).toBeTruthy();
    expect(scrollerEl().scrollTop).toBe(120);
  });

  it("a fresh visit starts at the top", () => {
    sessionStorage.setItem("mm.attention.scroll.decision.", "300");
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

  it("are not counted or on the default view; they can be listed and restored", async () => {
    const fetchMock = stubIgnored();
    renderAt("/attention");
    expect(screen.queryByText("ignored gap")).toBeNull();
    expect(screen.queryByRole("button", { name: /Show ignored/ })).toBeNull();
    expect(document.querySelector("[aria-live]")!.textContent).toBe("5 items");

    fireEvent.click(screen.getByRole("button", { name: /^Suggestions/ }));
    expect(screen.queryByText("ignored gap")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Show ignored" }));
    expect(await screen.findByText("ignored gap")).toBeTruthy();
    // Still not counted anywhere.
    expect(document.querySelector("[aria-live]")!.textContent).toBe("5 items");
    expect(screen.getByRole("link", { name: /Needs attention/ }).textContent).toContain("5");

    const row = screen.getByText("ignored gap").closest("div.px-3")!;
    fireEvent.click(within(row as HTMLElement).getByRole("button", { name: "Restore" }));
    await waitFor(() =>
      expect(fetchMock).toHaveBeenCalledWith(
        "/api/checks/track-gaps/findings/9/unignore",
        expect.objectContaining({ method: "POST" }),
      ),
    );
  });

  it("can be listed in a check-filtered view", async () => {
    stubIgnored();
    renderAt("/attention?check=track-gaps");
    fireEvent.click(screen.getByRole("button", { name: "Show ignored" }));
    expect(await screen.findByText("ignored gap")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Hide ignored" }));
    expect(screen.queryByText("ignored gap")).toBeNull();
  });
});

describe("long lists", () => {
  const many = Array.from({ length: 60 }, (_, i) => finding(100 + i, { summary: `many ${i}` }));

  it("caps a check's section at 50 rows with a Show all N button; counts stay true", () => {
    renderAt("/attention", client(many, []));
    expect(screen.getAllByText(/^many \d+$/)).toHaveLength(50);
    expect(document.querySelector("[aria-live]")!.textContent).toBe("60 items");
    expect(screen.getByRole("link", { name: /Needs attention/ }).textContent).toContain("60");
    expect(document.querySelector("#check-track-gaps")!.textContent).toContain("60");
    fireEvent.click(screen.getByRole("button", { name: "Show all 60" }));
    expect(screen.getAllByText(/^many \d+$/)).toHaveLength(60);
    expect(screen.queryByRole("button", { name: /Show all/ })).toBeNull();
  });

  it("a check-filtered view lists everything for that check", () => {
    renderAt("/attention?check=track-gaps", client(many, []));
    expect(screen.getAllByText(/^many \d+$/)).toHaveLength(60);
    expect(screen.queryByRole("button", { name: /Show all/ })).toBeNull();
  });
});
