/**
 * The album list in narrow panes (#327): its columns follow the width of
 * the list itself (ResizeObserver), not the window. Album and artist always
 * show, side by side or stacked; length, then track count, then year go as
 * the list narrows. A missing folder is said in words in List and Tiles,
 * not only by colour or a tooltip.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, render, screen, within } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";

import AlbumList from "@/components/AlbumList";
import { FULL_LIST_LAYOUT, listLayout } from "@/lib/albumListLayout";
import TileGrid from "@/components/TileGrid";
import { makeAlbum } from "@/test/fixtures";

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
function setWidth(width: number) {
  act(() => {
    for (const o of observers) {
      o.cb(
        o.els.map((target) => ({ target, contentRect: { width } }) as unknown as ResizeObserverEntry),
        o as unknown as ResizeObserver,
      );
    }
  });
}

beforeEach(() => {
  observers.clear();
  vi.stubGlobal("ResizeObserver", FakeResizeObserver);
});
afterEach(() => {
  vi.unstubAllGlobals();
});

const LONG = "An Extraordinarily Long Album Title That Will Never Fit In A Narrow Pane (Deluxe Remaster)";
const ALBUMS = [
  makeAlbum({ id: "long", title: LONG, artist: "Someone", album_artist: "Someone" }),
  makeAlbum({ id: "gone", title: "Gone Album", missing_since: "2026-03-04T00:00:00Z" }),
];

function renderList() {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
  qc.setQueryData(["ai-providers"], { selected: null, providers: [], uninstalled: [] });
  return render(
    <QueryClientProvider client={qc}>
      <MemoryRouter initialEntries={["/library?view=list"]}>
        <AlbumList albums={ALBUMS} />
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

const header = () =>
  [...document.querySelector("[data-list-layout] > div > div")!.children].map((c) => c.textContent);
const layout = () => document.querySelector("[data-list-layout]")!.getAttribute("data-list-layout");

describe("listLayout", () => {
  it("drops length, then tracks, then stacks, then drops year", () => {
    expect(listLayout(null)).toEqual(FULL_LIST_LAYOUT);
    expect(listLayout(900)).toEqual(FULL_LIST_LAYOUT);
    expect(listLayout(600)).toEqual({ stacked: false, year: true, tracks: true, length: false });
    expect(listLayout(500)).toEqual({ stacked: false, year: true, tracks: false, length: false });
    expect(listLayout(400)).toEqual({ stacked: true, year: true, tracks: false, length: false });
    expect(listLayout(320)).toEqual({ stacked: true, year: false, tracks: false, length: false });
  });
});

describe("the album list follows its own width", () => {
  it("shows every column when wide, and keeps album and artist at every width", () => {
    renderList();
    setWidth(1000);
    expect(layout()).toBe("columns+year+tracks+length");
    expect(header()).toEqual(["", "Album", "Artist", "Year", "Tracks", "Length"]);

    setWidth(560);
    expect(header()).toEqual(["", "Album", "Artist", "Year", "Tracks"]);

    setWidth(480);
    expect(header()).toEqual(["", "Album", "Artist", "Year"]);

    setWidth(400);
    expect(layout()).toBe("stacked+year");
    expect(header()).toEqual(["", "Album · Artist", "Year"]);

    setWidth(320);
    expect(layout()).toBe("stacked");
    expect(header()).toEqual(["", "Album · Artist"]);

    // Album identity is still there, its full text on hover for long titles.
    const row = document.querySelector<HTMLElement>('[data-album-id="long"]')!;
    expect(within(row).getByText(LONG)).toHaveAttribute("title", LONG);
    expect(within(row).getByText("Someone")).toBeTruthy();
    // Every row lines up with the header.
    expect(row.style.gridTemplateColumns).toBe("40px minmax(0px, 1fr)");
  });
});

describe("a missing folder", () => {
  it("is named in words in List, at any width", () => {
    renderList();
    for (const w of [1000, 320]) {
      setWidth(w);
      const row = document.querySelector<HTMLElement>('[data-album-id="gone"]')!;
      const badge = row.querySelector("[data-missing-folder]")!;
      expect(badge).toHaveTextContent(/^Folder missing, not seen on disk since /);
      expect(document.querySelector('[data-album-id="long"] [data-missing-folder]')).toBeNull();
    }
  });

  it("and in Tiles", () => {
    const qc = new QueryClient();
    qc.setQueryData(["ai-providers"], { selected: null, providers: [], uninstalled: [] });
    render(
      <QueryClientProvider client={qc}>
        <MemoryRouter initialEntries={["/library"]}>
          <TileGrid albums={ALBUMS} />
        </MemoryRouter>
      </QueryClientProvider>,
    );
    const tile = document.querySelector<HTMLElement>('[data-album-id="gone"]')!;
    expect(within(tile).getByText("Folder missing")).toBeTruthy();
    expect(screen.getAllByText(/not seen on disk since/).length).toBe(1);
  });
});
