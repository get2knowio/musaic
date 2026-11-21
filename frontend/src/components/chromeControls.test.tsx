/**
 * Titlebar, filter bar and sidebar controls (#197, #333): nothing that does
 * nothing, nothing shown twice. Search, sort and Tiles/List sit together in
 * the Library's filter bar; Settings is the sidebar's; the theme is under
 * View → Appearance; the key hints show the keys that are wired
 * (useGlobalShortcuts); the sidebar lists only real destinations and
 * lights one entry at a time; the theme choice is stored.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter, Route, Routes, useLocation } from "react-router-dom";

vi.mock("sonner", () => ({ toast: Object.assign(vi.fn(), { error: vi.fn() }) }));

import FilterBar from "@/components/FilterBar";
import Sidebar from "@/components/Sidebar";
import Titlebar from "@/components/Titlebar";
import { setTileColumns } from "@/hooks/useTileColumns";
import type { AppConfig, Job } from "@/lib/types";

function client(jobs: Partial<Job>[] = []) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
  qc.setQueryData(["config"], { library_dir: "/Users/someone/Music", exists: true } as AppConfig);
  qc.setQueryData(["ai-providers"], { selected: null, providers: [], uninstalled: [] });
  qc.setQueryData(["checks"], []);
  qc.setQueryData(["findings"], []);
  qc.setQueryData(["jobs"], jobs);
  qc.setQueryData(["albums-snapshot"], []);
  return qc;
}

beforeEach(() => {
  vi.stubGlobal("fetch", vi.fn(async () => new Response("{}", { status: 404 })));
  localStorage.clear();
  document.documentElement.classList.add("dark");
});
afterEach(() => {
  vi.unstubAllGlobals();
  setTileColumns(null);
});

function titlebarAt(url: string) {
  return render(
    <QueryClientProvider client={client()}>
      <MemoryRouter initialEntries={[url]}>
        <Titlebar />
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

function sidebarAt(url: string, jobs: Partial<Job>[] = []) {
  return render(
    <QueryClientProvider client={client(jobs)}>
      <MemoryRouter initialEntries={[url]}>
        <Sidebar />
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

function filterBarAt(url: string) {
  return render(
    <QueryClientProvider client={client()}>
      <MemoryRouter initialEntries={[url]}>
        <FilterBar totalCount={3} libraryDir="/Users/someone/Music" scannedAt={null} />
        <Routes>
          <Route path="*" element={<Where />} />
        </Routes>
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

function Where() {
  const l = useLocation();
  return <output data-testid="where">{l.pathname + l.search}</output>;
}

describe("titlebar", () => {
  it("carries no Library controls, no Settings and no theme button on any page (#333)", () => {
    for (const url of ["/library", "/review", "/settings"]) {
      const v = titlebarAt(url);
      expect(screen.queryByRole("searchbox")).toBeNull();
      expect(screen.queryByTitle("Tile view")).toBeNull();
      expect(screen.queryByRole("link", { name: "Settings" })).toBeNull();
      expect(screen.queryByTitle("Toggle theme")).toBeNull();
      expect(screen.getByRole("button", { name: /^Activity/ })).toBeInTheDocument();
      v.unmount();
    }
  });

  it("does not repeat the library folder (the Library filter bar shows it)", () => {
    titlebarAt("/library");
    expect(screen.queryByText(/Music/)).toBeNull();
    expect(screen.getByText("Library")).toBeInTheDocument();
  });

  it("stores the theme choice from View → Appearance and starts from it", async () => {
    titlebarAt("/library");
    fireEvent.pointerDown(screen.getByRole("button", { name: /View/ }), { button: 0, ctrlKey: false });
    const light = await screen.findByRole("menuitemradio", { name: "Light" });
    expect(screen.getByRole("menuitemradio", { name: "Dark" })).toHaveAttribute("aria-checked", "true");
    fireEvent.click(light);
    expect(document.documentElement.classList.contains("dark")).toBe(false);
    expect(localStorage.getItem("mm.theme")).toBe("light");

    document.documentElement.classList.add("dark");
    const { applyStoredTheme } = await import("@/lib/theme");
    applyStoredTheme();
    expect(document.documentElement.classList.contains("dark")).toBe(false);
  });
});

describe("library filter bar", () => {
  it("groups search, sort and Tiles/List above the collection", () => {
    filterBarAt("/library");
    const box = screen.getByRole("searchbox");
    expect(box).toHaveAttribute("placeholder", "Filter albums and artists…");
    expect(box.parentElement?.querySelector("kbd")).toHaveTextContent("/");
    expect(screen.getByRole("button", { name: /^Sort:/ })).toBeInTheDocument();
    expect(screen.getByTitle("Tile view")).toHaveAttribute("aria-pressed", "true");
  });

  it("Tiles/List runs the View command: the URL keeps the choice", () => {
    filterBarAt("/library?sort=year-desc");
    fireEvent.click(screen.getByTitle("List view"));
    expect(screen.getByTestId("where").textContent).toBe("/library?sort=year-desc&view=list");
    expect(screen.getByTitle("List view")).toHaveAttribute("aria-pressed", "true");
    fireEvent.click(screen.getByTitle("Tile view"));
    expect(screen.getByTestId("where").textContent).toBe("/library?sort=year-desc");
  });

  it("resets the tile size with a visible control", () => {
    setTileColumns(4);
    filterBarAt("/library");
    expect(screen.getByRole("slider")).toHaveAttribute("aria-label", expect.stringContaining("4"));
    fireEvent.click(screen.getByRole("button", { name: /Back to automatic tile size/ }));
    expect(screen.getByText("Auto")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Back to automatic tile size/ })).toBeNull();
  });

  it("hides the tile size in the list view", () => {
    filterBarAt("/library?view=list");
    expect(screen.queryByRole("slider")).toBeNull();
  });
});

describe("sidebar", () => {
  it("lists no unreleased destinations and hints the Settings key", () => {
    sidebarAt("/library");
    for (const gone of ["Coming soon", "Playlists", "Plex sync", "Scan history"]) {
      expect(screen.queryByText(gone)).toBeNull();
    }
    const settings = screen.getByRole("link", { name: /Settings/ });
    expect(settings).toHaveAttribute("aria-keyshortcuts", ",");
    expect(settings.querySelector("kbd")).toHaveTextContent(",");
  });

  it("lights one entry: the current collection under Library (All albums without a filter)", () => {
    const a = sidebarAt("/library");
    expect(screen.getByRole("link", { name: "Library" }).className).not.toContain("bg-accent-soft");
    expect(screen.getByRole("link", { name: /All albums/ })).toHaveAttribute("aria-current", "page");
    expect(screen.getByRole("link", { name: /All albums/ }).className).toContain("bg-accent-soft");
    a.unmount();

    sidebarAt("/library?filter=missing-art");
    expect(screen.getByRole("link", { name: "Library" }).className).not.toContain("bg-accent-soft");
    expect(screen.getByRole("link", { name: /All albums/ })).not.toHaveAttribute("aria-current");
    expect(screen.getByRole("link", { name: /Missing artwork/ }).className).toContain("bg-accent-soft");
  });

  it("leaves running jobs to the Activity control: Review's entry counts decisions only", () => {
    const jobs: Partial<Job>[] = [
      { id: "1", status: "running" },
      { id: "2", status: "running" },
      { id: "3", status: "queued" },
    ];
    sidebarAt("/library", jobs);
    const review = screen.getByRole("link", { name: /Review/ });
    expect(review.querySelector("[data-badge]")).toBeNull();
    expect(screen.queryByLabelText(/running/)).toBeNull();
    render(
      <QueryClientProvider client={client(jobs)}>
        <MemoryRouter initialEntries={["/library"]}>
          <Titlebar />
        </MemoryRouter>
      </QueryClientProvider>,
    );
    expect(screen.getByRole("button", { name: /^Activity: 2 jobs running/ })).toBeInTheDocument();
  });
});
