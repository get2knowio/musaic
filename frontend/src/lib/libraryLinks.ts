/**
 * Addresses of the Library page (`pages/Library.tsx`). The URL holds the
 * browsing state:
 *
 *  - `/library/<id>`: an album open in the inspector;
 *  - `filter=missing-art|missing|compilations|needs-review`: the collection
 *    (none is All albums);
 *  - `q=<text>`: the search;
 *  - `sort=<field>-<dir>` and `view=list`: the user's preferences, which
 *    every move between collections keeps.
 *
 * The Sidebar, the empty states and (later) the native menus build their
 * links here so they agree on what each one clears.
 */
import { FILTERS_BY_KEY, isValidFilterKey, type FilterKey } from "./filters";

/** A collection of the library: All albums, or one of the filters. */
export type LibraryCollection = "all" | FilterKey;

export const LIBRARY_PATH = "/library";

/** The preferences kept by every collection link (and by All albums). */
const PREFERENCE_PARAMS = ["sort", "view"] as const;

function paramsOf(from: string): URLSearchParams {
  const i = from.indexOf("?");
  return new URLSearchParams(i >= 0 ? from.slice(i + 1) : "");
}

function build(p: URLSearchParams): string {
  const qs = p.toString();
  return `${LIBRARY_PATH}${qs ? `?${qs}` : ""}`;
}

/**
 * All albums: a true reset. No collection filter, no search and no open
 * inspector; the sort and the tiles/list choice of `from` (a Library URL,
 * by default none) are kept.
 */
export function allAlbumsUrl(from: string = LIBRARY_PATH): string {
  const old = paramsOf(from);
  const p = new URLSearchParams();
  for (const k of PREFERENCE_PARAMS) {
    const v = old.get(k);
    if (v) p.set(k, v);
  }
  return build(p);
}

/**
 * One collection, from the Library URL `from`: keeps its search, sort and
 * view, closes the inspector (a collection click should not reopen an
 * album the user closed earlier). `"all"` is the same as `allAlbumsUrl`.
 */
export function collectionUrl(collection: LibraryCollection, from: string = LIBRARY_PATH): string {
  if (collection === "all") return allAlbumsUrl(from);
  const p = paramsOf(from);
  p.set("filter", collection);
  return build(p);
}

/** `from` without its search (`q`); the collection and preferences stay. */
export function clearSearchUrl(from: string): string {
  const p = paramsOf(from);
  p.delete("q");
  return build(p);
}

/** `from` without its collection filter; the search and preferences stay. */
export function clearFilterUrl(from: string): string {
  const p = paramsOf(from);
  p.delete("filter");
  return build(p);
}

/** The collection a Library URL (or its query string) shows. */
export function collectionOf(urlOrSearch: string): LibraryCollection {
  const f = paramsOf(urlOrSearch).get("filter");
  return isValidFilterKey(f) ? f : "all";
}

/** The name a collection goes by in the Sidebar and the page header. */
export function collectionName(collection: LibraryCollection): string {
  return collection === "all" ? "All albums" : FILTERS_BY_KEY[collection].label;
}
