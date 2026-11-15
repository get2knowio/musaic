import { AlertTriangle, CloudOff, Image as ImageIcon, Loader2, RotateCcw, Sparkles, Trash2, X } from "lucide-react";
import { useEffect, useId, useRef, useState, type Ref } from "react";
import { Link } from "react-router-dom";
import { toast } from "sonner";

import AlbumHero from "./AlbumHero";
import PropTable from "./PropTable";
import AudioAnalysis from "./AudioAnalysis";
import TrackList from "./TrackList";
import AISuggestions from "./AISuggestions";
import ApplyProgress from "./ApplyProgress";
import FindingsSection from "./FindingsSection";
import MoreMenu from "./MoreMenu";
import MyTagsSection from "./MyTagsSection";
import Section from "./Section";
import HintDialog from "./HintDialog";
import { subscribePanelFocus, takePanelFocusRequest } from "./panelFocus";
import { PANEL_MIN_WIDTH, panelFillsRow } from "./layout";

import { useTracks } from "@/hooks/useTracks";
import {
  useActiveJobForAlbum,
  useDeleteJob,
  useEnqueueJob,
  useJobs,
  useMarkJobActedOn,
  usePriorCoverAttempts,
  useRetryJob,
} from "@/hooks/useJobs";
import { useAlbumResolution } from "@/hooks/useAlbumResolution";
import { useAiStatus } from "@/hooks/useAiStatus";
import {
  canQueueJob,
  metadataJobType,
  modelFreeCoverage,
  offersHintEntry,
} from "@/lib/aiGating";
import { useApplyStream } from "@/hooks/useApplyStream";
import { useResizableWidth } from "@/hooks/useResizableWidth";
import { clearAlbumMbAnchor, removeAlbumFromLibrary } from "@/lib/api";
import { isApplyInFlight } from "@/lib/applyRegistry";
import { jobCoverPreviewUrl } from "@/lib/coverPreview";
import type {
  Album,
  ApplyPayload,
  CoverSuggestionResponse,
  JobConflictDetail,
  JobType,
  MetadataSuggestionResponse,
} from "@/lib/types";

export interface AlbumPanelProps {
  /** The album to show. */
  album: Album;
  /** Close the panel (the owner decides where that leads). */
  onClose: () => void;
  /** Called when an apply rewrote the album, so the owner can update its list. */
  onAlbumUpdated?: (album: Album) => void;
  /** Width of the row the panel is docked in, when known. */
  availableWidth?: number | null;
  /** Px the list beside the panel keeps however wide the panel is dragged. */
  listMinWidth?: number;
  /** The panel's `<aside>`, for the owner's F6 handling. */
  ref?: Ref<HTMLElement>;
}

export const ALBUM_PANEL_ID = "album-panel";

/**
 * Album panel: a non-modal, resizable `<aside>` docked on the right of a
 * list. The list beside it stays interactive; showing another album swaps
 * the content. Knows nothing about routing — the owner passes the album and
 * what closing means — so it can sit beside any album list.
 *
 * Esc closes it only while focus is inside it, and only when nothing inside
 * (a menu, a tag picker, a dialog opened from it) has already used the key.
 * Dialogs opened from the panel are ordinary Radix modals over the window.
 *
 * When the row is too narrow for both the list's minimum and the panel's,
 * the panel covers the whole row (the owner positions the row `relative`
 * and keeps the list mounted, inert, underneath) and has no resize handle.
 */
export default function AlbumPanel({
  album,
  onClose,
  onAlbumUpdated,
  availableWidth = null,
  listMinWidth = 320,
  ref,
}: AlbumPanelProps) {
  const headingId = useId();
  const headingRef = useRef<HTMLHeadingElement>(null);

  // Drag-to-resize. Width persisted across sessions via localStorage.
  const { width, min, max, resizing, handleProps } = useResizableWidth({
    storageKey: "mm.sheet.width",
    defaultWidth: 480,
    minWidth: PANEL_MIN_WIDTH,
    maxWidth: 1200,
    available: availableWidth,
    reserve: listMinWidth,
  });

  // Opened from the keyboard: move focus to the heading.
  useEffect(() => {
    if (takePanelFocusRequest(album.id)) headingRef.current?.focus();
  }, [album.id]);
  useEffect(
    () =>
      subscribePanelFocus((id) => {
        if (id === album.id && takePanelFocusRequest(id)) headingRef.current?.focus();
      }),
    [album.id],
  );

  const fillsRow = panelFillsRow(availableWidth, listMinWidth);

  // Covering the row inerts the list. Focus left on a tile there (a mouse
  // open, or the window narrowed while the grid had focus) would be stranded
  // where no key reaches the panel, so bring it to the heading.
  useEffect(() => {
    if (!fillsRow) return;
    const heading = headingRef.current;
    const active = document.activeElement;
    if (!heading || heading.closest("aside")?.contains(active)) return;
    if (active?.closest('[role="dialog"], [role="alertdialog"]')) return;
    heading.focus({ preventScroll: true });
  }, [fillsRow, album.id]);

  const onKeyDown = (e: React.KeyboardEvent<HTMLElement>) => {
    if (e.key !== "Escape" || e.defaultPrevented) return;
    // React bubbles events out of portals; a dialog opened from the panel
    // is not inside it in the DOM.
    if (!e.currentTarget.contains(e.target as Node)) return;
    e.preventDefault();
    onClose();
  };

  return (
    <aside
      ref={ref}
      id={ALBUM_PANEL_ID}
      aria-labelledby={headingId}
      data-fills-row={fillsRow ? "" : undefined}
      className={`album-panel min-h-0 flex flex-col bg-panel ${
        fillsRow ? "absolute inset-0 z-10" : "relative shrink-0 border-l border-line"
      }`}
      style={fillsRow ? undefined : { width: `${width}px` }}
      onKeyDown={onKeyDown}
    >
      {/* Resize handle on the left edge — anchored absolutely so it sits on
          top of the border without affecting flex layout. Wider hit area
          than the visual hairline so it's easy to grab. */}
      {!fillsRow && <div
        className={`absolute top-0 left-0 bottom-0 w-1.5 -translate-x-1/2 cursor-col-resize z-10 group outline-none ${
          resizing ? "" : "transition-colors"
        }`}
        {...handleProps}
        role="separator"
        tabIndex={0}
        aria-orientation="vertical"
        aria-label="Resize album panel"
        aria-controls={ALBUM_PANEL_ID}
        aria-valuenow={Math.round(width)}
        aria-valuemin={min}
        aria-valuemax={Math.round(max)}
        title="Drag to resize; double-click to reset"
      >
        {/* Hairline that highlights on hover / focus / during drag */}
        <div
          className={`absolute top-0 bottom-0 left-1/2 -translate-x-1/2 w-px ${
            resizing
              ? "bg-accent w-0.5"
              : "bg-transparent group-hover:bg-accent/60 group-focus-visible:bg-accent group-focus-visible:w-0.5"
          }`}
        />
      </div>}
      <SheetBody
        album={album}
        onAlbumUpdated={onAlbumUpdated}
        onClose={onClose}
        headingId={headingId}
        headingRef={headingRef}
      />
    </aside>
  );
}

function SheetBody({
  album,
  onAlbumUpdated,
  onClose,
  headingId,
  headingRef,
}: {
  album: Album;
  onAlbumUpdated?: (a: Album) => void;
  onClose: () => void;
  headingId: string;
  headingRef: Ref<HTMLHeadingElement>;
}) {
  const tracks = useTracks(album.id);
  // Lives in the app-level apply registry: switching albums or closing the
  // panel mid-write is safe (the server finishes the apply regardless), and
  // the progress is back when this album is opened again.
  const apply = useApplyStream(album.id);
  const enqueue = useEnqueueJob();
  const markActedOn = useMarkJobActedOn();
  const deleteJob = useDeleteJob();
  const retryJob = useRetryJob();
  const { data: allJobs = [] } = useJobs();

  // The active job for THIS album — drives the suggestion display. Sourced
  // from the live jobs cache (kept fresh by the App-level useJobsStream
  // SSE subscriber), so close/reopen of the sheet preserves state and the
  // panel updates the moment the worker finishes.
  const activeJob = useActiveJobForAlbum(album.id);

  // Past cover-art lookups for this album that didn't yield a cover —
  // surfaced in the HintDialog when the user is about to enqueue another
  // cover lookup, so they don't unintentionally repeat past dead ends.
  const priorCoverAttempts = usePriorCoverAttempts(album.id);

  // Deterministic resolver output. When loaded with a confident anchor +
  // 0 unresolved tracks, the worker will skip the AI call entirely —
  // we mirror that in the button label so the user knows the click will
  // be free + fast rather than the usual long AI round-trip.
  const { data: resolution } = useAlbumResolution(album.id);
  // Same thresholds as the worker's short-circuit (lib/aiGating.ts).
  const coverage = modelFreeCoverage(resolution);
  const deterministicCovered = coverage.metadata;
  // CAA cover already verified for the anchored release-group? The
  // worker will short-circuit ai.suggest_cover when this is true.
  const coverFromMb = coverage.cover;
  // Whether a model backend can run. Without one, only jobs the resolver
  // covers may be queued, and they queue without the hint dialog.
  const ai = useAiStatus();

  // Whether the album has any cover at all — drives auto-bundling. If
  // "Suggest fixes" is clicked on a coverless album, we enqueue a
  // metadata+cover job instead of metadata-only.
  const albumHasCover = album.has_embedded_cover || !!album.cover_file;
  const metadataJob = metadataJobType(albumHasCover, coverage, ai);
  const canQueueMetadata = canQueueJob(metadataJob, coverage, ai);
  const canQueueCover = canQueueJob("cover", coverage, ai);

  // Hint dialog state. `pendingType` is the job type the user is about to
  // enqueue once they submit a hint.
  const [pendingType, setPendingType] = useState<JobType | null>(null);

  // Reset per-album local state when switching albums.
  useEffect(() => {
    setPendingType(null);
  }, [album.id]);

  // Start an apply. When it finishes successfully — even if the panel has
  // since moved to another album or closed — propagate the updated album to
  // the owner (so the tile re-renders with new cover/title/etc.) and mark
  // the job that was active when it started as acted-on (action="applied")
  // so it leaves the active queue and the "needs review" badge.
  //
  // Cache invalidation is handled by the App-level useLibraryEventsStream
  // subscriber — the apply pipeline publishes AlbumTagsChanged (+
  // AlbumCoverChanged when art changed), which fans out to
  // ["albums-snapshot"], ["tracks", id], ["album-resolution", id],
  // ["findings"], ["checks"] on its own.
  const startApply = (payload: ApplyPayload) => {
    const jobId = activeJob?.id ?? null;
    return apply.start(payload, {
      onUpdatedAlbum: (updated) => {
        onAlbumUpdated?.(updated);
        if (jobId) markActedOn.mutate({ jobId, action: "applied" });
      },
    });
  };

  // Map the active job's result back into the legacy AISuggestions component
  // shape. AISuggestions takes a metadata response + cover response separately.
  const metaResult: MetadataSuggestionResponse | null =
    activeJob?.status === "completed" && activeJob.result?.metadata
      ? {
          album_id: album.id,
          suggestions: activeJob.result.metadata.suggestions,
          tracks: activeJob.result.metadata.tracks,
          notes: activeJob.result.metadata.notes,
          confidence: activeJob.result.metadata.confidence,
          raw_excerpt: "",
          // MBID flows through to the AISuggestions UI so it can render
          // the "identified on MusicBrainz" status line. The MBID itself
          // has already been persisted server-side by the time we get
          // here (worker side-effect on job completion).
          mb_release_group_mbid: activeJob.result.metadata.mb_release_group_mbid ?? null,
        }
      : null;
  const coverResult: CoverSuggestionResponse | null =
    activeJob?.status === "completed" && activeJob.result?.cover
      ? {
          album_id: album.id,
          cover_url: activeJob.result.cover.cover_url,
          source: activeJob.result.cover.source,
          notes: activeJob.result.cover.notes,
          confidence: activeJob.result.cover.confidence,
          raw_excerpt: "",
        }
      : null;

  // Mode derivation
  const isApplying = isApplyInFlight(apply.state);
  const isQueued = activeJob?.status === "queued" || activeJob?.status === "running";
  const showSuggestions = !isApplying && activeJob?.status === "completed" && (metaResult || coverResult);
  const showApplying = isApplying || apply.state.status === "done" || apply.state.status === "error";

  // Most recent failed job for this album that hasn't been acted on. Surfaced
  // in the banner so the user can retry / inspect the error inline rather
  // than navigating to /jobs and hunting the row.
  const recentFailedJob =
    !activeJob && !isApplying
      ? allJobs
          .filter((j) => j.album_id === album.id && j.status === "failed")
          .sort((a, b) => (a.created_at < b.created_at ? 1 : -1))[0]
      : null;

  const onCancelQueued = async () => {
    if (!activeJob) return;
    try {
      await deleteJob.mutateAsync(activeJob.id);
      toast("Cancelled queued job", { duration: 2500 });
    } catch (err) {
      const e = err as Error & { status?: number };
      if (e.status === 409) {
        toast.warning("Job already started — can't cancel mid-run.");
      } else {
        toast.error(e.message || "Couldn't cancel job");
      }
    }
  };

  const onRetryFailed = async (jobId: string) => {
    try {
      await retryJob.mutateAsync(jobId);
      toast.success("Retrying…", { duration: 2500 });
    } catch (err) {
      const e = err as Error;
      toast.error(e.message || "Couldn't retry job");
    }
  };
  // Retrying a job that needs a model is certain to fail without one.
  const canRetryFailed =
    !recentFailedJob || canQueueJob(recentFailedJob.job_type, coverage, ai);

  // Live album for hero — use the apply-updated version once available.
  const displayAlbum: Album = apply.state.updatedAlbum ?? album;

  // ---- Click handlers ----

  const onSuggestMetadata = () => {
    // Auto-bundle cover lookup when album has no cover (metadataJobType).
    if (!canQueueMetadata) return;
    if (offersHintEntry(ai)) setPendingType(metadataJob);
    else void queueJob(metadataJob, "", true);
  };
  const onSuggestCover = () => {
    if (!canQueueCover) return;
    if (offersHintEntry(ai)) setPendingType("cover");
    else void queueJob("cover", "", true);
  };

  const onHintSubmit = async (hint: string, attachCover: boolean) => {
    const which = pendingType;
    setPendingType(null);
    if (!which) return;
    await queueJob(which, hint, attachCover);
  };

  const queueJob = async (which: JobType, hint: string, attachCover: boolean) => {
    try {
      await enqueue.mutateAsync({
        albumId: album.id, jobType: which, hint, attachCover,
      });
      const label =
        which === "metadata" ? "Metadata"
        : which === "cover" ? "Cover art"
        : "Metadata + cover";
      toast(`${label} queued`, { duration: 3000 });
    } catch (err) {
      // 409 = album already has an active job.
      const e = err as Error & { status?: number };
      const detail = (err as { detail?: JobConflictDetail }).detail;
      if (e.status === 409 || (typeof detail === "object" && detail?.error === "active_job_exists")) {
        toast.warning("Already queued for this album", { duration: 4000 });
      } else {
        toast.error("Could not queue job", {
          description: (err as Error).message || undefined,
          duration: 4000,
        });
      }
    }
  };

  const onHintCancel = () => setPendingType(null);

  const onApply = async () => {
    const trackSuggestions: Record<string, Record<string, string>> = {};
    for (const entry of metaResult?.tracks ?? []) {
      if (Object.keys(entry.suggestions).length > 0) {
        trackSuggestions[entry.filename] = entry.suggestions as Record<string, string>;
      }
    }
    await startApply({
      suggestions: metaResult?.suggestions ?? {},
      track_suggestions: trackSuggestions,
      cover_url: coverResult?.cover_url ?? null,
    });
    // The job is marked acted-on when the apply reports the updated album.
  };

  const onDiscard = () => {
    if (activeJob) {
      markActedOn.mutate({ jobId: activeJob.id, action: "discarded" });
    }
  };

  // v15 Missing-state — when the scanner doesn't see this album's
  // dir on disk, `missing_since` is set. Sheet swaps in a banner +
  // "Remove from library" footer; mutation affordances disable
  // (every read/write would fail at the FS layer).
  const isMissing = !!displayAlbum.missing_since;

  // Explicit user-initiated removal — the only path that actually
  // drops the row + cascades fingerprints/tags/blocklists. After
  // success, close the panel.
  const [removing, setRemoving] = useState(false);
  const onRemoveFromLibrary = async () => {
    if (removing) return;
    const ok = window.confirm(
      `Remove "${displayAlbum.title || displayAlbum.dir_name}" from the library?\n\n` +
      `This drops the cached metadata, MusicBrainz match, AcoustID fingerprints, ` +
      `and any per-album blocklist entries. The FLAC files (if they ever return ` +
      `to disk) will be re-scanned from scratch.\n\n` +
      `If this is a transient disk issue, click Cancel and re-mount the drive ` +
      `instead — the album will return automatically on the next scan.`,
    );
    if (!ok) return;
    setRemoving(true);
    try {
      await removeAlbumFromLibrary(album.id);
      toast.success("Removed from library", { duration: 3000 });
      // The album_id is gone from the snapshot now; useLibraryEvents-
      // Stream's AlbumDeleted handler already invalidates ['albums-
      // snapshot'], so closing lands on the refreshed list with this
      // album absent.
      onClose();
    } catch (err) {
      toast.error((err as Error).message || "Couldn't remove album");
    } finally {
      setRemoving(false);
    }
  };

  // "Wrong match" escape hatch: clears the cached MB anchor so the
  // synthesizer stops returning the same wrong cover/title from a
  // stale release-group. Discards the current suggestions in the same
  // step so the user immediately sees the standard sheet (with empty
  // resolution) and can re-trigger AI — typically with a hint.
  const [clearingAnchor, setClearingAnchor] = useState(false);
  const onClearAnchor = async () => {
    setClearingAnchor(true);
    try {
      const res = await clearAlbumMbAnchor(album.id);
      // Cache invalidation is handled by the App-level
      // useLibraryEventsStream subscriber — the backend publishes
      // AlbumTagsChanged after wiping the anchor.
      onDiscard();
      const rerun = ai.ready ? " Re-run AI (a hint helps narrow the search)." : "";
      const toastMsg = res.blocked_mbid
        ? `Rejected ${res.blocked_mbid.slice(0, 8)}… and added it to this album's blocklist.${rerun}`
        : `Cleared MusicBrainz match.${rerun}`;
      toast.success(toastMsg, { duration: 6000 });
    } catch (err) {
      toast.error((err as Error).message || "Couldn't clear MusicBrainz anchor");
    } finally {
      setClearingAnchor(false);
    }
  };

  return (
    <>
      {/* Panel head */}
      <div className="flex items-center justify-between gap-2 px-4 h-11 border-b border-line shrink-0">
        <div className="flex items-center gap-1.5 mono text-xs text-fg-3 min-w-0">
          <span aria-hidden="true">Library</span>
          <span className="text-fg-4" aria-hidden="true">▸</span>
          <h2
            id={headingId}
            ref={headingRef}
            tabIndex={-1}
            data-panel-heading=""
            className="text-fg-2 truncate font-normal rounded-sm outline-none focus-visible:ring-2 focus-visible:ring-accent"
            title={displayAlbum.title ?? displayAlbum.dir_name}
          >
            {displayAlbum.title || displayAlbum.dir_name}
          </h2>
        </div>
        <button
          type="button"
          onClick={onClose}
          className="w-8 h-8 shrink-0 grid place-items-center text-fg-3 hover:text-fg hover:bg-bg-3 rounded transition"
          aria-label="Close album panel"
          title="Close (Esc)"
        >
          <X className="w-4 h-4" />
        </button>
      </div>

      {/* Panel body — scrolls */}
      <div className="flex-1 overflow-y-auto">
        <AlbumHero album={displayAlbum} />

        {/* Missing-state banner — shown when the scanner didn't see
            this album's dir on disk. The row + cached metadata are
            still here (so the user can review what we knew); the
            footer swaps to a single Remove button. If this is a
            transient mount issue, the album will come back on its
            own at the next scan. */}
        {isMissing && (
          <div className="mx-4 my-3 rounded-md border border-warn/40 bg-warn-soft p-3 text-sm flex items-start gap-2.5">
            <CloudOff className="w-4 h-4 text-warn shrink-0 mt-0.5" />
            <div className="flex-1 min-w-0">
              <div className="font-medium text-fg">
                Missing from disk
                {displayAlbum.missing_since && (
                  <span className="text-fg-3 font-normal ml-1">
                    (since {formatMissingDate(displayAlbum.missing_since)})
                  </span>
                )}
              </div>
              <div className="text-xs text-fg-3 mt-1 leading-relaxed">
                The album directory isn't on disk right now. The
                cached metadata below is what we knew the last time
                we saw it. If you moved the file, unmounted the
                drive, or had a network blip, the album will come
                back automatically on the next scan — no action
                needed. If it's gone for good, "Remove from library"
                drops the cached metadata.
              </div>
            </div>
          </div>
        )}

        {/* In-flight job banner — shown while a job for THIS album is queued
            or running. Source of truth: the live jobs cache via SSE. Survives
            close/reopen of the sheet correctly. Queued jobs offer an inline
            "Cancel queued job" link so the user doesn't have to navigate to
            /jobs to back out of a click they regret. Running jobs can't be
            cancelled (worker is mid-call) so we just show status. */}
        {isQueued && activeJob && (
          <div className="mx-4 my-3 rounded-md border border-accent/40 bg-accent-soft p-3 text-sm flex items-center gap-2.5">
            <Loader2 className="w-4 h-4 spin text-accent shrink-0" />
            <div className="flex-1 min-w-0">
              <div className="font-medium text-fg">
                {activeJob.status === "queued" ? "Queued" : "Running"}
              </div>
              <div className="text-xs text-fg-3 mt-0.5">
                {jobTypeBlurb(activeJob.job_type)} · you can close this panel and we'll let you know
              </div>
            </div>
            {activeJob.status === "queued" && (
              <button
                onClick={onCancelQueued}
                disabled={deleteJob.isPending}
                className="h-7 px-2.5 border border-line bg-panel rounded text-xs text-fg-2 hover:bg-bg-3 hover:text-fg transition disabled:opacity-60 shrink-0"
              >
                {deleteJob.isPending ? "Cancelling…" : "Cancel"}
              </button>
            )}
          </div>
        )}

        {/* Failed-job banner — surfaces the most recent failure for this
            album so the user can Retry / inspect inline, instead of navigating
            to /jobs and finding the row. */}
        {recentFailedJob && (
          <div className="mx-4 my-3 rounded-md border border-err/40 bg-err-soft p-3 text-sm flex items-start gap-2.5">
            <AlertTriangle className="w-4 h-4 text-err shrink-0 mt-0.5" />
            <div className="flex-1 min-w-0">
              <div className="font-medium text-fg">Last attempt failed</div>
              {recentFailedJob.error && (
                <div
                  className="text-xs text-fg-3 mt-0.5 truncate"
                  title={recentFailedJob.error}
                >
                  {recentFailedJob.error}
                </div>
              )}
            </div>
            <div className="flex items-center gap-1.5 shrink-0">
              <button
                onClick={() => onRetryFailed(recentFailedJob.id)}
                disabled={retryJob.isPending || !canRetryFailed}
                title={
                  canRetryFailed
                    ? undefined
                    : "This job can be retried once MusicBrainz covers this album."
                }
                className="h-7 px-2.5 border border-line bg-panel rounded text-xs text-fg-2 hover:bg-bg-3 hover:text-fg transition disabled:opacity-60 inline-flex items-center gap-1"
              >
                <RotateCcw className="w-3 h-3" />
                Retry
              </button>
              <Link
                to="/jobs"
                className="h-7 px-2.5 border border-line bg-panel rounded text-xs text-fg-2 hover:bg-bg-3 hover:text-fg transition inline-flex items-center"
              >
                View in Jobs
              </Link>
            </div>
          </div>
        )}

        {/* Apply progress lives at top of body when active */}
        {showApplying && <ApplyProgress state={apply.state} />}

        {/* Suggestion mode replaces the regular sections */}
        {showSuggestions ? (
          <AISuggestions
            album={displayAlbum}
            metadata={metaResult}
            cover={coverResult}
            onAccept={onApply}
            onDiscard={onDiscard}
            onClearAnchor={onClearAnchor}
            clearingAnchor={clearingAnchor}
            applying={isApplying}
            coverPreviewSrc={activeJob ? jobCoverPreviewUrl(activeJob.id) : null}
            aiReady={ai.ready}
          />
        ) : (
          <>
            <Section id="album-info" title="Album info">
              <PropTable album={displayAlbum} aiReady={ai.ready} />
            </Section>

            {/* Findings (auto-hidden when this album has no open
                findings AND no MusicBrainz corrections to preview).
                Embeds the MB-preview deterministic apply UI at the
                top — those corrections are themselves a kind of
                finding (mb_canonical_mismatch produces them as
                findings rows too).

                Hidden on Missing albums: their open findings persist
                (PR 1 design choice — findings stay real across the
                Present → Missing transition), but every Fix path
                would fail at the FS layer. Findings will reappear
                when the album returns. */}
            {!isMissing && (
              <FindingsSection
                album={displayAlbum}
                hasActiveJob={!!activeJob}
                applying={isApplying}
                onApply={async (suggestions, trackSuggestions) => {
                  await startApply({
                    suggestions,
                    track_suggestions: trackSuggestions,
                    cover_url: null,
                  });
                }}
              />
            )}

            {/* My Tags — user's own taxonomy ("Cocktail Hour", "Christmas",
                "Instrumental"…) for shuffling and playlist construction.
                Distinct from FLAC GENRE; stored per-track in the cache + a
                hidden sidecar JSON in the album dir.

                Hidden on Missing albums: tag mutations write the
                sidecar JSON into the album dir, which doesn't exist. */}
            {!isMissing && (
              <MyTagsSection albumId={album.id} tracks={tracks.data?.tracks ?? []} />
            )}

            {/* Audio analysis — hidden on Missing albums (analysis
                reads the FLAC files, which aren't accessible). */}
            {!isMissing && (
              <Section
                id="audio-analysis"
                title="Audio analysis"
                meta={
                  tracks.data
                    ? <AnalysisMeta tracks={tracks.data.tracks} />
                    : null
                }
              >
                {tracks.data ? (
                  <AudioAnalysis albumId={album.id} tracks={tracks.data.tracks} />
                ) : (
                  <div className="px-4 py-3 text-xs text-fg-4">Loading…</div>
                )}
              </Section>
            )}

            <Section
              id="tracks"
              title="Tracks"
              defaultOpen
              meta={
                tracks.data
                  ? <span>{tracks.data.tracks.length} tracks</span>
                  : null
              }
            >
              {tracks.data ? (
                <TrackList
                  album={displayAlbum}
                  tracks={tracks.data.tracks}
                  applyState={showApplying ? apply.state.filesByIndex : undefined}
                  resolution={resolution}
                />
              ) : (
                <div className="px-4 py-3 text-xs text-fg-4">Loading tracks…</div>
              )}
            </Section>
          </>
        )}
      </div>

      {/* Hint dialog — gates each AI call. The mode mirrors the pending
          job type 1:1; HintDialog's prior-attempts banner already
          conditions itself on (mode === "cover" || "metadata+cover"). */}
      <HintDialog
        open={pendingType !== null}
        mode={pendingType ?? "metadata"}
        albumTitle={displayAlbum.title || displayAlbum.dir_name}
        priorCoverAttempts={
          pendingType === "cover" || pendingType === "metadata+cover"
            ? priorCoverAttempts
            : []
        }
        // Drives whether the "Include album cover" checkbox renders
        // in the dialog. Toggle is meaningful only for metadata jobs
        // on albums that actually have a cover to attach.
        hasCover={albumHasCover}
        onSubmit={onHintSubmit}
        onCancel={onHintCancel}
      />

      {/* Panel footer */}
      <div className="flex items-center gap-2 h-12 px-3 border-t border-line bg-panel-2 shrink-0">
        {isApplying ? (
          <span className="text-xs text-fg-3 mono truncate">
            Writing — you can switch albums or close the panel
          </span>
        ) : isMissing ? (
          // Missing albums get a single destructive affordance + a
          // hint about the no-op recovery path. Apply / AI / cover
          // upload would all fail at the FS layer for an album whose
          // dir isn't on disk, so we don't even surface them.
          <>
            <span className="text-xs text-fg-3 mono truncate">
              Re-mount the drive to bring it back, or remove it for good.
            </span>
            <div className="flex-1" />
            <button
              onClick={onRemoveFromLibrary}
              disabled={removing}
              className="h-8 px-3 rounded border border-err/40 bg-err-soft text-err text-sm font-medium hover:bg-err/15 transition disabled:opacity-60 inline-flex items-center gap-1.5 whitespace-nowrap shrink-0"
              title="Drop this album from the cache. Cascades to its fingerprints + track tags + per-album blocklists."
            >
              {removing ? (
                <><Loader2 className="w-3.5 h-3.5 spin" /> Removing…</>
              ) : (
                <><Trash2 className="w-3.5 h-3.5" /> Remove from library</>
              )}
            </button>
          </>
        ) : (
          <>
            {/* Buttons disabled while a job exists for this album (queued,
                running, or completed-not-yet-acted). One job per album. */}
            <button
              onClick={onSuggestCover}
              disabled={!!activeJob || !canQueueCover}
              className="h-8 px-3 border border-line rounded text-sm text-fg-2 hover:bg-bg-3 transition disabled:opacity-60 inline-flex items-center gap-1.5 whitespace-nowrap shrink-0"
              title={
                // No AI wording when no model can run.
                activeJob
                  ? ai.ready
                    ? "An AI job already exists for this album — view it above"
                    : "A job already exists for this album — view it above"
                  : !canQueueCover
                  ? "A cover can be fetched once the Cover Art Archive has one for this album's MusicBrainz match."
                  : coverFromMb
                  ? ai.ready
                    ? "Cover already verified in Cover Art Archive — no AI quota burned"
                    : "Cover already verified in Cover Art Archive"
                  : undefined
              }
            >
              <ImageIcon className="w-3.5 h-3.5 shrink-0" />
              <span>{coverFromMb ? "Get cover from MusicBrainz" : "Find cover art"}</span>
            </button>
            <button
              onClick={onSuggestMetadata}
              disabled={!!activeJob || !canQueueMetadata}
              className="h-8 px-3 rounded bg-accent text-accent-fg font-medium text-sm hover:opacity-90 transition disabled:opacity-60 inline-flex items-center gap-1.5 whitespace-nowrap shrink-0"
              title={
                activeJob
                  ? ai.ready
                    ? "An AI job already exists for this album — view it above"
                    : "A job already exists for this album — view it above"
                  : !canQueueMetadata
                  ? "Corrections can be found once MusicBrainz identifies this album and every track."
                  : deterministicCovered
                  ? ai.ready
                    ? "MusicBrainz already covers this album — suggestions appear instantly, no AI quota burned"
                    : "MusicBrainz already covers this album — suggestions appear instantly"
                  : metadataJob === "metadata+cover"
                  ? "No cover on disk — will also search for cover art"
                  : undefined
              }
            >
              <Sparkles className="w-3.5 h-3.5 shrink-0" />
              <span>
                {deterministicCovered
                  ? "Find MusicBrainz corrections"
                  : `Suggest fixes${metadataJob === "metadata+cover" && !activeJob ? " (+ cover)" : ""}`}
              </span>
            </button>
            <div className="flex-1" />
            <MoreMenu disabled={!!activeJob} />
          </>
        )}
      </div>

    </>
  );
}

function jobTypeBlurb(t: JobType): string {
  switch (t) {
    case "metadata":       return "Identifying album & proposing metadata corrections";
    case "cover":          return "Looking up cover art on Cover Art Archive";
    case "metadata+cover": return "Identifying album, proposing corrections, and looking up cover art";
  }
}

function AnalysisMeta({ tracks }: { tracks: { bpm: number | null; replaygain_track_gain: string | null }[] }) {
  if (tracks.length === 0) return null;
  const bpm = tracks.filter((t) => t.bpm !== null).length;
  const rg = tracks.filter((t) => t.replaygain_track_gain).length;
  const total = tracks.length;
  return (
    <span className="flex items-center gap-1.5">
      <Pill done={bpm === total && total > 0}>BPM {bpm === total && total > 0 ? "✓" : "—"}</Pill>
      <Pill done={rg === total && total > 0}>RG {rg === total && total > 0 ? "✓" : "—"}</Pill>
    </span>
  );
}

function Pill({ children, done }: { children: React.ReactNode; done: boolean }) {
  return (
    <span className={`mono text-[10px] px-1.5 py-0.5 rounded-sm border ${done ? "bg-ok-soft text-ok border-ok/30" : "bg-bg-3 text-fg-4 border-line"}`}>
      {children}
    </span>
  );
}

function formatMissingDate(iso: string): string {
  try {
    const d = new Date(iso);
    return d.toLocaleDateString();
  } catch {
    return iso;
  }
}
