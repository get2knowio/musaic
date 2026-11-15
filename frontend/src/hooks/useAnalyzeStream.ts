/**
 * useAnalyzeStream — drives the SSE audio-analysis flow.
 *
 * Returns a `start(albumId, what)` function that opens an SSE POST to
 * /api/albums/{id}/analyze/stream and surfaces per-track progress.
 * `what` is "bpm" or "replaygain"; the same hook drives both since the
 * event sequence is identical apart from the per-file payload shape.
 *
 * On `done`, the album's tracks query is invalidated so the new BPM /
 * RG values land in the track list without a manual reload.
 */

import { useCallback, useReducer, useRef } from "react";
import { postSse } from "@/lib/sse";

export type AnalysisKind = "bpm" | "replaygain";

export type AnalyzeStatus = "idle" | "running" | "done" | "error";

export interface AnalyzeFileState {
  index: number;
  filename: string;
  status: "active" | "done" | "error";
  /** When done, the value(s) computed for this track. */
  bpm?: number;
  rgGain?: string;
  rgPeak?: string;
  error?: string;
}

export interface AnalyzeState {
  status: AnalyzeStatus;
  what: AnalysisKind | null;
  fileCount: number;
  filesByIndex: Record<number, AnalyzeFileState>;
  scannedCount: number;
  errors: number;
  albumGain: string | null;
  albumPeak: string | null;
  error: string | null;
  /** Rewrite warnings from the tag writes, as "filename: text" (see
   *  FileChange.warnings). */
  warnings: string[];
}

interface FileDonePayload {
  index: number;
  filename: string;
  bpm?: number;
  replaygain_track_gain?: string;
  replaygain_track_peak?: string;
  error?: string;
  warnings?: string[];
}

const INITIAL: AnalyzeState = {
  status: "idle",
  what: null,
  fileCount: 0,
  filesByIndex: {},
  scannedCount: 0,
  errors: 0,
  albumGain: null,
  albumPeak: null,
  error: null,
  warnings: [],
};

type Action =
  | { type: "reset" }
  | { type: "start"; what: AnalysisKind; fileCount: number }
  | { type: "file_started"; index: number; filename: string }
  | { type: "file_done"; payload: FileDonePayload }
  | { type: "album_done"; gain: string; peak: string; warnings: string[] }
  | { type: "done"; errors: number }
  | { type: "error"; detail: string; what?: AnalysisKind };

function reducer(state: AnalyzeState, action: Action): AnalyzeState {
  switch (action.type) {
    case "reset":
      return INITIAL;
    case "start":
      return {
        ...INITIAL,
        status: "running",
        what: action.what,
        fileCount: action.fileCount,
      };
    case "file_started": {
      const next = { ...state.filesByIndex };
      next[action.index] = {
        index: action.index,
        filename: action.filename,
        status: "active",
      };
      return { ...state, filesByIndex: next };
    }
    case "file_done": {
      const next = { ...state.filesByIndex };
      const cur = next[action.payload.index] ?? {
        index: action.payload.index,
        filename: action.payload.filename,
        status: "active" as const,
      };
      next[action.payload.index] = {
        ...cur,
        status: action.payload.error ? "error" : "done",
        bpm: action.payload.bpm,
        rgGain: action.payload.replaygain_track_gain,
        rgPeak: action.payload.replaygain_track_peak,
        error: action.payload.error,
      };
      const fileWarnings = (action.payload.warnings ?? []).map(
        (w) => `${action.payload.filename}: ${w}`,
      );
      return {
        ...state,
        filesByIndex: next,
        scannedCount: state.scannedCount + 1,
        warnings: fileWarnings.length ? [...state.warnings, ...fileWarnings] : state.warnings,
      };
    }
    case "album_done":
      return {
        ...state,
        albumGain: action.gain,
        albumPeak: action.peak,
        // Already "filename: text" from the server (one event for all files).
        warnings: action.warnings.length ? [...state.warnings, ...action.warnings] : state.warnings,
      };
    case "done":
      return { ...state, status: "done", errors: action.errors };
    case "error":
      // A refusal before the stream starts (409: album Missing, restore
      // running, folder gone) arrives with no `start` event, so `what`
      // is still null; take it from the action so the card that was
      // clicked can show the server's message.
      return {
        ...state,
        status: "error",
        error: action.detail,
        what: state.what ?? action.what ?? null,
      };
  }
}

export interface UseAnalyzeStreamResult {
  state: AnalyzeState;
  start: (albumId: string, what: AnalysisKind) => Promise<void>;
  reset: () => void;
}

export function useAnalyzeStream(): UseAnalyzeStreamResult {
  const [state, dispatch] = useReducer(reducer, INITIAL);
  const abortRef = useRef<AbortController | null>(null);

  const reset = useCallback(() => {
    abortRef.current?.abort();
    abortRef.current = null;
    dispatch({ type: "reset" });
  }, []);

  const start = useCallback(
    async (albumId: string, what: AnalysisKind) => {
      reset();
      const abort = new AbortController();
      abortRef.current = abort;

      try {
        await postSse(
          `/api/albums/${encodeURIComponent(albumId)}/analyze/stream`,
          { what },
          {
            start: (data) => {
              const d = data as { what: AnalysisKind; file_count: number };
              dispatch({ type: "start", what: d.what, fileCount: d.file_count });
            },
            file_started: (data) => {
              const d = data as { index: number; filename: string };
              dispatch({ type: "file_started", index: d.index, filename: d.filename });
            },
            file_done: (data) => {
              dispatch({ type: "file_done", payload: data as FileDonePayload });
            },
            album_done: (data) => {
              const d = data as {
                replaygain_album_gain: string;
                replaygain_album_peak: string;
                warnings?: string[];
              };
              dispatch({
                type: "album_done",
                gain: d.replaygain_album_gain,
                peak: d.replaygain_album_peak,
                warnings: d.warnings ?? [],
              });
            },
            updated_album: () => {
              // Album row may have shifted (mtime); tracks refetch is
              // handled by useLibraryEventsStream — audio_analysis
              // publishes AlbumTagsChanged after writing.
            },
            done: (data) => {
              const d = data as { errors: number };
              dispatch({ type: "done", errors: d.errors });
            },
            error: (data) => dispatch({ type: "error", detail: (data as { detail: string }).detail }),
          },
          abort.signal,
        );
      } catch (err) {
        if ((err as Error).name === "AbortError") return;
        dispatch({ type: "error", detail: (err as Error).message, what });
      } finally {
        abortRef.current = null;
      }
    },
    [reset],
  );

  return { state, start, reset };
}
