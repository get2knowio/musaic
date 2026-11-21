/**
 * Settings is indexed by task. Each section has a stable hash link that
 * scrolls to it and focuses its control (the AcoustID deep link's
 * behaviour), and the album inspector's "Settings → My Tags" link goes to
 * tag management and back to the same album with its browse context.
 */
import { afterEach, beforeEach, describe, it, expect, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter, Route, Routes, useLocation } from "react-router-dom";

vi.mock("sonner", () => ({
  toast: Object.assign(vi.fn(), { error: vi.fn(), success: vi.fn(), warning: vi.fn() }),
}));

import SettingsPage from "@/pages/Settings";
import MyTagsSection from "@/components/DetailSheet/MyTagsSection";
import type { VocabularyResponse } from "@/lib/types";

function client(): QueryClient {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
  qc.setQueryData(["ai-providers"], { selected: null, providers: [], uninstalled: [] });
  qc.setQueryData(["vocabulary"], { vocabulary: [], orphans: [] } satisfies VocabularyResponse);
  qc.setQueryData(["album-tags", "a1"], { album_id: "a1", track_tags: {}, album_tags: [] });
  qc.setQueryData(["albums-snapshot"], []);
  return qc;
}

beforeEach(() => {
  sessionStorage.clear();
  localStorage.clear();
  vi.stubGlobal(
    "fetch",
    vi.fn(async () =>
      new Response(JSON.stringify({ detail: "not found" }), {
        status: 404,
        headers: { "content-type": "application/json" },
      }),
    ),
  );
});
afterEach(() => vi.unstubAllGlobals());

function renderSettings(entry = "/settings") {
  return render(
    <QueryClientProvider client={client()}>
      <MemoryRouter initialEntries={[entry]}>
        <SettingsPage />
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

describe("Settings index", () => {
  it("lists the six tasks as links in a named navigation", () => {
    renderSettings();
    const nav = screen.getByRole("navigation", { name: "Settings sections" });
    const links = within(nav).getAllByRole("link").map((a) => a.textContent);
    expect(links).toEqual([
      "Library folder",
      "Tags",
      "File naming",
      "Matching & integrations",
      "Backup & restore",
      "Assistants",
    ]);
    // Real links, so Tab reaches them and Enter follows them.
    for (const a of within(nav).getAllByRole("link")) expect(a).toHaveAttribute("href");
  });

  it("each link lands on its section's control", () => {
    renderSettings();
    const nav = screen.getByRole("navigation", { name: "Settings sections" });
    const go = (name: string) => fireEvent.click(within(nav).getByRole("link", { name }));

    go("Tags");
    expect(screen.getByPlaceholderText(/New tag/)).toHaveFocus();
    go("File naming");
    expect(document.activeElement).toBe(document.querySelector("#file-naming input"));
    expect(document.activeElement).toHaveProperty("tagName", "INPUT");
    go("Backup & restore");
    expect(screen.getByRole("button", { name: /Back up catalog database/ })).toHaveFocus();
    go("Assistants");
    expect(screen.getByRole("textbox", { name: "Assistant name" })).toHaveFocus();
    go("Library folder");
    expect(document.activeElement).toBe(document.getElementById("library-folder"));
    go("Matching & integrations");
    expect(document.activeElement).toBe(document.getElementById("matching-integrations"));
  });

  it.each([
    ["#tags", () => screen.getByPlaceholderText(/New tag/)],
    ["#backup-restore", () => screen.getByRole("button", { name: /Back up catalog database/ })],
    ["#library-folder", () => document.getElementById("library-folder")],
  ])("opening /settings%s directly focuses the section", (hash, target) => {
    renderSettings(`/settings${hash}`);
    expect(target()).toHaveFocus();
  });

  it("an unknown hash and the older #acoustid-key hash do not break the page", () => {
    renderSettings("/settings#nothing-here");
    expect(screen.getByRole("heading", { name: "Settings" })).toBeInTheDocument();
    renderSettings("/settings#acoustid-key");
    expect(screen.getAllByRole("heading", { name: "Settings" }).length).toBeGreaterThan(0);
  });

  it("sections are named groups", () => {
    renderSettings();
    for (const name of ["Library folder", "Tags", "File naming", "Matching & integrations", "Backup & restore", "Assistants"]) {
      expect(screen.getByRole("group", { name })).toBeInTheDocument();
    }
  });
});

describe("album inspector to My Tags and back", () => {
  function Where() {
    const l = useLocation();
    return <div data-testid="where">{l.pathname + l.search + l.hash}</div>;
  }

  it("goes straight to tag management, then back to the same album and context with focus kept", async () => {
    const qc = client();
    render(
      <QueryClientProvider client={qc}>
        <MemoryRouter initialEntries={["/attention", "/library/a1?filter=review&q=blue"]} initialIndex={1}>
          <Where />
          <Routes>
            <Route path="/library/:albumId" element={<MyTagsSection albumId="a1" tracks={[]} />} />
            <Route path="/settings" element={<SettingsPage />} />
          </Routes>
        </MemoryRouter>
      </QueryClientProvider>,
    );

    // The section is collapsed by default; open it.
    fireEvent.click(screen.getByRole("button", { name: /My Tags/ }));
    fireEvent.click(screen.getByRole("link", { name: /Settings → My Tags/ }));

    // Straight to tag management, the add-tag field focused.
    expect(screen.getByTestId("where").textContent).toBe("/settings#tags");
    expect(screen.getByPlaceholderText(/New tag/)).toHaveFocus();

    // The user creates a tag there (the vocabulary cache gets it the way the
    // panel's own mutation puts it).
    act(() => {
      qc.setQueryData(["vocabulary"], {
        vocabulary: [{ tag: "Cocktail Hour", description: "", count: 0 }],
        orphans: [],
      });
    });

    fireEvent.click(screen.getByRole("button", { name: "Back to album" }));
    await waitFor(() =>
      expect(screen.getByTestId("where").textContent).toBe("/library/a1?filter=review&q=blue"),
    );
    // The new tag is available, and focus lands on the My Tags region.
    expect(screen.getByRole("button", { name: /Add tag to all tracks/ })).toBeInTheDocument();
    await waitFor(() => expect(screen.getByRole("group", { name: "My Tags" })).toHaveFocus());
  });

  it("Settings opened any other way offers no Back to album", () => {
    renderSettings("/settings#tags");
    expect(screen.queryByRole("button", { name: "Back to album" })).toBeNull();
  });
});
