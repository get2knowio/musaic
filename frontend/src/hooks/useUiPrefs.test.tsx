/**
 * The theme and tile size are kept by the server (the desktop window's
 * address changes on every launch, so browser storage forgets them):
 * the server's value wins, a change is written to the server, and a window
 * whose browser storage is unavailable still works.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ReactNode } from "react";
import { act, renderHook, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";

import { setTheme } from "@/lib/theme";
import { setTileColumns, useTileColumns } from "@/hooks/useTileColumns";
import { useSyncUiPrefs } from "@/hooks/useUiPrefs";
import type { UiPrefs } from "@/lib/api";

let serverPrefs: UiPrefs;
let puts: unknown[];

function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
}

function wrapper() {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return ({ children }: { children: ReactNode }) => (
    <QueryClientProvider client={qc}>{children}</QueryClientProvider>
  );
}

function mount() {
  return renderHook(
    () => {
      useSyncUiPrefs();
      return useTileColumns();
    },
    { wrapper: wrapper() },
  );
}

const isDark = () => document.documentElement.classList.contains("dark");

beforeEach(() => {
  serverPrefs = { theme: null, tile_columns: null };
  puts = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string, init?: RequestInit) => {
      if (url !== "/api/ui-prefs") return json({ detail: "not found" }, 404);
      if (init?.method === "PUT") {
        const body = JSON.parse(String(init.body)) as Partial<UiPrefs>;
        puts.push(body);
        serverPrefs = { ...serverPrefs, ...body };
      }
      return json(serverPrefs);
    }),
  );
  localStorage.clear();
  document.documentElement.classList.add("dark");
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  setTileColumns(null);
});

describe("UI preferences kept by the server", () => {
  it("the server's values win over what this window had cached", async () => {
    localStorage.setItem("mm.theme", "dark");
    localStorage.setItem("mm.tile.columns", "6");
    serverPrefs = { theme: "light", tile_columns: 4 };
    const { result } = mount();
    await waitFor(() => expect(result.current).toBe(4));
    expect(isDark()).toBe(false);
    // The cache follows, so the next first paint is right.
    expect(localStorage.getItem("mm.theme")).toBe("light");
    expect(localStorage.getItem("mm.tile.columns")).toBe("4");
  });

  it("nothing chosen on the server means the default theme and automatic tiles", async () => {
    setTileColumns(5);
    puts = [];
    serverPrefs = { theme: null, tile_columns: null };
    document.documentElement.classList.remove("dark");
    const { result } = mount();
    await waitFor(() => expect(result.current).toBeNull());
    expect(isDark()).toBe(true);
    expect(puts).toEqual([]); // applying the server's answer writes nothing back
  });

  it("a change is written to the server", async () => {
    mount();
    act(() => setTheme("light"));
    act(() => setTileColumns(6));
    await waitFor(() => expect(puts).toEqual([{ theme: "light" }, { tile_columns: 6 }]));
    expect(isDark()).toBe(false);
    act(() => setTileColumns(null));
    await waitFor(() => expect(puts.at(-1)).toEqual({ tile_columns: null }));
  });

  it("still works when browser storage is unavailable", async () => {
    vi.spyOn(Storage.prototype, "getItem").mockImplementation(() => {
      throw new Error("denied");
    });
    vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => {
      throw new Error("denied");
    });
    vi.spyOn(Storage.prototype, "removeItem").mockImplementation(() => {
      throw new Error("denied");
    });
    serverPrefs = { theme: "light", tile_columns: 3 };
    const { result } = mount();
    await waitFor(() => expect(result.current).toBe(3));
    expect(isDark()).toBe(false);
    act(() => setTheme("dark"));
    expect(isDark()).toBe(true);
    await waitFor(() => expect(puts).toEqual([{ theme: "dark" }]));
  });

  it("keeps the choice in this window when the server cannot be reached", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => {
        throw new TypeError("network down");
      }),
    );
    const { result } = mount();
    act(() => setTheme("light"));
    act(() => setTileColumns(5));
    await waitFor(() => expect(result.current).toBe(5));
    expect(isDark()).toBe(false);
  });
});
