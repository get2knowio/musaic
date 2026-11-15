/**
 * App-level store of album apply runs, keyed by album id.
 *
 * The server finishes an apply even when the client disconnects
 * (rust/crates/musaic-api/src/routes/apply.rs), so the album panel does not
 * need to stay open while one runs. Keeping the stream and its progress
 * here, outside any component, lets the user switch albums or close the
 * panel mid-write and find the progress (or the result) again on reopening.
 *
 * Lifetime of an entry: it exists from `startApply` on. Once it has settled
 * (done or error) it is dropped when the last view of that album lets go of
 * it, so a result that arrived while nobody was looking is still shown once.
 */
import { useEffect, useSyncExternalStore } from "react";

import { postSse } from "@/lib/sse";
import type { Album, ApplyPayload, FileChange, TagDiff } from "@/lib/types";

export type FileStatus = "pending" | "active" | "done" | "error";

export interface ApplyFileState {
  index: number;
  filename: string;
  status: FileStatus;
  /** `new: null` = the tag is removed. */
  diffs: Record<string, TagDiff>;
  art_changed: boolean;
  saved: boolean;
  error: string | null;
  warnings: string[];
}

export interface ApplyState {
  status: "idle" | "downloading_cover" | "writing" | "done" | "error";
  fileCount: number;
  filesByIndex: Record<number, ApplyFileState>;
  scannedCount: number;          // files with status "done" or "error"
  savedCount: number;
  errors: number;
  artChanged: boolean;
  coverUrl: string | null;       // currently downloading
  error: string | null;
  updatedAlbum: Album | null;
  /** Every file's rewrite warnings, as "filename: text". */
  warnings: string[];
}

export const INITIAL_APPLY_STATE: ApplyState = {
  status: "idle",
  fileCount: 0,
  filesByIndex: {},
  scannedCount: 0,
  savedCount: 0,
  errors: 0,
  artChanged: false,
  coverUrl: null,
  error: null,
  updatedAlbum: null,
  warnings: [],
};

type Action =
  | { type: "begin" }
  | { type: "cover_downloading"; url: string }
  | { type: "cover_downloaded" }
  | { type: "start"; fileCount: number }
  | { type: "file_started"; index: number; filename: string }
  | { type: "file_done"; change: FileChange & { index?: number } }
  | { type: "done"; saved: number; errors: number; artChanged: number }
  | { type: "updated_album"; album: Album }
  | { type: "error"; detail: string };

function reducer(state: ApplyState, action: Action): ApplyState {
  switch (action.type) {
    case "begin":
      // In flight from the first moment, so the panel disables its apply
      // buttons before the server's first frame arrives.
      return { ...INITIAL_APPLY_STATE, status: "writing" };
    case "cover_downloading":
      return { ...state, status: "downloading_cover", coverUrl: action.url };
    case "cover_downloaded":
      return { ...state, coverUrl: null };
    case "start":
      return { ...state, status: "writing", fileCount: action.fileCount, filesByIndex: {} };
    case "file_started": {
      const next = { ...state.filesByIndex };
      next[action.index] = {
        index: action.index,
        filename: action.filename,
        status: "active",
        diffs: {},
        art_changed: false,
        saved: false,
        error: null,
        warnings: [],
      };
      return { ...state, filesByIndex: next };
    }
    case "file_done": {
      const next = { ...state.filesByIndex };
      // file_done events don't carry their index back from the backend, so
      // match by filename.
      const idx = Object.values(state.filesByIndex).find(
        (f) => f.filename === action.change.filename,
      )?.index;
      if (idx !== undefined) {
        next[idx] = {
          ...next[idx],
          status: action.change.error ? "error" : "done",
          diffs: action.change.diffs,
          art_changed: action.change.art_changed,
          saved: action.change.saved,
          error: action.change.error,
          warnings: action.change.warnings ?? [],
        };
      }
      const fileWarnings = (action.change.warnings ?? []).map(
        (w) => `${action.change.filename}: ${w}`,
      );
      return {
        ...state,
        filesByIndex: next,
        scannedCount: state.scannedCount + 1,
        warnings: fileWarnings.length ? [...state.warnings, ...fileWarnings] : state.warnings,
      };
    }
    case "done":
      return {
        ...state,
        status: "done",
        savedCount: action.saved,
        errors: action.errors,
        artChanged: action.artChanged > 0,
      };
    case "updated_album":
      return { ...state, updatedAlbum: action.album };
    case "error":
      return { ...state, status: "error", error: action.detail };
  }
}

const states = new Map<string, ApplyState>();
const listeners = new Set<() => void>();
/** How many mounted views show each album's apply state. */
const viewers = new Map<string, number>();

function emit() {
  for (const l of listeners) l();
}

function dispatch(albumId: string, action: Action) {
  states.set(albumId, reducer(states.get(albumId) ?? INITIAL_APPLY_STATE, action));
  emit();
}

export function getApplyState(albumId: string): ApplyState {
  return states.get(albumId) ?? INITIAL_APPLY_STATE;
}

export function isApplyInFlight(state: ApplyState): boolean {
  return state.status === "writing" || state.status === "downloading_cover";
}

export function subscribeApply(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

export interface StartApplyOptions {
  /** Called once the server reports the rewritten album, whether or not
   *  any view of the album is still mounted. */
  onUpdatedAlbum?: (album: Album) => void;
}

/**
 * Start an apply for `albumId` and stream its progress into the store.
 * Never aborted from the client: the server completes the run regardless,
 * and the store keeps reporting it.
 */
export async function startApply(
  albumId: string,
  payload: ApplyPayload,
  options: StartApplyOptions = {},
): Promise<void> {
  if (isApplyInFlight(getApplyState(albumId))) return;
  dispatch(albumId, { type: "begin" });
  const d = (action: Action) => dispatch(albumId, action);
  try {
    await postSse(
      `/api/albums/${encodeURIComponent(albumId)}/apply/stream`,
      payload,
      {
        cover_downloading: (data) => d({ type: "cover_downloading", url: (data as { url: string }).url }),
        cover_downloaded: () => d({ type: "cover_downloaded" }),
        start: (data) => d({ type: "start", fileCount: (data as { file_count: number }).file_count }),
        file_started: (data) => {
          const f = data as { index: number; filename: string };
          d({ type: "file_started", index: f.index, filename: f.filename });
        },
        file_done: (data) => d({ type: "file_done", change: data as FileChange }),
        done: (data) => {
          const r = data as { saved_count: number; errors: number; art_changed_count: number };
          d({ type: "done", saved: r.saved_count, errors: r.errors, artChanged: r.art_changed_count });
        },
        // No explicit tracks invalidation — the apply pipeline publishes
        // AlbumTagsChanged on completion, which useLibraryEventsStream maps
        // to ["tracks", album.id].
        updated_album: (data) => d({ type: "updated_album", album: data as Album }),
        removed_album: () => d({ type: "error", detail: "Album folder no longer exists" }),
        error: (data) => d({ type: "error", detail: (data as { detail: string }).detail }),
      },
    );
  } catch (err) {
    d({ type: "error", detail: (err as Error).message });
    return;
  }
  const final = getApplyState(albumId);
  if (final.status === "writing" || final.status === "downloading_cover") {
    // The stream ended without a terminal frame.
    d({ type: "error", detail: "The apply stream ended early; rescan to see the current tags." });
    return;
  }
  if (final.status === "done" && final.updatedAlbum) {
    options.onUpdatedAlbum?.(final.updatedAlbum);
  }
  // A settled result nobody is looking at stays until a view has shown it
  // and let go (`release`).
}

function retain(albumId: string) {
  viewers.set(albumId, (viewers.get(albumId) ?? 0) + 1);
}

function release(albumId: string) {
  const n = (viewers.get(albumId) ?? 1) - 1;
  if (n > 0) {
    viewers.set(albumId, n);
    return;
  }
  viewers.delete(albumId);
  // Deferred so a StrictMode unmount/remount (or a quick swap back) keeps
  // the entry; dropped only when no view has picked it up again.
  queueMicrotask(() => {
    if (viewers.has(albumId)) return;
    const s = states.get(albumId);
    if (s && !isApplyInFlight(s)) {
      states.delete(albumId);
      emit();
    }
  });
}

/**
 * The live apply state for `albumId`. While mounted, the caller counts as a
 * view of that album: a settled result is kept until the last view goes.
 */
export function useApplyState(albumId: string): ApplyState {
  useEffect(() => {
    retain(albumId);
    return () => release(albumId);
  }, [albumId]);
  return useSyncExternalStore(
    subscribeApply,
    () => getApplyState(albumId),
    () => getApplyState(albumId),
  );
}

/** Test-only: forget every entry. */
export function resetApplyRegistryForTests(): void {
  states.clear();
  viewers.clear();
  emit();
}
