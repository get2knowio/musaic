/**
 * Focus hand-offs between an album list (tiles or rows carrying
 * `data-album-id`) and the album panel docked beside it.
 *
 *  - Opening an album from the keyboard moves focus to the panel heading;
 *    a mouse click leaves focus where it is. The list asks with
 *    `requestPanelFocus(id)`; the panel showing `id` takes the request.
 *  - Closing returns focus to the album's tile, or to `<main>` when the
 *    tile is not rendered (filtered out, or the user is leaving the page).
 */

let pending: string | null = null;
const subscribers = new Set<(albumId: string) => void>();

/** Ask the panel to focus its heading once it shows `albumId`. */
export function requestPanelFocus(albumId: string): void {
  pending = albumId;
  for (const s of subscribers) s(albumId);
}

/** True when a focus request for `albumId` is waiting; clears any request. */
export function takePanelFocusRequest(albumId: string): boolean {
  const hit = pending === albumId;
  pending = null;
  return hit;
}

/** Be told of requests, e.g. for the album the panel already shows. */
export function subscribePanelFocus(cb: (albumId: string) => void): () => void {
  subscribers.add(cb);
  return () => {
    subscribers.delete(cb);
  };
}

export function albumTileSelector(albumId: string): string {
  return `[data-album-id="${albumId.replace(/["\\]/g, "\\$&")}"]`;
}

export function findAlbumTile(albumId: string, root: ParentNode = document): HTMLElement | null {
  return root.querySelector<HTMLElement>(albumTileSelector(albumId));
}

/** Focus `<main>`, making it programmatically focusable if needed. */
export function focusMain(): void {
  const main = document.querySelector<HTMLElement>("main");
  if (!main) return;
  if (!main.hasAttribute("tabindex")) main.setAttribute("tabindex", "-1");
  main.focus({ preventScroll: true });
}

export function focusAlbumTileOrMain(albumId: string): void {
  const tile = findAlbumTile(albumId);
  if (tile) tile.focus();
  else focusMain();
}
