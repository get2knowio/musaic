/**
 * The album panel docked beside the library grid (#190): opening an album
 * pushes a history entry, swapping albums replaces it; closing goes back to
 * the page the album was opened from, else to the library; Esc closes only
 * from inside the panel; focus moves to the panel on keyboard opens and back
 * to the tile on close; F6 moves between grid and panel; the resize handle
 * is a keyboard-operable separator.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { Link, MemoryRouter, Route, Routes, useLocation, useNavigationType } from "react-router-dom";

vi.mock("sonner", () => ({
  toast: Object.assign(vi.fn(), { error: vi.fn(), success: vi.fn(), warning: vi.fn() }),
}));
vi.mock("@/lib/sse", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/lib/sse")>()),
  postSse: vi.fn(),
}));

/** ResizeObserver stand-in: every observed element reports `rowWidth`. */
const observers = new Set<{ cb: ResizeObserverCallback; els: Element[] }>();
class FakeResizeObserver {
  private entry: { cb: ResizeObserverCallback; els: Element[] };
  constructor(cb: ResizeObserverCallback) {
    this.entry = { cb, els: [] };
    observers.add(this.entry);
  }
  observe(el: Element) {
    this.entry.els.push(el);
  }
  unobserve() {}
  disconnect() {
    observers.delete(this.entry);
  }
}
/** Pretend the row the panel docks in is now `width` px wide. */
function setRowWidth(width: number) {
  act(() => {
    for (const o of observers) {
      o.cb(
        o.els.map((target) => ({ target, contentRect: { width } }) as unknown as ResizeObserverEntry),
        o as unknown as ResizeObserver,
      );
    }
  });
}

import LibraryPage from "@/pages/Library";
import { useTrackLibraryUrl } from "@/hooks/useRememberedLibraryUrl";
import { postSse } from "@/lib/sse";
import { resetApplyRegistryForTests, startApply } from "@/lib/applyRegistry";
import { makeAlbum } from "@/test/fixtures";
import type { Album, AppConfig } from "@/lib/types";

const ALBUMS: Album[] = [
  makeAlbum({ id: "a1", title: "First Album", dir_name: "Artist - First Album [FLAC]" }),
  makeAlbum({ id: "a2", title: "Second Album", dir_name: "Artist - Second Album [FLAC]" }),
];

function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
}

beforeEach(() => {
  observers.clear();
  resetApplyRegistryForTests();
  vi.stubGlobal("ResizeObserver", FakeResizeObserver);
  sessionStorage.clear();
  try { window.localStorage?.clear(); } catch { /* unavailable under some Node versions */ }
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
  qc.setQueryData(["albums-snapshot"], ALBUMS);
  return qc;
}

/** Shows where the router is, how it got there, and the entry's state. */
function Probe() {
  const location = useLocation();
  const type = useNavigationType();
  return (
    <div data-testid="probe">
      {`${type} ${location.pathname}${location.search} ${JSON.stringify(location.state ?? null)}`}
    </div>
  );
}

function Shell() {
  useTrackLibraryUrl();
  return (
    <>
      <Probe />
      <main>
        <Routes>
          <Route path="/library" element={<LibraryPage />}>
            <Route path=":albumId" element={null} />
          </Route>
          <Route
            path="/health"
            element={<Link to="/library/a1">open from health</Link>}
          />
        </Routes>
      </main>
    </>
  );
}

function renderAt(entry: string | { pathname: string; search?: string; state?: unknown }) {
  return render(
    <QueryClientProvider client={client()}>
      <MemoryRouter initialEntries={[entry]}>
        <Shell />
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

const probe = () => screen.getByTestId("probe").textContent;
const tile = (id: string) =>
  document.querySelector<HTMLElement>(`main [data-album-id="${id}"]`)!;
const panel = () => screen.queryByRole("complementary");
const mouseClick = (el: HTMLElement) => fireEvent.click(el, { detail: 1 });
/** A click synthesised from Enter on a link carries detail 0. */
const keyboardClick = (el: HTMLElement) => fireEvent.click(el, { detail: 0 });

async function tilesReady() {
  await waitFor(() => expect(tile("a1")).toBeTruthy());
}

describe("album panel routing", () => {
  it("pushes when opening, replaces when swapping, and keeps location.state", async () => {
    renderAt({ pathname: "/library", search: "?q=album", state: { from: "x" } });
    await tilesReady();

    mouseClick(tile("a1"));
    expect(probe()).toBe('PUSH /library/a1?q=album {"from":"x"}');
    expect(panel()).toHaveAccessibleName("First Album");

    mouseClick(tile("a2"));
    expect(probe()).toBe('REPLACE /library/a2?q=album {"from":"x"}');
    expect(panel()).toHaveAccessibleName("Second Album");
  });

  it("closes to the library with the filters when opened from the grid", async () => {
    renderAt("/library?q=album");
    await tilesReady();
    mouseClick(tile("a1"));
    fireEvent.click(screen.getByRole("button", { name: "Close album panel" }));
    expect(probe()).toBe("PUSH /library?q=album null");
    expect(panel()).toBeNull();
  });

  it("closes back to the page the album was opened from, after swapping albums", async () => {
    renderAt("/health");
    fireEvent.click(screen.getByText("open from health"));
    await waitFor(() => expect(panel()).toHaveAccessibleName("First Album"));

    mouseClick(tile("a2"));
    expect(probe()).toMatch(/^REPLACE \/library\/a2/);

    fireEvent.click(screen.getByRole("button", { name: "Close album panel" }));
    await waitFor(() => expect(probe()).toBe("POP /health null"));
    // Leaving the page: focus goes to <main>, not to a vanished tile.
    expect(document.activeElement).toBe(document.querySelector("main"));
  });

  it("a deep link with no origin closes to the library", async () => {
    renderAt("/library/a2");
    await waitFor(() => expect(panel()).toHaveAccessibleName("Second Album"));
    fireEvent.click(screen.getByRole("button", { name: "Close album panel" }));
    expect(probe()).toBe("PUSH /library null");
  });

  it("marks the open album's tile as current", async () => {
    renderAt("/library/a1");
    await tilesReady();
    expect(tile("a1")).toHaveAttribute("aria-current", "true");
    expect(tile("a2")).not.toHaveAttribute("aria-current");
  });
});

describe("album panel keyboard and focus", () => {
  it("Esc closes only when focus is inside the panel", async () => {
    renderAt("/library");
    await tilesReady();
    mouseClick(tile("a1"));

    tile("a1").focus();
    fireEvent.keyDown(tile("a1"), { key: "Escape" });
    expect(panel()).not.toBeNull();

    const close = screen.getByRole("button", { name: "Close album panel" });
    close.focus();
    fireEvent.keyDown(close, { key: "Escape" });
    expect(panel()).toBeNull();
    expect(document.activeElement).toBe(tile("a1"));
  });

  it("Esc already used by something inside (a menu, a dialog) does not close", async () => {
    renderAt("/library/a1");
    await waitFor(() => expect(panel()).not.toBeNull());
    // What Radix's dismissable layers do: handle Esc on the document in
    // the capture phase and mark it handled.
    const consume = (e: KeyboardEvent) => e.preventDefault();
    document.addEventListener("keydown", consume, true);
    try {
      const close = screen.getByRole("button", { name: "Close album panel" });
      fireEvent.keyDown(close, { key: "Escape" });
      expect(panel()).not.toBeNull();
    } finally {
      document.removeEventListener("keydown", consume, true);
    }
  });

  it("a keyboard open focuses the heading; a mouse open leaves focus alone", async () => {
    renderAt("/library");
    await tilesReady();

    tile("a1").focus();
    mouseClick(tile("a1"));
    expect(document.activeElement).toBe(tile("a1"));

    keyboardClick(tile("a2"));
    await waitFor(() =>
      expect(document.activeElement).toBe(within(panel()!).getByRole("heading", { name: "Second Album" })),
    );

    // Enter on the tile of the album already shown also lands in the panel.
    tile("a2").focus();
    keyboardClick(tile("a2"));
    expect(document.activeElement).toBe(within(panel()!).getByRole("heading", { name: "Second Album" }));
  });

  it("F6 moves focus between the grid and the panel", async () => {
    renderAt("/library");
    await tilesReady();
    mouseClick(tile("a2"));
    tile("a1").focus();

    fireEvent.keyDown(window, { key: "F6" });
    expect(document.activeElement).toBe(within(panel()!).getByRole("heading"));

    fireEvent.keyDown(window, { key: "F6" });
    expect(document.activeElement).toBe(tile("a2"));
  });

  it("the resize handle is a keyboard-operable separator", async () => {
    renderAt("/library/a1");
    await waitFor(() => expect(panel()).not.toBeNull());
    const sep = screen.getByRole("separator", { name: "Resize album panel" });
    expect(sep).toHaveAttribute("tabindex", "0");
    expect(sep).toHaveAttribute("aria-valuenow", "480");
    expect(sep).toHaveAttribute("aria-valuemin", "360");
    const max = Number(sep.getAttribute("aria-valuemax"));
    expect(max).toBeGreaterThan(480);

    fireEvent.keyDown(sep, { key: "ArrowLeft" });
    expect(sep).toHaveAttribute("aria-valuenow", "496");
    fireEvent.keyDown(sep, { key: "ArrowRight" });
    fireEvent.keyDown(sep, { key: "ArrowRight" });
    expect(sep).toHaveAttribute("aria-valuenow", "464");
    fireEvent.keyDown(sep, { key: "Home" });
    expect(sep).toHaveAttribute("aria-valuenow", "360");
    fireEvent.keyDown(sep, { key: "End" });
    expect(sep).toHaveAttribute("aria-valuenow", String(max));
    // Arrow keys on the handle are not an Esc; the panel stays.
    expect(panel()).not.toBeNull();

    fireEvent.doubleClick(sep);
    expect(sep).toHaveAttribute("aria-valuenow", "480");
    expect(panel()).toHaveStyle({ width: "480px" });
  });

  it("the panel is not modal: the grid stays reachable", async () => {
    renderAt("/library/a1");
    await waitFor(() => expect(panel()).not.toBeNull());
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(tile("a2").closest("[aria-hidden='true']")).toBeNull();
    await act(async () => {});
  });
});

describe("album panel at narrow widths", () => {
  const grid = () => tile("a1").closest<HTMLElement>("[tabindex='-1']")!;
  const separator = () => screen.queryByRole("separator", { name: "Resize album panel" });

  it("docks beside the grid when both minimums fit, and the grid keeps 320 px", async () => {
    renderAt("/library/a1");
    await tilesReady();
    setRowWidth(1000);
    expect(panel()).not.toHaveAttribute("data-fills-row");
    expect(separator()).toHaveAttribute("aria-valuemax", "680");
    expect(grid()).not.toHaveAttribute("inert");
  });

  it("covers the row when it cannot fit 320 + 360 px; closing brings the grid back as it was", async () => {
    renderAt("/library");
    await tilesReady();
    setRowWidth(600);
    grid().scrollTop = 123;
    mouseClick(tile("a2"));

    expect(panel()).toHaveAttribute("data-fills-row");
    expect(panel()!.style.width).toBe("");
    expect(separator()).toBeNull();
    // Hidden, not unmounted: the tiles and the scroll position stay.
    expect(grid()).toHaveAttribute("inert");
    expect(tile("a2")).toHaveAttribute("aria-current", "true");
    expect(grid().scrollTop).toBe(123);
    // Even a mouse open moves focus in: the tile it was on is now inert.
    expect(document.activeElement).toBe(within(panel()!).getByRole("heading"));

    // F6 has nothing to move to.
    const close = screen.getByRole("button", { name: "Close album panel" });
    close.focus();
    fireEvent.keyDown(window, { key: "F6" });
    expect(document.activeElement).toBe(close);

    fireEvent.click(close);
    expect(panel()).toBeNull();
    expect(grid()).not.toHaveAttribute("inert");
    expect(grid().scrollTop).toBe(123);
    expect(document.activeElement).toBe(tile("a2"));
  });

  it("switches modes on resize without losing the album or its apply progress", async () => {
    vi.mocked(postSse).mockImplementation(async (_url, _body, events) => {
      events.start?.({ file_count: 2 });
      await new Promise(() => {}); // still writing
    });
    renderAt("/library/a1");
    await tilesReady();
    setRowWidth(1000);
    act(() => {
      void startApply("a1", { suggestions: {} } as never);
    });
    await screen.findByText(/Writing FLAC tags/);
    const aside = panel();

    tile("a2").focus();
    setRowWidth(600);
    expect(panel()).toBe(aside);
    // The grid went inert under the panel; focus moved off it.
    expect(document.activeElement).toBe(within(panel()!).getByRole("heading"));
    expect(panel()).toHaveAttribute("data-fills-row");
    expect(panel()).toHaveAccessibleName("First Album");
    expect(screen.getByText(/Writing FLAC tags/)).toBeInTheDocument();

    setRowWidth(1000);
    expect(panel()).toBe(aside);
    expect(panel()).not.toHaveAttribute("data-fills-row");
    expect(separator()).not.toBeNull();
    expect(screen.getByText(/Writing FLAC tags/)).toBeInTheDocument();
  });
});
