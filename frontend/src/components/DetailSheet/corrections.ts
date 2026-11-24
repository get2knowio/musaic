/**
 * The album panel's proposed corrections: where they come from, which are
 * selected, and the apply request they make.
 *
 * Two feeds produce a `Proposal` (lib/suggestions.ts):
 *
 * - `resolverProposal`: the resolver's MusicBrainz data for the album, with
 *   no job. Its cover previews through `GET /api/albums/{id}/cover-preview`.
 * - `jobProposal`: a finished job's result. Its cover previews through
 *   `GET /api/ai/jobs/{id}/cover-preview`.
 *
 * Both become the same rows (`correctionRows`, one comparison rule), shown
 * by the one diff component (CorrectionsDiff) and applied with the same
 * request (`correctionsPayload`). The panel owns the job bookkeeping (mark
 * applied / discarded).
 */
import { useState } from "react";
import { useQuery } from "@tanstack/react-query";

import { albumCoverPreviewUrl, fetchCoverPreview, jobCoverPreviewUrl, type CoverPreview } from "@/lib/coverPreview";
import { modelFreeCoverage } from "@/lib/aiGating";
import {
  COVER_ROW_KEY,
  correctionRows,
  correctionsPayload,
  type CorrectionRow,
  type Proposal,
} from "@/lib/suggestions";
import type { Album, AlbumResolution, ApplyPayload, Job, Track } from "@/lib/types";

/**
 * The resolver's proposal, or null when the album has no MusicBrainz
 * anchor. The cover is part of it only when `includeCover` (the album has
 * no cover, or the user asked for the MusicBrainz one) and the resolver's
 * Cover Art Archive cover clears the same confidence bar the research
 * synthesis uses.
 */
export function resolverProposal(
  resolution: AlbumResolution | null | undefined,
  { includeCover }: { includeCover: boolean },
): Proposal | null {
  if (!resolution || resolution.anchor === null) return null;
  const v = (f: { value: string } | null) => f?.value ?? null;
  const coverOk = includeCover && modelFreeCoverage(resolution).cover && !!resolution.cover_url;
  return {
    album: {
      title: v(resolution.canonical_title),
      album_artist: v(resolution.canonical_album_artist),
      year: v(resolution.canonical_year),
      genre: v(resolution.canonical_genre),
      album_artist_sort: v(resolution.canonical_album_artist_sort),
    },
    tracks: resolution.tracks.map((t) => ({
      filename: t.filename,
      fields: {
        tracknumber: v(t.canonical_tracknumber),
        title: v(t.canonical_title),
        artist: v(t.canonical_artist),
      },
    })),
    cover: coverOk
      ? { url: resolution.cover_url!.value, previewSrc: albumCoverPreviewUrl(resolution.album_id) }
      : null,
  };
}

/** A finished job's proposal, or null when the job has no result. */
export function jobProposal(job: Job): Proposal | null {
  const meta = job.result?.metadata;
  const cover = job.result?.cover;
  if (!meta && !cover) return null;
  return {
    album: { ...(meta?.suggestions ?? {}) },
    tracks: (meta?.tracks ?? []).map((t) => ({ filename: t.filename, fields: { ...t.suggestions } })),
    cover: cover?.cover_url ? { url: cover.cover_url, previewSrc: jobCoverPreviewUrl(job.id) } : null,
  };
}

/** Whether the resolver could propose the MusicBrainz cover for this album. */
export function resolverOffersCover(resolution: AlbumResolution | null | undefined): boolean {
  return !!resolution && resolution.anchor !== null && modelFreeCoverage(resolution).cover;
}

export type CoverPreviewState = CoverPreview | { status: "loading" };

export interface Corrections {
  /** Where the rows came from; null when nothing is under review. */
  source: "resolver" | "job" | null;
  /** The finished job under review (source "job"). */
  job: Job | null;
  rows: CorrectionRow[];
  isSelected: (key: string) => boolean;
  /** Whether the row's checkbox can be used (a cover only once previewed). */
  isSelectable: (key: string) => boolean;
  toggle: (key: string) => void;
  selectedCount: number;
  payload: () => ApplyPayload;
  /** The cover row's preview, when there is a cover row. */
  coverPreview: CoverPreviewState | null;
  retryCoverPreview: () => void;
}

export interface CorrectionsInput {
  album: Album;
  /** The album's active job (queued, running or finished-not-acted-on). */
  activeJob: Job | null;
  resolution: AlbumResolution | null | undefined;
  /** The album's tracks; undefined while they load (rows wait for them, so
   *  an apply never starts without its track rows). */
  tracks: Track[] | undefined;
  /** The user asked for the MusicBrainz cover (album already has one). */
  coverRequested: boolean;
  /** The user discarded the resolver's proposal for this album. */
  resolverDismissed: boolean;
  /** Missing album, or an apply is running: nothing is under review. */
  suppressed: boolean;
}

export function useCorrections({
  album,
  activeJob,
  resolution,
  tracks,
  coverRequested,
  resolverDismissed,
  suppressed,
}: CorrectionsInput): Corrections {
  const finishedJob = activeJob?.status === "completed" ? activeJob : null;
  const jobPending = activeJob?.status === "queued" || activeJob?.status === "running";
  const albumHasCover = album.has_embedded_cover || !!album.cover_file;

  let source: Corrections["source"] = null;
  let proposal: Proposal | null = null;
  if (!suppressed && tracks) {
    if (finishedJob) {
      source = "job";
      proposal = jobProposal(finishedJob);
    } else if (!jobPending && !resolverDismissed) {
      proposal = resolverProposal(resolution, { includeCover: !albumHasCover || coverRequested });
      if (proposal) source = "resolver";
    }
  }
  const rows = proposal && tracks ? correctionRows(proposal, album, tracks) : [];
  if (source === "resolver" && rows.length === 0) source = null;

  // Per-row ticks the user changed, for this album and this source. A new
  // album or a new job starts from the defaults again.
  const scope = `${album.id}|${source}|${finishedJob?.id ?? ""}`;
  const [overrides, setOverrides] = useState<{ scope: string; ticks: Map<string, boolean> }>({
    scope,
    ticks: new Map(),
  });
  const ticks = overrides.scope === scope ? overrides.ticks : new Map<string, boolean>();

  const coverRow = rows.find((r) => r.kind === "cover");
  const previewSrc = coverRow?.kind === "cover" ? coverRow.previewSrc : null;
  const preview = useQuery<CoverPreview>({
    queryKey: ["cover-preview", previewSrc],
    queryFn: ({ signal }) => fetchCoverPreview(previewSrc!, signal),
    enabled: !!previewSrc,
    // Not kept fresh by SSE: the server sends `no-store` and each fetch
    // goes out to the Cover Art Archive, so fetch once per row shown and
    // forget it when the row goes (Retry fetches again).
    staleTime: Infinity,
    gcTime: 0,
    retry: false,
  });
  const coverPreview: CoverPreviewState | null = previewSrc
    ? (preview.data ?? { status: "loading" })
    : null;

  const isSelectable = (key: string) =>
    key !== COVER_ROW_KEY || coverPreview?.status === "ok";
  const rowByKey = new Map(rows.map((r) => [r.key, r]));
  const isSelected = (key: string) => {
    const row = rowByKey.get(key);
    if (!row || !isSelectable(key)) return false;
    return ticks.get(key) ?? row.defaultSelected;
  };
  const toggle = (key: string) => {
    if (!isSelectable(key)) return;
    const next = new Map(ticks);
    next.set(key, !isSelected(key));
    setOverrides({ scope, ticks: next });
  };
  const selected = new Set(rows.filter((r) => isSelected(r.key)).map((r) => r.key));

  return {
    source,
    job: source === "job" ? finishedJob : null,
    rows,
    isSelected,
    isSelectable,
    toggle,
    selectedCount: selected.size,
    payload: () => correctionsPayload(rows, selected),
    coverPreview,
    retryCoverPreview: () => void preview.refetch(),
  };
}
