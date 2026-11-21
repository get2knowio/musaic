/**
 * Library browsing (#326): Library resumes the last browsing state; All
 * albums is a true reset (no collection, no search, no open inspector; sort
 * and tiles/list kept) from any page; the collections are nested under
 * Library with their names and counts; empty results offer Clear search,
 * Clear filter and Show all albums; an album opened from a Review list says
 * where closing leads ("Back to Track gaps") and returns to that list's
 * filter, row (marked and focused) and scroll position, while a direct
 * album link closes to the Library.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { Link, MemoryRouter, Route, Routes, useLocation } from "react-router-dom";

vi.mock("sonner", () => ({
  toast: Object.assign(vi.fn(), { error: vi.fn(), success: vi.fn(), warning: vi.fn() }),
}));

import LibraryPage from "@/pages/Library";
import ReviewPage from "@/pages/Review";
import Sidebar from "@/components/Sidebar";
import { useTrackLibraryUrl } from "@/hooks/useRememberedLibraryUrl";
import {
  allAlbumsUrl,
  clearFilterUrl,
  clearSearchUrl,
  collectionName,
  collectionOf,
  collectionUrl,
} from "@/lib/libraryLinks";
import { reviewViewName } from "@/lib/reviewLinks";
import { makeAlbum } from "@/test/fixtures";
import type { Album, AppConfig, CheckSummary, Finding } from "@/lib/types";

const ALBUMS: Album[] = [
  makeAlbum({ id: "a1", title: "First Album" }),
  makeAlbum({ id: "a2", title: "Second Album", has_embedded_cover: false, cover_file: null }),
  makeAlbum({ id: "a3", title: "Third Album", is_compilation: true }),
  makeAlbum({ id: "a4", title: "Fourth Album", missing_since: "2026-01-02T00:00:00Z" }),
];

const CHECKS: CheckSummary[] = [
  {
    meta: {
      id: "track-gaps",
      label: "Track gaps",
      description: "",
      category: "metadata",
      cost: "cheap",
      runs_during_scan: true,
      fix_kind: "none",
      bulk_action_label: null,
    },
    open_count: 2,
    last_run: null,
  },
];

const finding = (id: number, album: string): Finding => ({
  id,
  check_id: "track-gaps",
  album_id: album,
  severity: "warn",
  summary: `gap ${id}`,
  detail: null,
  first_seen_at: "2026-01-01T00:00:00Z",
  last_seen_at: "2026-01-01T00:00:00Z",
  resolved_at: null,
  ignored_at: null,
  ignored_reason: null,
});

function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
}

beforeEach(() => {
  sessionStorage.clear();
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string) => {
      if (String(url).startsWith("/api/albums?") || url === "/api/albums") {
        return json({ albums: ALBUMS, count: ALBUMS.length, library_dir: "/music", scanned_at: null });
      }
      return json({ detail: "not found" }, 404);
    }),
  );
});
afterEach(() => {
  vi.unstubAllGlobals();
});

function client(): QueryClient {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
  qc.setQueryData(["config"], { library_dir: "/music", exists: true } as AppConfig);
  qc.setQueryData(["ai-providers"], { selected: null, providers: [], uninstalled: [] });
  qc.setQueryData(["jobs"], []);
  qc.setQueryData(["checks"], CHECKS);
  qc.setQueryData(["findings"], [finding(1, "a1"), finding(2, "a2")]);
  qc.setQueryData(["albums-snapshot"], ALBUMS);
  return qc;
}

function Where() {
  const l = useLocation();
  return <div data-testid="where">{l.pathname + l.search}</div>;
}

function Shell() {
  useTrackLibraryUrl();
  return (
    <>
      <Sidebar />
      <Where />
      <main>
        <Routes>
          <Route path="/library" element={<LibraryPage />}>
            <Route path=":albumId" element={null} />
          </Route>
          <Route path="/review" element={<ReviewPage />} />
          <Route path="/settings" element={<Link to="/library/a2">open from settings</Link>} />
        </Routes>
      </main>
    </>
  );
}

function renderAt(url: string) {
  return render(
    <QueryClientProvider client={client()}>
      <MemoryRouter initialEntries={[url]}>
        <Shell />
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

const where = () => screen.getByTestId("where").textContent;
const collections = () => screen.getByRole("list", { name: "Library collections" });
const collection = (name: RegExp) => within(collections()).getByRole("link", { name });
const panel = () => screen.queryByRole("complementary");
/** The Library has its albums (they arrive from the scan stream's first load). */
const loaded = () => waitFor(() => expect(document.querySelectorAll("main [data-album-id]").length).toBeGreaterThan(0));
/** The no-results message and its actions. */
const emptyState = (title: string) => screen.getByRole("heading", { name: title }).parentElement!;

describe("library links", () => {
  it("All albums keeps only the sort and the view", () => {
    expect(allAlbumsUrl("/library/a1?filter=missing&q=abc&sort=year-desc&view=list")).toBe(
      "/library?sort=year-desc&view=list",
    );
    expect(allAlbumsUrl()).toBe("/library");
  });

  it("a collection keeps the search and preferences and closes the inspector", () => {
    expect(collectionUrl("missing-art", "/library/a1?q=abc&view=list")).toBe(
      "/library?q=abc&view=list&filter=missing-art",
    );
    expect(collectionUrl("all", "/library?filter=missing&q=x&sort=title-asc")).toBe("/library?sort=title-asc");
  });

  it("clear search and clear filter each drop one thing", () => {
    expect(clearSearchUrl("/library?filter=missing&q=x&view=list")).toBe("/library?filter=missing&view=list");
    expect(clearFilterUrl("/library?filter=missing&q=x&view=list")).toBe("/library?q=x&view=list");
  });

  it("names the collections explicitly", () => {
    expect(["all", "missing-art", "missing", "compilations"].map((c) => collectionName(c as never))).toEqual([
      "All albums",
      "Missing artwork",
      "Missing folders",
      "Compilations",
    ]);
    expect(collectionOf("/library?filter=missing")).toBe("missing");
    expect(collectionOf("?filter=bogus")).toBe("all");
  });

  it("names a Review view for its return link", () => {
    const label = (id: string) => (id === "track-gaps" ? "Track gaps" : id);
    expect(reviewViewName("?check=track-gaps&album=a1", label)).toBe("Track gaps");
    expect(reviewViewName("?kind=failed", label)).toBe("Failed jobs");
    expect(reviewViewName("?show=ignored", label)).toBe("Ignored findings");
    expect(reviewViewName("", label)).toBeNull();
  });
});

describe("collections in the Sidebar", () => {
  it("are nested under Library, named, counted in albums, and the current one is marked", async () => {
    renderAt("/library?filter=missing");
    await loaded();
    const rows = within(collections())
      .getAllByRole("link")
      .map((l) => l.textContent);
    expect(rows).toEqual([
      "All albums4 albums",
      "Missing artwork1 album",
      "Missing folders1 album",
      "Compilations1 album",
    ]);
    expect(collection(/Missing folders/)).toHaveAttribute("aria-current", "page");
    // The page header names the collection and its count too.
    expect(document.querySelector("[data-collection-name]")).toHaveTextContent("Missing folders");
    expect(document.querySelector("[data-collection-count]")).toHaveTextContent("1 of 4 albums");
  });
});

describe("All albums is a true reset", () => {
  it("clears the collection, the search and the open inspector; keeps sort and view", async () => {
    renderAt("/library/a2?filter=missing-art&q=second&sort=year-desc&view=list");
    await loaded();
    expect(panel()).not.toBeNull();
    fireEvent.click(collection(/All albums/));
    expect(where()).toBe("/library?sort=year-desc&view=list");
    expect(panel()).toBeNull();
    expect(document.querySelector("[data-collection-name]")).toHaveTextContent("All albums");
    await waitFor(() => expect(document.querySelectorAll("main [data-album-id]").length).toBe(4));
  });

  it("from another page as well", () => {
    renderAt("/library?filter=compilations&q=third&view=list");
    fireEvent.click(screen.getByRole("link", { name: /Review/ }));
    expect(where()).toBe("/review");
    fireEvent.click(collection(/All albums/));
    expect(where()).toBe("/library?view=list");
  });

  it("a collection keeps the search and closes the inspector", () => {
    renderAt("/library/a1?q=album&view=list");
    fireEvent.click(collection(/Compilations/));
    expect(where()).toBe("/library?q=album&view=list&filter=compilations");
  });
});

describe("Library resumes the last browsing state", () => {
  it("returns to the collection, search, open album and view it left", async () => {
    renderAt("/library/a3?filter=compilations&q=third&view=list");
    fireEvent.click(screen.getByRole("link", { name: /Review/ }));
    expect(where()).toBe("/review");
    fireEvent.click(screen.getByRole("link", { name: "Library" }));
    expect(where()).toBe("/library/a3?filter=compilations&q=third&view=list");
    await loaded();
    expect(panel()).not.toBeNull();
    // A return to the library, not an album opened from Review: closing
    // stays in the Library.
    expect(screen.queryByRole("button", { name: /^Back to/ })).toBeNull();
  });
});

describe("no results", () => {
  it("a search inside a collection offers Clear search, Clear filter and Show all albums", () => {
    renderAt("/library?filter=compilations&q=zzz&view=list");
    expect(screen.getByRole("heading", { name: 'Nothing in Compilations matches "zzz"' })).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Clear search" }));
    expect(where()).toBe("/library?filter=compilations&view=list");
  });

  it("Clear filter keeps the search", async () => {
    renderAt("/library?filter=compilations&q=first");
    const empty = emptyState('Nothing in Compilations matches "first"');
    fireEvent.click(within(empty).getByRole("button", { name: "Clear filter" }));
    expect(where()).toBe("/library?q=first");
    await loaded();
    expect(screen.getAllByText("First Album").length).toBeGreaterThan(0);
  });

  it("a search alone offers Clear search and Show all albums, no Clear filter", () => {
    renderAt("/library?q=zzz&sort=title-asc");
    expect(screen.queryByRole("button", { name: "Clear filter" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Show all albums" }));
    expect(where()).toBe("/library?sort=title-asc");
  });

  it("an empty collection offers Show all albums", () => {
    renderAt("/library?filter=needs-review");
    expect(screen.getByRole("heading", { name: "No albums in Jobs to review" })).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Clear search" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Show all albums" }));
    expect(where()).toBe("/library");
  });
});

describe("returning from an album", () => {
  const scroller = () => document.querySelector('main div[tabindex="-1"]') as HTMLElement;
  const reviewRows = () => [...document.querySelectorAll<HTMLElement>("[data-review-item]")];

  it("opened from a filtered Review list: Back to Track gaps restores the filter, row, focus and scroll", async () => {
    renderAt("/review?check=track-gaps");
    scroller().scrollTop = 80;
    fireEvent.scroll(scroller());
    const second = reviewRows()[1];
    fireEvent.click(within(second).getByRole("link", { name: "View album detail" }));
    expect(where()).toBe("/library/a2");
    await loaded();
    expect(panel()).not.toBeNull();

    const back = await screen.findByRole("button", { name: "Back to Track gaps" });
    fireEvent.click(back);
    await waitFor(() => expect(where()).toBe("/review?check=track-gaps"));
    expect(screen.getByRole("heading", { name: "Track gaps" })).toBeTruthy();
    expect(scroller().scrollTop).toBe(80);
    const marked = reviewRows().filter((r) => r.dataset.current === "true");
    expect(marked.map((r) => r.dataset.reviewItem)).toEqual(["finding:2"]);
    await waitFor(() => expect(marked[0].contains(document.activeElement)).toBe(true));
  });

  it("opened from All open: Back to Review", async () => {
    renderAt("/review");
    fireEvent.click(within(reviewRows()[0]).getByRole("link", { name: "View album detail" }));
    expect(await screen.findByRole("button", { name: "Back to Review" })).toBeTruthy();
  });

  it("opened from Settings: Back to Settings", async () => {
    renderAt("/settings");
    fireEvent.click(screen.getByRole("link", { name: "open from settings" }));
    fireEvent.click(await screen.findByRole("button", { name: "Back to Settings" }));
    expect(where()).toBe("/settings");
  });

  it("a direct album link has no return action and closes to the Library", async () => {
    renderAt("/library/a1?view=list");
    await loaded();
    expect(screen.queryByRole("button", { name: /^Back to/ })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Close album panel" }));
    expect(where()).toBe("/library?view=list");
  });
});
