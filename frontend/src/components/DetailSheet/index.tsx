import { AlertTriangle, ArrowLeft, CheckCircle2, CloudOff, Loader2, RotateCcw, Sparkles, Trash2, X } from "lucide-react";
import { useEffect, useId, useRef, useState, type Ref } from "react";
import { Link } from "react-router-dom";
import { toast } from "sonner";

import AlbumHero from "./AlbumHero";
import PropTable from "./PropTable";
import AudioAnalysis from "./AudioAnalysis";
import TrackList from "./TrackList";
import CorrectionsDiff, { CORRECTIONS_ID } from "./CorrectionsDiff";
import { resolverOffersCover, useCorrections } from "./corrections";
import ApplyProgress from "./ApplyProgress";
import FindingsSection from "./FindingsSection";
import MoreMenu from "./MoreMenu";
import MyTagsSection from "./MyTagsSection";
import Section from "./Section";
import { ResearchGroup, RequestResearch } from "../ResearchGroup";
import HintDialog from "./HintDialog";
import RemoveAlbumDialog from "./RemoveAlbumDialog";
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
import { canQueueJob, modelFreeCoverage, offersHintEntry } from "@/lib/aiGating";
import { useApplyStream } from "@/hooks/useApplyStream";
import { useResizableWidth } from "@/hooks/useResizableWidth";
import { removeAlbumFromLibrary } from "@/lib/api";
import { isApplyInFlight } from "@/lib/applyRegistry";
import type { Album, AlbumResolution, Job, JobConflictDetail, JobType } from "@/lib/types";

export interface AlbumPanelProps {
  /** The album to show. */
  album: Album;
  /** Close the panel (the owner decides where that leads). */
  onClose: () => void;
  /** Where closing leads, when not to the list beside it ("Back to Track
   *  gaps"): shown as the panel's return action. */
  returnLabel?: string;
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
  returnLabel,
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
        returnLabel={returnLabel}
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
  returnLabel,
  headingId,
  headingRef,
}: {
  album: Album;
  onAlbumUpdated?: (a: Album) => void;
  onClose: () => void;
  returnLabel?: string;
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

  // The active job for THIS album (queued, running, or finished and not
  // yet applied or discarded). Sourced from the live jobs cache (kept fresh
  // by the App-level useJobsStream SSE subscriber), so close/reopen of the
  // panel preserves state and the panel updates the moment the worker
  // finishes.
  const activeJob = useActiveJobForAlbum(album.id);

  // Past cover-art lookups for this album that didn't yield a cover —
  // surfaced in the HintDialog when the user is about to queue another
  // AI cover search, so they don't repeat past dead ends.
  const priorCoverAttempts = usePriorCoverAttempts(album.id);

  const { data: resolution } = useAlbumResolution(album.id);
  // The worker's thresholds for skipping the model (lib/aiGating.ts);
  // only a failed job's Retry still asks.
  const coverage = modelFreeCoverage(resolution);
  // Whether a model backend can run. Every AI-only entry point (the hint
  // dialog and what opens it) is offered only then; without one the panel
  // is complete with the resolver's corrections.
  const ai = useAiStatus();

  // Hint dialog state. `pendingType` is the job type the user is about to
  // enqueue once they submit a hint.
  const [pendingType, setPendingType] = useState<JobType | null>(null);
  // "Remove from library" confirmation (Missing albums only).
  const [confirmRemove, setConfirmRemove] = useState(false);
  // The user asked for the MusicBrainz cover on an album that has one.
  const [coverRequested, setCoverRequested] = useState(false);
  // The user discarded the resolver's corrections; they come back when the
  // album is opened again.
  const [resolverDismissed, setResolverDismissed] = useState(false);

  // Reset per-album local state when switching albums.
  useEffect(() => {
    setPendingType(null);
    setConfirmRemove(false);
    setCoverRequested(false);
    setResolverDismissed(false);
  }, [album.id]);

  // Missing (`missing_since` set): the scan did not find the album's folder.
  // The backend refuses every write to it (apply, cover, My Tags, analysis:
  // 409) and reads tracks from disk, so the panel keeps only what is stored
  // (hero, album info) plus "Remove from library".
  const isMissing = !!(apply.state.updatedAlbum ?? album).missing_since;
  const isApplying = isApplyInFlight(apply.state);
  const showApplying = isApplying || apply.state.status === "done" || apply.state.status === "error";

  // Live album for hero — use the apply-updated version once available.
  const displayAlbum: Album = apply.state.updatedAlbum ?? album;

  // The proposed corrections, from a finished job or else from the
  // resolver; one diff, one set of ticks, one apply. A job's result for a
  // Missing album could not be applied (apply answers 409), so it is not
  // shown here; Review still lists it.
  const corrections = useCorrections({
    album: displayAlbum,
    activeJob: activeJob?.result?.delegated ? null : activeJob,
    resolution,
    tracks: tracks.data?.tracks ?? (tracks.isError ? [] : undefined),
    coverRequested,
    resolverDismissed,
    suppressed: isMissing || isApplying,
  });
  const reviewing = corrections.source !== null;
  const reviewedJob = corrections.job;
  const isQueued = !isMissing && (activeJob?.status === "queued" || activeJob?.status === "running");

  // Most recent failed job for this album that hasn't been acted on. Surfaced
  // in the banner so the user can retry / inspect the error inline rather
  // than navigating to Review and hunting the row.
  const recentFailedJob =
    !activeJob && !isApplying && !isMissing
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

  // ---- AI-only entry points (offered only when AI is ready) ----

  const aiEntry = offersHintEntry(ai) && !isMissing && !isApplying;
  const onAskAi = (which: JobType) => {
    if (!offersHintEntry(ai) || activeJob) return;
    setPendingType(which);
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

  // ---- The cover ----

  // "Use MusicBrainz cover" adds the cover row to the corrections, where it
  // is previewed and applied with the rest. Offered while nothing else is
  // under review from a job, when the resolver has a cover and it is not
  // already proposed (an album with no cover gets the row on its own).
  const coverAlreadyProposed = corrections.rows.some((r) => r.kind === "cover");
  const offerMusicBrainzCover =
    !isMissing && !isApplying && !activeJob && resolverOffersCover(resolution) && !coverAlreadyProposed;
  const onUseMusicBrainzCover = () => {
    setCoverRequested(true);
    setResolverDismissed(false);
    requestAnimationFrame(() =>
      document.getElementById(CORRECTIONS_ID)?.scrollIntoView?.({ block: "nearest", behavior: "smooth" }),
    );
  };

  // ---- Apply / Discard (the footer) ----

  // When an apply finishes successfully — even if the panel has since moved
  // to another album or closed — propagate the updated album to the owner
  // (so the tile re-renders with new cover/title/etc.) and, for a job's
  // proposal, mark that job applied so it leaves the queue and the "needs
  // review" badge.
  //
  // Cache invalidation is handled by the App-level useLibraryEventsStream
  // subscriber — the apply pipeline publishes AlbumTagsChanged (+
  // AlbumCoverChanged when art changed), which fans out to
  // ["albums-snapshot"], ["tracks", id], ["album-resolution", id],
  // ["findings"], ["checks"] on its own.
  const onApply = () => {
    if (corrections.selectedCount === 0 || isApplying) return;
    const jobId = reviewedJob?.id ?? null;
    const payload = corrections.payload();
    // The requested cover is part of this apply; once written, the album
    // has it and nothing is left to ask for.
    setCoverRequested(false);
    void apply.start(payload, {
      onUpdatedAlbum: (updated) => {
        onAlbumUpdated?.(updated);
        if (jobId) markActedOn.mutate({ jobId, action: "applied" });
      },
    });
  };

  const onDiscard = () => {
    if (reviewedJob) {
      markActedOn.mutate({ jobId: reviewedJob.id, action: "discarded" });
    } else {
      setResolverDismissed(true);
    }
    setCoverRequested(false);
  };

  // "Remove from library" (`DELETE /api/albums/{id}`): the only path that
  // drops the album's row, with its fingerprints, My Tags and MusicBrainz
  // blocklist. Confirmed in RemoveAlbumDialog; on success the panel closes.
  const [removing, setRemoving] = useState(false);
  const onRemoveFromLibrary = async () => {
    if (removing) return;
    setRemoving(true);
    try {
      await removeAlbumFromLibrary(album.id);
      toast.success("Removed from library", { duration: 3000 });
      // The album_id is gone from the snapshot now; useLibraryEvents-
      // Stream's AlbumDeleted handler already invalidates ['albums-
      // snapshot'], so closing lands on the refreshed list with this
      // album absent.
      setConfirmRemove(false);
      onClose();
    } catch (err) {
      toast.error((err as Error).message || "Couldn't remove album");
    } finally {
      setRemoving(false);
    }
  };

  const footerHasContent = isApplying || isMissing || reviewing || aiEntry;

  return (
    <>
      {/* Panel head */}
      <div className="flex items-center justify-between gap-2 px-4 h-11 border-b border-line shrink-0">
        <div className="flex items-center gap-1.5 mono text-xs text-fg-3 min-w-0">
          {returnLabel ? (
            // Opened from another page: say where closing leads.
            <button
              type="button"
              onClick={onClose}
              data-panel-return=""
              className="inline-flex items-center gap-1 h-7 px-1.5 -ml-1.5 rounded text-accent hover:bg-bg-3 shrink-0 max-w-[60%] focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
              title={`${returnLabel} (Esc)`}
            >
              <ArrowLeft className="w-3.5 h-3.5 shrink-0" />
              <span className="truncate">{returnLabel}</span>
            </button>
          ) : (
            <span aria-hidden="true">Library</span>
          )}
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
          title={returnLabel ? `Close: ${returnLabel.replace(/^Back/, "back")} (Esc)` : "Close (Esc)"}
        >
          <X className="w-4 h-4" />
        </button>
      </div>

      {/* Panel body — scrolls */}
      <div className="flex-1 overflow-y-auto">
        <AlbumHero
          album={displayAlbum}
          onUseMusicBrainzCover={offerMusicBrainzCover ? onUseMusicBrainzCover : undefined}
          onAskAiForCover={aiEntry && !activeJob ? () => onAskAi("cover") : undefined}
        />

        {/* Missing: one message. What it says rests on the scan: a folder
            back at the same path is the same album id, so its row (and
            everything attached) is Present again; a folder at a new path
            hashes to a new id and is scanned in as a separate album. */}
        {isMissing && displayAlbum.missing_since && (
          <MissingNotice since={displayAlbum.missing_since} path={displayAlbum.path} />
        )}

        {/* In-flight job banner — shown while a job for THIS album is queued
            or running. Queued jobs offer an inline Cancel; running jobs
            can't be cancelled (the worker is mid-call). */}
        {activeJob?.result?.delegated && <ResearchGroup job={activeJob} />}
        {!activeJob && !ai.ready && !isMissing && !isApplying && <RequestResearch onRequest={(kind,hint)=>queueJob(kind,hint,false)} pending={enqueue.isPending} />}
        {isQueued && activeJob && !activeJob.result?.delegated && (
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
            to Review and finding the row. */}
        {recentFailedJob?.result?.delegated && <ResearchGroup job={recentFailedJob} />}
        {recentFailedJob && !recentFailedJob.result?.delegated && (
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
                to={`/review?job=${encodeURIComponent(recentFailedJob.id)}`}
                className="h-7 px-2.5 border border-line bg-panel rounded text-xs text-fg-2 hover:bg-bg-3 hover:text-fg transition inline-flex items-center"
              >
                View in Review
              </Link>
            </div>
          </div>
        )}

        {/* Apply progress lives at top of body when active */}
        {showApplying && <ApplyProgress state={apply.state} />}

        {/* The proposed corrections: the one place they are shown and
            picked. Apply / Discard are in the footer. */}
        {reviewing && (
          <CorrectionsDiff
            album={displayAlbum}
            title={
              corrections.source === "job" && ai.ready
                ? "Suggested corrections"
                : "MusicBrainz corrections"
            }
            meta={correctionsMeta(corrections.source, reviewedJob, resolution)}
            rows={corrections.rows}
            isSelected={corrections.isSelected}
            isSelectable={corrections.isSelectable}
            onToggle={corrections.toggle}
            coverPreview={corrections.coverPreview}
            onRetryCoverPreview={corrections.retryCoverPreview}
            emptyMessage={reviewedJob ? jobEmptyMessage(reviewedJob) : undefined}
            notes={reviewedJob ? jobNotes(reviewedJob) : null}
            disabled={isApplying}
          />
        )}

        <Section id="album-info" title="Album info">
          <PropTable album={displayAlbum} aiReady={ai.ready} />
        </Section>

        {/* Findings. Hidden on Missing albums: their open findings persist
            (findings stay real across the Present → Missing transition),
            but every Fix path would fail at the FS layer. Findings will
            reappear when the album returns. */}
        {!isMissing && (
          <FindingsSection
            album={displayAlbum}
            genreRowShown={corrections.rows.some((r) => r.key === "album:genre")}
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

        {/* Tracks are read from the files, so a Missing album has none
            to show; the hero keeps the stored track count. */}
        {!isMissing && (
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
        )}
      </div>

      {/* Hint dialog — the AI entry points' only door; never opened
          without a ready AI backend. */}
      <HintDialog
        open={pendingType !== null && offersHintEntry(ai)}
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
        hasCover={displayAlbum.has_embedded_cover || !!displayAlbum.cover_file}
        onSubmit={onHintSubmit}
        onCancel={() => setPendingType(null)}
      />

      {/* Panel footer: the panel's one primary action. Shown only when it
          has something to offer. */}
      {footerHasContent && (
      <div className="flex items-center gap-2 h-12 px-3 border-t border-line bg-panel-2 shrink-0">
        {isApplying ? (
          <span className="text-xs text-fg-3 mono truncate">
            Writing — you can switch albums or close the panel
          </span>
        ) : isMissing ? (
          <>
            <div className="flex-1" />
            <button
              type="button"
              onClick={() => setConfirmRemove(true)}
              disabled={removing}
              className="h-8 px-3 rounded border border-err/40 bg-err-soft text-err text-sm font-medium hover:bg-err/15 transition disabled:opacity-60 inline-flex items-center gap-1.5 whitespace-nowrap shrink-0"
            >
              <Trash2 className="w-3.5 h-3.5" /> Remove from library…
            </button>
          </>
        ) : (
          <>
            {aiEntry && (
              <button
                type="button"
                onClick={() => onAskAi("metadata")}
                disabled={!!activeJob}
                className="h-8 px-3 border border-line rounded text-sm text-fg-2 hover:bg-bg-3 transition disabled:opacity-60 inline-flex items-center gap-1.5 whitespace-nowrap shrink-0"
                title={activeJob ? "An AI job already exists for this album" : undefined}
              >
                <Sparkles className="w-3.5 h-3.5 shrink-0" />
                <span>Ask AI for corrections…</span>
              </button>
            )}
            <div className="flex-1" />
            <MoreMenu disabled={!!activeJob} />
            {reviewing && (
              <>
                <button
                  type="button"
                  onClick={onDiscard}
                  className="h-8 px-3 border border-line rounded text-sm text-fg-2 hover:bg-bg-3 transition inline-flex items-center whitespace-nowrap shrink-0"
                  title={
                    reviewedJob
                      ? "Discard this job's proposal"
                      : "Hide these corrections until the album is opened again"
                  }
                >
                  Discard
                </button>
                {corrections.rows.length > 0 && (
                  <button
                    type="button"
                    onClick={onApply}
                    disabled={corrections.selectedCount === 0}
                    className="h-8 px-3 rounded bg-accent text-accent-fg font-medium text-sm hover:opacity-90 transition disabled:opacity-60 inline-flex items-center gap-1.5 whitespace-nowrap shrink-0"
                  >
                    <CheckCircle2 className="w-3.5 h-3.5 shrink-0" />
                    {applyLabel(corrections.selectedCount)}
                  </button>
                )}
              </>
            )}
          </>
        )}
      </div>
      )}

      <RemoveAlbumDialog
        open={confirmRemove}
        albumTitle={displayAlbum.title || displayAlbum.dir_name}
        removing={removing}
        onConfirm={() => void onRemoveFromLibrary()}
        onCancel={() => setConfirmRemove(false)}
      />
    </>
  );
}

function applyLabel(n: number): string {
  return n === 0 ? "Apply" : `Apply ${n} change${n === 1 ? "" : "s"}`;
}

function correctionsMeta(
  source: "resolver" | "job" | null,
  job: Job | null,
  resolution: AlbumResolution | undefined,
): string | undefined {
  if (source === "resolver" && resolution?.anchor) {
    const strategy = resolution.anchor_strategy !== "none" ? ` (${resolution.anchor_strategy})` : "";
    return `source: ${resolution.anchor.source}${strategy}`;
  }
  if (source === "job" && job) {
    const confidence = job.result?.metadata?.confidence ?? job.result?.cover?.confidence;
    return confidence ? `confidence: ${confidence}` : undefined;
  }
  return undefined;
}

/** What a finished job that proposes nothing says, by what it looked for. */
function jobEmptyMessage(job: Job): string {
  const looked = { metadata: !!job.result?.metadata, cover: !!job.result?.cover };
  if (looked.metadata && looked.cover) return "No corrections to apply, and no cover was found.";
  if (looked.cover) return "No cover was found for this album.";
  return "No corrections to apply.";
}

function jobNotes(job: Job): string | null {
  const notes = [job.result?.metadata?.notes, job.result?.cover?.notes]
    .map((n) => n?.trim())
    .filter((n): n is string => !!n);
  return notes.length > 0 ? [...new Set(notes)].join(" ") : null;
}

function MissingNotice({ since, path }: { since: string; path: string }) {
  return (
    <div
      role="status"
      className="mx-4 my-3 rounded-md border border-warn/40 bg-warn-soft p-3 text-sm flex items-start gap-2.5"
    >
      <CloudOff className="w-4 h-4 text-warn shrink-0 mt-0.5" aria-hidden="true" />
      <div className="flex-1 min-w-0">
        <div className="font-medium text-fg">Folder not found</div>
        <p className="text-xs text-fg-3 mt-1 leading-relaxed">
          The library scan on {formatMissingDate(since)} did not find this album at{" "}
          <code className="mono text-fg-2 break-all">{path}</code>. Musaic still has
          what it recorded before then, shown below.
        </p>
        <p className="text-xs text-fg-3 mt-1.5 leading-relaxed">
          If the folder is back at that path, the next scan brings the album back
          with its MusicBrainz match, fingerprints and My Tags. A folder moved or
          renamed inside the library is added as a separate album, and this one
          stays here until you remove it.
        </p>
      </div>
    </div>
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
