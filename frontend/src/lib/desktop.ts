/**
 * The desktop window (Tauri shell) and what it offers the page.
 *
 * The window loads the same SPA from the in-process server on
 * `http://127.0.0.1:<port>`; there is no IPC bridge. Its initialization
 * script (rust/crates/musaic-desktop/src/gate.rs, `DIALOG_SHIM_JS`)
 * defines a frozen `window.__musaicDesktop` marker. A plain browser has
 * none, so everything here degrades to the browser behaviour there.
 *
 * Folder picker: `POST /__musaic/dialog/folder` opens the
 * system "choose a folder" panel and answers `{"path": "/abs/dir"}`, or
 * `{"path": null}` when the user cancels.
 *
 * Reveal ("Show in Finder"): `POST
 * /__musaic/reveal/album/{album_id}` shows the album's folder and `POST
 * /__musaic/reveal/trash` the app's trash folder in Finder / Explorer /
 * the file manager. The page sends an album id, never a path: the shell
 * looks the folder up itself. 204 when shown; otherwise `{"detail": …}`.
 */

interface DesktopMarker {
  folderPicker?: boolean;
  /** The reveal endpoints exist. */
  reveal?: boolean;
  /** What the platform's file manager is called ("Finder", "Explorer",
   *  "file manager"), for the button label. */
  fileManager?: string;
  /** The shell shows a native menu bar (macOS): its commands arrive as
   *  `musaic:command` events and it takes its enabled state from
   *  `POST /__musaic/menu-state` (rust/crates/musaic-desktop/src/menu.rs). */
  nativeMenu?: boolean;
}

declare global {
  interface Window {
    __musaicDesktop?: DesktopMarker;
  }
}

export const FOLDER_PICKER_PATH = "/__musaic/dialog/folder";
export const MENU_STATE_PATH = "/__musaic/menu-state";

/** Whether this page runs in the desktop window (any platform). */
export function isDesktopWindow(): boolean {
  return typeof window !== "undefined" && window.__musaicDesktop !== undefined;
}

/** Whether the desktop window has a native menu bar driving the commands. */
export function hasNativeMenu(): boolean {
  return typeof window !== "undefined" && window.__musaicDesktop?.nativeMenu === true;
}

/** One native menu item's state, as the shell reads it. */
export interface MenuItemState {
  id: string;
  enabled: boolean;
  checked?: boolean;
}

/** Tell the shell which menu commands are available. Best effort: a
 *  failed post leaves the menu as it was. */
export async function postMenuState(items: MenuItemState[]): Promise<void> {
  try {
    await fetch(MENU_STATE_PATH, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ items }),
    });
  } catch {
    /* the next change posts again */
  }
}

/** Whether this page runs in the desktop window with a native folder picker. */
export function hasNativeFolderPicker(): boolean {
  return typeof window !== "undefined" && window.__musaicDesktop?.folderPicker === true;
}

/**
 * Open the native folder picker. Resolves to the chosen absolute path, or
 * `null` when cancelled. Rejects with the server's message on a refusal.
 */
export async function pickFolder(): Promise<string | null> {
  const res = await fetch(FOLDER_PICKER_PATH, { method: "POST" });
  let body: unknown = null;
  try {
    body = await res.json();
  } catch {
    /* empty or non-JSON body */
  }
  const obj = (body && typeof body === "object" ? body : {}) as { path?: unknown; detail?: unknown };
  if (!res.ok) {
    throw new Error(typeof obj.detail === "string" ? obj.detail : `Folder picker failed (${res.status})`);
  }
  return typeof obj.path === "string" && obj.path ? obj.path : null;
}

/** Help text under the library path field: the browser-only
 *  note about why there is no folder picker is not shown in the window. */
export function libraryPathHelp(): string {
  return hasNativeFolderPicker()
    ? "Choose the folder, or paste an absolute path."
    : "Paste an absolute path. We don't pop a native folder picker because the browser file dialog returns files, not folders.";
}

/** Whether this page runs in the desktop window and can reveal folders in
 *  the platform's file manager. */
export function canRevealInFileManager(): boolean {
  return typeof window !== "undefined" && window.__musaicDesktop?.reveal === true;
}

/** "Show in Finder" / "Show in Explorer" / "Show in file manager". */
export function revealLabel(): string {
  const name = typeof window !== "undefined" ? window.__musaicDesktop?.fileManager : undefined;
  return `Show in ${typeof name === "string" && name.trim() ? name.trim() : "file manager"}`;
}

export const REVEAL_TRASH_PATH = "/__musaic/reveal/trash";

export function revealAlbumPath(albumId: string): string {
  return `/__musaic/reveal/album/${encodeURIComponent(albumId)}`;
}

async function reveal(path: string): Promise<void> {
  const res = await fetch(path, { method: "POST" });
  if (res.ok) return;
  let detail: unknown = null;
  try {
    detail = ((await res.json()) as { detail?: unknown } | null)?.detail;
  } catch {
    /* empty or non-JSON body */
  }
  throw new Error(typeof detail === "string" && detail ? detail : `Couldn't open the folder (${res.status})`);
}

/** Show an album's folder in the file manager. Rejects with the server's
 *  message (unknown album, folder not on disk, …). */
export function revealAlbumFolder(albumId: string): Promise<void> {
  return reveal(revealAlbumPath(albumId));
}

/** Show the app's trash folder in the file manager. */
export function revealTrash(): Promise<void> {
  return reveal(REVEAL_TRASH_PATH);
}
