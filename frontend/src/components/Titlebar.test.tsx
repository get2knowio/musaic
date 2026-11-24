/**
 * The titlebar and the Library filter bar must not make the page wider
 * than the window (the desktop window goes down to 720 px; at 1280 px the
 * old fixed-width items already overflowed, #201). jsdom has no layout, so
 * this pins the classes that make them shrink: the header may shrink below
 * its content, the location crumb and the search box shrink, and
 * lower-priority controls hide below `lg`.
 */
import { afterEach, describe, expect, it, vi } from "vitest";
import { render, screen, within } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";

vi.mock("sonner", () => ({ toast: Object.assign(vi.fn(), { error: vi.fn() }) }));

import FilterBar from "@/components/FilterBar";
import Titlebar from "@/components/Titlebar";
import { setScanning } from "@/lib/rescanController";
import type { AppConfig } from "@/lib/types";

afterEach(() => {
  setScanning(false);
  vi.unstubAllGlobals();
});

function renderChrome() {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
  qc.setQueryData(["config"], {
    library_dir: "/Users/someone/Music/A very long library folder name that will not fit/FLAC",
    exists: true,
  } as AppConfig);
  qc.setQueryData(["albums-snapshot"], []);
  vi.stubGlobal("fetch", vi.fn(async () => new Response("{}", { status: 404 })));
  return render(
    <QueryClientProvider client={qc}>
      <MemoryRouter initialEntries={["/library"]}>
        <Titlebar />
        <FilterBar
          totalCount={120}
          libraryDir="/Users/someone/Music/A very long library folder name that will not fit/FLAC"
          scannedAt={null}
        />
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

describe("Rescan while a scan runs", () => {
  it("keeps its name and is disabled, so only the Activity control says Scanning", () => {
    setScanning(true);
    renderChrome();
    const header = within(screen.getByRole("banner"));
    const rescan = header.getByRole("button", { name: "Rescan" });
    expect(rescan).toBeDisabled();
    expect(within(rescan).getByText("Rescan")).toBeInTheDocument();
    expect(header.queryByText("Scanning…")).toBeNull();
  });
});

describe("chrome at narrow widths", () => {
  it("the titlebar can shrink below its content instead of widening the page", () => {
    renderChrome();
    const header = screen.getByRole("banner");
    expect(header).toHaveClass("min-w-0", "overflow-hidden");
  });

  it("shrinks the crumb and the search box first", () => {
    renderChrome();
    const crumb = within(screen.getByRole("banner")).getByText("Library");
    expect(crumb).toHaveClass("truncate");
    expect(crumb.parentElement).toHaveClass("min-w-0", "truncate");
    const search = screen.getByRole("searchbox");
    expect(search).toHaveClass("min-w-0");
    expect(search.parentElement).toHaveClass("shrink", "min-w-28");
    expect(search.parentElement).not.toHaveClass("shrink-0");
  });

  it("hides the tile-size slider, the folder and the Rescan label when narrow; the button keeps its name", () => {
    renderChrome();
    const slider = screen.getByRole("slider");
    expect(slider.parentElement).toHaveClass("hidden", "lg:inline-flex");
    const rescan = screen.getByRole("button", { name: "Rescan" });
    expect(within(rescan).getByText("Rescan")).toHaveClass("hidden", "lg:inline");
    const folder = screen.getByTitle(/A very long library folder/);
    expect(folder.parentElement).toHaveClass("hidden", "xl:contents");
  });
});
