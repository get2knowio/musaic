/**
 * Titlebar and sidebar controls (#197): nothing that does nothing, nothing
 * shown twice. Search and the view toggle belong to Library only; the key
 * hints show the keys that are wired (useGlobalShortcuts); the sidebar lists only real
 * destinations and lights one entry at a time; the theme choice is stored.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";

vi.mock("sonner", () => ({ toast: Object.assign(vi.fn(), { error: vi.fn() }) }));

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

describe("titlebar", () => {
  it("shows search and the view toggle on Library only", () => {
    const lib = titlebarAt("/library");
    expect(screen.getByRole("searchbox")).toBeInTheDocument();
    expect(screen.getByTitle("Tile view")).toBeInTheDocument();
    lib.unmount();
    for (const url of ["/review", "/settings"]) {
      const v = titlebarAt(url);
      expect(screen.queryByRole("searchbox")).toBeNull();
      expect(screen.queryByTitle("Tile view")).toBeNull();
      expect(screen.queryByTitle("List view")).toBeNull();
      v.unmount();
    }
  });

  it("promises only what the search does, and shows the wired key hint", () => {
    titlebarAt("/library");
    const box = screen.getByRole("searchbox");
    expect(box).toHaveAttribute("placeholder", "Filter albums and artists…");
    expect(box.parentElement?.querySelector("kbd")).toHaveTextContent("/");
  });

  it("does not repeat the library folder (the Library filter bar shows it)", () => {
    titlebarAt("/library");
    expect(screen.queryByText(/Music/)).toBeNull();
    expect(screen.getByText("Library")).toBeInTheDocument();
  });

  it("resets the tile size with a visible control", () => {
    setTileColumns(4);
    titlebarAt("/library");
    expect(screen.getByRole("slider")).toHaveAttribute("aria-label", expect.stringContaining("4"));
    fireEvent.click(screen.getByRole("button", { name: /Back to automatic tile size/ }));
    expect(screen.getByText("Auto")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Back to automatic tile size/ })).toBeNull();
  });

  it("stores the theme choice and starts from it", async () => {
    titlebarAt("/library");
    fireEvent.click(screen.getByTitle("Toggle theme"));
    expect(document.documentElement.classList.contains("dark")).toBe(false);
    expect(localStorage.getItem("mm.theme")).toBe("light");

    document.documentElement.classList.add("dark");
    const { applyStoredTheme } = await import("@/lib/theme");
    applyStoredTheme();
    expect(document.documentElement.classList.contains("dark")).toBe(false);
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

  it("lights one entry: Library without a filter, the filter when one is set", () => {
    const a = sidebarAt("/library");
    expect(screen.getByRole("link", { name: "Library" }).className).toContain("bg-accent-soft");
    expect(screen.getByRole("button", { name: /All albums/ }).className).not.toContain("bg-accent-soft");
    a.unmount();

    sidebarAt("/library?filter=missing-art");
    expect(screen.getByRole("link", { name: "Library" }).className).not.toContain("bg-accent-soft");
    expect(screen.getByRole("button", { name: /Missing art/ }).className).toContain("bg-accent-soft");
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
