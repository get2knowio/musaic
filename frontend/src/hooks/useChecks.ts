/**
 * Health-check hooks. Mirror the AI-jobs hook layer:
 *
 *   - `useChecks` / `useCheck` — registry + per-check detail
 *   - `useFindings` / `useFindingsForAlbum` / `useFindingsForCheck` — open findings
 *   - mutations for run / ignore / unignore / fix
 *
 * The `["checks"]` query is hydrated initially via REST, then kept fresh by
 * `useChecksStream` (App-level SSE subscriber). Findings live under
 * `["findings"]` and are kept fresh through the same SSE stream.
 */
import { useMemo } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import {
  deleteDuplicateFile,
  fixFinding,
  getCheck,
  getDuplicateFindingCanonical,
  ignoreFinding,
  listChecks,
  listFindings,
  runCheck,
  unignoreFinding,
} from "@/lib/api";
import type {
  CheckDetail,
  CheckSummary,
  Finding,
} from "@/lib/types";

export const CHECKS_QUERY_KEY = ["checks"] as const;
export const FINDINGS_QUERY_KEY = ["findings"] as const;
export const CHECK_DETAIL_KEY = (id: string) => ["check", id] as const;

// ---- Reads ----

export function useChecks() {
  return useQuery<CheckSummary[]>({
    queryKey: CHECKS_QUERY_KEY,
    queryFn: async () => (await listChecks()).checks,
    staleTime: Infinity,  // SSE keeps it fresh
  });
}

export function useCheck(checkId: string | null | undefined) {
  return useQuery<CheckDetail>({
    queryKey: CHECK_DETAIL_KEY(checkId ?? ""),
    queryFn: () => getCheck(checkId!),
    enabled: !!checkId,
    // The check detail isn't kept fresh by SSE today (we only push the
    // summary list). 5s staleTime keeps the page reactive without spamming
    // the server.
    staleTime: 5_000,
  });
}

export function useFindings() {
  return useQuery<Finding[]>({
    queryKey: FINDINGS_QUERY_KEY,
    queryFn: async () => (await listFindings()).findings,
    staleTime: Infinity,  // SSE keeps it fresh
  });
}

/** Open findings for a single album. Used by the detail-sheet section. */
export function useFindingsForAlbum(albumId: string | null | undefined): Finding[] {
  const { data: findings = [] } = useFindings();
  return useMemo(
    () => (albumId ? findings.filter((f) => f.album_id === albumId) : []),
    [albumId, findings],
  );
}

/** Open findings for a single check. Used by the Health detail page. */
export function useFindingsForCheck(checkId: string | null | undefined): Finding[] {
  const { data: findings = [] } = useFindings();
  return useMemo(
    () => (checkId ? findings.filter((f) => f.check_id === checkId) : []),
    [checkId, findings],
  );
}

// ---- Mutations ----

export function useRunCheck() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (args: { checkId: string; scope?: "all" | "incremental" | "albums"; albumIds?: string[] }) =>
      runCheck(args.checkId, { scope: args.scope ?? "all", albumIds: args.albumIds }),
    onSuccess: (_run, args) => {
      // SSE pushes the next snapshot, but invalidate so the detail page
      // re-fetches its sidebar of recent runs immediately.
      qc.invalidateQueries({ queryKey: CHECK_DETAIL_KEY(args.checkId) });
      qc.invalidateQueries({ queryKey: CHECKS_QUERY_KEY });
      qc.invalidateQueries({ queryKey: FINDINGS_QUERY_KEY });
    },
  });
}

export function useIgnoreFinding() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (args: { checkId: string; findingId: number; reason?: string }) =>
      ignoreFinding(args.checkId, args.findingId, args.reason),
    onSuccess: (_data, args) => {
      qc.invalidateQueries({ queryKey: FINDINGS_QUERY_KEY });
      qc.invalidateQueries({ queryKey: CHECKS_QUERY_KEY });
      qc.invalidateQueries({ queryKey: CHECK_DETAIL_KEY(args.checkId) });
    },
  });
}

export function useUnignoreFinding() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (args: { checkId: string; findingId: number }) =>
      unignoreFinding(args.checkId, args.findingId),
    onSuccess: (_data, args) => {
      qc.invalidateQueries({ queryKey: FINDINGS_QUERY_KEY });
      qc.invalidateQueries({ queryKey: CHECKS_QUERY_KEY });
      qc.invalidateQueries({ queryKey: CHECK_DETAIL_KEY(args.checkId) });
    },
  });
}

export function useFixFinding() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (args: { checkId: string; findingId: number; hint?: string }) =>
      fixFinding(args.checkId, args.findingId, args.hint ?? ""),
    onSuccess: (_data, args) => {
      qc.invalidateQueries({ queryKey: FINDINGS_QUERY_KEY });
      qc.invalidateQueries({ queryKey: CHECKS_QUERY_KEY });
      qc.invalidateQueries({ queryKey: CHECK_DETAIL_KEY(args.checkId) });
      // Fix may have enqueued an AI job; refresh the jobs cache so the
      // sidebar badge / activity indicator picks it up immediately.
      qc.invalidateQueries({ queryKey: ["jobs"] });
    },
  });
}

/** What MusicBrainz says the duplicate cluster's recording_mbid actually
 *  is. Lazy-fetched (one HTTP call to MB per MBID, cached server-side
 *  forever since recordings are immutable). Used by the resolve dialog
 *  to disambiguate which mistagged file is which. The query is gated on
 *  `enabled` so opening the dialog triggers the fetch but list rendering
 *  doesn't. */
export function useDuplicateFindingCanonical(
  findingId: number | null | undefined,
  enabled: boolean,
) {
  return useQuery({
    queryKey: ["duplicate-canonical", findingId],
    queryFn: () => getDuplicateFindingCanonical(findingId as number),
    enabled: enabled && findingId != null,
    // MB recordings don't change; keep this fresh for a long time so
    // repeat opens of the same dialog don't refetch.
    staleTime: 24 * 60 * 60 * 1000,  // 1 day
    gcTime: 7 * 24 * 60 * 60 * 1000, // keep in cache for a week
  });
}


/** Delete one of the duplicate files for a duplicate-tracks finding. The
 *  finding stays open until the cluster shrinks to <= 1 file; subsequent
 *  deletes hit the same hook. */
export function useDeleteDuplicateFile() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (args: { findingId: number; filename: string }) =>
      deleteDuplicateFile(args.findingId, args.filename),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: FINDINGS_QUERY_KEY });
      qc.invalidateQueries({ queryKey: CHECKS_QUERY_KEY });
      qc.invalidateQueries({ queryKey: CHECK_DETAIL_KEY("duplicate-tracks") });
    },
  });
}

// ---- Bulk quick-fix (MB only) -----------------------------------------

/**
 * One progress event from the quick-fix-all SSE stream. Mirrors the
 * backend's per-finding "progress" event payload.
 */
export interface QuickFixProgress {
  finding_id: number;
  outcome: "fixed" | "skipped" | "error" | "not-applicable";
  message: string;
}

export interface QuickFixSummary {
  total: number;
  fixed: number;
  skipped: number;
  errors: number;
  not_applicable: number;
}

/**
 * Stream the bulk quick-fix-all run. Returns an EventSource handle plus
 * cleanup, with callbacks fired as events arrive. The caller drives the UI
 * — we don't push into TanStack here because the cache invalidation is
 * better done after the stream ends (otherwise we'd refetch on every
 * single fixed finding).
 *
 * Returns a cancel function the caller can call to abort the stream
 * (useful for "Cancel" button mid-run).
 */
export function streamQuickFixAll(
  checkId: string,
  callbacks: {
    onStart?: (total: number) => void;
    onProgress?: (p: QuickFixProgress) => void;
    onDone?: (summary: QuickFixSummary) => void;
    onError?: (detail: string) => void;
  },
): () => void {
  // Browser EventSource only supports GET. We need a POST. fetch() with
  // streaming body parsing handles SSE manually but reliably.
  const ctrl = new AbortController();
  (async () => {
    try {
      const resp = await fetch(`/api/checks/${encodeURIComponent(checkId)}/quick-fix-all`, {
        method: "POST",
        signal: ctrl.signal,
        headers: { "Accept": "text/event-stream" },
      });
      if (!resp.ok || !resp.body) {
        callbacks.onError?.(`HTTP ${resp.status}`);
        return;
      }
      const reader = resp.body.getReader();
      const decoder = new TextDecoder();
      let buf = "";
      while (true) {
        const { value, done } = await reader.read();
        if (done) break;
        buf += decoder.decode(value, { stream: true });
        // SSE blocks are separated by \n\n.
        let idx;
        while ((idx = buf.indexOf("\n\n")) !== -1) {
          const block = buf.slice(0, idx);
          buf = buf.slice(idx + 2);
          parseBlock(block, callbacks);
        }
      }
    } catch (e) {
      if ((e as Error).name === "AbortError") return;
      callbacks.onError?.((e as Error).message);
    }
  })();
  return () => ctrl.abort();
}

function parseBlock(block: string, callbacks: {
  onStart?: (total: number) => void;
  onProgress?: (p: QuickFixProgress) => void;
  onDone?: (summary: QuickFixSummary) => void;
  onError?: (detail: string) => void;
}): void {
  let event = "message";
  const dataLines: string[] = [];
  for (const line of block.split("\n")) {
    if (line.startsWith("event: ")) event = line.slice(7).trim();
    else if (line.startsWith("data: ")) dataLines.push(line.slice(6));
    // Ignore other lines (id:, retry:, comments).
  }
  if (!dataLines.length) return;
  let payload: any;
  try {
    payload = JSON.parse(dataLines.join("\n"));
  } catch {
    return;
  }
  if (event === "start") callbacks.onStart?.(payload.total);
  else if (event === "progress") callbacks.onProgress?.(payload);
  else if (event === "done") callbacks.onDone?.(payload);
  else if (event === "error") callbacks.onError?.(payload.detail || "stream error");
}
