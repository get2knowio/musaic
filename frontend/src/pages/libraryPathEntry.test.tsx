/**
 * Path entry: the Library empty state carries the path entry
 * itself; a saved, existing directory shows the grid and opens the scan
 * stream at once. In the desktop window (the `window.__musaicDesktop`
 * marker its initialization script defines) a native folder picker is
 * offered through POST /__musaic/dialog/folder and the browser-only help
 * text is not shown; in a plain browser the text field stays. Settings
 * keeps its field and goes to the grid after a save.
 */
import { afterEach, beforeEach, describe, it, expect, vi } from "vitest";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter, Route, Routes } from "react-router-dom";

vi.mock("sonner", () => ({
  toast: Object.assign(vi.fn(), { error: vi.fn(), success: vi.fn(), warning: vi.fn() }),
}));

import LibraryPage from "@/pages/Library";
import SettingsPage from "@/pages/Settings";
import { MockEventSource } from "@/test/setup";
import type { AppConfig } from "@/lib/types";

const BROWSER_HELP = /browser file dialog returns files, not folders/;
const PICKED = "/srv/music/FLAC";

interface Call {
  url: string;
  method: string;
  body: unknown;
}

let calls: Call[];
let configAfterSave: AppConfig;

function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
}

beforeEach(() => {
  calls = [];
  MockEventSource.instances.length = 0;
  configAfterSave = { library_dir: PICKED, exists: true } as AppConfig;
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string, init?: RequestInit) => {
      const method = init?.method ?? "GET";
      calls.push({ url: String(url), method, body: init?.body ? JSON.parse(String(init.body)) : undefined });
      if (url === "/__musaic/dialog/folder" && method === "POST") return json({ path: PICKED });
      if (url === "/api/config" && method === "POST") return json(configAfterSave);
      if (String(url).startsWith("/api/albums")) {
        return json({ albums: [], count: 0, library_dir: PICKED, scanned_at: null });
      }
      return json({ detail: "not found" }, 404);
    }),
  );
});
afterEach(() => {
  vi.unstubAllGlobals();
  delete (window as { __musaicDesktop?: unknown }).__musaicDesktop;
});

function client(config: AppConfig): QueryClient {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
  qc.setQueryData(["config"], config);
  qc.setQueryData(["ai-providers"], { selected: null, providers: [], uninstalled: [] });
  qc.setQueryData(["jobs"], []);
  qc.setQueryData(["albums-snapshot"], []);
  return qc;
}

function renderLibrary(config: AppConfig = { library_dir: null, exists: false } as AppConfig) {
  return render(
    <QueryClientProvider client={client(config)}>
      <MemoryRouter initialEntries={["/library"]}>
        <Routes>
          <Route path="/library" element={<LibraryPage />} />
          <Route path="/settings" element={<div>SETTINGS PAGE</div>} />
        </Routes>
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

const scanStreams = () => MockEventSource.instances.filter((e) => e.url.startsWith("/api/scan/stream"));

describe("library path entry", () => {
  it("browser: the empty state has the text field (no Settings link, no picker) and saving scans at once", async () => {
    renderLibrary();
    expect(screen.queryByText("Set library directory in Settings")).toBeNull();
    expect(screen.queryByRole("button", { name: /Choose folder/ })).toBeNull();
    expect(screen.getByText(BROWSER_HELP)).toBeInTheDocument();

    fireEvent.change(screen.getByRole("textbox", { name: "Library directory path" }), {
      target: { value: `  ${PICKED}  ` },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() => expect(scanStreams().length).toBe(1));
    const post = calls.find((c) => c.url === "/api/config" && c.method === "POST");
    expect(post?.body).toEqual({ library_dir: PICKED });
    // The grid replaced the empty state.
    expect(screen.queryByText("Configure your library")).toBeNull();
  });

  it("desktop window: a native folder picker saves the chosen folder; no browser help text", async () => {
    (window as { __musaicDesktop?: unknown }).__musaicDesktop = Object.freeze({ folderPicker: true });
    renderLibrary();
    expect(screen.queryByText(BROWSER_HELP)).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Choose folder…" }));

    await waitFor(() => expect(scanStreams().length).toBe(1));
    const order = calls.filter((c) => c.method === "POST").map((c) => c.url);
    expect(order.slice(0, 2)).toEqual(["/__musaic/dialog/folder", "/api/config"]);
    expect(calls.find((c) => c.url === "/api/config")?.body).toEqual({ library_dir: PICKED });
  });

  it("desktop window: a cancelled picker saves nothing", async () => {
    (window as { __musaicDesktop?: unknown }).__musaicDesktop = Object.freeze({ folderPicker: true });
    vi.mocked(fetch).mockImplementation(async (url, init) => {
      calls.push({ url: String(url), method: init?.method ?? "GET", body: undefined });
      return json({ path: null });
    });
    renderLibrary();
    fireEvent.click(screen.getByRole("button", { name: "Choose folder…" }));
    await waitFor(() => expect(calls.some((c) => c.url === "/__musaic/dialog/folder")).toBe(true));
    await new Promise((r) => setTimeout(r, 20));
    expect(calls.some((c) => c.url === "/api/config")).toBe(false);
    expect(screen.getByText("Configure your library")).toBeInTheDocument();
  });

  it("a path that does not exist keeps the empty state and opens no scan", async () => {
    configAfterSave = { library_dir: "/nope", exists: false } as AppConfig;
    renderLibrary();
    fireEvent.change(screen.getByRole("textbox", { name: "Library directory path" }), {
      target: { value: "/nope" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(calls.some((c) => c.url === "/api/config")).toBe(true));
    await waitFor(() => expect(screen.getByText(/The configured directory doesn't exist/)).toBeInTheDocument());
    expect(scanStreams().length).toBe(0);
  });

  it("Settings: the field still works and a save goes to the grid", async () => {
    render(
      <QueryClientProvider client={client({ library_dir: null, exists: false } as AppConfig)}>
        <MemoryRouter initialEntries={["/settings"]}>
          <Routes>
            <Route path="/settings" element={<SettingsPage />} />
            <Route path="/library" element={<div>LIBRARY GRID</div>} />
          </Routes>
        </MemoryRouter>
      </QueryClientProvider>,
    );
    expect(screen.getByText(BROWSER_HELP)).toBeInTheDocument();
    const field = screen.getByRole("textbox", { name: "Library directory path" });
    fireEvent.change(field, { target: { value: PICKED } });
    fireEvent.click(within(field.parentElement!).getByRole("button", { name: "Save" }));
    await waitFor(() => expect(screen.getByText("LIBRARY GRID")).toBeInTheDocument());
    expect(calls.find((c) => c.url === "/api/config" && c.method === "POST")?.body).toEqual({ library_dir: PICKED });
  });

  it("Settings in the desktop window: picker offered, browser help text gone", () => {
    (window as { __musaicDesktop?: unknown }).__musaicDesktop = Object.freeze({ folderPicker: true });
    render(
      <QueryClientProvider client={client({ library_dir: null, exists: false } as AppConfig)}>
        <MemoryRouter initialEntries={["/settings"]}>
          <SettingsPage />
        </MemoryRouter>
      </QueryClientProvider>,
    );
    expect(screen.getByRole("button", { name: "Choose folder…" })).toBeInTheDocument();
    expect(screen.queryByText(BROWSER_HELP)).toBeNull();
  });
});
