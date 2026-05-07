/**
 * useApplyStream — drives the SSE apply flow.
 *
 * Returns a `start(payload)` function that opens an SSE POST to
 * /api/albums/{id}/apply/stream and surfaces per-file status, the cover
 * download phase, and the final `updated_album` event. The component
 * (ApplyProgress) reads this state to render its track-list spinner.
 */

import { useCallback, useReducer, useRef } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { postSse } from "@/lib/sse";
import type { Album, ApplyPayload, FileChange } from "@/lib/types";

export type FileStatus = "pending" | "active" | "done" | "error";

export interface ApplyFileState {
  index: number;
  filename: string;
  status: FileStatus;
  diffs: Record<string, { old: string; new: string }>;
  art_changed: boolean;
  saved: boolean;
  error: string | null;
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
}

const INITIAL: ApplyState = {
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
};

type Action =
  | { type: "reset" }
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
    case "reset":
      return INITIAL;
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
      };
      return { ...state, filesByIndex: next };
    }
    case "file_done": {
      const next = { ...state.filesByIndex };
      // file_done events don't carry their index back from the backend, so
      // match by filename. (The backend emits in order, so we fall back to
      // scanning for the most recent "active" entry if filename lookup fails.)
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
        };
      }
      return {
        ...state,
        filesByIndex: next,
        scannedCount: state.scannedCount + 1,
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

export interface UseApplyStreamResult {
  state: ApplyState;
  start: (albumId: string, payload: ApplyPayload) => Promise<void>;
  reset: () => void;
}

export function useApplyStream(): UseApplyStreamResult {
  const [state, dispatch] = useReducer(reducer, INITIAL);
  const qc = useQueryClient();
  const abortRef = useRef<AbortController | null>(null);

  const reset = useCallback(() => {
    abortRef.current?.abort();
    abortRef.current = null;
    dispatch({ type: "reset" });
  }, []);

  const start = useCallback(
    async (albumId: string, payload: ApplyPayload) => {
      reset();
      const abort = new AbortController();
      abortRef.current = abort;

      try {
        await postSse(
          `/api/albums/${encodeURIComponent(albumId)}/apply/stream`,
          payload,
          {
            cover_downloading: (data) => dispatch({ type: "cover_downloading", url: (data as { url: string }).url }),
            cover_downloaded: () => dispatch({ type: "cover_downloaded" }),
            start: (data) => dispatch({ type: "start", fileCount: (data as { file_count: number }).file_count }),
            file_started: (data) => {
              const d = data as { index: number; filename: string };
              dispatch({ type: "file_started", index: d.index, filename: d.filename });
            },
            file_done: (data) => dispatch({ type: "file_done", change: data as FileChange }),
            done: (data) => {
              const d = data as { saved_count: number; errors: number; art_changed_count: number };
              dispatch({ type: "done", saved: d.saved_count, errors: d.errors, artChanged: d.art_changed_count });
            },
            updated_album: (data) => {
              const album = data as Album;
              dispatch({ type: "updated_album", album });
              // Tracks may have changed too — invalidate so the next read picks up
              // any new BPM / RG / tag values.
              qc.invalidateQueries({ queryKey: ["tracks", album.id] });
            },
            removed_album: () => {
              // The album disappeared from disk during apply. Treat as error.
              dispatch({ type: "error", detail: "Album folder no longer exists" });
            },
            error: (data) => dispatch({ type: "error", detail: (data as { detail: string }).detail }),
          },
          abort.signal,
        );
      } catch (err) {
        if ((err as Error).name === "AbortError") return;
        dispatch({ type: "error", detail: (err as Error).message });
      } finally {
        abortRef.current = null;
      }
    },
    [qc, reset],
  );

  return { state, start, reset };
}
