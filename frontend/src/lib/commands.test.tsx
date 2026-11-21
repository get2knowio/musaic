/**
 * The command registry (#333, #334): one availability rule and one way to
 * run each command, whichever face starts it (the macOS menu bar through
 * the `musaic:command` event, a toolbar control, a shortcut, the album
 * inspector). Album commands follow the selection and are dimmed when they
 * cannot act; nothing runs while a modal dialog is open.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter, useLocation, useNavigate } from "react-router-dom";

vi.mock("sonner", () => ({ toast: Object.assign(vi.fn(), { error: vi.fn() }) }));

import { useCommandHost } from "@/hooks/useCommands";
import {
  COMMANDS,
  COMMAND_EVENT,
  commandEnabled,
  menuState,
  runCommand,
  type CommandId,
  type CommandState,
} from "@/lib/commands";
import { makeAlbum } from "@/test/fixtures";
import type { AppConfig } from "@/lib/types";
import {
  openShortcutsDialog,
  requestAlbumSection,
  resetUiRequests,
  useSearchFocusRequest,
} from "@/lib/uiRequests";
import Section from "@/components/DetailSheet/Section";
import KeyboardShortcutsDialog from "@/components/KeyboardShortcutsDialog";

const A = "aaaaaaaaaaaaaaaa";
const B = "bbbbbbbbbbbbbbbb";

function state(over: Partial<CommandState> = {}): CommandState {
  return {
    pathname: "/library",
    search: "",
    libraryUrl: "/library",
    library: "available",
    scanning: false,
    modalOpen: false,
    canReveal: true,
    theme: "dark",
    selection: null,
    ...over,
  };
}

const present = (id: string) => ({ pathname: `/library/${id}`, selection: { id, album: { id, missing_since: null } } });
const missing = (id: string) => ({
  pathname: `/library/${id}`,
  selection: { id, album: { id, missing_since: "2026-11-01T00:00:00Z" } },
});

const ALBUM: CommandId[] = ["album.reveal", "album.my-tags", "album.audio-analysis"];

let calls: string[];
beforeEach(() => {
  calls = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string, init?: RequestInit) => {
      calls.push(`${init?.method ?? "GET"} ${url}`);
      if (String(url).startsWith("/__musaic/")) return new Response(null, { status: 204 });
      return new Response("{}", { status: 404 });
    }),
  );
});
afterEach(() => {
  vi.unstubAllGlobals();
  delete (window as { __musaicDesktop?: unknown }).__musaicDesktop;
  resetUiRequests();
});

describe("availability", () => {
  it("no selection: album commands are dimmed, the rest is available", () => {
    const s = state();
    for (const id of ALBUM) expect(commandEnabled(id, s)).toBe(false);
    for (const id of ["app.settings", "library.find", "view.library", "view.review", "library.rescan"] as const) {
      expect(commandEnabled(id, s)).toBe(true);
    }
    expect(commandEnabled("library.cancel-scan", s)).toBe(false);
  });

  it("a present album: every album command is available", () => {
    const s = state(present(A));
    for (const id of ALBUM) expect(commandEnabled(id, s)).toBe(true);
  });

  it("a missing folder: nothing to show in Finder, no tags or analysis to open", () => {
    const s = state(missing(A));
    for (const id of ALBUM) expect(commandEnabled(id, s)).toBe(false);
  });

  it("an album not loaded yet, or a window that cannot reveal: Show in Finder is dimmed", () => {
    expect(commandEnabled("album.reveal", state({ pathname: `/library/${A}`, selection: { id: A, album: null } }))).toBe(false);
    expect(commandEnabled("album.reveal", state({ ...present(A), canReveal: false }))).toBe(false);
    // The tags and analysis sections are in a browser too.
    expect(commandEnabled("album.my-tags", state({ ...present(A), canReveal: false }))).toBe(true);
  });

  it("a busy scan: no second rescan, no folder change, backup or restore; Cancel Scan is available", () => {
    const s = state({ ...present(A), scanning: true });
    for (const id of ["library.rescan", "library.choose-folder", "catalog.backup", "catalog.restore"] as const) {
      expect(commandEnabled(id, s)).toBe(false);
    }
    expect(commandEnabled("library.cancel-scan", s)).toBe(true);
    // Reading commands stay available while the scan runs.
    for (const id of ["album.reveal", "library.find", "view.review"] as const) {
      expect(commandEnabled(id, s)).toBe(true);
    }
  });

  it("a modal dialog: every command is dimmed and none runs", async () => {
    const s = state({ ...present(A), modalOpen: true });
    for (const c of COMMANDS) expect(commandEnabled(c.id, s)).toBe(false);
    const navigate = vi.fn();
    expect(await runCommand("view.review", { state: s, navigate })).toBe(false);
    expect(await runCommand("album.reveal", { state: s, navigate })).toBe(false);
    expect(navigate).not.toHaveBeenCalled();
    expect(calls).toEqual([]);
  });

  it("no library folder: Find, Rescan, Tiles/List and Sort are dimmed; Settings and the pages are not", () => {
    const s = state({ library: "unavailable" });
    for (const id of ["library.find", "library.rescan", "view.tiles", "view.list", "sort.year-desc"] as const) {
      expect(commandEnabled(id, s)).toBe(false);
    }
    for (const id of ["app.settings", "library.choose-folder", "view.library", "view.review"] as const) {
      expect(commandEnabled(id, s)).toBe(true);
    }
  });
});

describe("running", () => {
  it("Show in Finder reveals the selected album and follows the selection", async () => {
    const navigate = vi.fn();
    await runCommand("album.reveal", { state: state(present(A)), navigate });
    await runCommand("album.reveal", { state: state(present(B)), navigate });
    expect(calls).toEqual([`POST /__musaic/reveal/album/${A}`, `POST /__musaic/reveal/album/${B}`]);
  });

  it("the menu marks the current view, sort and theme", () => {
    const items = new Map(menuState(state({ search: "?view=list&sort=year-desc", theme: "light" })).map((i) => [i.id, i]));
    expect(items.get("view.list")?.checked).toBe(true);
    expect(items.get("view.tiles")?.checked).toBe(false);
    expect(items.get("sort.year-desc")?.checked).toBe(true);
    expect(items.get("sort.artist-asc")?.checked).toBe(false);
    expect(items.get("appearance.light")?.checked).toBe(true);
    expect(items.get("album.reveal")).toEqual({ id: "album.reveal", enabled: false });
    expect(items.size).toBe(COMMANDS.length);
  });

  it("sort and view keep the rest of the Library URL, from Review too", async () => {
    const navigate = vi.fn();
    await runCommand("sort.year-desc", { state: state({ search: "?q=bach&view=list" }), navigate });
    expect(navigate).toHaveBeenLastCalledWith("/library?q=bach&view=list&sort=year-desc", { replace: true });
    await runCommand("view.tiles", {
      state: state({ pathname: "/review", libraryUrl: `/library/${A}?view=list` }),
      navigate,
    });
    expect(navigate).toHaveBeenLastCalledWith(`/library/${A}`, { replace: false });
  });

  it("only opens screens: commands that ask for input navigate and write nothing", async () => {
    const navigate = vi.fn();
    for (const c of COMMANDS.filter((c) => c.label(state()).endsWith("…"))) {
      await runCommand(c.id, { state: state(present(A)), navigate });
    }
    expect(calls).toEqual([]);
    expect(navigate.mock.calls.map((c) => c[0])).toEqual([
      "/settings",
      "/settings#library-folder",
      "/settings#backup-restore",
      "/settings#backup-restore",
    ]);
  });
});

describe("screens the commands open", () => {
  it("Album → Edit My Tags… opens and focuses the inspector's section", async () => {
    localStorage.setItem("mm.section.my-tags", "0");
    render(
      <Section id="my-tags" title="My Tags">
        <p>tag editor</p>
      </Section>,
    );
    expect(screen.queryByText("tag editor")).toBeNull();
    await act(async () => requestAlbumSection("my-tags"));
    expect(screen.getByText("tag editor")).toBeInTheDocument();
    await waitFor(() => expect(screen.getByRole("button", { name: /My Tags/ })).toHaveFocus());
  });

  it("Help → Keyboard Shortcuts lists search and inspector focus, and the menu keys in the window", async () => {
    Object.defineProperty(window, "__musaicDesktop", {
      value: Object.freeze({ reveal: true, nativeMenu: false }),
      configurable: true,
    });
    render(<KeyboardShortcutsDialog />);
    await act(async () => openShortcutsDialog());
    const dialog = screen.getByRole("dialog", { name: "Keyboard Shortcuts" });
    expect(dialog).toHaveTextContent("Focus the search field");
    expect(dialog).toHaveTextContent("Move focus between the album list and the inspector");
    expect(dialog).toHaveTextContent(/(⌘|Ctrl\+)F/);
    expect(dialog).toHaveTextContent(/(⌘|Ctrl\+)2/);
  });

  it("a browser lists no menu keys (they stay the browser's)", async () => {
    render(<KeyboardShortcutsDialog />);
    await act(async () => openShortcutsDialog());
    const dialog = screen.getByRole("dialog", { name: "Keyboard Shortcuts" });
    expect(dialog).not.toHaveTextContent(/(⌘|Ctrl\+)F/);
    expect(dialog).toHaveTextContent("Focus the search field");
  });
});

// ---- the host: the native menu's side --------------------------------------

function Host() {
  useCommandHost();
  const loc = useLocation();
  const navigate = useNavigate();
  const focus = useSearchFocusRequest();
  return (
    <div>
      <output data-testid="where">{loc.pathname}</output>
      <output data-testid="focus">{focus}</output>
      <button onClick={() => navigate(`/library/${B}`)}>select B</button>
    </div>
  );
}

function host(at: string) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
  qc.setQueryData(["config"], { library_dir: "/music", exists: true } as AppConfig);
  qc.setQueryData(["albums-snapshot"], [
    makeAlbum({ id: A }),
    makeAlbum({ id: B, missing_since: "2026-11-01T00:00:00Z" }),
  ]);
  return render(
    <QueryClientProvider client={qc}>
      <MemoryRouter initialEntries={[at]}>
        <Host />
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

const send = (id: string) =>
  act(async () => {
    window.dispatchEvent(new CustomEvent(COMMAND_EVENT, { detail: id }));
  });

describe("native menu", () => {
  it("runs the command the menu sends; Find from Review opens Library and asks for the search field", async () => {
    host("/review");
    await send("library.find");
    expect(screen.getByTestId("where").textContent).toBe("/library");
    expect(screen.getByTestId("focus").textContent).not.toBe("0");
    await send("view.review");
    expect(screen.getByTestId("where").textContent).toBe("/review");
  });

  it("ignores ids that are not commands", async () => {
    host("/review");
    await send("evil.command");
    await send("");
    expect(screen.getByTestId("where").textContent).toBe("/review");
  });

  it("tells the shell what is available, and again when the selection changes", async () => {
    Object.defineProperty(window, "__musaicDesktop", {
      value: Object.freeze({ reveal: true, nativeMenu: true }),
      configurable: true,
    });
    host(`/library/${A}`);
    const posted = () =>
      (vi.mocked(fetch).mock.calls as [string, RequestInit?][])
        .filter(([u]) => u === "/__musaic/menu-state")
        .map(([, init]) => JSON.parse(String(init?.body)) as { items: { id: string; enabled: boolean }[] });
    await waitFor(() => expect(posted().length).toBeGreaterThan(0));
    const reveal = (n: number) => posted()[n].items.find((i) => i.id === "album.reveal")?.enabled;
    expect(reveal(posted().length - 1)).toBe(true);
    // B's folder is missing: Show in Finder dims.
    await act(async () => screen.getByText("select B").click());
    await waitFor(() => expect(reveal(posted().length - 1)).toBe(false));
  });

  it("posts nothing without a menu bar", async () => {
    host(`/library/${A}`);
    await act(async () => {});
    expect(calls.filter((c) => c.includes("menu-state"))).toEqual([]);
  });
});
