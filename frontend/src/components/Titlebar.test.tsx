/**
 * The titlebar must not make the page wider than the window (the desktop
 * window goes down to 720 px; at 1280 px the old fixed-width items already
 * overflowed, #201). jsdom has no layout, so this pins the classes that make
 * it shrink: the header may shrink below its content, the location crumb and
 * the search box shrink, and lower-priority controls hide below `lg`.
 */
import { afterEach, describe, expect, it, vi } from "vitest";
import { render, screen, within } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";

vi.mock("sonner", () => ({ toast: Object.assign(vi.fn(), { error: vi.fn() }) }));

import Titlebar from "@/components/Titlebar";
import type { AppConfig } from "@/lib/types";

afterEach(() => {
  vi.unstubAllGlobals();
});

function renderTitlebar() {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
  qc.setQueryData(["config"], {
    library_dir: "/Users/someone/Music/A very long library folder name that will not fit/FLAC",
    exists: true,
  } as AppConfig);
  qc.setQueryData(["ai-providers"], { selected: null, providers: [], uninstalled: [] });
  vi.stubGlobal("fetch", vi.fn(async () => new Response("{}", { status: 404 })));
  return render(
    <QueryClientProvider client={qc}>
      <MemoryRouter initialEntries={["/library"]}>
        <Titlebar />
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

describe("titlebar at narrow widths", () => {
  it("can shrink below its content instead of widening the page", () => {
    renderTitlebar();
    const header = screen.getByRole("banner");
    expect(header).toHaveClass("min-w-0", "overflow-hidden");
  });

  it("shrinks the crumb and the search box first", () => {
    renderTitlebar();
    const crumb = screen.getByTitle(/A very long library folder name/);
    expect(crumb).toHaveClass("truncate");
    expect(crumb.parentElement).toHaveClass("min-w-0", "truncate");
    const search = screen.getByRole("searchbox");
    expect(search).toHaveClass("min-w-0");
    expect(search.parentElement).toHaveClass("shrink", "min-w-32");
    expect(search.parentElement).not.toHaveClass("shrink-0");
  });

  it("hides the tile-size slider and the Rescan label below lg; the button keeps its name", () => {
    renderTitlebar();
    const slider = screen.getByRole("slider");
    expect(slider.parentElement).toHaveClass("hidden", "lg:inline-flex");
    const rescan = screen.getByRole("button", { name: "Rescan" });
    expect(within(rescan).getByText("Rescan")).toHaveClass("hidden", "lg:inline");
    expect(screen.getByRole("link", { name: "Settings" })).toHaveClass("shrink-0");
  });
});
