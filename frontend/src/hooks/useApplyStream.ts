/**
 * useApplyStream — one album's view of the SSE apply flow.
 *
 * The stream and its state live in the app-level apply registry
 * (lib/applyRegistry.ts), keyed by album id, so an apply keeps reporting
 * while the album panel shows another album or is closed, and its progress
 * reappears when the album is opened again. ApplyProgress and TrackList read
 * `state`.
 */

import { useCallback } from "react";

import {
  startApply,
  useApplyState,
  type ApplyState,
  type StartApplyOptions,
} from "@/lib/applyRegistry";
import type { ApplyPayload } from "@/lib/types";

export type { ApplyFileState, ApplyState, FileStatus } from "@/lib/applyRegistry";

export interface UseApplyStreamResult {
  state: ApplyState;
  start: (payload: ApplyPayload, options?: StartApplyOptions) => Promise<void>;
}

export function useApplyStream(albumId: string): UseApplyStreamResult {
  const state = useApplyState(albumId);
  const start = useCallback(
    (payload: ApplyPayload, options?: StartApplyOptions) => startApply(albumId, payload, options),
    [albumId],
  );
  return { state, start };
}
