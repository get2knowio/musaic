/**
 * The command registry: every app-level action that a native menu item, a
 * toolbar or sidebar control, a keyboard shortcut or an inspector button
 * can start, with one rule for when it is available and one way to run it.
 *
 * Every face goes through here:
 *
 *  - the macOS menu bar (rust/crates/musaic-desktop/src/menu.rs) sends a
 *    `musaic:command` DOM event carrying one of these ids; the command host
 *    (`hooks/useCommands.ts`, mounted once in App) runs it, and posts each
 *    command's `enabled` / `checked` back to the shell so the menu dims
 *    what is unavailable;
 *  - the window's own controls (Titlebar, FilterBar, the View menu button,
 *    the inspector's Show in Finder) and the keyboard shortcuts call
 *    `runCommand` with the same state, so Windows and Linux, which have no
 *    menu bar, keep every command in the window.
 *
 * A command never runs while it is unavailable (`runCommand` checks), and
 * nothing here writes files by merely opening a screen: the commands that
 * ask for more input (their labels end in "…") only navigate, focus or open
 * a section; the writes stay behind that screen's own buttons.
 */
import { toast } from "sonner";

import { DEFAULT_SORT, SORT_OPTIONS, parseSort, type SortKey } from "./albumSort";
import { openActivityPanel } from "./activityPanel";
import { revealAlbumFolder, revealLabel } from "./desktop";
import { triggerRescan } from "./rescanController";
import { cancelRunningScan } from "./scanActivity";
import { setTheme, type Theme } from "./theme";
import type { Album } from "./types";
import { openShortcutsDialog, requestAlbumSection, requestSearchFocus } from "./uiRequests";

/** The DOM event the native menu dispatches (menu.rs `EVENT`). */
export const COMMAND_EVENT = "musaic:command";

/** The sort commands, one per sort order (menu.rs lists the same ids). */
export const SORT_COMMANDS = {
  "artist-asc": "sort.artist-asc",
  "artist-desc": "sort.artist-desc",
  "title-asc": "sort.title-asc",
  "title-desc": "sort.title-desc",
  "year-desc": "sort.year-desc",
  "year-asc": "sort.year-asc",
  "scanned-desc": "sort.scanned-desc",
  "scanned-asc": "sort.scanned-asc",
} as const satisfies Record<SortKey, string>;

type SortCommandId = (typeof SORT_COMMANDS)[SortKey];

export type CommandId =
  | "app.settings"
  | "library.choose-folder"
  | "library.rescan"
  | "library.cancel-scan"
  | "catalog.backup"
  | "catalog.restore"
  | "library.find"
  | "view.library"
  | "view.review"
  | "view.activity"
  | "view.tiles"
  | "view.list"
  | SortCommandId
  | "appearance.light"
  | "appearance.dark"
  | "album.reveal"
  | "album.my-tags"
  | "album.audio-analysis"
  | "help.shortcuts";

/** What the availability rules look at. Built by `useCommandState`. */
export interface CommandState {
  /** The current route. */
  pathname: string;
  /** Its query string, with the leading "?" (or ""). */
  search: string;
  /** The Library URL the user last browsed (Show Library returns there). */
  libraryUrl: string;
  /** The library folder: configured and on disk, not, or not known yet. */
  library: "available" | "unavailable" | "unknown";
  /** A library scan is starting, running or cancelling. */
  scanning: boolean;
  /** A modal dialog owns the keyboard (Activity panel, a Resolve dialog…). */
  modalOpen: boolean;
  /** The desktop window can show folders in the file manager. */
  canReveal: boolean;
  theme: Theme;
  /**
   * The album open in the inspector (`/library/<id>`, opened from Library
   * or from a Review item); `album` is its record, null while unknown.
   */
  selection: { id: string; album: Pick<Album, "id" | "missing_since"> | null } | null;
}

export interface CommandContext {
  state: CommandState;
  navigate: (to: string, opts?: { replace?: boolean }) => void;
}

export type MenuGroup = "Musaic" | "File" | "Edit" | "View" | "Album" | "Help";

export interface Command {
  id: CommandId;
  label: (s: CommandState) => string;
  group: MenuGroup;
  /** The key with Cmd (macOS) / Ctrl (elsewhere), when it has one. */
  modKey?: string;
  enabled: (s: CommandState) => boolean;
  /** For choices shown with a check mark. */
  checked?: (s: CommandState) => boolean;
  run: (ctx: CommandContext) => void | Promise<void>;
}

// ---- helpers ----------------------------------------------------------------

const onLibrary = (s: CommandState) => s.pathname === "/library" || s.pathname.startsWith("/library/");

/** The Library URL a view or sort change starts from. */
function libraryBase(s: CommandState): string {
  return onLibrary(s) ? s.pathname + s.search : s.libraryUrl;
}

function withParam(url: string, key: string, value: string | null): string {
  const i = url.indexOf("?");
  const path = i >= 0 ? url.slice(0, i) : url;
  const p = new URLSearchParams(i >= 0 ? url.slice(i + 1) : "");
  if (value === null) p.delete(key);
  else p.set(key, value);
  const qs = p.toString();
  return `${path}${qs ? `?${qs}` : ""}`;
}

function paramOf(s: CommandState, key: string): string | null {
  const url = libraryBase(s);
  const i = url.indexOf("?");
  return new URLSearchParams(i >= 0 ? url.slice(i + 1) : "").get(key);
}

export function currentView(s: CommandState): "tiles" | "list" {
  return paramOf(s, "view") === "list" ? "list" : "tiles";
}

export function currentSort(s: CommandState): SortKey {
  return parseSort(paramOf(s, "sort"));
}

/** The selected album, if it is on disk (not Missing). */
function presentSelection(s: CommandState): string | null {
  const a = s.selection?.album;
  return a && !a.missing_since ? a.id : null;
}

/**
 * Whether "Show in Finder" can act on `album`: the window can reveal
 * folders and the album's folder is on disk. The inspector's own button
 * and Album → Show in Finder both ask this.
 */
export function albumRevealAvailable(album: Pick<Album, "missing_since"> | null | undefined, canReveal: boolean): boolean {
  return canReveal && !!album && !album.missing_since;
}

/** Show an album's folder in the file manager. Rejects with the reason. */
export function revealAlbum(albumId: string): Promise<void> {
  return revealAlbumFolder(albumId);
}

const free = (s: CommandState) => !s.modalOpen;
const libraryReady = (s: CommandState) => free(s) && s.library === "available";

function setLibraryParam(ctx: CommandContext, key: string, value: string | null) {
  const s = ctx.state;
  ctx.navigate(withParam(libraryBase(s), key, value), { replace: onLibrary(s) });
}

function reportFailure(fallback: string) {
  return (e: unknown) => {
    toast.error((e as Error)?.message || fallback);
  };
}

// ---- the registry -------------------------------------------------------------

const sortCommands: Command[] = SORT_OPTIONS.map((o) => ({
  id: SORT_COMMANDS[o.key],
  label: () => o.label,
  group: "View",
  enabled: libraryReady,
  checked: (s) => currentSort(s) === o.key,
  run: (ctx) => setLibraryParam(ctx, "sort", o.key === DEFAULT_SORT ? null : o.key),
}));

export const COMMANDS: readonly Command[] = [
  {
    id: "app.settings",
    label: () => "Settings…",
    group: "Musaic",
    modKey: ",",
    enabled: free,
    run: ({ navigate }) => navigate("/settings"),
  },
  {
    id: "library.choose-folder",
    label: () => "Choose Music Folder…",
    group: "File",
    enabled: (s) => free(s) && !s.scanning,
    run: ({ navigate }) => navigate("/settings#library-folder"),
  },
  {
    id: "library.rescan",
    label: () => "Rescan Library",
    group: "File",
    enabled: (s) => libraryReady(s) && !s.scanning,
    run: () => triggerRescan().then(() => undefined, reportFailure("Couldn't start rescan")),
  },
  {
    id: "library.cancel-scan",
    label: () => "Cancel Scan",
    group: "File",
    enabled: (s) => free(s) && s.scanning,
    run: () => cancelRunningScan().then(() => undefined, reportFailure("Couldn't cancel the scan")),
  },
  {
    id: "catalog.backup",
    label: () => "Back Up Catalog Database…",
    group: "File",
    enabled: (s) => free(s) && !s.scanning,
    run: ({ navigate }) => navigate("/settings#backup-restore"),
  },
  {
    id: "catalog.restore",
    label: () => "Restore Catalog Database…",
    group: "File",
    enabled: (s) => free(s) && !s.scanning,
    run: ({ navigate }) => navigate("/settings#backup-restore"),
  },
  {
    id: "library.find",
    label: () => "Find in Library",
    group: "Edit",
    modKey: "F",
    enabled: libraryReady,
    run: ({ state, navigate }) => {
      requestSearchFocus();
      if (!onLibrary(state)) navigate(state.libraryUrl);
    },
  },
  {
    id: "view.library",
    label: () => "Show Library",
    group: "View",
    modKey: "1",
    enabled: free,
    run: ({ state, navigate }) => {
      if (!onLibrary(state)) navigate(state.libraryUrl);
    },
  },
  {
    id: "view.review",
    label: () => "Show Review",
    group: "View",
    modKey: "2",
    enabled: free,
    run: ({ state, navigate }) => {
      if (!state.pathname.startsWith("/review")) navigate("/review");
    },
  },
  {
    id: "view.activity",
    label: () => "Show Activity",
    group: "View",
    enabled: free,
    run: () => openActivityPanel(),
  },
  {
    id: "view.tiles",
    label: () => "Tiles",
    group: "View",
    enabled: libraryReady,
    checked: (s) => currentView(s) === "tiles",
    run: (ctx) => setLibraryParam(ctx, "view", null),
  },
  {
    id: "view.list",
    label: () => "List",
    group: "View",
    enabled: libraryReady,
    checked: (s) => currentView(s) === "list",
    run: (ctx) => setLibraryParam(ctx, "view", "list"),
  },
  ...sortCommands,
  {
    id: "appearance.light",
    label: () => "Light",
    group: "View",
    enabled: free,
    checked: (s) => s.theme === "light",
    run: () => setTheme("light"),
  },
  {
    id: "appearance.dark",
    label: () => "Dark",
    group: "View",
    enabled: free,
    checked: (s) => s.theme === "dark",
    run: () => setTheme("dark"),
  },
  {
    id: "album.reveal",
    label: () => revealLabel(),
    group: "Album",
    enabled: (s) => free(s) && s.library === "available" && albumRevealAvailable(s.selection?.album, s.canReveal),
    run: ({ state }) => {
      const id = presentSelection(state);
      if (!id) return;
      return revealAlbum(id).catch(reportFailure("Couldn't open the folder"));
    },
  },
  {
    id: "album.my-tags",
    label: () => "Edit My Tags…",
    group: "Album",
    enabled: (s) => free(s) && presentSelection(s) !== null,
    run: () => requestAlbumSection("my-tags"),
  },
  {
    id: "album.audio-analysis",
    label: () => "Audio Analysis…",
    group: "Album",
    enabled: (s) => free(s) && presentSelection(s) !== null,
    run: () => requestAlbumSection("audio-analysis"),
  },
  {
    id: "help.shortcuts",
    label: () => "Keyboard Shortcuts",
    group: "Help",
    enabled: free,
    run: () => openShortcutsDialog(),
  },
];

const BY_ID = new Map<string, Command>(COMMANDS.map((c) => [c.id, c]));

export function isCommandId(id: unknown): id is CommandId {
  return typeof id === "string" && BY_ID.has(id);
}

export function getCommand(id: CommandId): Command {
  const c = BY_ID.get(id);
  if (!c) throw new Error(`unknown command ${id}`);
  return c;
}

export function commandEnabled(id: CommandId, s: CommandState): boolean {
  return getCommand(id).enabled(s);
}

export function commandChecked(id: CommandId, s: CommandState): boolean | undefined {
  return getCommand(id).checked?.(s);
}

/**
 * Run a command if it is available now. Resolves to whether it ran (its
 * own failures are reported to the user, not thrown).
 */
export async function runCommand(id: CommandId, ctx: CommandContext): Promise<boolean> {
  const c = getCommand(id);
  if (!c.enabled(ctx.state)) return false;
  await c.run(ctx);
  return true;
}

/** Every command's state, as the native menu takes it. */
export function menuState(s: CommandState): { id: CommandId; enabled: boolean; checked?: boolean }[] {
  return COMMANDS.map((c) => {
    const checked = c.checked?.(s);
    return checked === undefined ? { id: c.id, enabled: c.enabled(s) } : { id: c.id, enabled: c.enabled(s), checked };
  });
}

/** Whether the platform's modifier is Cmd (macOS) rather than Ctrl. */
export function isMac(): boolean {
  return typeof navigator !== "undefined" && /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent);
}

/** "⌘F" on macOS, "Ctrl+F" elsewhere. */
export function modShortcut(key: string): string {
  return isMac() ? `⌘${key}` : `Ctrl+${key}`;
}
